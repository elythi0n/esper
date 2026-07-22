//! Port forwarding: one TCP connection, one QUIC stream.
//!
//! A forward reaches a service on the host that is not on the network — a
//! database bound to `127.0.0.1`, a web admin panel, a dev server. The shape
//! is `ssh -L`'s, because that is the shape everybody already has in their
//! hands.
//!
//! # What a forward may reach
//!
//! **The host's loopback, and nothing else, unless the host's owner named
//! something wider.** ssh forwards anywhere by default and is right to: a
//! login already grants the whole machine, so a forward adds no reach. Esper's
//! `forward` can be granted *without* `terminal` or `control`, so an
//! unrestricted forward would make the narrowest grant the one with the widest
//! reach — every machine on the host's LAN, through a host that belongs to
//! somebody else.
//!
//! So targets are matched against [`Allowlist`] **before anything is
//! resolved**, and the dial happens in [`Allowlist::dial`] and nowhere else.
//! That is not a stylistic preference: a check that lives somewhere other than
//! the dial is a check a later caller can forget.
//!
//! # No framing over the payload
//!
//! [`ForwardOpen`] is the first frame on the stream and the last framed thing
//! on it. After the reply the stream is the connection's bytes. QUIC already
//! gives every stream its own flow control, so a slow reader stalls its own
//! connection and nothing else; a second framing layer on top would be a
//! second, worse copy of that.

use std::net::SocketAddr;

use esper_proto::{ForwardOpen, ForwardRefusal, ForwardReply};
use quinn::{RecvStream, SendStream};
use tokio::io::AsyncWriteExt as _;
use tokio::net::TcpStream;

use crate::{Control, ControlError};

/// How much each direction of a forwarded connection copies at a time.
///
/// Two of these exist per connection. Large enough that a bulk transfer is not
/// a syscall per few kilobytes, small enough that a session's worth of idle
/// forwards is not measured in megabytes.
const COPY_BUFFER: usize = 32 * 1024;

/// How long the host waits for a target to answer.
///
/// A dial that hangs holds a stream and a slot in the connection cap. Shorter
/// than any sensible session timeout, longer than a loopback connect — which
/// is the case this exists for — by three orders of magnitude.
const DIAL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// What can go wrong forwarding a connection.
#[derive(Debug, thiserror::Error)]
pub enum ForwardError {
    /// The host refused, and the peer has been told the same reason.
    #[error("{}", .0.message())]
    Refused(ForwardRefusal),

    /// The control stream failed.
    #[error("forward stream: {0}")]
    Control(String),

    /// The local socket failed.
    #[error("{0}")]
    Io(#[from] std::io::Error),
}

impl From<ControlError> for ForwardError {
    fn from(e: ControlError) -> Self {
        Self::Control(e.to_string())
    }
}

/// One entry in a host's forwarding allowlist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rule {
    /// The host, as written. Compared case-insensitively.
    host: String,
    /// The port, or `None` for any port.
    port: Option<u16>,
    /// Whether what this rule matches must still resolve to a loopback
    /// address.
    ///
    /// True only for the rules that exist because nobody wrote any. `localhost`
    /// is a name, and a name is whatever the host's resolver says it is — so
    /// the default guarantee, "loopback and nothing else", is checked against
    /// the address that came back rather than against the name that was asked
    /// for.
    loopback_only: bool,
}

/// The targets a host will forward to.
///
/// Empty means the default: the three ways of writing loopback, any port. It
/// is never "everything" — a list nobody wrote is the safe list, not the
/// absent one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Allowlist {
    rules: Vec<Rule>,
}

impl Default for Allowlist {
    fn default() -> Self {
        Self::loopback_only()
    }
}

impl Allowlist {
    /// The default: this machine's own loopback, any port.
    #[must_use]
    pub fn loopback_only() -> Self {
        let rules = ["localhost", "127.0.0.1", "::1"]
            .into_iter()
            .map(|host| Rule {
                host: host.to_owned(),
                port: None,
                loopback_only: true,
            })
            .collect();
        Self { rules }
    }

    /// Read a list somebody wrote, as `host:port` entries separated by commas.
    ///
    /// `*` as the port means any port on that host. An IPv6 literal is
    /// bracketed — `[::1]:5432` — because `::1:5432` cannot be split at its
    /// last colon and mean one thing.
    ///
    /// An empty or blank list is [`Allowlist::loopback_only`], which is the
    /// point: a host with nothing configured still forwards to its own
    /// services, and still forwards nowhere else.
    pub fn parse(spec: &str) -> Result<Self, String> {
        let mut rules = Vec::new();
        for entry in spec.split(',').map(str::trim).filter(|e| !e.is_empty()) {
            rules.push(Self::rule(entry)?);
        }
        if rules.is_empty() {
            return Ok(Self::loopback_only());
        }
        Ok(Self { rules })
    }

    /// One entry.
    fn rule(entry: &str) -> Result<Rule, String> {
        let (host, port) = split_target(entry)
            .ok_or_else(|| format!("{entry:?} is not host:port — try 127.0.0.1:5432"))?;
        if host.is_empty() {
            return Err(format!("{entry:?} has no host"));
        }
        // **There is no wildcard host.** A port may be `*`, because "this
        // service, whichever port it moves to" is a real thing to mean. A host
        // may not, because "anywhere this machine can reach" is the
        // unrestricted forward this design refused — and an escape hatch that
        // exists is one somebody eventually writes to make a problem go away.
        // A range of machines is a feature to add deliberately, with a syntax
        // that says what it covers, not a `*` that says everything.
        if host == "*" {
            return Err(format!(
                "{entry:?}: there is no wildcard host — name the machines a forward may reach"
            ));
        }
        let port = if port == "*" {
            None
        } else {
            let n: u16 = port
                .parse()
                .map_err(|_| format!("{entry:?}: {port:?} is not a port number or *"))?;
            if n == 0 {
                return Err(format!("{entry:?}: port 0 is not a port"));
            }
            Some(n)
        };
        Ok(Rule {
            host: host.to_ascii_lowercase(),
            port,
            loopback_only: false,
        })
    }

    /// The rule that permits this target, if one does.
    ///
    /// Matched against the string the controller sent, before any lookup: a
    /// name the host's owner did not write is never resolved, let alone
    /// dialled.
    #[must_use]
    pub fn allows(&self, host: &str, port: u16) -> Option<&Rule> {
        let wanted = host.to_ascii_lowercase();
        self.rules
            .iter()
            .find(|r| (r.host == "*" || r.host == wanted) && r.port.is_none_or(|p| p == port))
    }

    /// What this list permits, for a log line or `config show`.
    #[must_use]
    pub fn describe(&self) -> String {
        self.rules
            .iter()
            .map(|r| match r.port {
                Some(p) => format!("{}:{p}", r.host),
                None => format!("{}:*", r.host),
            })
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// Connect to a target, if this list permits it.
    ///
    /// **The only place a forwarded connection is dialled.** The check and the
    /// dial are one function so that no future caller can have the second
    /// without the first.
    ///
    /// The order matters and is the whole security property:
    ///
    /// 1. the request is checked for shape,
    /// 2. the target as written is matched against this list,
    /// 3. only then is the name resolved,
    /// 4. and for the default rules, only loopback addresses survive the
    ///    lookup — so a `localhost` that resolves somewhere else is refused
    ///    rather than dialled.
    pub async fn dial(&self, open: &ForwardOpen) -> Result<TcpStream, ForwardRefusal> {
        if !open.is_well_formed() {
            return Err(ForwardRefusal::Malformed);
        }
        let Some(rule) = self.allows(&open.host, open.port) else {
            return Err(ForwardRefusal::NotAllowed);
        };

        let allowed = permitted(rule, resolve(&open.host, open.port).await?);
        if allowed.is_empty() {
            // The name resolved, and to nothing this rule covers. Not a dial
            // failure: nothing was dialled, and calling it one would tell the
            // peer that a target it may not reach exists.
            return Err(ForwardRefusal::NotAllowed);
        }

        let mut last = None;
        for address in allowed {
            match tokio::time::timeout(DIAL_TIMEOUT, TcpStream::connect(address)).await {
                Ok(Ok(stream)) => {
                    // Interactive traffic through a tunnel is the case this
                    // exists for, and Nagle's algorithm adds up to a round trip
                    // of delay to every small write.
                    let _ = stream.set_nodelay(true);
                    return Ok(stream);
                }
                Ok(Err(e)) => last = Some(e.to_string()),
                Err(_) => last = Some("timed out".to_owned()),
            }
        }
        tracing::debug!(target = %open.target(), reason = ?last, "forward dial failed");
        Err(ForwardRefusal::DialFailed)
    }
}

/// The addresses a rule actually permits, out of what the name resolved to.
///
/// Its own function because it is the second half of the default guarantee and
/// the half that cannot be tested through a real lookup: proving that a name
/// resolving off loopback is refused would otherwise need a resolver that says
/// so.
fn permitted(rule: &Rule, addresses: Vec<SocketAddr>) -> Vec<SocketAddr> {
    addresses
        .into_iter()
        .filter(|a| !rule.loopback_only || a.ip().is_loopback())
        .collect()
}

/// Look a target up, without letting the lookup block the runtime.
async fn resolve(host: &str, port: u16) -> Result<Vec<SocketAddr>, ForwardRefusal> {
    let target = if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    };
    match tokio::net::lookup_host(target).await {
        Ok(addrs) => Ok(addrs.collect()),
        Err(_) => Err(ForwardRefusal::DialFailed),
    }
}

/// Split `host:port`, accepting a bracketed IPv6 literal.
fn split_target(entry: &str) -> Option<(&str, &str)> {
    if let Some(rest) = entry.strip_prefix('[') {
        let (host, after) = rest.split_once(']')?;
        return Some((host, after.strip_prefix(':')?));
    }
    let (host, port) = entry.rsplit_once(':')?;
    // An unbracketed second colon is an IPv6 literal somebody wrote without
    // brackets, and guessing which colon they meant is how a rule ends up
    // permitting a different address than it reads as.
    if host.contains(':') {
        return None;
    }
    Some((host, port))
}

/// How many forwarded connections a session may hold at once.
///
/// Each one is a QUIC stream, a socket on the host and two copy buffers, so a
/// session that opened them without limit would exhaust a host's file
/// descriptors from the other side of the world.
///
/// A **guard**, not a counter: the slot is given back when the connection's
/// task ends, however it ends. A pair of `fetch_add`/`fetch_sub` around a copy
/// loop leaks a slot every time the loop ends in a way somebody did not think
/// of, and a cap that leaks is a cap that eventually refuses everything.
#[derive(Debug, Clone)]
pub struct Slots {
    open: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    most: usize,
}

/// One connection's place, given back when this is dropped.
#[derive(Debug)]
pub struct Slot {
    open: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

impl Drop for Slot {
    fn drop(&mut self) {
        self.open.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
    }
}

impl Slots {
    /// Room for `most` connections at once.
    #[must_use]
    pub fn new(most: usize) -> Self {
        Self {
            open: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            most,
        }
    }

    /// Take a slot, if there is one.
    ///
    /// Taken **before** the dial: a resource cap checked after the resource
    /// exists is not a cap. The exchange is a compare-and-swap rather than a
    /// load and a store, because two streams arriving at once would otherwise
    /// both read the same count and both be let through.
    pub fn take(&self) -> Option<Slot> {
        use std::sync::atomic::Ordering;
        let mut open = self.open.load(Ordering::Relaxed);
        loop {
            if open >= self.most {
                return None;
            }
            match self.open.compare_exchange_weak(
                open,
                open.saturating_add(1),
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => {
                    return Some(Slot {
                        open: std::sync::Arc::clone(&self.open),
                    });
                }
                Err(actual) => open = actual,
            }
        }
    }

    /// How many are open now.
    #[must_use]
    pub fn open(&self) -> usize {
        self.open.load(std::sync::atomic::Ordering::Relaxed)
    }
}

/// What a host decided about one forward request.
#[derive(Debug)]
pub enum Decision {
    /// Connected, with the session's slot held for as long as it lasts.
    Open(TcpStream, Slot),
    /// Refused, and why. Nothing was dialled unless the reason says so.
    Refuse(ForwardRefusal),
}

/// Everything a host checks about one forward request, in one place.
///
/// The order is the security property, and it is why this is a function rather
/// than a sequence somebody writes at each call site:
///
/// 1. **The permission, now.** Not as it stood when the session opened — a
///    revocation has to reach a session that is already running.
/// 2. **A slot**, taken rather than counted, before anything is dialled.
/// 3. **The allowlist**, matched against the target as written, and only then
///    the lookup and the dial.
///
/// A caller that got any of that in the wrong order would have a working
/// forward and a broken guarantee, and nothing would fail until somebody
/// looked.
pub async fn decide(
    open: &ForwardOpen,
    allow: &Allowlist,
    slots: &Slots,
    grants: &dyn crate::Grants,
) -> Decision {
    if !grants.allows(esper_proto::Permission::Forward) {
        return Decision::Refuse(ForwardRefusal::NotPermitted);
    }
    let Some(slot) = slots.take() else {
        return Decision::Refuse(ForwardRefusal::TooMany);
    };
    match allow.dial(open).await {
        Ok(socket) => Decision::Open(socket, slot),
        // The slot goes back with the guard, here as everywhere else.
        Err(why) => Decision::Refuse(why),
    }
}

/// A forwarded connection's stream, once it is carrying payload.
#[derive(Debug)]
pub struct Forwarded {
    send: SendStream,
    recv: RecvStream,
    /// Bytes that arrived in the same read as the last frame. The first bytes
    /// of the payload, and dropping them would truncate it.
    ahead: Vec<u8>,
}

impl Forwarded {
    /// Copy between this stream and whatever is at the local end until both
    /// are done.
    ///
    /// Returns how many bytes went each way, as `(sent, received)` — a count,
    /// never the bytes. What flows through a tunnel is the user's, and this is
    /// the layer that must never be tempted to look.
    ///
    /// Half-close is honoured: a side that finishes sending shuts the other's
    /// write half rather than tearing the connection down, which is what a
    /// close-delimited protocol needs to see.
    ///
    /// Generic over the local end because there are two of them: a TCP
    /// connection for a listener, and this process's own input and output for
    /// `--stdio`.
    pub async fn pump<S>(self, local: &mut S) -> std::io::Result<(u64, u64)>
    where
        S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + ?Sized,
    {
        let Self { send, recv, ahead } = self;
        if !ahead.is_empty() {
            local.write_all(&ahead).await?;
        }
        let mut remote = tokio::io::join(recv, send);
        let (received, sent) =
            tokio::io::copy_bidirectional_with_sizes(&mut remote, local, COPY_BUFFER, COPY_BUFFER)
                .await?;
        Ok((sent, received))
    }
}

/// The controller's half: ask the host to open one connection.
///
/// The reply arrives before any payload, so a refusal is a refusal rather than
/// a stream that closes for no stated reason.
pub async fn open(mut control: Control, target: &ForwardOpen) -> Result<Forwarded, ForwardError> {
    control.send(target).await?;
    match control.recv::<ForwardReply>().await? {
        ForwardReply::Opened => {
            let (send, recv, ahead) = control.into_raw();
            Ok(Forwarded { send, recv, ahead })
        }
        ForwardReply::Refused(why) => Err(ForwardError::Refused(why)),
    }
}

/// The host's half: read what the peer wants opened.
pub async fn requested(control: &mut Control) -> Result<ForwardOpen, ForwardError> {
    Ok(control.recv::<ForwardOpen>().await?)
}

/// Tell the peer it has its connection, and give up the framing.
pub async fn accepted(mut control: Control) -> Result<Forwarded, ForwardError> {
    control.send(&ForwardReply::Opened).await?;
    let (send, recv, ahead) = control.into_raw();
    Ok(Forwarded { send, recv, ahead })
}

/// Tell the peer why it does not.
///
/// Sent, and then the stream is finished — a refusal the peer never reads
/// because the stream was dropped is the same as no refusal at all.
pub async fn refuse(mut control: Control, why: ForwardRefusal) -> Result<(), ForwardError> {
    control.send(&ForwardReply::Refused(why)).await?;
    control.finish()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    fn open_to(host: &str, port: u16) -> ForwardOpen {
        ForwardOpen {
            host: host.to_owned(),
            port,
        }
    }

    #[test]
    fn a_list_nobody_wrote_is_loopback_rather_than_everything() {
        let list = Allowlist::parse("   ").unwrap();
        assert!(list.allows("127.0.0.1", 5432).is_some());
        assert!(list.allows("localhost", 80).is_some());
        assert!(list.allows("::1", 8080).is_some());
        // The whole point: an empty list is not an absent one.
        assert!(list.allows("10.0.0.5", 5432).is_none());
        assert!(list.allows("example.com", 443).is_none());
    }

    #[test]
    fn a_named_port_is_the_only_port_that_host_allows() {
        let list = Allowlist::parse("10.0.0.5:5432").unwrap();
        assert!(list.allows("10.0.0.5", 5432).is_some());
        assert!(list.allows("10.0.0.5", 5433).is_none());
        // And naming a host does not bring loopback along with it.
        assert!(list.allows("127.0.0.1", 5432).is_none());
    }

    #[test]
    fn a_star_port_allows_that_host_and_only_that_host() {
        let list = Allowlist::parse("db.internal:*, 10.0.0.5:5432").unwrap();
        assert!(list.allows("db.internal", 1).is_some());
        assert!(list.allows("db.internal", 65535).is_some());
        assert!(list.allows("db.other", 1).is_none());
    }

    #[test]
    fn a_host_is_matched_without_regard_to_case() {
        // DNS names are case-insensitive, and a rule that refused
        // `DB.Internal` because it was written `db.internal` would be a
        // support conversation rather than a security property.
        let list = Allowlist::parse("DB.Internal:5432").unwrap();
        assert!(list.allows("db.internal", 5432).is_some());
        assert!(list.allows("DB.INTERNAL", 5432).is_some());
    }

    #[test]
    fn an_ipv6_literal_must_be_bracketed() {
        // `::1:5432` has no unambiguous split, so it is refused rather than
        // guessed at: a rule that reads as one address and permits another is
        // the worst outcome available here.
        assert!(Allowlist::parse("::1:5432").is_err());
        let list = Allowlist::parse("[::1]:5432").unwrap();
        assert!(list.allows("::1", 5432).is_some());
        assert!(list.allows("::1", 5433).is_none());
    }

    #[test]
    fn there_is_no_wildcard_host() {
        // A wildcard port is a real thing to mean; a wildcard host is the
        // unrestricted forward the whole design refused.
        assert!(Allowlist::parse("localhost:*").is_ok());
        let e = Allowlist::parse("*:5432").unwrap_err();
        assert!(e.contains("no wildcard host"), "{e}");
        assert!(Allowlist::parse("*:*").is_err());
    }

    #[test]
    fn a_rule_that_is_not_host_and_port_is_refused_when_it_is_read() {
        // At load, where somebody is looking at the message — not at the first
        // connection, hours later, on a machine nobody is watching.
        assert!(Allowlist::parse("5432").is_err());
        assert!(Allowlist::parse("localhost:").is_err());
        assert!(Allowlist::parse(":5432").is_err());
        assert!(Allowlist::parse("localhost:0").is_err());
        assert!(Allowlist::parse("localhost:notaport").is_err());
    }

    #[test]
    fn the_default_rules_keep_only_loopback_addresses() {
        // The other half of "loopback and nothing else": `localhost` is a
        // name, and this is what happens when the resolver answers with
        // something that is not loopback.
        let list = Allowlist::loopback_only();
        let rule = list.allows("localhost", 80).unwrap();
        let addrs: Vec<SocketAddr> = vec![
            "93.184.216.34:80".parse().unwrap(),
            "127.0.0.1:80".parse().unwrap(),
        ];
        assert_eq!(
            permitted(rule, addrs),
            vec!["127.0.0.1:80".parse::<SocketAddr>().unwrap()]
        );

        // And a rule somebody wrote is theirs: they named it, so it is dialled
        // wherever it points.
        let named = Allowlist::parse("db.internal:5432").unwrap();
        let rule = named.allows("db.internal", 5432).unwrap();
        let addrs: Vec<SocketAddr> = vec!["10.0.0.5:5432".parse().unwrap()];
        assert_eq!(permitted(rule, addrs.clone()), addrs);
    }

    #[test]
    fn what_it_permits_can_be_read_back() {
        let list = Allowlist::parse("localhost:*, 10.0.0.5:5432").unwrap();
        assert_eq!(list.describe(), "localhost:*, 10.0.0.5:5432");
    }

    /// A target outside the list is refused *without being connected to*.
    ///
    /// Asserted at the dial site rather than inferred from the refusal: a
    /// listener is put where the request points, and the test fails if
    /// anything ever arrives on it. Inferring "it was not dialled" from "it
    /// was refused" would pass just as happily against a version that dialled
    /// first and refused afterwards, which is exactly the bug that matters.
    #[tokio::test]
    async fn a_target_outside_the_list_is_never_dialled() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        // Loopback, and not this port.
        let list = Allowlist::parse(&format!("127.0.0.1:{}", port.wrapping_add(1))).unwrap();
        let refused = list.dial(&open_to("127.0.0.1", port)).await.unwrap_err();
        assert_eq!(refused, ForwardRefusal::NotAllowed);

        let arrived =
            tokio::time::timeout(std::time::Duration::from_millis(250), listener.accept()).await;
        assert!(
            arrived.is_err(),
            "something connected to a target the allowlist does not name"
        );
    }

    #[tokio::test]
    async fn a_malformed_request_is_refused_before_the_allowlist_is_consulted() {
        // Port zero is not a port, and a host with a slash in it is not a
        // host. Neither reaches a resolver.
        let list = Allowlist::parse("127.0.0.1:*, 127.0.0.1/../etc:80").unwrap();
        assert_eq!(
            list.dial(&open_to("127.0.0.1", 0)).await.unwrap_err(),
            ForwardRefusal::Malformed
        );
        assert_eq!(
            list.dial(&open_to("127.0.0.1/../etc", 80))
                .await
                .unwrap_err(),
            ForwardRefusal::Malformed
        );
    }

    #[tokio::test]
    async fn an_allowed_target_that_is_not_listening_is_a_dial_failure() {
        // Bound and dropped, so the port is one nothing is listening on.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);

        let list = Allowlist::loopback_only();
        assert_eq!(
            list.dial(&open_to("127.0.0.1", port)).await.unwrap_err(),
            ForwardRefusal::DialFailed
        );
    }

    #[test]
    fn the_connection_cap_holds_and_gives_slots_back() {
        let slots = Slots::new(2);
        let first = slots.take().expect("one");
        let second = slots.take().expect("two");
        assert_eq!(slots.open(), 2);
        assert!(slots.take().is_none(), "a third was let through a cap of 2");

        // Given back by ending, not by remembering to.
        drop(first);
        assert_eq!(slots.open(), 1);
        let third = slots.take().expect("the freed slot");
        assert!(slots.take().is_none());
        drop((second, third));
        assert_eq!(slots.open(), 0);
    }

    #[test]
    fn a_cap_of_none_lets_nothing_through() {
        // Not a configuration anybody should write, but the arithmetic must
        // not wrap into "unlimited" if somebody does.
        assert!(Slots::new(0).take().is_none());
    }

    #[tokio::test]
    async fn many_takers_at_once_never_exceed_the_cap() {
        // Two streams arriving together must not both read the same count and
        // both be let through, which is what a load-then-store would do.
        let slots = Slots::new(8);
        let mut running = Vec::new();
        for _ in 0..32 {
            let slots = slots.clone();
            running.push(tokio::spawn(async move {
                let held = slots.take();
                assert!(slots.open() <= 8, "the cap was exceeded: {}", slots.open());
                held
            }));
        }
        let mut held = Vec::new();
        for t in running {
            held.push(t.await.expect("a taker"));
        }
        assert_eq!(held.iter().filter(|h| h.is_some()).count(), 8);
        drop(held);
        assert_eq!(slots.open(), 0);
    }

    /// A session that has all the connections it may get is refused the next
    /// one — and refused *before* anything is dialled.
    #[tokio::test]
    async fn a_session_at_its_connection_cap_is_refused_the_next_one() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let allow = Allowlist::loopback_only();
        let may = vec![esper_proto::Permission::Forward];
        let slots = Slots::new(2);
        let target = open_to("127.0.0.1", port);

        let accepting = tokio::spawn(async move {
            let mut held = Vec::new();
            for _ in 0..2 {
                held.push(listener.accept().await.map(|(s, _)| s));
            }
            (listener, held)
        });

        let mut held = Vec::new();
        for _ in 0..2 {
            match decide(&target, &allow, &slots, &may).await {
                Decision::Open(socket, slot) => held.push((socket, slot)),
                Decision::Refuse(why) => panic!("refused inside the cap: {why:?}"),
            }
        }
        let (listener, _accepted) = accepting.await.unwrap();

        match decide(&target, &allow, &slots, &may).await {
            Decision::Refuse(why) => assert_eq!(why, ForwardRefusal::TooMany),
            Decision::Open(..) => panic!("a third connection was let through a cap of 2"),
        }
        // And it was refused rather than dialled: nothing more arrived.
        let arrived =
            tokio::time::timeout(std::time::Duration::from_millis(250), listener.accept()).await;
        assert!(arrived.is_err(), "the refused request was dialled anyway");

        // A connection ending gives its slot back, so the next one is allowed.
        held.pop();
        assert!(matches!(
            decide(&target, &allow, &slots, &may).await,
            Decision::Open(..)
        ));
    }

    /// A device that no longer holds the permission is refused, whatever the
    /// session was granted when it opened.
    #[tokio::test]
    async fn a_revoked_device_is_refused_before_the_allowlist_or_a_slot() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let slots = Slots::new(8);
        let nothing: Vec<esper_proto::Permission> = Vec::new();

        match decide(
            &open_to("127.0.0.1", port),
            &Allowlist::loopback_only(),
            &slots,
            &nothing,
        )
        .await
        {
            Decision::Refuse(why) => assert_eq!(why, ForwardRefusal::NotPermitted),
            Decision::Open(..) => panic!("a device with no permissions opened a forward"),
        }
        // No slot was spent on it, and nothing was dialled.
        assert_eq!(slots.open(), 0);
        let arrived =
            tokio::time::timeout(std::time::Duration::from_millis(250), listener.accept()).await;
        assert!(arrived.is_err(), "a refused device's target was dialled");
    }

    #[tokio::test]
    async fn an_allowed_target_is_connected_to() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let accepted = tokio::spawn(async move { listener.accept().await.map(|(s, _)| s) });

        let list = Allowlist::loopback_only();
        let stream = list.dial(&open_to("127.0.0.1", port)).await.unwrap();
        assert_eq!(stream.peer_addr().unwrap().port(), port);
        accepted.await.unwrap().unwrap();
    }
}
