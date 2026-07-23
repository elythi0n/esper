//! Answering queries about this device.
//!
//! # What it answers, and what it ignores
//!
//! Only questions about `_esper._udp.local` and about this device's own
//! instance. Everything else on the segment — printers, media players, other
//! people's laptops — is somebody else's business, and answering it would be
//! both wrong and rude.
//!
//! # Announce, and say goodbye
//!
//! A responder announces itself unsolicited when it starts, because a browser
//! that was already open would otherwise not see it until it next asked. And
//! it sends the same records with a TTL of zero when it stops, which is how a
//! machine that has gone stops being offered as connectable — otherwise every
//! browser on the segment keeps it for two minutes and a person clicks a
//! device that is not there.

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use crate::name::Name;
use crate::service::{Advertisement, SERVICE};
use crate::wire::{Data, Message, Record, TYPE_ANY, TYPE_PTR, TYPE_SRV, TYPE_TXT};
use crate::{DiscoveryError, socket};

/// The least time between two answers **to the same asker**.
///
/// The documents ask for a second between responses to the same question.
/// Per asker rather than globally, which is the fix for the thing a single
/// global limiter gets wrong: one machine asking twice a second would
/// otherwise silence this responder for everybody else on the segment, which
/// is a denial of service that needs no privilege and no spoofing.
const PER_ASKER: Duration = Duration::from_secs(1);

/// The least time between two answers to **anybody**.
///
/// The other half: a segment with a hundred queriers on it must not turn this
/// into a hundred packets. Short enough that the ordinary case — one machine
/// looking for another — is answered immediately.
const FLOOR: Duration = Duration::from_millis(50);

/// Who has been answered, and when.
///
/// Its own type because it is the whole rate-limiting policy, and a policy
/// that lives inside an `async fn` next to a socket is a policy nobody can
/// test without a network.
///
/// **The table is the only thing a stranger can make grow, and it is bounded
/// by arithmetic rather than by a cap.** An entry is added only when an answer
/// is *sent*; answers are at most one per [`FLOOR`]; entries live
/// [`PER_ASKER`]. So it holds at most `PER_ASKER / FLOOR` of them — twenty —
/// however many machines are shouting. A cap on top of that would be a guard
/// that could never fire, and this project has found enough of those to know
/// they are worse than none.
#[derive(Debug, Default)]
struct Pace {
    /// Askers and when each was last answered, oldest first.
    recent: Vec<(std::net::IpAddr, Instant)>,
    /// When anybody was last answered.
    last: Option<Instant>,
}

impl Pace {
    /// Whether to answer this asker now, recording it if so.
    fn may_answer(&mut self, who: std::net::IpAddr, now: Instant) -> bool {
        // Anything older than the per-asker interval can never refuse
        // anything, so it is dropped rather than searched.
        self.recent
            .retain(|(_, at)| now.saturating_duration_since(*at) < PER_ASKER);

        if self
            .last
            .is_some_and(|then| now.saturating_duration_since(then) < FLOOR)
        {
            return false;
        }
        if self
            .recent
            .iter()
            .any(|(addr, at)| *addr == who && now.saturating_duration_since(*at) < PER_ASKER)
        {
            return false;
        }

        self.recent.push((who, now));
        self.last = Some(now);
        true
    }
}

/// Whether an address is somewhere a question about this segment could have
/// come from.
///
/// **This is the anti-reflection rule.** The socket is bound to every address
/// on the machine, so anybody on the internet can send a unicast query to port
/// 5353 and be answered — and an answer is several times larger than the
/// question, which is the shape of an amplifier. Multicast never crosses a
/// router; a neighbour therefore has a neighbour's address.
///
/// The cost is stated rather than hidden: a LAN that uses public addresses —
/// rare, and mostly universities — loses mDNS discovery, and keeps every other
/// rung of the ladder. The alternative is answering the internet.
fn could_be_a_neighbour(addr: std::net::IpAddr) -> bool {
    match addr {
        std::net::IpAddr::V4(v4) => {
            v4.is_loopback() || v4.is_private() || v4.is_link_local() || is_shared(v4)
        }
        // The socket is IPv4, so this cannot arrive; answered honestly rather
        // than with an `unreachable`.
        std::net::IpAddr::V6(v6) => v6.is_loopback(),
    }
}

/// `100.64.0.0/10`, the carrier-grade NAT range — and where a tailnet lives.
const fn is_shared(addr: std::net::Ipv4Addr) -> bool {
    let [a, b, ..] = addr.octets();
    a == 100 && b >= 64 && b <= 127
}

/// Announces this device and answers questions about it.
#[derive(Debug)]
pub struct Responder {
    socket: socket::Multicast,
    advertisement: Advertisement,
    service: Name,
    instance: Name,
    /// Who has been answered lately, and when.
    pace: Pace,
    /// Counted rather than logged one by one: a segment with a misbehaving
    /// implementation on it would otherwise fill the log.
    dropped: u64,
    answered: u64,
}

impl Responder {
    /// Open a socket and announce, on whichever interfaces were named.
    ///
    /// An empty list means the one the system would choose, which is what a
    /// machine with one route to the LAN wants and what a machine whose
    /// interfaces cannot be listed gets.
    pub async fn start(
        advertisement: Advertisement,
        on: &[std::net::Ipv4Addr],
    ) -> Result<Self, DiscoveryError> {
        let socket = socket::open(on).map_err(|e| DiscoveryError::Socket(e.to_string()))?;
        let service = Name::parse(SERVICE)?;
        let instance = advertisement.instance()?;
        let mut me = Self {
            socket,
            advertisement,
            service,
            instance,
            pace: Pace::default(),
            dropped: 0,
            answered: 0,
        };
        me.announce().await?;
        Ok(me)
    }

    /// What is being advertised.
    #[must_use]
    pub const fn advertisement(&self) -> &Advertisement {
        &self.advertisement
    }

    /// How many questions have been answered, and how many packets dropped.
    #[must_use]
    pub const fn counts(&self) -> (u64, u64) {
        (self.answered, self.dropped)
    }

    /// Send the records unsolicited.
    pub async fn announce(&mut self) -> Result<(), DiscoveryError> {
        let message = self.response()?;
        self.send_to(&message, socket::destination()).await
    }

    /// Tell the segment this device is going.
    ///
    /// The same records with a TTL of zero, which is how the protocol spells
    /// "forget this". Best-effort by nature: it is one packet at shutdown, and
    /// the TTL covers the case where it does not arrive.
    pub async fn goodbye(&mut self) -> Result<(), DiscoveryError> {
        let mut message = self.response()?;
        for record in message
            .answers
            .iter_mut()
            .chain(message.additional.iter_mut())
        {
            record.ttl = 0;
        }
        self.send_to(&message, socket::destination()).await
    }

    /// Wait for one packet and answer it if it was for us.
    ///
    /// A malformed packet is counted and dropped: it is a peer's bytes, not a
    /// fault of ours, and a responder that stopped on one could be stopped by
    /// anyone on the segment.
    pub async fn serve_one(&mut self) -> Result<(), DiscoveryError> {
        let mut buffer = vec![0u8; crate::wire::MAX_PACKET];
        let (len, from) = self
            .socket
            .socket()
            .recv_from(&mut buffer)
            .await
            .map_err(|e| DiscoveryError::Socket(e.to_string()))?;
        let Some(packet) = buffer.get(..len) else {
            self.dropped = self.dropped.saturating_add(1);
            return Ok(());
        };
        let Ok(message) = Message::decode(packet) else {
            self.dropped = self.dropped.saturating_add(1);
            return Ok(());
        };
        if message.response {
            return Ok(());
        }

        // **Before anything is parsed further: is this a neighbour?** A
        // question about this segment came from this segment, and a question
        // from anywhere else is somebody using this machine to send a larger
        // packet to a third party.
        if !could_be_a_neighbour(from.ip()) {
            self.dropped = self.dropped.saturating_add(1);
            return Ok(());
        }

        let wants = message.questions.iter().find(|q| self.concerns(q));
        let Some(question) = wants else {
            return Ok(());
        };

        let now = Instant::now();
        if !self.pace.may_answer(from.ip(), now) {
            return Ok(());
        }
        self.answered = self.answered.saturating_add(1);

        let reply = self.response()?;
        // A querier that asked for a direct answer gets one; otherwise the
        // group hears it, which is what lets a browser learn about a device
        // somebody else asked about.
        let to = if question.unicast {
            from
        } else {
            socket::destination()
        };
        self.send_to(&reply, to).await
    }

    /// Whether a question is one this device should answer.
    fn concerns(&self, question: &crate::wire::Question) -> bool {
        let about_service =
            question.name == self.service && matches!(question.kind, TYPE_PTR | TYPE_ANY);
        let about_us = question.name == self.instance
            && matches!(question.kind, TYPE_SRV | TYPE_TXT | TYPE_ANY);
        about_service || about_us
    }

    /// The full answer: the pointer, and everything needed to act on it.
    fn response(&self) -> Result<Message, DiscoveryError> {
        let (ptr, extra) = self.advertisement.records()?;
        Ok(Message {
            id: 0,
            response: true,
            questions: Vec::new(),
            answers: vec![ptr],
            additional: extra,
        })
    }

    async fn send_to(&self, message: &Message, to: SocketAddr) -> Result<(), DiscoveryError> {
        let bytes = message.encode();
        self.socket
            .send_to(&bytes, to)
            .await
            .map_err(|e| DiscoveryError::Socket(e.to_string()))
    }

    /// Where this machine is announcing itself, for a log line.
    #[must_use]
    pub fn describe_interfaces(&self) -> String {
        self.socket.describe()
    }
}

/// The records a responder would send, without a socket.
///
/// For testing what goes on the wire, and for the interoperability check that
/// parses it with something else.
pub fn records_of(advertisement: &Advertisement) -> Result<Vec<Record>, DiscoveryError> {
    let (ptr, extra) = advertisement.records()?;
    let mut all = vec![ptr];
    all.extend(extra);
    Ok(all)
}

/// Whether a record set says a device has gone.
#[must_use]
pub fn is_goodbye(records: &[Record]) -> bool {
    records
        .iter()
        .any(|r| r.ttl == 0 && matches!(r.data, Data::Ptr(_)))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use std::net::{IpAddr, Ipv4Addr};

    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().expect("an address")
    }

    #[test]
    fn one_asker_asking_twice_is_answered_once() {
        let mut pace = Pace::default();
        let start = Instant::now();
        assert!(pace.may_answer(ip("192.168.1.5"), start));
        assert!(!pace.may_answer(ip("192.168.1.5"), start + Duration::from_millis(200)));
        // And again once its second has passed.
        assert!(pace.may_answer(ip("192.168.1.5"), start + Duration::from_millis(1100)));
    }

    /// One noisy machine must not silence the responder for everybody else.
    ///
    /// This is what a single global limiter got wrong: a querier asking twice
    /// a second would hold the only slot there was, and every other machine on
    /// the segment would be met with silence — a denial of service that needs
    /// no privilege and no spoofing.
    #[test]
    fn a_noisy_asker_does_not_silence_the_segment() {
        let mut pace = Pace::default();
        let start = Instant::now();
        assert!(pace.may_answer(ip("192.168.1.5"), start));
        assert!(!pace.may_answer(ip("192.168.1.5"), start + Duration::from_millis(60)));
        assert!(
            pace.may_answer(ip("192.168.1.6"), start + Duration::from_millis(60)),
            "a different machine was refused because of somebody else"
        );
    }

    #[test]
    fn a_burst_from_many_askers_is_still_paced() {
        // The other half: a hundred queriers must not become a hundred packets
        // in the same instant.
        let mut pace = Pace::default();
        let start = Instant::now();
        let mut answered = 0;
        for n in 0..100u8 {
            if pace.may_answer(IpAddr::V4(Ipv4Addr::new(192, 168, 1, n)), start) {
                answered += 1;
            }
        }
        assert_eq!(answered, 1, "the floor did not hold");
    }

    /// A thousand strangers cost twenty entries.
    ///
    /// The table is the only thing an outsider can make grow, and what bounds
    /// it is the floor rather than a cap: an entry is written only when an
    /// answer goes out, and answers are one per fifty milliseconds.
    #[test]
    fn a_flood_of_askers_cannot_grow_the_table() {
        let most = (PER_ASKER.as_millis() / FLOOR.as_millis()) as usize + 1;
        let start = Instant::now();

        // Spaced exactly at the floor, which is the fastest an asker can be
        // recorded at all.
        let mut pace = Pace::default();
        for n in 0..1000u32 {
            let addr = Ipv4Addr::from(n.saturating_add(0x0A00_0000));
            let at = start + FLOOR.saturating_mul(n);
            let _ = pace.may_answer(IpAddr::V4(addr), at);
            assert!(pace.recent.len() <= most, "{} entries", pace.recent.len());
        }

        // And all in the same instant, which is what a flood actually looks
        // like: one is answered and the rest are not recorded at all.
        let mut pace = Pace::default();
        for n in 0..1000u32 {
            let addr = Ipv4Addr::from(n.saturating_add(0x0A00_0000));
            let _ = pace.may_answer(IpAddr::V4(addr), start);
        }
        assert_eq!(pace.recent.len(), 1);
    }

    /// A question from outside the segment is not answered.
    ///
    /// The socket is bound to every address on this machine, so anybody on the
    /// internet can send a unicast query to port 5353. An answer is several
    /// times larger than the question, and a spoofed source address makes this
    /// machine into an amplifier pointed at somebody else.
    #[test]
    fn only_a_neighbour_is_answered() {
        for near in [
            "127.0.0.1",
            "192.168.1.5",
            "10.0.0.7",
            "172.16.4.4",
            "169.254.1.1",
            "100.64.0.3",
        ] {
            assert!(
                could_be_a_neighbour(ip(near)),
                "{near} is on a local network"
            );
        }
        for far in [
            "8.8.8.8",
            "1.1.1.1",
            "93.184.216.34",
            "172.32.0.1",
            "100.128.0.1",
        ] {
            assert!(
                !could_be_a_neighbour(ip(far)),
                "{far} is not on this segment and would be answered"
            );
        }
    }
}
