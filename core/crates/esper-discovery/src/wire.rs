//! DNS messages, in the four record types this needs.
//!
//! # Reading is the hostile direction
//!
//! Everything decoded here arrived as multicast from whoever is on the
//! segment, so the reader's job is to refuse rather than to interpret
//! generously: a length that runs past the packet, a record whose data is
//! shorter than its type requires, a name with a pointer that does not point
//! backwards. All of those return an error and the packet is dropped.
//!
//! Records of types this does not need are **skipped by their length** rather
//! than refused. A responder on the segment answering some other service is
//! entitled to put whatever it likes in its packets, and a browser that fell
//! over on an unfamiliar record would be broken by every printer on the
//! network.

use std::net::Ipv4Addr;

use crate::name::{Name, NameError};

/// Record types, by their protocol numbers.
pub const TYPE_A: u16 = 1;
pub const TYPE_PTR: u16 = 12;
pub const TYPE_TXT: u16 = 16;
pub const TYPE_SRV: u16 = 33;
pub const TYPE_AAAA: u16 = 28;
/// "Any", which is what a querier asks for when it wants everything about a
/// name.
pub const TYPE_ANY: u16 = 255;

/// The internet class, and the bit multicast DNS overloads on top of it.
pub const CLASS_IN: u16 = 1;
/// In a question: answer me directly rather than to the group.
pub const UNICAST_RESPONSE: u16 = 0x8000;
/// In a record: replace anything cached for this name and type.
pub const CACHE_FLUSH: u16 = 0x8000;

/// The largest packet this will read.
///
/// Multicast DNS permits up to 9000 bytes over IPv4. Nothing here produces
/// anything like that — an announcement is a couple of hundred — and a bound
/// is one fewer thing a peer can decide.
///
/// **It bounds reading, not writing, and those are not symmetrical.** An
/// incoming packet may compress its names with pointers and this writer
/// deliberately does not, so a legal packet just inside the limit can expand
/// past it when written back out. See
/// [`a_compressed_packet_can_expand_past_the_read_limit`].
pub const MAX_PACKET: usize = 9000;

/// Why a message could not be read.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WireError {
    /// The bytes ran out.
    #[error("the packet is truncated in the {part}")]
    Truncated { part: &'static str },

    /// A name could not be read.
    #[error(transparent)]
    Name(#[from] NameError),

    /// A record's data is not the length its type requires.
    #[error("a {kind} record carries {len} bytes, which is not what it needs")]
    BadRecordData { kind: &'static str, len: usize },

    /// The packet is larger than this will read.
    #[error("a packet of {len} bytes is over the {MAX_PACKET}-byte limit")]
    TooLarge { len: usize },
}

/// What a record carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Data {
    /// A pointer to another name: the service type to an instance.
    Ptr(Name),
    /// Where an instance actually is.
    Srv {
        priority: u16,
        weight: u16,
        port: u16,
        target: Name,
    },
    /// Key-value strings about an instance.
    Txt(Vec<Vec<u8>>),
    /// An IPv4 address.
    A(Ipv4Addr),
}

impl Data {
    /// The type number this data is carried under.
    #[must_use]
    pub const fn kind(&self) -> u16 {
        match self {
            Self::Ptr(_) => TYPE_PTR,
            Self::Srv { .. } => TYPE_SRV,
            Self::Txt(_) => TYPE_TXT,
            Self::A(_) => TYPE_A,
        }
    }
}

/// One resource record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    pub name: Name,
    /// Seconds. Zero means "forget this", which is how a responder leaves.
    pub ttl: u32,
    /// Whether a receiver should drop what it had cached for this name.
    pub cache_flush: bool,
    pub data: Data,
}

/// One question.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Question {
    pub name: Name,
    pub kind: u16,
    /// Whether the querier asked to be answered directly.
    pub unicast: bool,
}

/// A whole message.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Message {
    pub id: u16,
    pub response: bool,
    pub questions: Vec<Question>,
    pub answers: Vec<Record>,
    /// Records offered without being asked for, which is where the address and
    /// text of an instance travel in an answer about its pointer.
    pub additional: Vec<Record>,
}

impl Message {
    /// A query for one name and type.
    #[must_use]
    pub fn query(name: Name, kind: u16) -> Self {
        Self {
            id: 0,
            response: false,
            questions: vec![Question {
                name,
                kind,
                unicast: false,
            }],
            answers: Vec::new(),
            additional: Vec::new(),
        }
    }

    /// Encode to bytes.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(512);
        out.extend_from_slice(&self.id.to_be_bytes());
        // Flags: a response sets QR and AA — a multicast responder is
        // authoritative for what it announces, by definition.
        let flags: u16 = if self.response { 0x8400 } else { 0x0000 };
        out.extend_from_slice(&flags.to_be_bytes());
        out.extend_from_slice(&count(self.questions.len()).to_be_bytes());
        out.extend_from_slice(&count(self.answers.len()).to_be_bytes());
        out.extend_from_slice(&0u16.to_be_bytes());
        out.extend_from_slice(&count(self.additional.len()).to_be_bytes());

        for q in &self.questions {
            q.name.write(&mut out);
            out.extend_from_slice(&q.kind.to_be_bytes());
            let class = if q.unicast {
                CLASS_IN | UNICAST_RESPONSE
            } else {
                CLASS_IN
            };
            out.extend_from_slice(&class.to_be_bytes());
        }
        for r in self.answers.iter().chain(self.additional.iter()) {
            write_record(r, &mut out);
        }
        out
    }

    /// Decode from bytes.
    pub fn decode(packet: &[u8]) -> Result<Self, WireError> {
        if packet.len() > MAX_PACKET {
            return Err(WireError::TooLarge { len: packet.len() });
        }
        let header = packet
            .get(..12)
            .ok_or(WireError::Truncated { part: "header" })?;
        let id = be16(header, 0)?;
        let flags = be16(header, 2)?;
        let questions = usize::from(be16(header, 4)?);
        let answers = usize::from(be16(header, 6)?);
        let authority = usize::from(be16(header, 8)?);
        let additional = usize::from(be16(header, 10)?);

        let mut at = 12usize;
        let mut out = Self {
            id,
            response: flags & 0x8000 != 0,
            questions: Vec::new(),
            answers: Vec::new(),
            additional: Vec::new(),
        };

        for _ in 0..questions {
            let (name, next) = Name::read(packet, at)?;
            at = next;
            let kind = be16_at(packet, at)?;
            let class = be16_at(packet, at.saturating_add(2))?;
            at = at.saturating_add(4);
            out.questions.push(Question {
                name,
                kind,
                unicast: class & UNICAST_RESPONSE != 0,
            });
        }

        // Authority records are read and dropped: nothing here needs them, and
        // they have to be walked past to reach the additional section.
        for section in 0..3 {
            let n = match section {
                0 => answers,
                1 => authority,
                _ => additional,
            };
            for _ in 0..n {
                let (record, next) = read_record(packet, at)?;
                at = next;
                match (section, record) {
                    (0, Some(r)) => out.answers.push(r),
                    (2, Some(r)) => out.additional.push(r),
                    _ => {}
                }
            }
        }
        Ok(out)
    }
}

/// A section count that cannot overflow the field.
///
/// A message with more than 65535 records in a section is not something this
/// builds, and truncating the count silently would produce a packet that
/// describes itself wrongly — so it saturates, and the encoder's own records
/// are what they are.
fn count(n: usize) -> u16 {
    u16::try_from(n).unwrap_or(u16::MAX)
}

fn write_record(r: &Record, out: &mut Vec<u8>) {
    r.name.write(out);
    out.extend_from_slice(&r.data.kind().to_be_bytes());
    let class = if r.cache_flush {
        CLASS_IN | CACHE_FLUSH
    } else {
        CLASS_IN
    };
    out.extend_from_slice(&class.to_be_bytes());
    out.extend_from_slice(&r.ttl.to_be_bytes());

    // The length goes before the data and is only known after it, so the two
    // bytes are reserved and filled in — rather than encoding into a second
    // buffer and copying.
    let length_at = out.len();
    out.extend_from_slice(&0u16.to_be_bytes());
    match &r.data {
        Data::Ptr(name) => name.write(out),
        Data::Srv {
            priority,
            weight,
            port,
            target,
        } => {
            out.extend_from_slice(&priority.to_be_bytes());
            out.extend_from_slice(&weight.to_be_bytes());
            out.extend_from_slice(&port.to_be_bytes());
            target.write(out);
        }
        Data::Txt(strings) => {
            for s in strings {
                // A string longer than a byte can describe is written as much
                // of itself as fits. Nothing here produces one.
                let len = u8::try_from(s.len()).unwrap_or(u8::MAX);
                out.push(len);
                out.extend_from_slice(s.get(..usize::from(len)).unwrap_or(&[]));
            }
            // A TXT record with no strings is not legal; one empty string is
            // how "no keys" is spelled.
            if strings.is_empty() {
                out.push(0);
            }
        }
        Data::A(addr) => out.extend_from_slice(&addr.octets()),
    }
    let len = count(out.len().saturating_sub(length_at).saturating_sub(2));
    if let Some(slot) = out.get_mut(length_at..length_at.saturating_add(2)) {
        slot.copy_from_slice(&len.to_be_bytes());
    }
}

/// Read one record, or `None` for a type this does not need.
fn read_record(packet: &[u8], at: usize) -> Result<(Option<Record>, usize), WireError> {
    let (name, after_name) = Name::read(packet, at)?;
    let kind = be16_at(packet, after_name)?;
    let class = be16_at(packet, after_name.saturating_add(2))?;
    let ttl = be32_at(packet, after_name.saturating_add(4))?;
    let len = usize::from(be16_at(packet, after_name.saturating_add(8))?);
    let data_at = after_name.saturating_add(10);
    let end = data_at.saturating_add(len);
    let data = packet.get(data_at..end).ok_or(WireError::Truncated {
        part: "record data",
    })?;

    let parsed = match kind {
        TYPE_PTR => {
            let (target, _) = Name::read(packet, data_at)?;
            Some(Data::Ptr(target))
        }
        TYPE_SRV => {
            if len < 7 {
                return Err(WireError::BadRecordData { kind: "SRV", len });
            }
            let (target, _) = Name::read(packet, data_at.saturating_add(6))?;
            Some(Data::Srv {
                priority: be16(data, 0)?,
                weight: be16(data, 2)?,
                port: be16(data, 4)?,
                target,
            })
        }
        TYPE_TXT => Some(Data::Txt(read_strings(data))),
        TYPE_A => {
            let octets = data
                .get(..4)
                .ok_or(WireError::BadRecordData { kind: "A", len })?;
            let mut b = [0u8; 4];
            b.copy_from_slice(octets);
            Some(Data::A(Ipv4Addr::from(b)))
        }
        // Everything else on the segment: skipped by its length, not refused.
        _ => None,
    };

    Ok((
        parsed.map(|data| Record {
            name,
            ttl,
            cache_flush: class & CACHE_FLUSH != 0,
            data,
        }),
        end,
    ))
}

/// The length-prefixed strings of a TXT record.
///
/// A prefix longer than what remains takes the rest and stops, rather than
/// failing: a TXT record is a bag of hints, and one malformed string is no
/// reason to discard the address that came with it.
fn read_strings(mut data: &[u8]) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    while let Some((&len, rest)) = data.split_first() {
        let take = usize::from(len).min(rest.len());
        if let Some(s) = rest.get(..take)
            && !s.is_empty()
        {
            out.push(s.to_vec());
        }
        data = rest.get(take..).unwrap_or(&[]);
    }
    out
}

fn be16(bytes: &[u8], at: usize) -> Result<u16, WireError> {
    let pair = bytes
        .get(at..at.saturating_add(2))
        .ok_or(WireError::Truncated { part: "a field" })?;
    match pair {
        [hi, lo] => Ok(u16::from(*hi) << 8 | u16::from(*lo)),
        _ => Err(WireError::Truncated { part: "a field" }),
    }
}

fn be16_at(packet: &[u8], at: usize) -> Result<u16, WireError> {
    be16(packet, at)
}

fn be32_at(packet: &[u8], at: usize) -> Result<u32, WireError> {
    let quad = packet
        .get(at..at.saturating_add(4))
        .ok_or(WireError::Truncated { part: "a ttl" })?;
    match quad {
        [a, b, c, d] => {
            Ok(u32::from(*a) << 24 | u32::from(*b) << 16 | u32::from(*c) << 8 | u32::from(*d))
        }
        _ => Err(WireError::Truncated { part: "a ttl" }),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    fn name(text: &str) -> Name {
        Name::parse(text).expect("a name")
    }

    fn announcement() -> Message {
        Message {
            id: 0,
            response: true,
            questions: Vec::new(),
            answers: vec![Record {
                name: name("_esper._udp.local"),
                ttl: 4500,
                cache_flush: false,
                data: Data::Ptr(name("elythion._esper._udp.local")),
            }],
            additional: vec![
                Record {
                    name: name("elythion._esper._udp.local"),
                    ttl: 120,
                    cache_flush: true,
                    data: Data::Srv {
                        priority: 0,
                        weight: 0,
                        port: 7645,
                        target: name("elythion.local"),
                    },
                },
                Record {
                    name: name("elythion._esper._udp.local"),
                    ttl: 120,
                    cache_flush: true,
                    data: Data::Txt(vec![b"fp=c6a8baa216f1078e".to_vec(), b"pv=1".to_vec()]),
                },
                Record {
                    name: name("elythion.local"),
                    ttl: 120,
                    cache_flush: true,
                    data: Data::A(Ipv4Addr::new(192, 168, 1, 20)),
                },
            ],
        }
    }

    #[test]
    fn a_message_survives_the_round_trip() {
        let original = announcement();
        let decoded = Message::decode(&original.encode()).expect("decode");
        assert_eq!(decoded, original);
    }

    #[test]
    fn a_query_survives_the_round_trip() {
        let query = Message::query(name("_esper._udp.local"), TYPE_PTR);
        let decoded = Message::decode(&query.encode()).expect("decode");
        assert_eq!(decoded, query);
        assert!(!decoded.response);
    }

    #[test]
    fn a_unicast_question_is_carried_in_the_class() {
        let mut query = Message::query(name("_esper._udp.local"), TYPE_PTR);
        if let Some(q) = query.questions.first_mut() {
            q.unicast = true;
        }
        let decoded = Message::decode(&query.encode()).expect("decode");
        assert!(
            decoded.questions.first().is_some_and(|q| q.unicast),
            "the bit must survive the round trip"
        );
    }

    /// **A record type this does not know is walked past, not refused.**
    ///
    /// The segment is full of other services. A browser that fell over on an
    /// unfamiliar record would be broken by every printer on the network — and
    /// worse, it would drop the packet's *other* records, which may be the
    /// ones it was waiting for.
    #[test]
    fn an_unknown_record_type_is_skipped_and_the_rest_still_read() {
        let mut packet = Vec::new();
        packet.extend_from_slice(&0u16.to_be_bytes());
        packet.extend_from_slice(&0x8400u16.to_be_bytes());
        packet.extend_from_slice(&0u16.to_be_bytes());
        packet.extend_from_slice(&2u16.to_be_bytes());
        packet.extend_from_slice(&0u16.to_be_bytes());
        packet.extend_from_slice(&0u16.to_be_bytes());

        // Something nobody here has heard of, with six bytes of payload.
        name("weird.local").write(&mut packet);
        packet.extend_from_slice(&99u16.to_be_bytes());
        packet.extend_from_slice(&CLASS_IN.to_be_bytes());
        packet.extend_from_slice(&60u32.to_be_bytes());
        packet.extend_from_slice(&6u16.to_be_bytes());
        packet.extend_from_slice(&[1, 2, 3, 4, 5, 6]);

        // Then something it does know, which must still arrive.
        name("_esper._udp.local").write(&mut packet);
        packet.extend_from_slice(&TYPE_PTR.to_be_bytes());
        packet.extend_from_slice(&CLASS_IN.to_be_bytes());
        packet.extend_from_slice(&4500u32.to_be_bytes());
        let mut target = Vec::new();
        name("elythion._esper._udp.local").write(&mut target);
        packet.extend_from_slice(&count(target.len()).to_be_bytes());
        packet.extend_from_slice(&target);

        let decoded = Message::decode(&packet).expect("decode");
        assert_eq!(
            decoded.answers.len(),
            1,
            "the unknown record is dropped and the known one kept"
        );
        assert!(matches!(
            decoded.answers.first().map(|r| &r.data),
            Some(Data::Ptr(_))
        ));
    }

    /// Authority records sit between the answers and the additional records,
    /// so they have to be walked past or everything after them is misread.
    #[test]
    fn records_after_an_authority_section_are_still_found() {
        let mut packet = Vec::new();
        packet.extend_from_slice(&0u16.to_be_bytes());
        packet.extend_from_slice(&0x8400u16.to_be_bytes());
        packet.extend_from_slice(&0u16.to_be_bytes());
        packet.extend_from_slice(&0u16.to_be_bytes());
        // One authority record, then one additional.
        packet.extend_from_slice(&1u16.to_be_bytes());
        packet.extend_from_slice(&1u16.to_be_bytes());

        name("elythion.local").write(&mut packet);
        packet.extend_from_slice(&TYPE_A.to_be_bytes());
        packet.extend_from_slice(&CLASS_IN.to_be_bytes());
        packet.extend_from_slice(&120u32.to_be_bytes());
        packet.extend_from_slice(&4u16.to_be_bytes());
        packet.extend_from_slice(&[10, 0, 0, 1]);

        name("other.local").write(&mut packet);
        packet.extend_from_slice(&TYPE_A.to_be_bytes());
        packet.extend_from_slice(&CLASS_IN.to_be_bytes());
        packet.extend_from_slice(&120u32.to_be_bytes());
        packet.extend_from_slice(&4u16.to_be_bytes());
        packet.extend_from_slice(&[10, 0, 0, 2]);

        let decoded = Message::decode(&packet).expect("decode");
        assert!(decoded.answers.is_empty());
        assert_eq!(
            decoded.additional.first().map(|r| r.data.clone()),
            Some(Data::A(Ipv4Addr::new(10, 0, 0, 2))),
            "the authority record must be skipped, not counted as the additional one"
        );
    }

    #[test]
    fn a_truncated_packet_is_refused_rather_than_guessed_at() {
        let full = announcement().encode();
        for cut in 1..full.len() {
            let part = full.get(..cut).expect("a prefix");
            // Whatever it does, it must not panic and must not claim records
            // it could not read.
            if let Ok(message) = Message::decode(part) {
                let claimed = message.answers.len() + message.additional.len();
                assert!(
                    claimed <= 4,
                    "a {cut}-byte prefix produced {claimed} records"
                );
            }
        }
    }

    #[test]
    fn a_header_shorter_than_a_header_is_refused() {
        for len in 0..12 {
            assert!(matches!(
                Message::decode(&vec![0u8; len]),
                Err(WireError::Truncated { .. })
            ));
        }
    }

    /// A record whose data length runs past the packet is refused, not read to
    /// the end of what happens to be there.
    #[test]
    fn a_record_claiming_more_data_than_there_is_is_refused() {
        let mut packet = Vec::new();
        packet.extend_from_slice(&0u16.to_be_bytes());
        packet.extend_from_slice(&0x8400u16.to_be_bytes());
        packet.extend_from_slice(&0u16.to_be_bytes());
        packet.extend_from_slice(&1u16.to_be_bytes());
        packet.extend_from_slice(&0u16.to_be_bytes());
        packet.extend_from_slice(&0u16.to_be_bytes());
        name("elythion.local").write(&mut packet);
        packet.extend_from_slice(&TYPE_A.to_be_bytes());
        packet.extend_from_slice(&CLASS_IN.to_be_bytes());
        packet.extend_from_slice(&120u32.to_be_bytes());
        packet.extend_from_slice(&400u16.to_be_bytes());
        packet.extend_from_slice(&[10, 0, 0, 1]);

        assert!(matches!(
            Message::decode(&packet),
            Err(WireError::Truncated { .. })
        ));
    }

    #[test]
    fn a_packet_larger_than_the_limit_is_refused() {
        assert!(matches!(
            Message::decode(&vec![0u8; MAX_PACKET + 1]),
            Err(WireError::TooLarge { .. })
        ));
    }

    /// An `SRV` record needs seven bytes before its target can begin, and a
    /// shorter one is a lie about its own contents.
    #[test]
    fn a_short_service_record_is_refused() {
        let mut packet = Vec::new();
        packet.extend_from_slice(&0u16.to_be_bytes());
        packet.extend_from_slice(&0x8400u16.to_be_bytes());
        packet.extend_from_slice(&0u16.to_be_bytes());
        packet.extend_from_slice(&1u16.to_be_bytes());
        packet.extend_from_slice(&0u16.to_be_bytes());
        packet.extend_from_slice(&0u16.to_be_bytes());
        name("elythion._esper._udp.local").write(&mut packet);
        packet.extend_from_slice(&TYPE_SRV.to_be_bytes());
        packet.extend_from_slice(&CLASS_IN.to_be_bytes());
        packet.extend_from_slice(&120u32.to_be_bytes());
        packet.extend_from_slice(&6u16.to_be_bytes());
        packet.extend_from_slice(&[0, 0, 0, 0, 0x1d, 0xdd]);

        assert!(matches!(
            Message::decode(&packet),
            Err(WireError::BadRecordData { kind: "SRV", .. })
        ));
    }

    /// A text record's strings are length-prefixed, and a prefix longer than
    /// what remains takes the rest rather than failing: a TXT record is a bag
    /// of hints, and one malformed string is no reason to throw away the
    /// address that came with it.
    #[test]
    fn a_text_record_with_an_overlong_string_keeps_what_it_can() {
        let strings = read_strings(&[3, b'a', b'b', b'c', 200, b'x', b'y']);
        assert_eq!(strings, vec![b"abc".to_vec(), b"xy".to_vec()]);
    }

    /// **Reading and writing are not symmetrical, and this is the case that
    /// proves it.**
    ///
    /// Found by the fuzzer in two minutes: a packet inside the read limit
    /// whose names are compressed expands past that limit when written back
    /// out uncompressed. Nothing in the product re-emits a message it decoded
    /// — a responder builds its own records — so this is a stated property
    /// rather than a bug, and it is stated here so that anyone who later adds
    /// a forwarding path finds it before their peers do.
    #[test]
    fn a_compressed_packet_can_expand_past_the_read_limit() {
        // A long name once, then many records whose names are a two-byte
        // pointer to it.
        let long = (0..4).map(|_| "a".repeat(60)).collect::<Vec<_>>().join(".");
        let target = name(&long);

        let mut packet = Vec::new();
        packet.extend_from_slice(&0u16.to_be_bytes());
        packet.extend_from_slice(&0x8400u16.to_be_bytes());
        packet.extend_from_slice(&0u16.to_be_bytes());
        let records: u16 = 500;
        packet.extend_from_slice(&records.to_be_bytes());
        packet.extend_from_slice(&0u16.to_be_bytes());
        packet.extend_from_slice(&0u16.to_be_bytes());

        // The name in full, as the first record's owner.
        let name_at = u16::try_from(packet.len()).expect("early in the packet");
        target.write(&mut packet);
        packet.extend_from_slice(&TYPE_A.to_be_bytes());
        packet.extend_from_slice(&CLASS_IN.to_be_bytes());
        packet.extend_from_slice(&120u32.to_be_bytes());
        packet.extend_from_slice(&4u16.to_be_bytes());
        packet.extend_from_slice(&[10, 0, 0, 1]);

        // And then the same name as a pointer, over and over.
        let pointer = 0xC000u16 | name_at;
        for _ in 1..records {
            packet.extend_from_slice(&pointer.to_be_bytes());
            packet.extend_from_slice(&TYPE_A.to_be_bytes());
            packet.extend_from_slice(&CLASS_IN.to_be_bytes());
            packet.extend_from_slice(&120u32.to_be_bytes());
            packet.extend_from_slice(&4u16.to_be_bytes());
            packet.extend_from_slice(&[10, 0, 0, 1]);
        }

        assert!(
            packet.len() <= MAX_PACKET,
            "the point of this test is that the *input* is legal: {} bytes",
            packet.len()
        );
        let decoded = Message::decode(&packet).expect("a legal packet");
        assert_eq!(decoded.answers.len(), usize::from(records));
        assert!(
            decoded.encode().len() > MAX_PACKET,
            "uncompressed, this must be larger than the limit — otherwise the \
             asymmetry this documents does not exist and the note should go"
        );
    }

    #[test]
    fn an_empty_text_record_is_still_a_legal_record() {
        let record = Record {
            name: name("elythion._esper._udp.local"),
            ttl: 120,
            cache_flush: true,
            data: Data::Txt(Vec::new()),
        };
        let message = Message {
            id: 0,
            response: true,
            questions: Vec::new(),
            answers: vec![record],
            additional: Vec::new(),
        };
        let decoded = Message::decode(&message.encode()).expect("decode");
        assert_eq!(
            decoded.answers.first().map(|r| r.data.clone()),
            Some(Data::Txt(Vec::new()))
        );
    }
}
