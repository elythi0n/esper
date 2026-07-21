//! Forwarding for two peers that could not find a path to each other.
//!
//! Tier 4, and the only tier that costs somebody real bandwidth — which is why
//! it is off unless the operator turns it on, and why it is the only one with
//! a ceiling.
//!
//! # A socket with a lookup table
//!
//! It forwards **opaque UDP**. It does not terminate the peers' QUIC, does not
//! decrypt anything, does not try, and could not: the session inside is
//! authenticated and encrypted between the two device keys, and a relay that
//! altered a byte would be caught by the peers rather than by itself.
//!
//! Not tunnelling QUIC inside QUIC, which is the obvious alternative and the
//! wrong one: two congestion controllers stacked on one path fight each other,
//! and a lost packet in the outer connection stalls every stream in the inner
//! one. A forwarder adds no header, no retransmission and no ordering, so the
//! peers' own loss recovery sees the path as it is.
//!
//! # One socket per side
//!
//! An allocation is **two** UDP sockets, one per peer. That is what removes the
//! need for either peer to send anything a plain QUIC endpoint cannot send:
//! whichever side a packet arrives on says which peer it came from, so there is
//! no token to exchange and no framing to agree.
//!
//! It also keeps each peer's view consistent, which QUIC requires: a peer sends
//! to one address and hears back from that same address, because its side's
//! socket is the one that answers it.
//!
//! # What a hostile party can do, stated
//!
//! Each side binds to the **first source that sends to it**, because under the
//! symmetric NAT that makes a relay necessary in the first place, a peer's
//! address here is *not* the one the broker already saw. So somebody who
//! guesses an allocation's port before the real peer sends can take the
//! binding and the session will fail to establish.
//!
//! That is a denial of service against one session by somebody who found a
//! random ephemeral port inside a few seconds, and it is **not** a way to read
//! anything: the QUIC handshake authenticates the device key, so a hijacker
//! gets a connection that refuses it. The relay is a socket with a lookup
//! table; it is not a party to the session.
//!
//! # It cannot be used to amplify
//!
//! It never sends to an address that has not sent to it, and it forwards one
//! packet for one packet with no header added. There is no request that makes
//! it emit more than it received, which is the property a reflector lacks.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// How long an allocation may go without a packet before it is freed.
///
/// A session that has stopped needs no forwarder, and an allocation is two
/// sockets and a place in a table. Sixty seconds is far longer than any gap a
/// live session has — the transport's own keepalive is ten — and short enough
/// that a peer that vanished costs a minute.
pub const IDLE: Duration = Duration::from_secs(60);

/// How many pairs one relay will forward for at once.
///
/// Each is two sockets and a task. An operator who wants more can say so; the
/// default is sized for a personal broker helping a family rather than for a
/// service.
pub const MAX_PAIRS: usize = 64;

/// Bytes per second one pair may use, each way, by default.
///
/// **The only tier with a ceiling**, because it is the only one spending
/// somebody else's bandwidth. Two megabytes a second is a comfortable 1080p
/// session; an operator who is paying for the line can lower it.
pub const DEFAULT_CEILING: u64 = 2 * 1024 * 1024;

/// What an operator will let a relay do.
#[derive(Debug, Clone, Copy)]
pub struct RelayLimits {
    /// Pairs forwarded for at once.
    pub pairs: usize,
    /// Bytes per second per pair, each way.
    pub ceiling: u64,
    /// How long an allocation may be idle.
    pub idle: Duration,
}

impl Default for RelayLimits {
    fn default() -> Self {
        Self {
            pairs: MAX_PAIRS,
            ceiling: DEFAULT_CEILING,
            idle: IDLE,
        }
    }
}

/// A ceiling on how fast bytes may pass, as a bucket that refills.
///
/// **Drops rather than queues.** This is real-time media: a frame delayed by a
/// second is worse than a frame missing, and a queue on a relay is a queue
/// nobody can see. Dropping also keeps the memory flat, which is the property
/// that matters on a machine somebody else is paying for.
#[derive(Debug)]
pub struct Bucket {
    /// Bytes per second.
    rate: u64,
    /// What is available now.
    available: f64,
    /// The most that can be saved up, so a quiet minute does not buy a burst.
    burst: f64,
    last: Instant,
}

impl Bucket {
    /// A bucket that refills at `rate` bytes a second.
    #[must_use]
    pub fn new(rate: u64, now: Instant) -> Self {
        Self {
            rate,
            // Starts full, so the handshake is not the thing that gets
            // dropped.
            available: rate as f64,
            // A quarter second of burst: enough for a keyframe, not enough to
            // matter to the line.
            burst: rate as f64 / 4.0,
            last: now,
        }
    }

    /// Whether this many bytes may pass now.
    pub fn allows(&mut self, bytes: usize, now: Instant) -> bool {
        let elapsed = now.saturating_duration_since(self.last).as_secs_f64();
        self.last = now;
        self.available =
            (self.available + elapsed * self.rate as f64).min(self.rate as f64 + self.burst);
        let wanted = bytes as f64;
        if self.available >= wanted {
            self.available -= wanted;
            true
        } else {
            false
        }
    }
}

/// One peer's side of an allocation.
#[derive(Debug)]
pub struct Side {
    /// Where that peer is, once it has sent something.
    pub seen: Option<SocketAddr>,
    /// What it may spend.
    pub bucket: Bucket,
}

impl Side {
    fn new(rate: u64, now: Instant) -> Self {
        Self {
            seen: None,
            bucket: Bucket::new(rate, now),
        }
    }
}

/// What to do with a packet that arrived.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Forward {
    /// Send it to this address, out of the other side's socket.
    To(SocketAddr),
    /// Drop it: the other side has not been seen yet, this source is not the
    /// one bound to this side, or the ceiling is reached.
    Drop,
}

/// The state of one forwarded pair, without any sockets in it.
///
/// Separate so the decisions — who is bound, what may pass, when it expires —
/// are testable without a network.
#[derive(Debug)]
pub struct Pair {
    a: Side,
    b: Side,
    last: Instant,
}

/// Which side of a pair a packet arrived on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Which {
    A,
    B,
}

impl Pair {
    /// A pair that has heard from neither side.
    #[must_use]
    pub fn new(ceiling: u64, now: Instant) -> Self {
        Self {
            a: Side::new(ceiling, now),
            b: Side::new(ceiling, now),
            last: now,
        }
    }

    /// A packet arrived on one side. Say where it goes.
    pub fn arrived(
        &mut self,
        which: Which,
        from: SocketAddr,
        bytes: usize,
        now: Instant,
    ) -> Forward {
        self.last = now;
        let (mine, theirs) = match which {
            Which::A => (&mut self.a, &self.b),
            Which::B => (&mut self.b, &self.a),
        };

        match mine.seen {
            // **First come, and then only that one.** See the note at the top
            // of this module: this is what a relay can do without a token, and
            // what it costs.
            None => mine.seen = Some(from),
            Some(bound) if bound != from => return Forward::Drop,
            Some(_) => {}
        }

        if !mine.bucket.allows(bytes, now) {
            return Forward::Drop;
        }
        match theirs.seen {
            Some(to) => Forward::To(to),
            // Nowhere to send it yet. The peers' own retransmission covers
            // this: QUIC resends what was not acknowledged.
            None => Forward::Drop,
        }
    }

    /// Whether nothing has passed for longer than this.
    #[must_use]
    pub fn idle_for(&self, now: Instant) -> Duration {
        now.saturating_duration_since(self.last)
    }
}

/// Every pair being forwarded for.
#[derive(Debug, Default)]
pub struct Pairs {
    inner: Arc<Mutex<HashMap<u64, Pair>>>,
    next: Arc<Mutex<u64>>,
}

impl Pairs {
    /// An empty set.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// How many pairs are being forwarded for.
    #[must_use]
    pub fn len(&self) -> usize {
        self.inner.lock().map_or(0, |p| p.len())
    }

    /// Whether none are.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Take a place for a new pair, if there is one.
    pub fn open(&self, limits: RelayLimits, now: Instant) -> Option<u64> {
        let mut pairs = self.inner.lock().ok()?;
        if pairs.len() >= limits.pairs {
            return None;
        }
        let mut next = self.next.lock().ok()?;
        let id = *next;
        *next = next.wrapping_add(1);
        pairs.insert(id, Pair::new(limits.ceiling, now));
        Some(id)
    }

    /// A packet arrived for a pair.
    pub fn arrived(
        &self,
        id: u64,
        which: Which,
        from: SocketAddr,
        bytes: usize,
        now: Instant,
    ) -> Forward {
        let Ok(mut pairs) = self.inner.lock() else {
            return Forward::Drop;
        };
        pairs
            .get_mut(&id)
            .map_or(Forward::Drop, |p| p.arrived(which, from, bytes, now))
    }

    /// Forget a pair.
    pub fn close(&self, id: u64) {
        if let Ok(mut pairs) = self.inner.lock() {
            pairs.remove(&id);
        }
    }

    /// Forget every pair that has gone quiet, and say which.
    pub fn sweep(&self, idle: Duration, now: Instant) -> Vec<u64> {
        let Ok(mut pairs) = self.inner.lock() else {
            return Vec::new();
        };
        let gone: Vec<u64> = pairs
            .iter()
            .filter(|(_, p)| p.idle_for(now) > idle)
            .map(|(id, _)| *id)
            .collect();
        for id in &gone {
            pairs.remove(id);
        }
        gone
    }
}

impl Clone for Pairs {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
            next: Arc::clone(&self.next),
        }
    }
}

/// Two sockets forwarding for one pair, until it goes quiet.
///
/// One packet in, one packet out, no header added and nothing kept. The task
/// ends when the pair has been idle for long enough or when either socket
/// fails, and it takes the pair's place in the table with it.
pub async fn forward(
    pairs: Pairs,
    id: u64,
    side_a: tokio::net::UdpSocket,
    side_b: tokio::net::UdpSocket,
    limits: RelayLimits,
) {
    // **Big enough that nothing is ever truncated.** A short buffer would not
    // fail, it would forward a *corrupted* packet — and a relay that quietly
    // damages somebody's session is worse than one that refuses to carry it.
    let mut from_a = vec![0u8; 65_535];
    let mut from_b = vec![0u8; 65_535];

    loop {
        let (which, from, bytes) = tokio::select! {
            got = side_a.recv_from(&mut from_a) => match got {
                Ok((n, from)) => (Which::A, from, n),
                Err(e) => {
                    tracing::debug!(error = %e, "a relay socket failed");
                    break;
                }
            },
            got = side_b.recv_from(&mut from_b) => match got {
                Ok((n, from)) => (Which::B, from, n),
                Err(e) => {
                    tracing::debug!(error = %e, "a relay socket failed");
                    break;
                }
            },
            // Nothing at all for as long as an allocation may be idle. If it
            // really has been, this ends; a pair that is merely quiet is not.
            () = tokio::time::sleep(limits.idle) => {
                if pairs.sweep(limits.idle, Instant::now()).contains(&id) {
                    break;
                }
                continue;
            }
        };

        let now = Instant::now();
        let Forward::To(to) = pairs.arrived(id, which, from, bytes, now) else {
            continue;
        };
        let (out, payload) = match which {
            Which::A => (&side_b, from_a.get(..bytes)),
            Which::B => (&side_a, from_b.get(..bytes)),
        };
        let Some(payload) = payload else { continue };
        if let Err(e) = out.send_to(payload, to).await {
            tracing::debug!(error = %e, "a relayed packet did not go out");
        }
    }

    pairs.close(id);
}

/// The address to tell a peer about, for a socket bound locally.
///
/// The port is the allocation's; the address is **the one that peer reached
/// this broker on**, which is the only one known to work from where it is
/// standing. A broker listening on every address cannot otherwise say which of
/// them to use.
#[must_use]
pub fn tell_peer(local: Option<IpAddr>, fallback: SocketAddr, port: u16) -> SocketAddr {
    match local {
        Some(ip) if !ip.is_unspecified() => SocketAddr::new(ip, port),
        _ => SocketAddr::new(fallback.ip(), port),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use std::net::Ipv4Addr;

    use super::*;

    fn addr(n: u8, port: u16) -> SocketAddr {
        (Ipv4Addr::new(203, 0, 113, n), port).into()
    }

    #[test]
    fn a_packet_goes_nowhere_until_both_sides_have_been_seen() {
        // The property that stops a relay being a reflector: it never sends to
        // an address that has not sent to it.
        let now = Instant::now();
        let mut pair = Pair::new(DEFAULT_CEILING, now);
        assert_eq!(
            pair.arrived(Which::A, addr(1, 1000), 1200, now),
            Forward::Drop,
            "nowhere to send it yet"
        );
        assert_eq!(
            pair.arrived(Which::B, addr(2, 2000), 1200, now),
            Forward::To(addr(1, 1000)),
            "now A is known"
        );
        assert_eq!(
            pair.arrived(Which::A, addr(1, 1000), 1200, now),
            Forward::To(addr(2, 2000))
        );
    }

    #[test]
    fn a_second_source_on_a_bound_side_is_dropped() {
        let now = Instant::now();
        let mut pair = Pair::new(DEFAULT_CEILING, now);
        pair.arrived(Which::A, addr(1, 1000), 100, now);
        pair.arrived(Which::B, addr(2, 2000), 100, now);
        assert_eq!(
            pair.arrived(Which::A, addr(9, 9999), 100, now),
            Forward::Drop,
            "a side is bound to one address, and somebody else is not it"
        );
        // And the real peer still works.
        assert_eq!(
            pair.arrived(Which::A, addr(1, 1000), 100, now),
            Forward::To(addr(2, 2000))
        );
    }

    #[test]
    fn the_ceiling_drops_rather_than_queues() {
        // A frame delayed by a second is worse than a frame missing, and a
        // queue on a relay is a queue nobody can see.
        let now = Instant::now();
        let rate = 10_000u64;
        let mut pair = Pair::new(rate, now);
        pair.arrived(Which::A, addr(1, 1000), 0, now);
        pair.arrived(Which::B, addr(2, 2000), 0, now);

        // A full bucket plus its burst, then nothing.
        let mut passed = 0u64;
        for _ in 0..100 {
            if pair.arrived(Which::A, addr(1, 1000), 1000, now) == Forward::To(addr(2, 2000)) {
                passed += 1000;
            }
        }
        assert!(
            passed <= rate + rate / 4,
            "{passed} bytes passed on a {rate}/s ceiling"
        );
        assert!(passed > 0, "the ceiling must not stop everything");

        // A second later, more may pass.
        let later = now + Duration::from_secs(1);
        assert_eq!(
            pair.arrived(Which::A, addr(1, 1000), 1000, later),
            Forward::To(addr(2, 2000))
        );
    }

    #[test]
    fn each_direction_has_its_own_ceiling() {
        // Otherwise a screen being sent one way would starve the keystrokes
        // coming back.
        let now = Instant::now();
        let mut pair = Pair::new(2000, now);
        pair.arrived(Which::A, addr(1, 1000), 0, now);
        pair.arrived(Which::B, addr(2, 2000), 0, now);
        // Spend A's allowance.
        while pair.arrived(Which::A, addr(1, 1000), 500, now) != Forward::Drop {}
        // B's is untouched.
        assert_eq!(
            pair.arrived(Which::B, addr(2, 2000), 500, now),
            Forward::To(addr(1, 1000))
        );
    }

    #[test]
    fn a_pair_that_has_gone_quiet_is_swept() {
        let now = Instant::now();
        let pairs = Pairs::new();
        let limits = RelayLimits::default();
        let id = pairs.open(limits, now).expect("a place");
        assert_eq!(pairs.len(), 1);

        assert!(pairs.sweep(limits.idle, now).is_empty(), "not yet");
        let later = now + limits.idle + Duration::from_secs(1);
        assert_eq!(pairs.sweep(limits.idle, later), vec![id]);
        assert!(pairs.is_empty());

        // And a packet for a pair that is gone goes nowhere.
        assert_eq!(
            pairs.arrived(id, Which::A, addr(1, 1000), 100, later),
            Forward::Drop
        );
    }

    #[test]
    fn a_relay_forwards_for_as_many_pairs_as_it_will_and_no_more() {
        let now = Instant::now();
        let pairs = Pairs::new();
        let limits = RelayLimits {
            pairs: 3,
            ..RelayLimits::default()
        };
        let ids: Vec<u64> = (0..3)
            .map(|_| pairs.open(limits, now).expect("within the limit"))
            .collect();
        assert!(pairs.open(limits, now).is_none(), "over the limit");

        pairs.close(*ids.first().expect("a pair was opened"));
        assert!(
            pairs.open(limits, now).is_some(),
            "a closed pair makes room"
        );
    }

    #[test]
    fn a_peer_is_told_the_address_it_reached_this_broker_on() {
        let fallback = SocketAddr::from(([10, 0, 0, 1], 7443));
        // The address the peer's own packets arrived on, which is the only one
        // known to work from where that peer is standing.
        assert_eq!(
            tell_peer(Some(IpAddr::from([198, 51, 100, 7])), fallback, 50000),
            SocketAddr::from(([198, 51, 100, 7], 50000))
        );
        // Unknown, or a wildcard, falls back to where the broker was told to
        // listen.
        assert_eq!(
            tell_peer(None, fallback, 50000),
            SocketAddr::from(([10, 0, 0, 1], 50000))
        );
        assert_eq!(
            tell_peer(Some(IpAddr::from([0, 0, 0, 0])), fallback, 50000),
            SocketAddr::from(([10, 0, 0, 1], 50000))
        );
    }
}
