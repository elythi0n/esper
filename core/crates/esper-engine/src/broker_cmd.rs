//! Reaching a peer through a broker.
//!
//! Tier 3 of the connectivity ladder: two machines behind ordinary routers
//! cannot dial each other, so a broker introduces them and is then dropped.
//! **There is no default broker address anywhere in Esper** — if nobody has
//! configured one, this tier does not exist, and a failed connection says so
//! rather than falling back to somebody else's server.
//!
//! # What is proven and what is not
//!
//! The rendezvous — pinning the broker, the signed request, the mutual naming,
//! the introduction, and dialling every candidate at once — is tested end to
//! end over loopback. The **hole punch** is not: there is no router between
//! two sockets on one machine, so what happens when two consumer NATs are
//! involved is an open question until it is tried on two networks. The code
//! for it is here, it is small, and it is marked.

use std::net::SocketAddr;
use std::time::Duration;

use esper_crypto::identity::KEY_LEN;
use esper_rendezvous::{Client, Told, Via};
use esper_transport::{Endpoint, Session};

/// How long each candidate gets before it is given up on.
///
/// Shorter than a plain dial's deadline because several are tried at once and
/// some are expected to fail: a LAN address that is not on this network fails
/// immediately, and a reflexive address a router will not honour fails
/// silently. Five seconds is a slow handshake and a long wait when something
/// else is already working.
const EACH_CANDIDATE: Duration = Duration::from_secs(5);

/// How long to keep punching before giving up on the mapping.
///
/// The dial is *expected* to fail, so this only bounds how long the attempt
/// spends failing.
const PUNCH: Duration = Duration::from_secs(2);

/// Read a broker's key from the hex it prints.
pub fn parse_key(raw: &str) -> Result<[u8; KEY_LEN], String> {
    let clean: String = raw.trim().chars().filter(|c| !c.is_whitespace()).collect();
    if clean.len() != KEY_LEN * 2 {
        return Err(format!(
            "a broker key is {} hex characters — `esper-broker serve` prints it; {:?} has {}",
            KEY_LEN * 2,
            clean,
            clean.len()
        ));
    }
    let mut key = [0u8; KEY_LEN];
    for (i, byte) in key.iter_mut().enumerate() {
        let pair = clean
            .get(i * 2..i * 2 + 2)
            .ok_or_else(|| "a broker key is hex".to_owned())?;
        *byte = u8::from_str_radix(pair, 16).map_err(|_| format!("{pair:?} is not hex"))?;
    }
    if !esper_crypto::Identity::public_key_is_usable(&key) {
        return Err("that is not a usable key".to_owned());
    }
    Ok(key)
}

/// Where this machine believes it can be reached, for a broker to pass on.
#[must_use]
pub fn candidates(bound: SocketAddr, toward: SocketAddr) -> Vec<SocketAddr> {
    esper_rendezvous::local_candidates(bound, toward)
}

/// Wait at a broker to be found, and punch towards whoever comes.
///
/// Runs for as long as the host does. Every introduction is reported, because
/// a host whose operator cannot see who asked after it through a broker has no
/// way to notice anything unusual.
pub async fn wait_to_be_found(
    endpoint: Endpoint,
    identity: std::sync::Arc<esper_crypto::Identity>,
    via: Via,
    paired: Vec<[u8; KEY_LEN]>,
    candidates: Vec<SocketAddr>,
) {
    let mut client =
        match Client::arrive(&endpoint, &identity, via.addr, via.key, &paired, candidates).await {
            Ok(client) => client,
            Err(e) => {
                // Not fatal: a host reachable by address works without a broker,
                // and saying so beats refusing to start.
                println!("  · not waiting at the broker ({e})");
                return;
            }
        };

    loop {
        match client.next().await {
            Ok(Told::Introduced(met)) => {
                println!(
                    "  · {} is looking for this machine through the broker",
                    esper_crypto::Fingerprint::of(&met.peer).hex()
                );
                punch(&endpoint, met.everywhere());
            }
            // **The other side could not find a direct path, so the broker is
            // forwarding.** This side has to send something before a forwarder
            // can learn where it is, and a peer that was not told has no
            // reason to send — which is why this arrives without being asked
            // for.
            Ok(Told::Relayed(path)) => {
                println!(
                    "  · forwarding through the broker for {} at {}",
                    esper_crypto::Fingerprint::of(&path.peer).hex(),
                    path.at
                );
                punch(&endpoint, vec![path.at]);
            }
            Err(e) => {
                println!("  · the broker is no longer introducing anybody ({e})");
                return;
            }
        }
    }
}

/// Punch towards a peer, without waiting for it.
///
/// **Spawned, never awaited.** A punch is fire-and-forget by design, and
/// waiting for one to fail would be waiting *instead of accepting*: an
/// incoming handshake is not driven until the application asks for it, so a
/// side that is busy punching is a side that answers nobody. Measured, in the
/// invitation flow: the other end dialled, waited five seconds and gave up
/// while this side was still punching at it.
fn punch(endpoint: &Endpoint, where_to: Vec<SocketAddr>) {
    let endpoint = endpoint.clone();
    tokio::spawn(async move {
        esper_rendezvous::punch_at(&endpoint, &where_to, PUNCH).await;
    });
}

/// Meet a peer at a broker and open a session with it.
///
/// The controller's half: it is given a broker and a device key, and every
/// address comes from the introduction.
pub async fn reach(
    endpoint: &Endpoint,
    identity: &esper_crypto::Identity,
    via: Via,
    peer: [u8; KEY_LEN],
    candidates: Vec<SocketAddr>,
    hello: esper_proto::Hello,
) -> Result<Session, String> {
    println!("  broker     {} — waiting to be introduced", via.addr);
    let (met, session) = esper_rendezvous::reach(
        endpoint,
        identity,
        via,
        peer,
        candidates,
        hello,
        EACH_CANDIDATE,
    )
    .await
    .map_err(|e| e.to_string())?;

    if let Some(seen) = met.seen_at {
        println!("      the broker saw that device at {seen}");
    }
    // Where it was reached and which rung that was is the ladder's line to
    // print: saying it here as well would say it twice.
    Ok(session)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use std::net::Ipv4Addr;

    use super::*;

    #[test]
    fn a_broker_key_round_trips_through_the_hex_it_prints() {
        let identity = esper_crypto::Identity::generate().unwrap();
        let key = identity.public_key();
        let printed: String = key.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(parse_key(&printed).unwrap(), key);
        // Whitespace a person copied along with it.
        assert_eq!(parse_key(&format!("  {printed}\n")).unwrap(), key);
    }

    #[test]
    fn a_fingerprint_is_not_accepted_as_a_key() {
        // The distinction that matters: a fingerprint is sixty-four bits of a
        // hash, for a person to read aloud. Pinning against it would be a
        // security decision made on a birthday bound, so the length check is
        // load-bearing rather than tidiness.
        let identity = esper_crypto::Identity::generate().unwrap();
        let fingerprint = identity.fingerprint().hex();
        let e = parse_key(&fingerprint).unwrap_err();
        assert!(e.contains("64 hex characters"), "{e}");
    }

    #[test]
    fn a_key_that_is_not_a_key_is_refused() {
        assert!(parse_key("").is_err());
        assert!(parse_key(&"z".repeat(64)).is_err());
        // All zeroes is the shape of a key and is not one: the identity layer
        // knows which points are unusable, and this asks it rather than
        // guessing.
        assert!(parse_key(&"00".repeat(32)).is_err());
    }

    #[test]
    fn a_bound_address_is_its_own_candidate() {
        let bound = SocketAddr::from((Ipv4Addr::new(192, 168, 1, 5), 7645));
        assert_eq!(
            candidates(bound, SocketAddr::from((Ipv4Addr::LOCALHOST, 9))),
            vec![bound]
        );
    }

    #[test]
    fn a_host_bound_to_everything_offers_the_address_that_reaches_the_broker() {
        // A machine bound to every address cannot say which of them a peer
        // should use, so it asks the kernel which one would reach the broker.
        // The port is its own, not the probe's.
        let bound = SocketAddr::from((Ipv4Addr::UNSPECIFIED, 7645));
        let toward = SocketAddr::from((Ipv4Addr::LOCALHOST, 9));
        let found = candidates(bound, toward);
        assert_eq!(found.len(), 1, "{found:?}");
        let one = found.first().expect("one candidate");
        assert_eq!(one.port(), 7645, "the port must be the one we listen on");
        assert_eq!(one.ip(), Ipv4Addr::LOCALHOST, "towards loopback, it is us");
    }
}
