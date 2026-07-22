//! Port forwarding, both halves: serving a target and being the listener.
//!
//! `ssh -L`'s shape, over a session that is already authenticated, encrypted
//! and reachable from wherever the laptop is. The service on the host never
//! listens on the network, and the port on the laptop never leaves loopback.
//!
//! # What each side is responsible for
//!
//! The **controller** owns the local socket and nothing else: it binds
//! `127.0.0.1`, and hands each accepted connection a QUIC stream. It does not
//! decide what may be reached — a controller that could would be a controller
//! that could ask for anything.
//!
//! The **host** owns the decision. Every stream's first frame names a target,
//! and [`esper_transport::forward::Allowlist`] answers before anything is
//! resolved or dialled.
//!
//! # Why the local socket is loopback and there is no flag for it
//!
//! A forward that bound `0.0.0.0` would put the host's private service on the
//! controller's network — which is the exact thing this feature exists to
//! avoid doing on the host's side. Somebody who genuinely wants that can put a
//! proxy in front of the loopback port and take the decision knowingly, which
//! is the right amount of friction for re-exposing somebody else's service.

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use esper_audit::{Entry, Event, Log};
use esper_crypto::identity::KEY_LEN;
use esper_proto::{ForwardOpen, ForwardRefusal, Permission, SessionKind, SessionRequest};
use esper_transport::forward::{self, Allowlist, Slots};
use esper_transport::{Session, request_session};

use crate::{net, paths, session_cmd};

/// How many policy refusals a session may collect before it is ended.
///
/// A target outside the allowlist is a mistake once and a probe the tenth
/// time. Ending the session stops the probe, which is better than going on
/// refusing it — and it is also what bounds how much a hostile peer can write
/// into this machine's audit log.
///
/// Only policy refusals count. A target that is simply not listening is a
/// service being down, and a client waiting for one to come up is doing
/// nothing wrong.
const MOST_REFUSALS: usize = 10;

// ── the host's half ─────────────────────────────────────────────────────────

/// How long a stream has to say what it wants.
///
/// A peer that opens a stream and says nothing is holding one, and until this
/// existed it was holding the *accept loop* as well — one silent stream and
/// the session forwarded nothing else, ever.
const ASK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Serve forwarded connections for one session, until the peer goes away.
///
/// **One task per stream, from the moment it is accepted.** Not from the
/// moment it is understood: everything before that — waiting for the request,
/// the lookup, the dial — is waiting on somebody else, and doing any of it on
/// the accept loop means one connection decides how quickly the next is
/// served.
pub async fn serve(
    session: &Session,
    allow: &Allowlist,
    at_once: usize,
    live: &crate::permits::Live,
    log: &Log,
    device: &[u8; KEY_LEN],
) -> Result<String, String> {
    println!("      forwarding to {}", allow.describe());

    let slots = Slots::new(at_once);
    let refusals = Arc::new(AtomicUsize::new(0));
    let served = Arc::new(AtomicUsize::new(0));
    // Closing the session is how a probe is stopped, and it happens inside a
    // task rather than by returning from here.
    let closer = session.media_channel();

    loop {
        let control = match session.accept_control().await {
            Ok(c) => c,
            // The session ended: the peer closed, or the revocation watchdog
            // closed it from under us. Either way there is nothing left to
            // accept.
            Err(_) => break,
        };

        let allow = allow.clone();
        let slots = slots.clone();
        let live = live.clone();
        let log = log.clone();
        let device = *device;
        let refusals = Arc::clone(&refusals);
        let served = Arc::clone(&served);
        let closer = closer.clone();
        tokio::spawn(async move {
            one(
                control, &allow, &slots, &live, &log, &device, &refusals, &served, &closer,
            )
            .await;
        });
    }

    Ok(match served.load(Ordering::Relaxed) {
        0 => "the controller forwarded nothing".to_owned(),
        1 => "1 forwarded connection".to_owned(),
        n => format!("{n} forwarded connections"),
    })
}

/// One forwarded connection, start to finish.
#[allow(clippy::too_many_arguments)]
async fn one(
    mut control: esper_transport::Control,
    allow: &Allowlist,
    slots: &Slots,
    live: &crate::permits::Live,
    log: &Log,
    device: &[u8; KEY_LEN],
    refusals: &AtomicUsize,
    served: &AtomicUsize,
    closer: &esper_transport::MediaChannel,
) {
    let open = match tokio::time::timeout(ASK_TIMEOUT, forward::requested(&mut control)).await {
        Ok(Ok(o)) => o,
        Ok(Err(e)) => {
            eprintln!("      · a forward stream said nothing usable: {e}");
            return;
        }
        Err(_) => {
            eprintln!("      · a forward stream said nothing for {ASK_TIMEOUT:?}");
            return;
        }
    };
    let target = open.target();

    // **Every refusal, in one place, in the order that matters.** The
    // permission as it stands now, then a slot taken rather than counted, then
    // the allowlist — and only then a lookup and a dial.
    let (socket, slot) = match forward::decide(&open, allow, slots, live).await {
        forward::Decision::Open(socket, slot) => (socket, slot),
        forward::Decision::Refuse(why) => {
            println!("      refused {target} — {}", why.message());
            let _ = forward::refuse(control, why).await;
            if is_policy(why) && over_the_limit(refusals, log, device, &target, why) {
                println!("      ending it: {MOST_REFUSALS} targets this device may not reach");
                // Closing discards what is queued, so this last refusal may
                // not arrive. That is the one place it does not matter: the
                // close carries `PERMISSION_DENIED` and the controller prints
                // it, so the peer is told why it was disconnected — which is
                // the more useful of the two messages anyway.
                closer.close(esper_proto::Reason::PERMISSION_DENIED);
            }
            return;
        }
    };

    let stream = match forward::accepted(control).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("      · could not open {target}: {e}");
            return;
        }
    };

    served.fetch_add(1, Ordering::Relaxed);
    println!("      forwarding {target}");
    record(
        log,
        device,
        Event::ForwardOpened {
            target: target.clone(),
        },
    );

    // Held for the life of the connection and given back by ending.
    let _slot = slot;
    let began = Instant::now();
    let mut socket = socket;
    let (sent, received) = stream.pump(&mut socket).await.unwrap_or_default();
    record(
        log,
        device,
        Event::ForwardClosed {
            target,
            duration_secs: began.elapsed().as_secs(),
            sent,
            received,
        },
    );
}

/// Whether a refusal is about policy rather than about the target being down.
const fn is_policy(why: ForwardRefusal) -> bool {
    matches!(
        why,
        ForwardRefusal::NotAllowed | ForwardRefusal::NotPermitted | ForwardRefusal::Malformed
    )
}

/// Record a policy refusal, and say whether this session has had enough of them.
fn over_the_limit(
    refusals: &AtomicUsize,
    log: &Log,
    device: &[u8; KEY_LEN],
    target: &str,
    why: ForwardRefusal,
) -> bool {
    record(
        log,
        device,
        Event::ForwardRefused {
            target: target.to_owned(),
            reason: why.message().to_owned(),
        },
    );
    refusals.fetch_add(1, Ordering::Relaxed).saturating_add(1) >= MOST_REFUSALS
}

/// Put one line in the audit trail, saying so when it cannot.
fn record(log: &Log, device: &[u8; KEY_LEN], event: Event) {
    if let Err(e) = log.append(&Entry::now(device, event)) {
        eprintln!("      · not recorded in the audit log: {e}");
    }
}

// ── the controller's half ───────────────────────────────────────────────────

/// What one forward carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Spec {
    /// Listen on a local port, and forward every connection.
    Listen { local: u16, host: String, port: u16 },
    /// Forward one connection, on this process's own input and output.
    ///
    /// The `ssh -W` shape, so `ProxyCommand` works: plain `ssh`, `scp`, `rsync`
    /// and `git` then run over Esper's pairing and its connectivity ladder,
    /// against an sshd bound to loopback that the network never sees.
    Stdio { host: String, port: u16 },
}

/// What `forward` was asked to do.
#[derive(Debug, Clone)]
pub struct Options {
    pub spec: Spec,
    pub addr: Option<SocketAddr>,
    pub named: Option<String>,
    pub via: Option<esper_rendezvous::Via>,
    pub mdns: bool,
}

/// Parse `forward` arguments.
pub fn parse(args: &[String]) -> Result<Options, String> {
    let mut stdio = false;
    let mut spec: Option<String> = None;
    let mut addr: Option<SocketAddr> = None;
    let mut named: Option<String> = None;
    let mut via_addr: Option<SocketAddr> = None;
    let mut via_key: Option<[u8; 32]> = None;
    let mut mdns = crate::settings::flag("discovery.mdns", true);

    let mut i = 0usize;
    while let Some(arg) = args.get(i) {
        match arg.as_str() {
            "--stdio" => {
                stdio = true;
                i += 1;
            }
            "--no-mdns" => {
                mdns = false;
                i += 1;
            }
            "--via" => {
                via_addr =
                    Some(net::parse_addr(args.get(i + 1).ok_or_else(|| {
                        "--via needs the broker's address".to_owned()
                    })?)?);
                i += 2;
            }
            "--broker-key" => {
                via_key = Some(crate::broker_cmd::parse_key(
                    args.get(i + 1)
                        .ok_or_else(|| "--broker-key needs the broker's key".to_owned())?,
                )?);
                i += 2;
            }
            // The forward itself first, then which machine — an address or a
            // name, settled the same way `connect` settles it.
            other if !other.starts_with('-') && spec.is_none() => {
                spec = Some(other.to_owned());
                i += 1;
            }
            other if !other.starts_with('-') && addr.is_none() && named.is_none() => {
                match net::parse_addr(other) {
                    Ok(a) => addr = Some(a),
                    Err(_) => named = Some(other.to_owned()),
                }
                i += 1;
            }
            other => return Err(format!("forward: unexpected argument {other:?}")),
        }
    }

    let spec = spec.ok_or_else(|| USAGE.to_owned())?;
    let spec = if stdio {
        let (host, port) = target(&spec)?;
        Spec::Stdio { host, port }
    } else {
        parse_listen(&spec)?
    };

    let via = match session_cmd::broker_from(via_addr, via_key)? {
        Some(via) => Some(via),
        None => crate::settings::broker()?,
    };
    Ok(Options {
        spec,
        addr,
        named,
        via,
        mdns,
    })
}

/// What to say when the arguments do not name a forward.
const USAGE: &str = "usage: esper-engine forward <local>:<host>:<port> [<device>]\n\
                     \x20      esper-engine forward --stdio <host>:<port> [<device>]\n\
                     for example: esper-engine forward 8080:localhost:80 elythion";

/// `8080:localhost:80`.
fn parse_listen(spec: &str) -> Result<Spec, String> {
    let (local, rest) = spec
        .split_once(':')
        .ok_or_else(|| format!("{spec:?} is not <local>:<host>:<port>\n{USAGE}"))?;
    let local: u16 = local
        .parse()
        .map_err(|_| format!("{local:?} is not a local port number"))?;
    if local == 0 {
        return Err("port 0 is not a port to listen on".to_owned());
    }
    let (host, port) = target(rest)?;
    Ok(Spec::Listen { local, host, port })
}

/// `localhost:80`, or `[::1]:80`.
fn target(spec: &str) -> Result<(String, u16), String> {
    let (host, port) = if let Some(rest) = spec.strip_prefix('[') {
        let (host, after) = rest
            .split_once(']')
            .ok_or_else(|| format!("{spec:?} opens a bracket and does not close it"))?;
        (
            host,
            after
                .strip_prefix(':')
                .ok_or_else(|| format!("{spec:?} has no port"))?,
        )
    } else {
        let (host, port) = spec
            .rsplit_once(':')
            .ok_or_else(|| format!("{spec:?} is not <host>:<port>\n{USAGE}"))?;
        // An unbracketed IPv6 literal has no unambiguous split, and guessing
        // is how a forward reaches somewhere other than what it reads as. The
        // brackets are what make it unambiguous, so this applies only here.
        if host.contains(':') {
            return Err(format!(
                "{spec:?}: bracket an IPv6 literal — [{host}]:{port} rather than {host}:{port}"
            ));
        }
        (host, port)
    };
    if host.is_empty() {
        return Err(format!("{spec:?} has no host"));
    }
    let port: u16 = port
        .parse()
        .map_err(|_| format!("{port:?} is not a port number"))?;
    if port == 0 {
        return Err("port 0 is not a port".to_owned());
    }
    Ok((host.to_owned(), port))
}

/// Forward a port to a paired host.
pub fn run(o: &Options) -> Result<(), String> {
    net::install_crypto();
    let identity = paths::identity()?;

    net::runtime()?.block_on(async {
        let session_cmd::Reached {
            endpoint, session, ..
        } = session_cmd::reach(
            &identity,
            &session_cmd::Reaching {
                label: "🔌 esper forward",
                addr: o.addr,
                named: o.named.as_deref(),
                via: o.via,
                mdns: o.mdns,
                // `--stdio` puts somebody's ssh connection on this process's
                // stdout. Nothing of ours may go there.
                aside: matches!(o.spec, Spec::Stdio { .. }),
            },
        )
        .await?;
        let mut session = session;

        let mut control = session
            .take_control()
            .ok_or_else(|| "the session has no control stream".to_owned())?;

        // **A forward and nothing else.** Asking for view as well would make
        // the host set up a screen for a session that will never look at one.
        let grant = request_session(
            &mut control,
            &SessionRequest {
                kind: SessionKind::Forward,
                want: vec![Permission::Forward],
                codecs: Vec::new(),
                reason: "a port forward".to_owned(),
            },
        )
        .await
        .map_err(|e| match e {
            esper_transport::AuthorizeError::Refused(d) => {
                format!("the host refused: {}", d.detail)
            }
            other => format!("asking to forward a port: {other}"),
        })?;
        if !grant.allows(Permission::Forward) {
            return Err(
                "the host granted a session but not forwarding, so there is nothing to do"
                    .to_owned(),
            );
        }

        let session = Arc::new(session);
        let outcome = match &o.spec {
            Spec::Stdio { host, port } => stdio(&session, host, *port).await,
            Spec::Listen { local, host, port } => listen(&session, *local, host, *port).await,
        };

        // The host is waiting to hear the session ended; give it the moment it
        // takes rather than dropping the connection on the floor.
        session.linger(std::time::Duration::from_secs(2)).await;
        drop(endpoint);
        outcome
    })
}

/// One connection, on this process's own input and output.
async fn stdio(session: &Session, host: &str, port: u16) -> Result<(), String> {
    let control = session
        .open_control()
        .await
        .map_err(|e| format!("opening a stream: {e}"))?;
    let stream = ask(control, host, port).await?;

    // Nothing is printed on this path. Whatever this program says on stdout is
    // in the middle of somebody's ssh session, and `ProxyCommand` is the whole
    // reason this mode exists.
    let mut local = tokio::io::join(tokio::io::stdin(), tokio::io::stdout());
    stream
        .pump(&mut local)
        .await
        .map_err(|e| format!("forwarding: {e}"))?;
    Ok(())
}

/// A local listener, and a stream per connection it accepts.
async fn listen(session: &Arc<Session>, local: u16, host: &str, port: u16) -> Result<(), String> {
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, local))
        .await
        .map_err(|e| format!("listening on 127.0.0.1:{local}: {e}"))?;
    println!("  forward    127.0.0.1:{local} → {host}:{port} on the host");
    println!("             ctrl-c to stop");
    println!();

    let target = ForwardOpen {
        host: host.to_owned(),
        port,
    };
    loop {
        let accepted = tokio::select! {
            r = listener.accept() => r,
            // The session going away ends the listener with it: a local port
            // that accepts connections it cannot forward is worse than a port
            // that is closed, because the connection succeeds and then hangs.
            //
            // **With the reason.** A revocation reaches a live forward through
            // this branch, and "the listener stopped" without saying why is
            // exactly the thing that sends somebody looking at their network.
            () = closed(session) => {
                match session.peer_close_reason() {
                    Some((_, why)) => println!("  the host ended it: {why}"),
                    None => println!("  the session ended"),
                }
                return Ok(());
            }
            r = tokio::signal::ctrl_c() => {
                r.map_err(|e| format!("listening for ctrl-c: {e}"))?;
                println!("  stopped");
                return Ok(());
            }
        };
        let (socket, from) = accepted.map_err(|e| format!("accepting: {e}"))?;

        let session = Arc::clone(session);
        let target = target.clone();
        tokio::spawn(async move {
            let control = match session.open_control().await {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("  · {from}: opening a stream: {e}");
                    return;
                }
            };
            match ask(control, &target.host, target.port).await {
                Ok(stream) => {
                    let mut socket = socket;
                    if let Err(e) = stream.pump(&mut socket).await {
                        eprintln!("  · {from}: {e}");
                    }
                }
                Err(e) => eprintln!("  · {from}: {e}"),
            }
        });
    }
}

/// Ask the host to open one connection, and say plainly if it will not.
async fn ask(
    control: esper_transport::Control,
    host: &str,
    port: u16,
) -> Result<forward::Forwarded, String> {
    forward::open(
        control,
        &ForwardOpen {
            host: host.to_owned(),
            port,
        },
    )
    .await
    .map_err(|e| e.to_string())
}

/// Resolves when the session ends.
async fn closed(session: &Session) {
    let _ = session.closed().await;
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    #[test]
    fn a_forward_reads_as_local_then_target() {
        assert_eq!(
            parse_listen("8080:localhost:80").unwrap(),
            Spec::Listen {
                local: 8080,
                host: "localhost".to_owned(),
                port: 80,
            }
        );
    }

    #[test]
    fn an_ipv6_target_must_be_bracketed() {
        // The same rule as the allowlist, and for the same reason: `::1:80`
        // has no unambiguous split.
        assert_eq!(
            parse_listen("8080:[::1]:80").unwrap(),
            Spec::Listen {
                local: 8080,
                host: "::1".to_owned(),
                port: 80,
            }
        );
        let e = parse_listen("8080:::1:80").unwrap_err();
        assert!(e.contains("bracket"), "{e}");
    }

    #[test]
    fn a_forward_that_is_not_three_parts_says_what_one_looks_like() {
        for bad in ["8080", "8080:localhost", "localhost:80", ":localhost:80"] {
            let e = parse_listen(bad).unwrap_err();
            assert!(
                e.contains("8080:localhost:80") || e.contains("port") || e.contains("host"),
                "{bad:?} said {e:?}"
            );
        }
    }

    #[test]
    fn port_zero_is_not_a_port_at_either_end() {
        assert!(parse_listen("0:localhost:80").is_err());
        assert!(parse_listen("8080:localhost:0").is_err());
    }

    #[test]
    fn stdio_takes_a_target_and_no_local_port() {
        let o = parse(&["--stdio".to_owned(), "localhost:22".to_owned()]).unwrap();
        assert_eq!(
            o.spec,
            Spec::Stdio {
                host: "localhost".to_owned(),
                port: 22,
            }
        );
    }

    #[test]
    fn a_device_can_be_named_after_the_forward() {
        let o = parse(&["8080:localhost:80".to_owned(), "elythion".to_owned()]).unwrap();
        assert_eq!(o.named.as_deref(), Some("elythion"));
        assert!(o.addr.is_none());
    }

    #[test]
    fn only_policy_refusals_count_against_the_session() {
        // A service that is down is not a probe, and a client waiting for one
        // to come up must not be disconnected for it.
        assert!(is_policy(ForwardRefusal::NotAllowed));
        assert!(is_policy(ForwardRefusal::NotPermitted));
        assert!(!is_policy(ForwardRefusal::DialFailed));
        assert!(!is_policy(ForwardRefusal::TooMany));
    }
}
