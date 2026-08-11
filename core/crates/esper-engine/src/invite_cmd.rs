//! One link, one paste, one session: the support flow.
//!
//! The case that genuinely needs a broker. Somebody with no LAN, no overlay
//! and no patience needs help, and every step you can remove from that is a
//! step they will not get wrong. So: the helper runs one command and sends one
//! link; the other person downloads one binary and pastes it. They configure
//! nothing, choose nothing and type no numbers.
//!
//! # What is in the link
//!
//! The broker's address and **its key, pinned**, so the invited machine is not
//! trusting whatever answers there. A random token, which is what admits the
//! two of them to each other at the broker before either knows the other's
//! key. A pairing code, which is what actually authenticates the pairing. And
//! an expiry, so a link that leaks stops working.
//!
//! **The helper's own key is deliberately not in it.** Pairing must not
//! pre-trust a key — the whole point of the exchange is to establish which key
//! to trust — and a link that named one would invite somebody to skip that.
//!
//! # The link is the secret
//!
//! Anyone holding it can pair with the helper until it expires. That is what
//! it is for, and it is why it is time limited, why the pairing it grants is
//! single use, and why the expiry is short enough to be sent and used rather
//! than filed.

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use esper_crypto::identity::KEY_LEN;
use esper_crypto::{Identity, Offer, PairingCode};
use esper_proto::{Hello, Role};
use esper_transport::{Endpoint, Session, pair, pairing_authority};

use crate::paths;

/// How long an invitation lasts if nobody says.
///
/// Long enough to send a message and have somebody read it; short enough that
/// a link left in a chat history is not a standing invitation. The *pairing
/// code* inside it still has its own sixty seconds once somebody arrives, so
/// this bounds how long the link is worth stealing, not how long the exchange
/// may take.
pub const DEFAULT_EXPIRY: Duration = Duration::from_secs(30 * 60);

/// The longest an invitation may be asked to last.
///
/// A day is already generous for "help me with this now", and a link that
/// works next week is a credential somebody has forgotten they issued.
pub const MAX_EXPIRY: Duration = Duration::from_secs(24 * 60 * 60);

/// What one link carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invitation {
    /// Where the broker is.
    pub broker: SocketAddr,
    /// The broker's key, pinned by whoever uses this.
    pub broker_key: [u8; KEY_LEN],
    /// What admits the two of them to each other at the broker.
    pub token: [u8; KEY_LEN],
    /// The pairing code, which is what authenticates the pairing itself.
    pub code: String,
    /// Seconds since the epoch, after which this is refused.
    pub expires_at: u64,
}

/// The scheme, so a link is recognisable as one and cannot be mistaken for a
/// web address.
const SCHEME: &str = "esper://join/";

impl Invitation {
    /// Whether this is still worth trying.
    #[must_use]
    pub fn is_live(&self, now: u64) -> bool {
        now < self.expires_at
    }

    /// How long is left.
    #[must_use]
    pub fn remaining(&self, now: u64) -> Duration {
        Duration::from_secs(self.expires_at.saturating_sub(now))
    }

    /// The link to send somebody.
    #[must_use]
    pub fn link(&self) -> String {
        format!("{SCHEME}{}", base64url(&self.encode()))
    }

    /// Read a link somebody pasted.
    ///
    /// Accepts the link with or without its scheme, because somebody will
    /// paste the middle of it.
    pub fn parse(raw: &str) -> Result<Self, String> {
        let text = raw.trim();
        let body = text.strip_prefix(SCHEME).unwrap_or(text);
        let bytes = unbase64url(body)?;
        Self::decode(&bytes)
    }

    /// The bytes inside the link.
    ///
    /// Hand-rolled rather than serialised: this goes in front of a person, so
    /// every byte counts, and a fixed layout is one anybody can reimplement
    /// from the source.
    fn encode(&self) -> Vec<u8> {
        let address = self.broker.to_string();
        let mut out = Vec::with_capacity(96 + address.len() + self.code.len());
        out.push(1); // the layout's own version
        out.extend_from_slice(&self.broker_key);
        out.extend_from_slice(&self.token);
        out.extend_from_slice(&self.expires_at.to_be_bytes());
        out.push(u8::try_from(address.len()).unwrap_or(u8::MAX));
        out.extend_from_slice(address.as_bytes());
        out.push(u8::try_from(self.code.len()).unwrap_or(u8::MAX));
        out.extend_from_slice(self.code.as_bytes());
        out
    }

    /// The reverse, refusing anything that does not fit exactly.
    fn decode(bytes: &[u8]) -> Result<Self, String> {
        let mut at = 0usize;
        let mut take = |n: usize| -> Result<&[u8], String> {
            let end = at.checked_add(n).ok_or("that link is truncated")?;
            let slice = bytes.get(at..end).ok_or("that link is truncated")?;
            at = end;
            Ok(slice)
        };

        let version = *take(1)?.first().ok_or("that link is empty")?;
        if version != 1 {
            return Err(format!(
                "that link was made by a newer version of Esper (layout {version})"
            ));
        }
        let broker_key: [u8; KEY_LEN] = take(KEY_LEN)?
            .try_into()
            .map_err(|_| "that link's broker key is the wrong size".to_owned())?;
        let token: [u8; KEY_LEN] = take(KEY_LEN)?
            .try_into()
            .map_err(|_| "that link's token is the wrong size".to_owned())?;
        let expires_at = u64::from_be_bytes(
            take(8)?
                .try_into()
                .map_err(|_| "that link's expiry is the wrong size".to_owned())?,
        );
        let address_len = usize::from(*take(1)?.first().ok_or("that link is truncated")?);
        let address = std::str::from_utf8(take(address_len)?)
            .map_err(|_| "that link's address is not text".to_owned())?
            .to_owned();
        let code_len = usize::from(*take(1)?.first().ok_or("that link is truncated")?);
        let code = std::str::from_utf8(take(code_len)?)
            .map_err(|_| "that link's code is not text".to_owned())?
            .to_owned();

        // Nothing may follow. A link with bytes on the end is not this link.
        if at != bytes.len() {
            return Err("that link has something extra on the end".to_owned());
        }

        Ok(Self {
            broker: address
                .parse()
                .map_err(|_| format!("that link's broker address ({address}) is not an address"))?,
            broker_key,
            token,
            code,
            expires_at,
        })
    }
}

/// The alphabet, without padding, safe in a URL and in a chat message.
const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

/// Base64url, unpadded.
///
/// Hand-rolled, and the reason is worth stating: the alternative is a
/// dependency for an alphabet and some bit shifting, and this project asks for
/// a reason before every one of those. It is thirty lines with a round-trip
/// test over every length.
fn base64url(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = u32::from(*chunk.first().unwrap_or(&0));
        let b1 = u32::from(*chunk.get(1).unwrap_or(&0));
        let b2 = u32::from(*chunk.get(2).unwrap_or(&0));
        let packed = (b0 << 16) | (b1 << 8) | b2;
        // Six bits at a time, and only as many characters as the input's
        // bytes actually carry.
        let characters = chunk.len() + 1;
        for i in 0..characters {
            let index = ((packed >> (18 - 6 * i)) & 0x3f) as usize;
            out.push(char::from(*ALPHABET.get(index).unwrap_or(&b'A')));
        }
    }
    out
}

/// The reverse, refusing anything that is not this alphabet.
fn unbase64url(text: &str) -> Result<Vec<u8>, String> {
    let mut out = Vec::with_capacity(text.len() / 4 * 3);
    let mut packed = 0u32;
    let mut bits = 0u32;
    for c in text.chars() {
        let value = ALPHABET
            .iter()
            .position(|a| char::from(*a) == c)
            .ok_or_else(|| format!("that link has a character that cannot be in one: {c:?}"))?;
        packed = (packed << 6) | value as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(u8::try_from((packed >> bits) & 0xff).unwrap_or(0));
        }
    }
    // Whatever is left must be zero padding, not data somebody appended.
    if packed & ((1 << bits) - 1) != 0 {
        return Err("that link is not quite a link".to_owned());
    }
    Ok(out)
}

/// Read a duration the way somebody writes one: `30m`, `2h`, `90s`.
pub fn parse_expiry(raw: &str) -> Result<Duration, String> {
    let text = raw.trim().to_ascii_lowercase();
    let (digits, unit) = match text.chars().last() {
        Some('s') => (&text[..text.len() - 1], 1u64),
        Some('m') => (&text[..text.len() - 1], 60),
        Some('h') => (&text[..text.len() - 1], 3600),
        Some('d') => (&text[..text.len() - 1], 86400),
        _ => (text.as_str(), 60),
    };
    let n: u64 = digits
        .parse()
        .map_err(|_| format!("{raw:?} is not a length of time — try 30m, 2h or 90s"))?;
    let d = Duration::from_secs(n.saturating_mul(unit));
    if d.is_zero() {
        return Err("an invitation that expires immediately invites nobody".to_owned());
    }
    if d > MAX_EXPIRY {
        return Err(format!(
            "the longest an invitation may last is {} hours — a link that works next week \
             is a credential somebody has forgotten they issued",
            MAX_EXPIRY.as_secs() / 3600
        ));
    }
    Ok(d)
}

// ── the two commands ────────────────────────────────────────────────────────

/// How long a guest waits for the broker to introduce it.
///
/// The helper may not be there — it may have stopped waiting, or never
/// started — and a guest that waited for ever would look like a program that
/// had hung. Thirty seconds is long enough for a slow link and short enough to
/// try again.
const WAIT_TO_MEET: Duration = Duration::from_secs(30);

/// How long each side spends trying to reach the other once introduced.
const EACH_CANDIDATE: Duration = Duration::from_secs(5);

/// What `invite` was asked for.
#[derive(Debug, Clone)]
pub struct InviteOptions {
    /// The broker to meet at.
    pub via: esper_rendezvous::Via,
    /// Who it is for. Shown on this machine, never sent.
    pub who: Option<String>,
    /// How long the link lasts.
    pub expires: Duration,
}

/// Parse `invite --via <addr> --broker-key <key> [--for <name>] [--expires 30m]`.
pub fn parse_invite(args: &[String]) -> Result<InviteOptions, String> {
    let mut addr = None;
    let mut key = None;
    let mut who = None;
    let mut expires = DEFAULT_EXPIRY;
    let mut rest = args.iter();
    while let Some(a) = rest.next() {
        let mut value = |name: &str| -> Result<String, String> {
            rest.next()
                .cloned()
                .ok_or_else(|| format!("{name} needs a value"))
        };
        match a.as_str() {
            "--via" => addr = Some(crate::net::parse_addr(&value("--via")?)?),
            "--broker-key" => key = Some(crate::broker_cmd::parse_key(&value("--broker-key")?)?),
            "--for" => who = Some(value("--for")?),
            "--expires" => expires = parse_expiry(&value("--expires")?)?,
            other => return Err(format!("unexpected argument {other:?}")),
        }
    }
    let (Some(addr), Some(key)) = (addr, key) else {
        // **With no broker configured, `invite` fails with an explanation and
        // does not fall back to anyone.** That refusal is the product working
        // correctly: there is no address for a broker anywhere in Esper, and
        // there is no one else's server to borrow.
        return Err(
            "invite needs a broker you run: --via <address> --broker-key <key>.\n  \
                    There is no default broker anywhere in Esper, and this will not use \
                    somebody else's.\n  Start one with: esper-broker serve --listen :7443"
                .to_owned(),
        );
    };
    Ok(InviteOptions {
        via: esper_rendezvous::Via { addr, key },
        who,
        expires,
    })
}

/// Offer somebody a way in, and wait for them.
pub fn run_invite(o: &InviteOptions) -> Result<(), String> {
    crate::net::install_crypto();
    let identity = paths::identity()?;

    // The code is minted now and used whenever the link is pasted, so its own
    // sixty seconds start on arrival. The link's expiry is what bounds the
    // wait.
    let code = PairingCode::generate();
    let mut token = [0u8; KEY_LEN];
    getrandom::getrandom(&mut token)
        .map_err(|e| format!("no randomness for an invitation: {e}"))?;

    let invitation = Invitation {
        broker: o.via.addr,
        broker_key: o.via.key,
        token,
        code: code.display(),
        expires_at: paths::now_secs().saturating_add(o.expires.as_secs()),
    };

    println!("✉️  esper invite");
    println!();
    if let Some(who) = &o.who {
        println!("  for        {who}");
    }
    println!("  broker     {}", o.via.addr);
    println!("  expires    in {}", describe(o.expires));
    println!();
    println!("  Send them this link, and nothing else:");
    println!();
    println!("      {}", invitation.link());
    println!();
    println!("  They run:  {} join <the link>", crate::net::me());
    println!();
    println!("  This device's fingerprint — read it aloud and check it matches:");
    for line in identity.fingerprint().display().lines() {
        println!("      {line}");
    }
    println!();
    println!("  Anyone holding that link can pair with this machine until it expires.");
    println!("  Waiting…");

    crate::net::runtime()?.block_on(wait_for_guest(identity, o.via, invitation, code))
}

/// Wait at the broker until somebody arrives with the invitation, then pair.
async fn wait_for_guest(
    identity: Identity,
    via: esper_rendezvous::Via,
    invitation: Invitation,
    code: PairingCode,
) -> Result<(), String> {
    // **The pairing authority accepts any key, and only for this.** The same
    // reasoning as `pair --listen`: it is a separate endpoint, bound for this
    // and dropped afterwards, rather than a mode a session endpoint can be put
    // into. Outbound is *not* any key — it starts with the broker's, pinned,
    // and learns the guest's from the introduction.
    let dial = std::sync::Arc::new(DialTo::starting_with(via.key));
    let endpoint = Endpoint::bind(
        &identity,
        crate::net::any_port(via.addr),
        pairing_authority(),
        dial.clone(),
    )
    .map_err(|e| format!("binding a local port: {e}"))?;
    let bound = endpoint
        .local_addr()
        .map_err(|e| format!("reading the bound address: {e}"))?;

    let mut client = esper_rendezvous::Client::invited(
        &endpoint,
        &identity,
        via.addr,
        via.key,
        &[],
        crate::broker_cmd::candidates(bound, via.addr),
        Some(invitation.token),
    )
    .await
    .map_err(|e| format!("reaching the broker at {}: {e}", via.addr))?;

    loop {
        let left = invitation.remaining(paths::now_secs());
        if left.is_zero() {
            return Err("nobody used that invitation before it expired".to_owned());
        }

        let told = match tokio::time::timeout(left, client.next()).await {
            Ok(Ok(told)) => told,
            Ok(Err(e)) => return Err(format!("waiting at the broker: {e}")),
            Err(_) => return Err("nobody used that invitation before it expired".to_owned()),
        };
        let esper_rendezvous::Told::Introduced(met) = told else {
            continue;
        };

        println!();
        println!("  · somebody arrived with the invitation");
        // They will dial us, so a path has to be open in this direction
        // first — and they must be dialable for the punch to go anywhere.
        dial.allow(met.peer);

        // **Spawned, never awaited.** A punch is fire-and-forget by design,
        // and waiting for one to fail would be waiting *instead of accepting*:
        // an incoming handshake is not driven until the application asks for
        // it, so a side that is busy punching is a side that answers nobody.
        // Measured: the guest dialled, waited five seconds and gave up while
        // this side was still punching at it.
        tokio::spawn({
            let endpoint = endpoint.clone();
            let where_to = met.everywhere();
            async move {
                esper_rendezvous::punch_at(&endpoint, &where_to, EACH_CANDIDATE).await;
            }
        });

        // A fresh offer, whose sixty seconds start now. Whoever arrived has
        // three guesses at the code inside that window, and they have the code.
        let mut offer = Offer::with_created_at(code.clone(), Instant::now());
        match pair_with_whoever_came(&endpoint, &identity, &mut offer).await {
            Ok(()) => return Ok(()),
            Err(e) => {
                // Not fatal: an attempt that failed is not the invitation
                // failing, and the person may simply have mistyped. The link
                // stands until it expires.
                println!("  · that attempt did not complete ({e}); still waiting");
            }
        }
    }
}

/// Accept one connection and pair over it.
async fn pair_with_whoever_came(
    endpoint: &Endpoint,
    identity: &Identity,
    offer: &mut Offer,
) -> Result<(), String> {
    let wait = offer
        .remaining(Instant::now())
        .ok_or("the pairing code expired before anybody used it")?;
    let conn = tokio::time::timeout(wait, endpoint.accept())
        .await
        .map_err(|_| "nobody completed the pairing in time".to_owned())?
        .ok_or("the endpoint closed")?
        .map_err(|e| format!("accepting: {e}"))?;

    let mut session = Session::open(
        conn.clone(),
        Hello::new(
            Role::Host,
            identity.public_key(),
            &crate::net::this_device(),
        ),
    )
    .await
    .map_err(|(e, _)| format!("opening the session: {e}"))?;
    let mut control = session
        .take_control()
        .ok_or("the session has no control stream")?;

    let peer_key = pair::host(&conn, &mut control, identity, offer, Instant::now())
        .await
        .map_err(|e| format!("pairing failed: {e}"))?;

    let name = session.peer_hello().name.clone();
    // **An invitation is one session, not a key for ever.** Somebody who
    // pasted a link to get help with their printer should not still be
    // reachable by that helper next year, and "remember to unpair them" is a
    // thing people forget while they are relieved the printer works.
    let device = crate::pairing::record_invited(peer_key, &name)?;
    crate::pairing::report(&device);

    // The host writes last, so the host must not close first: the exchange
    // ends with a confirmation the other side is still reading.
    tokio::time::timeout(Duration::from_secs(5), conn.closed())
        .await
        .ok();
    Ok(())
}

/// Take somebody up on an invitation.
pub fn run_join(args: &[String]) -> Result<(), String> {
    let [link] = args else {
        return Err(format!(
            "join takes the link somebody sent you: {} join <link>",
            crate::net::me()
        ));
    };
    let invitation = Invitation::parse(link)?;

    let now = paths::now_secs();
    if !invitation.is_live(now) {
        // Said without connecting to anything: an expired link is not the
        // broker's business, and telling somebody to ask for a new one is
        // better than a network error.
        return Err("that invitation has expired — ask for a new link".to_owned());
    }

    crate::net::install_crypto();
    let identity = paths::identity()?;
    let code = PairingCode::parse(&invitation.code)
        .map_err(|e| format!("that link's code cannot be used: {e}"))?;

    println!("🤝 esper join");
    println!();
    println!("  broker     {}", invitation.broker);
    println!("  expires    in {}", describe(invitation.remaining(now)));
    println!();

    crate::net::runtime()?.block_on(join(identity, invitation, code))
}

/// Meet the helper at the broker and pair with them.
async fn join(identity: Identity, invitation: Invitation, code: PairingCode) -> Result<(), String> {
    // Outbound starts pinned to the broker and learns the helper's key from
    // the introduction. **Never "any key"**: the broker's word about which key
    // to dial is not trust — the pairing code is what authenticates — but
    // dialling only what was named beats dialling anything at all.
    let dial = std::sync::Arc::new(DialTo::starting_with(invitation.broker_key));
    let endpoint = Endpoint::bind(
        &identity,
        crate::net::any_port(invitation.broker),
        // Inbound: nobody. A guest dials; it is not dialled.
        std::sync::Arc::new(esper_transport::NoPeers),
        dial.clone(),
    )
    .map_err(|e| format!("binding a local port: {e}"))?;
    let bound = endpoint
        .local_addr()
        .map_err(|e| format!("reading the bound address: {e}"))?;

    let mut client = esper_rendezvous::Client::invited(
        &endpoint,
        &identity,
        invitation.broker,
        invitation.broker_key,
        &[],
        crate::broker_cmd::candidates(bound, invitation.broker),
        Some(invitation.token),
    )
    .await
    .map_err(|e| format!("reaching the broker at {}: {e}", invitation.broker))?;

    let met = match tokio::time::timeout(WAIT_TO_MEET, client.next_introduction()).await {
        Ok(Ok(met)) => met,
        Ok(Err(e)) => return Err(format!("waiting to be introduced: {e}")),
        Err(_) => {
            return Err("nobody was waiting with that invitation — ask them to run \
                 `esper-engine invite` again"
                .to_owned());
        }
    };
    dial.allow(met.peer);
    println!("  · introduced; reaching them");

    let conn = esper_rendezvous::race_to(&endpoint, &met.everywhere(), EACH_CANDIDATE)
        .await
        .map_err(|failures| {
            format!(
                "could not reach them:\n{}",
                failures
                    .iter()
                    .map(|(addr, why)| format!("      {addr}: {why}"))
                    .collect::<Vec<_>>()
                    .join("\n")
            )
        })?;

    let mut session = Session::open(
        conn.clone(),
        Hello::new(
            Role::Controller,
            identity.public_key(),
            &crate::net::this_device(),
        ),
    )
    .await
    .map_err(|(e, _)| format!("opening the session: {e}"))?;
    let mut control = session
        .take_control()
        .ok_or("the session has no control stream")?;

    let peer_key = pair::controller(&conn, &mut control, &identity, &code)
        .await
        .map_err(|e| format!("pairing failed: {e}"))?;

    let name = session.peer_hello().name.clone();
    // **An invitation is one session, not a key for ever.** Somebody who
    // pasted a link to get help with their printer should not still be
    // reachable by that helper next year, and "remember to unpair them" is a
    // thing people forget while they are relieved the printer works.
    let device = crate::pairing::record_invited(peer_key, &name)?;
    crate::pairing::report(&device);

    // The guest read last, so the guest closes — which is what releases the
    // helper from its wait.
    conn.close(0u32.into(), b"paired");
    tokio::time::timeout(Duration::from_secs(2), endpoint.wait_idle())
        .await
        .ok();
    Ok(())
}

/// The keys this endpoint may dial, which grows as it learns them.
///
/// A fixed list cannot express "the broker, and then whoever it introduces me
/// to", and the alternative — an endpoint that dials any key — is the thing
/// pinning exists to avoid. This starts with one key and gains exactly the
/// ones a broker has named.
#[derive(Debug)]
struct DialTo(std::sync::Mutex<Vec<[u8; KEY_LEN]>>);

impl DialTo {
    fn starting_with(key: [u8; KEY_LEN]) -> Self {
        Self(std::sync::Mutex::new(vec![key]))
    }

    fn allow(&self, key: [u8; KEY_LEN]) {
        if let Ok(mut keys) = self.0.lock()
            && !keys.contains(&key)
        {
            keys.push(key);
        }
    }
}

impl esper_transport::PeerAuthority for DialTo {
    fn is_authorized(&self, public_key: &[u8; KEY_LEN]) -> bool {
        self.0.lock().is_ok_and(|keys| keys.contains(public_key))
    }

    fn describe(&self) -> &'static str {
        "the broker, and whoever it has introduced"
    }
}

/// A duration as somebody would say it.
fn describe(d: Duration) -> String {
    let secs = d.as_secs();
    if secs >= 3600 {
        format!("{} hour(s)", secs / 3600)
    } else if secs >= 60 {
        format!("{} minute(s)", secs / 60)
    } else {
        format!("{secs} second(s)")
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use std::net::Ipv4Addr;

    use super::*;

    fn invitation() -> Invitation {
        Invitation {
            broker: SocketAddr::from((Ipv4Addr::new(198, 51, 100, 7), 7443)),
            broker_key: [3u8; KEY_LEN],
            token: [9u8; KEY_LEN],
            code: "YC7P-9GPR".to_owned(),
            expires_at: 1_788_560_000,
        }
    }

    #[test]
    fn a_link_survives_being_sent_and_pasted() {
        let invite = invitation();
        let link = invite.link();
        assert!(link.starts_with("esper://join/"), "{link}");
        assert_eq!(Invitation::parse(&link).unwrap(), invite);

        // Somebody will paste the middle of it, or paste it with whitespace.
        let body = link.trim_start_matches("esper://join/");
        assert_eq!(Invitation::parse(body).unwrap(), invite);
        assert_eq!(Invitation::parse(&format!("  {link}\n")).unwrap(), invite);
    }

    #[test]
    fn base64url_round_trips_at_every_length() {
        // Every remainder of three, because that is where an encoder gets it
        // wrong, and every byte value.
        for len in 0..64usize {
            let bytes: Vec<u8> = (0..len).map(|i| (i as u8).wrapping_mul(37)).collect();
            let text = base64url(&bytes);
            assert_eq!(unbase64url(&text).unwrap(), bytes, "at length {len}");
            assert!(
                !text.contains('=') && !text.contains('+') && !text.contains('/'),
                "a link must survive being a URL and a chat message: {text}"
            );
        }
    }

    #[test]
    fn something_that_is_not_a_link_is_refused_rather_than_half_read() {
        assert!(Invitation::parse("").is_err());
        assert!(Invitation::parse("esper://join/").is_err());
        assert!(Invitation::parse("esper://join/!!!!").is_err());
        // Truncated.
        let link = invitation().link();
        let short = &link[..link.len() - 8];
        assert!(Invitation::parse(short).is_err());
        // And with something on the end, which is the shape of a link
        // somebody has appended to.
        assert!(Invitation::parse(&format!("{link}AAAA")).is_err());
    }

    #[test]
    fn a_newer_layout_is_refused_by_name() {
        // A future version of Esper may put more in a link. This one says so
        // rather than reading the first fields and guessing at the rest.
        let mut bytes = invitation().encode();
        *bytes.first_mut().unwrap() = 2;
        let e = Invitation::decode(&bytes).unwrap_err();
        assert!(e.contains("newer version"), "{e}");
    }

    #[test]
    fn an_expiry_is_read_the_way_somebody_writes_one() {
        assert_eq!(parse_expiry("30m").unwrap(), Duration::from_secs(1800));
        assert_eq!(parse_expiry("2h").unwrap(), Duration::from_secs(7200));
        assert_eq!(parse_expiry("90s").unwrap(), Duration::from_secs(90));
        assert_eq!(parse_expiry("15").unwrap(), Duration::from_secs(900));
        assert!(parse_expiry("0m").is_err());
        assert!(parse_expiry("soon").is_err());
        // A link that works next week is a credential somebody has forgotten
        // they issued.
        assert!(parse_expiry("2d").is_err());
        assert!(parse_expiry("25h").is_err());
        assert_eq!(parse_expiry("24h").unwrap(), MAX_EXPIRY);
    }

    #[test]
    fn an_expired_invitation_says_so_without_connecting_to_anything() {
        let invite = invitation();
        assert!(invite.is_live(invite.expires_at - 1));
        assert!(!invite.is_live(invite.expires_at));
        assert!(!invite.is_live(invite.expires_at + 10_000));
        assert_eq!(invite.remaining(invite.expires_at - 60).as_secs(), 60);
        assert_eq!(invite.remaining(invite.expires_at + 5).as_secs(), 0);
    }
}
