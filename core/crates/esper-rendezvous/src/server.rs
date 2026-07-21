//! The broker: a listener that introduces peers and remembers nobody.
//!
//! # It accepts any key
//!
//! On purpose, and it is one of only two places in Esper that does. A broker
//! cannot know in advance which devices will use it — that is what an
//! introducer is for — so the peer authority accepts every well-formed key,
//! and the **signature on the request** is what proves a peer is the device it
//! claims. Peers, for their part, pin the broker's key: they are dialling one
//! specific machine, so there is no reason for them to accept any other.
//!
//! # It is internet-facing and unauthenticated
//!
//! Which makes it the most exposed thing this project ships, so the limits are
//! not an afterthought:
//!
//! - Every message is bounded before it is parsed, by the control stream's own
//!   frame limit.
//! - Every request is checked cheapest-first — shape, then the broker it was
//!   meant for, then freshness, then the signature — so a flood of rubbish
//!   costs a parse rather than a curve operation.
//! - The table has a size, and one address gets a share of it.
//! - A connection that says nothing is dropped rather than held.
//! - **Nothing is written to disk, ever**, so there is nothing to grow and
//!   nothing to survive a restart.

use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use esper_crypto::Identity;
use esper_crypto::identity::KEY_LEN;
use esper_transport::{Control, Endpoint, NoPeers, pairing_authority};
use tokio::sync::mpsc;

use crate::relay::{Pairs, RelayLimits, tell_peer};
use crate::table::{Arrival, Table};
use crate::wire::{Answer, CLOCK_SLACK_SECS, Meet, RENDEZVOUS_VERSION, Refusal, Relayed, Request};

/// How long a connection may say nothing before it is dropped.
///
/// A peer connects in order to ask; one that has not asked is either broken or
/// occupying a socket on purpose. Ten seconds is a slow round trip and a long
/// pause for a program.
pub const SILENCE: Duration = Duration::from_secs(10);

/// How long to wait for a refused peer to take its answer.
///
/// **A refusal is stream data, and closing a QUIC connection discards what has
/// not been sent.** A broker that answered and returned threw the answer away
/// — measured, every time, with the peer seeing "connection lost" instead of
/// the reason it was refused. So it waits for the peer to go first, bounded,
/// because a peer that never closes must not hold a task.
pub const FAREWELL: Duration = Duration::from_secs(2);

/// How many introductions may be queued for one peer.
///
/// A host is introduced to each device that comes looking. If it has stopped
/// reading, the queue fills and it loses its place rather than the broker
/// growing a buffer on its behalf.
pub const QUEUE: usize = 32;

/// What a broker will hold.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    /// Longest a connection may say nothing.
    pub silence: Duration,
    /// How many introductions to queue for one peer.
    pub queue: usize,
    /// What it will forward, if it forwards at all.
    ///
    /// `None` means it introduces and does not relay, which is the default:
    /// introducing costs a few packets, forwarding costs somebody's
    /// bandwidth, and an operator who will do the first and not the second is
    /// making a reasonable choice.
    pub relay: Option<RelayLimits>,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            silence: SILENCE,
            queue: QUEUE,
            relay: None,
        }
    }
}

/// What can go wrong running one.
#[derive(Debug, thiserror::Error)]
pub enum BrokerError {
    /// The listener could not be set up.
    #[error("{0}")]
    Transport(#[from] esper_transport::TransportError),
}

/// A running broker.
#[derive(Debug)]
pub struct Broker {
    endpoint: Endpoint,
    key: [u8; KEY_LEN],
    table: Arc<Mutex<Table>>,
    limits: Limits,
    /// Every pair being forwarded for. Empty and unused unless relaying is on.
    pairs: Pairs,
    /// Where this broker was told to listen, for the address it hands out
    /// when a peer's connection cannot say which of ours it reached.
    listen: SocketAddr,
}

impl Broker {
    /// Bind a broker to an address.
    pub fn bind(
        identity: &Identity,
        listen: SocketAddr,
        limits: Limits,
    ) -> Result<Self, BrokerError> {
        // **Any key inbound, nobody outbound.** `pairing_authority` is the
        // same "accept any well-formed key" that pairing uses, for the same
        // reason: the peer is not known in advance. Outbound is `NoPeers`
        // because a broker never dials anybody, and a broker that could would
        // be a way to make somebody else's machine scan a network.
        let endpoint = Endpoint::bind(identity, listen, pairing_authority(), Arc::new(NoPeers))?;
        Ok(Self {
            endpoint,
            key: identity.public_key(),
            table: Arc::new(Mutex::new(Table::new())),
            limits,
            pairs: Pairs::new(),
            listen,
        })
    }

    /// Where it is listening.
    pub fn local_addr(&self) -> Result<SocketAddr, std::io::Error> {
        self.endpoint.local_addr()
    }

    /// This broker's own key, which peers pin.
    #[must_use]
    pub const fn key(&self) -> [u8; KEY_LEN] {
        self.key
    }

    /// How many peers are waiting.
    #[must_use]
    pub fn waiting(&self) -> usize {
        self.table.lock().map_or(0, |t| t.len())
    }

    /// How many pairs are being forwarded for.
    #[must_use]
    pub fn relaying(&self) -> usize {
        self.pairs.len()
    }

    /// Serve until told to stop.
    pub async fn run(&self, mut stopping: tokio::sync::watch::Receiver<bool>) {
        loop {
            let accepted = tokio::select! {
                _ = stopping.changed() => return,
                accepted = self.endpoint.accept() => accepted,
            };
            let Some(conn) = accepted else { return };
            let conn = match conn {
                Ok(conn) => conn,
                // A failed handshake is one peer, not a fault in the broker.
                // At debug rather than warn: an internet-facing listener sees
                // these constantly, and a log that fills with them is a log
                // nobody reads.
                Err(e) => {
                    tracing::debug!(error = %e, "a connection did not complete");
                    continue;
                }
            };

            let table = Arc::clone(&self.table);
            let key = self.key;
            let limits = self.limits;
            let pairs = self.pairs.clone();
            let listen = self.listen;
            // A task per peer, because a peer that waits is a peer that waits.
            tokio::spawn(async move {
                serve_one(conn, table, key, limits, pairs, listen).await;
            });
        }
    }
}

/// One peer, from its connection to its departure.
#[allow(clippy::too_many_arguments)]
async fn serve_one(
    conn: quinn::Connection,
    table: Arc<Mutex<Table>>,
    broker_key: [u8; KEY_LEN],
    limits: Limits,
    pairs: Pairs,
    listen: SocketAddr,
) {
    // The whole address, not just the host part: the port is the other half
    // of a reflexive candidate, and it is the half a router rewrites.
    let from = conn.remote_address();

    // The peer opens the stream, because the peer is the one with something to
    // say. Bounded: a connection that opens nothing is not waiting for us.
    let mut control = match tokio::time::timeout(limits.silence, conn.accept_bi()).await {
        Ok(Ok((send, recv))) => Control::new(send, recv),
        Ok(Err(e)) => {
            tracing::debug!(error = %e, "a peer opened no stream");
            return;
        }
        Err(_) => {
            tracing::debug!(%from, "a peer connected and said nothing");
            return;
        }
    };

    let meet = match tokio::time::timeout(limits.silence, control.recv::<Meet>()).await {
        Ok(Ok(meet)) => meet,
        Ok(Err(e)) => {
            tracing::debug!(error = %e, "a peer sent something that was not a request");
            return;
        }
        Err(_) => {
            tracing::debug!(%from, "a peer opened a stream and said nothing");
            return;
        }
    };

    if let Some(why) = refuse(&meet, broker_key, crate::now_secs()) {
        tracing::debug!(%from, ?why, "refused a request");
        say_no(control, &conn, why).await;
        return;
    }

    // Introductions for this peer arrive here: from its own arrival, and from
    // everybody who turns up later naming it.
    let (to_peer, mut for_peer) = mpsc::channel::<Answer>(limits.queue.max(1));

    let who = meet.me;
    // Which of *our* addresses this peer reached us on. Not where it is —
    // that is `from` — but the one it is known to be able to get to, which is
    // what a relay address has to be built from.
    let local = conn.local_ip();
    let arrival = {
        let Ok(mut t) = table.lock() else {
            tracing::error!("the broker's table is unusable");
            return;
        };
        t.arrive(meet, from, local, to_peer)
    };

    let id = match arrival {
        Arrival::Refused(why) => {
            tracing::debug!(%from, ?why, "no room");
            say_no(control, &conn, why).await;
            return;
        }
        Arrival::Waiting { id } => id,
        Arrival::Met { id, theirs } => {
            if control.send(&Answer::Introduced(theirs)).await.is_err() {
                leave(&table, id);
                return;
            }
            id
        }
    };
    tracing::debug!(%from, waiting = table.lock().map_or(0, |t| t.len()), "a peer is waiting");

    // From here the peer waits, and is told about each device that comes
    // looking for it. Two things end it: the connection going away, or the
    // broker stopping.
    loop {
        tokio::select! {
            told = for_peer.recv() => match told {
                Some(answer) => {
                    if control.send(&answer).await.is_err() {
                        break;
                    }
                }
                // The table dropped this place. Nothing more is coming.
                None => break,
            },
            // What the peer asks after its first request — at present, only a
            // relay, which is asked for when a direct path could not be found.
            asked = control.recv::<Request>() => match asked {
                Ok(Request::Relay { with }) => {
                    let answer = arrange_relay(
                        &table,
                        &pairs,
                        &limits,
                        listen,
                        conn.local_ip(),
                        &who,
                        &with,
                    );
                    if control.send(&answer).await.is_err() {
                        break;
                    }
                }
                Err(e) => {
                    tracing::debug!(%from, error = %e, "a peer's stream ended");
                    break;
                }
            },
            // **The peer's own end of the stream is watched, not ignored.**
            // Without this a controller that got its introduction and left
            // would keep its place until the connection's idle timeout, and a
            // later peer would be handed addresses for a connection nobody is
            // serving.
            ended = conn.closed() => {
                tracing::debug!(%from, reason = %ended, "a peer left");
                break;
            }
        }
    }

    leave(&table, id);
    let _ = who;
}

/// Tell a peer no, and make sure it hears.
async fn say_no(mut control: Control, conn: &quinn::Connection, why: Refusal) {
    let _ = control.send(&Answer::Refused(why)).await;
    let _ = control.finish();
    // See `FAREWELL`: returning here would close the connection with the
    // answer still queued, and the peer would learn nothing but that the
    // connection went away.
    let _ = tokio::time::timeout(FAREWELL, conn.closed()).await;
}

/// Open a path between two peers that could not find one themselves.
///
/// Answers the asking peer and **tells the other side too**, because a
/// forwarder cannot learn where a peer is until that peer has sent something,
/// and a peer that was not told has no reason to send.
fn arrange_relay(
    table: &Arc<Mutex<Table>>,
    pairs: &Pairs,
    limits: &Limits,
    listen: SocketAddr,
    asker_local: Option<IpAddr>,
    asker: &[u8; KEY_LEN],
    wanted: &[u8; KEY_LEN],
) -> Answer {
    let Some(relay_limits) = limits.relay else {
        return Answer::Refused(Refusal::NoRelay);
    };

    // Mutual, again: a relay is a path between two devices that have named
    // each other, and asking for one is not a way to reach somebody who has
    // not asked for you.
    let (them, their_local) = {
        let Ok(t) = table.lock() else {
            return Answer::Refused(Refusal::RelayFull);
        };
        match t.mutual_peer(asker, wanted) {
            Some(found) => found,
            None => return Answer::Refused(Refusal::Malformed),
        }
    };

    let now = std::time::Instant::now();
    let Some(id) = pairs.open(relay_limits, now) else {
        return Answer::Refused(Refusal::RelayFull);
    };

    // A socket each, so which side a packet arrived on says which peer sent
    // it — which is what removes the need for either peer to send anything a
    // plain QUIC endpoint cannot send.
    let bind = SocketAddr::new(listen.ip(), 0);
    let (Ok(side_a), Ok(side_b)) = (
        std::net::UdpSocket::bind(bind),
        std::net::UdpSocket::bind(bind),
    ) else {
        pairs.close(id);
        return Answer::Refused(Refusal::RelayFull);
    };
    let (Ok(port_a), Ok(port_b)) = (side_a.local_addr(), side_b.local_addr()) else {
        pairs.close(id);
        return Answer::Refused(Refusal::RelayFull);
    };
    for socket in [&side_a, &side_b] {
        if socket.set_nonblocking(true).is_err() {
            pairs.close(id);
            return Answer::Refused(Refusal::RelayFull);
        }
    }
    let (Ok(side_a), Ok(side_b)) = (
        tokio::net::UdpSocket::from_std(side_a),
        tokio::net::UdpSocket::from_std(side_b),
    ) else {
        pairs.close(id);
        return Answer::Refused(Refusal::RelayFull);
    };

    tokio::spawn(crate::relay::forward(
        pairs.clone(),
        id,
        side_a,
        side_b,
        relay_limits,
    ));

    // Each side is told the address *it* reached this broker on, with the
    // other side's port: it sends to the socket that will answer it, which is
    // what QUIC's address validation needs.
    let for_asker = tell_peer(asker_local, listen, port_a.port());
    let for_them = tell_peer(their_local, listen, port_b.port());

    if them
        .try_send(Answer::Relayed(Relayed {
            peer: *asker,
            at: for_them,
        }))
        .is_err()
    {
        // The other side is not reading, so there is nobody to forward to.
        pairs.close(id);
        return Answer::Refused(Refusal::Malformed);
    }

    tracing::debug!(?for_asker, ?for_them, "forwarding for a pair");
    Answer::Relayed(Relayed {
        peer: *wanted,
        at: for_asker,
    })
}

/// Give up a place.
fn leave(table: &Arc<Mutex<Table>>, id: u64) {
    if let Ok(mut t) = table.lock() {
        t.leave(id);
    }
}

/// Why this request will not be taken, if it will not.
///
/// **Cheapest first.** A flood of rubbish should cost a comparison and not a
/// signature verification, so the order is: shape, the broker it names, its
/// freshness, and only then the curve operation.
#[must_use]
pub fn refuse(meet: &Meet, broker_key: [u8; KEY_LEN], now: u64) -> Option<Refusal> {
    if meet.version != RENDEZVOUS_VERSION {
        return Some(Refusal::Version(RENDEZVOUS_VERSION));
    }
    if !meet.is_well_formed() {
        return Some(Refusal::Malformed);
    }
    if meet.broker != broker_key {
        return Some(Refusal::NotThisBroker);
    }
    if meet.at.abs_diff(now) > CLOCK_SLACK_SECS {
        return Some(Refusal::Stale);
    }
    if !meet.is_signed() {
        return Some(Refusal::NotSigned);
    }
    None
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use std::net::{Ipv4Addr, SocketAddr};

    use super::*;
    use crate::wire::KeyBytes;

    fn address() -> SocketAddr {
        (Ipv4Addr::new(192, 168, 1, 10), 7645).into()
    }

    fn signed(me: &Identity, with: &[u8; KEY_LEN], broker: [u8; KEY_LEN], at: u64) -> Meet {
        Meet::new(me, &[*with], vec![address()], broker, at)
    }

    #[test]
    fn a_well_made_request_is_taken() {
        let me = Identity::generate().unwrap();
        let peer = Identity::generate().unwrap();
        let broker = [3u8; KEY_LEN];
        let meet = signed(&me, &peer.public_key(), broker, 1_000_000);
        assert_eq!(refuse(&meet, broker, 1_000_000), None);
        // Clocks that disagree by less than the window still meet.
        assert_eq!(refuse(&meet, broker, 1_000_000 + CLOCK_SLACK_SECS), None);
    }

    #[test]
    fn a_request_for_another_broker_is_refused() {
        // Otherwise a request captured at one broker could be replayed at
        // another, which is why the broker's key is inside the signature.
        let me = Identity::generate().unwrap();
        let peer = Identity::generate().unwrap();
        let meet = signed(&me, &peer.public_key(), [3u8; KEY_LEN], 1_000_000);
        assert_eq!(
            refuse(&meet, [4u8; KEY_LEN], 1_000_000),
            Some(Refusal::NotThisBroker)
        );
    }

    #[test]
    fn an_old_request_is_refused() {
        let me = Identity::generate().unwrap();
        let peer = Identity::generate().unwrap();
        let broker = [3u8; KEY_LEN];
        let meet = signed(&me, &peer.public_key(), broker, 1_000_000);
        assert_eq!(
            refuse(&meet, broker, 1_000_000 + CLOCK_SLACK_SECS + 1),
            Some(Refusal::Stale)
        );
        // And one from the future, which is the same problem with the sign
        // flipped — a peer whose clock is ahead is not a reason to accept a
        // request for ever.
        assert_eq!(
            refuse(&meet, broker, 1_000_000 - CLOCK_SLACK_SECS - 1),
            Some(Refusal::Stale)
        );
    }

    #[test]
    fn every_field_the_signature_covers_is_actually_covered() {
        // **The test that makes the signature worth having.** Each of these is
        // a field an attacker would want to change: who is asking, who they
        // want, where to send the answer, which broker, and when.
        let me = Identity::generate().unwrap();
        let peer = Identity::generate().unwrap();
        let other = Identity::generate().unwrap();
        let broker = [3u8; KEY_LEN];
        let good = signed(&me, &peer.public_key(), broker, 1_000_000);
        assert!(good.is_signed());

        let mut tampered = good.clone();
        tampered.me = other.public_key();
        assert!(!tampered.is_signed(), "the asker's key is not covered");

        let mut tampered = good.clone();
        tampered.with = vec![KeyBytes(other.public_key())];
        assert!(!tampered.is_signed(), "who they want is not covered");

        let mut tampered = good.clone();
        tampered.candidates = vec![(Ipv4Addr::new(203, 0, 113, 9), 1).into()];
        assert!(!tampered.is_signed(), "the addresses are not covered");

        let mut tampered = good.clone();
        tampered.broker = [4u8; KEY_LEN];
        assert!(!tampered.is_signed(), "the broker is not covered");

        let mut tampered = good.clone();
        tampered.at = 2_000_000;
        assert!(!tampered.is_signed(), "the time is not covered");

        let mut tampered = good.clone();
        tampered.version = RENDEZVOUS_VERSION + 1;
        assert!(!tampered.is_signed(), "the version is not covered");

        // And a request signed by a different key does not pass as this one.
        let forged = signed(&other, &peer.public_key(), broker, 1_000_000);
        let mut swapped = forged;
        swapped.me = me.public_key();
        assert!(!swapped.is_signed());
    }

    #[test]
    fn a_request_that_is_outside_its_bounds_is_refused_before_anything_else() {
        let me = Identity::generate().unwrap();
        let peer = Identity::generate().unwrap();
        let broker = [3u8; KEY_LEN];

        // Nobody named.
        let mut bare = signed(&me, &peer.public_key(), broker, 1_000_000);
        bare.with.clear();
        assert_eq!(refuse(&bare, broker, 1_000_000), Some(Refusal::Malformed));

        // Naming yourself.
        let mut selfish = signed(&me, &me.public_key(), broker, 1_000_000);
        selfish.with = vec![KeyBytes(me.public_key())];
        assert_eq!(
            refuse(&selfish, broker, 1_000_000),
            Some(Refusal::Malformed)
        );

        // No address to be reached at.
        let mut nowhere = signed(&me, &peer.public_key(), broker, 1_000_000);
        nowhere.candidates.clear();
        assert_eq!(
            refuse(&nowhere, broker, 1_000_000),
            Some(Refusal::Malformed)
        );

        // More addresses than a machine has.
        let mut many = signed(&me, &peer.public_key(), broker, 1_000_000);
        many.candidates = (0..64)
            .map(|n| SocketAddr::from((Ipv4Addr::new(10, 0, 0, 1), 1000 + n)))
            .collect();
        assert_eq!(refuse(&many, broker, 1_000_000), Some(Refusal::Malformed));

        // More devices than a person owns.
        let mut crowd = signed(&me, &peer.public_key(), broker, 1_000_000);
        crowd.with = (0..128).map(|n| KeyBytes([n as u8; KEY_LEN])).collect();
        assert_eq!(refuse(&crowd, broker, 1_000_000), Some(Refusal::Malformed));
    }

    #[test]
    fn a_version_this_broker_does_not_speak_is_named_in_the_refusal() {
        // Named rather than refused blankly: an engine told "malformed" would
        // have nothing to act on, and this is the one refusal where an
        // upgrade is the answer.
        let me = Identity::generate().unwrap();
        let peer = Identity::generate().unwrap();
        let broker = [3u8; KEY_LEN];
        let mut ahead = signed(&me, &peer.public_key(), broker, 1_000_000);
        ahead.version = RENDEZVOUS_VERSION + 7;
        assert_eq!(
            refuse(&ahead, broker, 1_000_000),
            Some(Refusal::Version(RENDEZVOUS_VERSION))
        );
    }
}
