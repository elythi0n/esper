//! Looking for other Espers.
//!
//! # The address comes from the packet, not the record
//!
//! An answer carries an `A` record for the responder's own idea of its
//! address, which on a multi-homed machine may be an address this one cannot
//! reach. The source address of the packet is the one that demonstrably
//! works — it just arrived from there — so that is what a [`Found`] carries,
//! and a missing, wrong or malicious address record changes nothing.
//!
//! # Nothing here is trusted
//!
//! A fingerprint in a TXT record is a claim by whoever sent the packet.
//! Matching it against the paired devices decides whether to *offer* a
//! connection; the connection itself proves the key in the TLS handshake. So
//! the worst a liar achieves is being offered and then refused.

use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddr};
use std::time::{Duration, Instant};

use crate::name::Name;
use crate::service::{KEY_FINGERPRINT, KEY_PROTOCOL, SERVICE};
use crate::wire::{Data, Message, TYPE_PTR};
use crate::{DiscoveryError, socket};

/// One Esper somewhere on the segment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    /// The instance name, as advertised.
    pub instance: String,
    /// The device name, with the fingerprint suffix removed.
    pub device: String,
    /// The fingerprint it claims, if it gave a well-formed one.
    pub fingerprint: Option<String>,
    /// The protocol version it claims to speak.
    pub protocol: Option<u16>,
    /// Where it is: the address the packet came from, and the port it named.
    pub address: SocketAddr,
    /// When it was last heard from.
    pub seen: Instant,
}

impl Found {
    /// Whether this is the same instance as `other`, which is the identity a
    /// browser deduplicates on.
    #[must_use]
    pub fn is_same(&self, other: &Self) -> bool {
        self.instance == other.instance
    }
}

/// What has been heard, with no socket in it.
///
/// **Separate from [`Browser`] deliberately.** Everything interesting about a
/// browser — which packets it believes, which it drops, what it makes of a
/// fingerprint, when a device leaves the list — is a function of bytes and has
/// nothing to do with a network. Kept together with the socket, none of it
/// could be tested without one; kept apart, all of it is tested by handing it
/// packets.
#[derive(Debug)]
pub struct Sightings {
    service: Name,
    found: HashMap<String, Sighting>,
    /// Counted, because a malformed packet is a peer's doing and a browser
    /// that reported each one would be noisy on a segment with a broken
    /// implementation on it.
    dropped: u64,
}

/// What is known about one instance so far.
///
/// **Accumulated across packets rather than taken from one.** A responder is
/// free to answer with its pointer and its port in one message and its text in
/// another — text changes on its own and is re-announced by itself — so a
/// browser that read each message in isolation would show a device with no
/// fingerprint whenever that happened.
#[derive(Debug, Clone)]
struct Sighting {
    instance: String,
    device: String,
    fingerprint: Option<String>,
    protocol: Option<u16>,
    /// **The first address it was heard from, kept.** A multi-homed machine
    /// answers on every interface it has, so the same instance arrives from
    /// several of its addresses — all of them reachable, since a packet just
    /// came from there — and taking the newest each time would make a device
    /// list that flaps. Seen on the machine this was written on: the same
    /// responder at `192.168.1.112` and at `172.31.0.1`, a bridge.
    source: Ipv4Addr,
    /// `None` until a service record says where to connect, which is why an
    /// instance without one is not offered.
    port: Option<u16>,
    seen: Instant,
}

impl Sighting {
    /// A device to offer, if there is enough to offer.
    fn ready(&self) -> Option<Found> {
        Some(Found {
            instance: self.instance.clone(),
            device: self.device.clone(),
            fingerprint: self.fingerprint.clone(),
            protocol: self.protocol,
            address: SocketAddr::from((self.source, self.port?)),
            seen: self.seen,
        })
    }

    /// Take in what a later message said.
    ///
    /// Newer values win where there are any, and **absence never overwrites
    /// presence**: a message that mentions an instance without repeating its
    /// text must not erase the fingerprint an earlier one gave.
    fn merge(&mut self, heard: Heard) {
        if heard.fingerprint.is_some() {
            self.fingerprint = heard.fingerprint;
        }
        if heard.protocol.is_some() {
            self.protocol = heard.protocol;
        }
        if heard.port.is_some() {
            self.port = heard.port;
        }
        self.seen = Instant::now();
    }
}

impl Sightings {
    /// Somewhere to collect what is heard.
    pub fn new() -> Result<Self, DiscoveryError> {
        Ok(Self {
            service: Name::parse(SERVICE)?,
            found: HashMap::new(),
            dropped: 0,
        })
    }

    /// Everything heard so far, by instance name.
    #[must_use]
    pub fn all(&self) -> Vec<Found> {
        let mut all: Vec<Found> = self.found.values().filter_map(Sighting::ready).collect();
        all.sort_by(|a, b| a.instance.cmp(&b.instance));
        all
    }

    /// How many packets were dropped as unreadable.
    #[must_use]
    pub const fn dropped(&self) -> u64 {
        self.dropped
    }

    /// Take in one packet.
    ///
    /// Separated from receiving so the same code is exercised by a test with
    /// no network, and so a packet from a capture can be replayed into it.
    pub fn absorb(&mut self, packet: &[u8], from: SocketAddr) {
        let Ok(message) = Message::decode(packet) else {
            self.dropped = self.dropped.saturating_add(1);
            return;
        };
        if !message.response {
            return;
        }
        let source = match from {
            SocketAddr::V4(v4) => *v4.ip(),
            // A response over IPv6 about an IPv4 service is not something this
            // advertises, so there is nothing to do with it.
            SocketAddr::V6(_) => return,
        };

        // Instances this answer points at. A pointer with a zero TTL is a
        // responder leaving, and the instance goes rather than being refreshed.
        for record in &message.answers {
            let Data::Ptr(instance) = &record.data else {
                continue;
            };
            if record.name != self.service {
                continue;
            }
            let key = instance.to_string();
            if record.ttl == 0 {
                self.found.remove(&key);
                continue;
            }
            let heard = self.read_instance(&message, instance);
            match self.found.get_mut(&key) {
                Some(known) => known.merge(heard),
                None => {
                    self.found.insert(
                        key.clone(),
                        Sighting {
                            device: device_name(&key),
                            instance: key,
                            fingerprint: heard.fingerprint,
                            protocol: heard.protocol,
                            source,
                            port: heard.port,
                            seen: Instant::now(),
                        },
                    );
                }
            }
        }
    }

    /// Build a [`Found`] for `instance` from the rest of the message.
    ///
    /// `None` when the answer said where to look but not how to get there: an
    /// instance with no `SRV` record has no port, and a device without a port
    /// is not something a person can be offered.
    /// What this message says about `instance` — which may be nothing at all.
    fn read_instance(&self, message: &Message, instance: &Name) -> Heard {
        let mut port = None;
        let mut fingerprint = None;
        let mut protocol = None;
        for record in message.answers.iter().chain(message.additional.iter()) {
            if record.name != *instance {
                continue;
            }
            match &record.data {
                Data::Srv { port: p, .. } => port = Some(*p),
                Data::Txt(strings) => {
                    for s in strings {
                        if let Some((key, value)) = split_key(s) {
                            match key.as_str() {
                                KEY_FINGERPRINT => fingerprint = valid_fingerprint(&value),
                                KEY_PROTOCOL => protocol = value.parse().ok(),
                                _ => {}
                            }
                        }
                    }
                }
                Data::Ptr(_) | Data::A(_) => {}
            }
        }
        Heard {
            fingerprint,
            protocol,
            port,
        }
    }
}

/// What one message said about one instance.
#[derive(Debug, Default)]
struct Heard {
    fingerprint: Option<String>,
    protocol: Option<u16>,
    port: Option<u16>,
}

/// Sends queries and collects what answers.
#[derive(Debug)]
pub struct Browser {
    socket: socket::Multicast,
    query: Name,
    seen: Sightings,
}

impl Browser {
    /// Open a socket and ask once, on whichever interfaces were named.
    ///
    /// An empty list means the one the system would choose, which is what a
    /// machine with one route to the LAN wants.
    pub async fn start(on: &[std::net::Ipv4Addr]) -> Result<Self, DiscoveryError> {
        let socket = socket::open(on).map_err(|e| DiscoveryError::Socket(e.to_string()))?;
        let me = Self {
            socket,
            query: Name::parse(SERVICE)?,
            seen: Sightings::new()?,
        };
        me.ask().await?;
        Ok(me)
    }

    /// Ask the segment who is there.
    pub async fn ask(&self) -> Result<(), DiscoveryError> {
        let query = Message::query(self.query.clone(), TYPE_PTR);
        self.socket
            .send_to(&query.encode(), socket::destination())
            .await
            .map(|_| ())
            .map_err(|e| DiscoveryError::Socket(e.to_string()))
    }

    /// Everything heard so far.
    #[must_use]
    pub fn found(&self) -> Vec<Found> {
        self.seen.all()
    }

    /// How many packets were dropped as unreadable.
    #[must_use]
    pub const fn dropped(&self) -> u64 {
        self.seen.dropped()
    }

    /// Listen for up to `how_long`, collecting answers.
    ///
    /// Returns when the time is up rather than when something is found: a
    /// browser that stopped at the first answer would show one machine on a
    /// segment with four.
    pub async fn listen(&mut self, how_long: Duration) -> Result<(), DiscoveryError> {
        let deadline = Instant::now().checked_add(how_long);
        loop {
            let left = deadline.map_or(Duration::ZERO, |d| {
                d.saturating_duration_since(Instant::now())
            });
            if left.is_zero() {
                return Ok(());
            }
            let mut buffer = vec![0u8; crate::wire::MAX_PACKET];
            match tokio::time::timeout(left, self.socket.socket().recv_from(&mut buffer)).await {
                // Time is up, which is the ordinary way this ends.
                Err(_) => return Ok(()),
                Ok(Err(e)) => return Err(DiscoveryError::Socket(e.to_string())),
                Ok(Ok((len, from))) => {
                    let packet = buffer.get(..len).unwrap_or(&[]);
                    self.seen.absorb(packet, from);
                }
            }
        }
    }
}

/// `key=value`, as a TXT string spells it.
///
/// A string with no `=` is a key with no value, which the documents allow and
/// nothing here uses.
fn split_key(s: &[u8]) -> Option<(String, String)> {
    let text = String::from_utf8(s.to_vec()).ok()?;
    let (key, value) = text.split_once('=')?;
    Some((key.to_owned(), value.to_owned()))
}

/// A fingerprint only if it looks like one.
///
/// Sixteen hex digits, because that is what this protocol's fingerprints are.
/// Anything else is discarded rather than shown: a device list is where a
/// person picks by name and checks by fingerprint, and a made-up value there
/// is worse than none.
fn valid_fingerprint(value: &str) -> Option<String> {
    let ok = value.len() == 16 && value.chars().all(|c| c.is_ascii_hexdigit());
    ok.then(|| value.to_ascii_lowercase())
}

/// The device name out of an instance name: everything before the ` (`.
///
/// The fingerprint suffix exists to make the name unique, not to be read, and
/// a list showing it twice — once in the name, once in the column for it — is
/// harder to scan.
fn device_name(instance: &str) -> String {
    let first = instance.split('.').next().unwrap_or(instance);
    match first.rsplit_once(" (") {
        Some((name, _)) if !name.is_empty() => name.to_owned(),
        _ => first.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;
    use crate::service::Advertisement;
    use crate::wire::Record;

    fn advertisement(device: &str, fingerprint: &str, port: u16) -> Advertisement {
        Advertisement {
            device: device.to_owned(),
            fingerprint: fingerprint.to_owned(),
            port,
            protocol: 1,
            address: Some(Ipv4Addr::new(10, 0, 0, 9)),
            host: device.to_owned(),
        }
    }

    /// A response as a responder would send one.
    fn response(advertisement: &Advertisement) -> Vec<u8> {
        let (ptr, extra) = advertisement.records().expect("records");
        Message {
            id: 0,
            response: true,
            questions: Vec::new(),
            answers: vec![ptr],
            additional: extra,
        }
        .encode()
    }

    fn from(addr: &str) -> SocketAddr {
        addr.parse().expect("an address")
    }

    fn browser() -> Sightings {
        Sightings::new().expect("somewhere to collect")
    }

    #[test]
    fn an_announcement_becomes_a_device() {
        let mut b = browser();
        b.absorb(
            &response(&advertisement("elythion", "c6a8baa216f1078e", 7645)),
            from("192.168.1.5:5353"),
        );
        let found = b.all();
        let one = found.first().expect("a device");
        assert_eq!(one.device, "elythion");
        assert_eq!(one.fingerprint.as_deref(), Some("c6a8baa216f1078e"));
        assert_eq!(one.protocol, Some(1));
        // **The address is the packet's source, not the record's.** The
        // responder above claims 10.0.0.9; what arrived came from 192.168.1.5,
        // and that is the one that demonstrably works.
        assert_eq!(one.address, from("192.168.1.5:7645"));
    }

    #[test]
    fn the_same_device_heard_twice_is_one_entry() {
        let mut b = browser();
        let packet = response(&advertisement("elythion", "c6a8baa216f1078e", 7645));
        b.absorb(&packet, from("192.168.1.5:5353"));
        b.absorb(&packet, from("192.168.1.5:5353"));
        assert_eq!(b.all().len(), 1);
    }

    #[test]
    fn two_devices_are_two_entries() {
        let mut b = browser();
        b.absorb(
            &response(&advertisement("elythion", "c6a8baa216f1078e", 7645)),
            from("192.168.1.5:5353"),
        );
        b.absorb(
            &response(&advertisement("plum", "aabbccdd11223344", 7646)),
            from("192.168.1.6:5353"),
        );
        assert_eq!(b.all().len(), 2);
    }

    /// A responder leaving sends its records with nothing left to live, and a
    /// browser that ignored that would offer a machine that has gone.
    #[test]
    fn a_goodbye_takes_a_device_off_the_list() {
        let mut b = browser();
        let ad = advertisement("elythion", "c6a8baa216f1078e", 7645);
        b.absorb(&response(&ad), from("192.168.1.5:5353"));
        assert_eq!(b.all().len(), 1);

        let (mut ptr, extra) = ad.records().expect("records");
        ptr.ttl = 0;
        let goodbye = Message {
            id: 0,
            response: true,
            questions: Vec::new(),
            answers: vec![ptr],
            additional: extra,
        }
        .encode();
        b.absorb(&goodbye, from("192.168.1.5:5353"));
        assert!(b.all().is_empty(), "a goodbye must remove it");
    }

    /// **An instance with no service record has no port**, and a device
    /// without a port is not something a person can be offered — the browser
    /// would print an address that goes nowhere.
    #[test]
    fn an_instance_with_no_port_is_not_offered() {
        let mut b = browser();
        let ad = advertisement("elythion", "c6a8baa216f1078e", 7645);
        let (ptr, extra) = ad.records().expect("records");
        let without_srv: Vec<Record> = extra
            .into_iter()
            .filter(|r| !matches!(r.data, Data::Srv { .. }))
            .collect();
        let packet = Message {
            id: 0,
            response: true,
            questions: Vec::new(),
            answers: vec![ptr],
            additional: without_srv,
        }
        .encode();
        b.absorb(&packet, from("192.168.1.5:5353"));
        assert!(b.all().is_empty());
    }

    /// A question is not an answer. A browser that took records out of a
    /// *query* would believe whatever any machine asked about.
    #[test]
    fn a_query_is_not_absorbed() {
        let mut b = browser();
        let mut message = Message::decode(&response(&advertisement(
            "elythion",
            "c6a8baa216f1078e",
            7645,
        )))
        .expect("decode");
        message.response = false;
        b.absorb(&message.encode(), from("192.168.1.5:5353"));
        assert!(b.all().is_empty());
    }

    #[test]
    fn a_pointer_for_another_service_is_ignored() {
        let mut b = browser();
        let message = Message {
            id: 0,
            response: true,
            questions: Vec::new(),
            answers: vec![Record {
                name: Name::parse("_printer._tcp.local").expect("a name"),
                ttl: 4500,
                cache_flush: false,
                data: Data::Ptr(Name::parse("laser._printer._tcp.local").expect("a name")),
            }],
            additional: Vec::new(),
        };
        b.absorb(&message.encode(), from("192.168.1.7:5353"));
        assert!(b.all().is_empty(), "somebody else's service");
    }

    /// **A responder may split its answer across packets**, and it is
    /// entitled to: text changes on its own and gets re-announced by itself.
    /// A browser that read each message in isolation would show a device with
    /// no fingerprint the moment that happened.
    #[test]
    fn an_answer_split_across_packets_is_put_back_together() {
        let ad = advertisement("elythion", "c6a8baa216f1078e", 7645);
        let (ptr, extra) = ad.records().expect("records");
        let (text, rest): (Vec<Record>, Vec<Record>) = extra
            .into_iter()
            .partition(|r| matches!(r.data, Data::Txt(_)));

        let first = Message {
            id: 0,
            response: true,
            questions: Vec::new(),
            answers: vec![ptr.clone()],
            additional: rest,
        }
        .encode();
        let second = Message {
            id: 0,
            response: true,
            questions: Vec::new(),
            answers: vec![ptr],
            additional: text,
        }
        .encode();

        let mut b = browser();
        b.absorb(&first, from("192.168.1.5:5353"));
        assert_eq!(
            b.all().first().and_then(|f| f.fingerprint.clone()),
            None,
            "nothing has said what its fingerprint is yet"
        );
        b.absorb(&second, from("192.168.1.5:5353"));
        assert_eq!(
            b.all().first().and_then(|f| f.fingerprint.clone()),
            Some("c6a8baa216f1078e".to_owned()),
            "and now it has"
        );
        assert_eq!(b.all().len(), 1, "still one device");
    }

    /// And the other way round: a later message that says nothing about the
    /// text must not erase it.
    #[test]
    fn a_later_packet_without_the_text_does_not_erase_it() {
        let ad = advertisement("elythion", "c6a8baa216f1078e", 7645);
        let mut b = browser();
        b.absorb(&response(&ad), from("192.168.1.5:5353"));

        let (ptr, extra) = ad.records().expect("records");
        let without_text: Vec<Record> = extra
            .into_iter()
            .filter(|r| !matches!(r.data, Data::Txt(_)))
            .collect();
        let sparse = Message {
            id: 0,
            response: true,
            questions: Vec::new(),
            answers: vec![ptr],
            additional: without_text,
        }
        .encode();
        b.absorb(&sparse, from("192.168.1.5:5353"));
        assert_eq!(
            b.all().first().and_then(|f| f.fingerprint.clone()),
            Some("c6a8baa216f1078e".to_owned()),
            "absence must not overwrite presence"
        );
    }

    /// **A multi-homed responder answers from every address it has**, and the
    /// device list must not flap between them. Found against `avahi` on this
    /// machine, which answered from both a LAN address and a bridge.
    #[test]
    fn a_device_heard_from_two_addresses_keeps_the_first() {
        let ad = advertisement("elythion", "c6a8baa216f1078e", 7645);
        let mut b = browser();
        b.absorb(&response(&ad), from("192.168.1.5:5353"));
        b.absorb(&response(&ad), from("172.31.0.1:5353"));
        b.absorb(&response(&ad), from("10.9.9.9:5353"));
        assert_eq!(
            b.all().first().map(|f| f.address),
            Some(from("192.168.1.5:7645")),
            "one device, at the address it was first heard from"
        );
        assert_eq!(b.all().len(), 1);
    }

    #[test]
    fn an_unreadable_packet_is_counted_and_dropped() {
        let mut b = browser();
        b.absorb(&[0xFF; 20], from("192.168.1.5:5353"));
        b.absorb(&[], from("192.168.1.5:5353"));
        assert_eq!(b.dropped(), 2);
        assert!(b.all().is_empty());
    }

    /// **A fingerprint is a claim, and a malformed one is shown as none.**
    ///
    /// A device list is where a person picks by name and checks by
    /// fingerprint. Something that is not a fingerprint displayed as one is
    /// worse than nothing: it invites a comparison that cannot succeed.
    #[test]
    fn a_fingerprint_that_is_not_one_is_not_shown() {
        for claim in [
            "",
            "short",
            "c6a8baa216f1078",
            "c6a8baa216f1078ee",
            "not-hex-at-all!!",
            "zzzzzzzzzzzzzzzz",
        ] {
            let mut b = browser();
            b.absorb(
                &response(&advertisement("elythion", claim, 7645)),
                from("192.168.1.5:5353"),
            );
            let found = b.all();
            let one = found.first().expect("still a device");
            assert_eq!(one.fingerprint, None, "{claim:?} is not a fingerprint");
        }
    }

    #[test]
    fn a_fingerprint_is_compared_in_one_case() {
        let mut b = browser();
        b.absorb(
            &response(&advertisement("elythion", "C6A8BAA216F1078E", 7645)),
            from("192.168.1.5:5353"),
        );
        assert_eq!(
            b.all().first().and_then(|f| f.fingerprint.clone()),
            Some("c6a8baa216f1078e".to_owned()),
            "hex is hex whatever case it arrives in"
        );
    }

    #[test]
    fn a_device_name_is_the_instance_without_the_fingerprint() {
        assert_eq!(
            device_name("elythion (c6a8baa2)._esper._udp.local"),
            "elythion"
        );
        // A name with brackets of its own keeps everything up to the last one.
        assert_eq!(
            device_name("elythion (spare) (c6a8baa2)._esper._udp.local"),
            "elythion (spare)"
        );
        // And something that is not shaped like ours is shown as it is.
        assert_eq!(device_name("plain._esper._udp.local"), "plain");
    }
}
