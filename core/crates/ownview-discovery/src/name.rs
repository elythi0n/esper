//! DNS names: labels on the wire, and the two ways reading them goes wrong.
//!
//! # Compression pointers are the dangerous part
//!
//! A name in a DNS message may end in a pointer to a name earlier in the same
//! message, and the pointed-at name may itself end in a pointer. Following
//! them naively on a packet from the network is an infinite loop — a packet
//! whose pointer points at itself hangs the reader for ever, and every byte
//! here comes from whoever is on the segment.
//!
//! Two bounds make that impossible rather than unlikely:
//!
//! - a pointer must point **strictly backwards** — before the pointer itself,
//!   which is what the protocol means by naming a prior occurrence, and before
//!   every earlier jump, which alone rules out every loop because each jump
//!   then lands nearer the start;
//! - and the total name is bounded at 255 bytes, which is the protocol's own
//!   limit, so a chain of legal backwards pointers still cannot produce an
//!   unbounded name.

use core::fmt;

/// The longest name the protocol allows, in wire bytes.
pub const MAX_NAME: usize = 255;

/// The longest single label.
pub const MAX_LABEL: usize = 63;

/// A pointer's two high bits, and the mask for the rest.
const POINTER_TAG: u8 = 0b1100_0000;
const POINTER_MASK: u16 = 0x3FFF;

/// Why a name could not be used.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NameError {
    /// A label was longer than the protocol allows.
    #[error("a label of {len} bytes is longer than the {MAX_LABEL} allowed")]
    LabelTooLong { len: usize },

    /// The whole name was longer than the protocol allows.
    #[error("a name of {len} bytes is longer than the {MAX_NAME} allowed")]
    TooLong { len: usize },

    /// A label was empty in the middle of a name.
    #[error("a name cannot contain an empty label")]
    EmptyLabel,

    /// The bytes ran out before the name ended.
    #[error("the name is truncated")]
    Truncated,

    /// A compression pointer that does not point backwards.
    ///
    /// Its own case because it is the one that matters: a forward or
    /// self-referential pointer is how a reader is made to loop for ever, and
    /// a forward one is malformed besides.
    #[error("a compression pointer at {at} points to {to}, which is not backwards")]
    PointerNotBackwards { at: usize, to: usize },
}

/// A DNS name, as labels.
///
/// Comparison is case-insensitive, because the protocol says names are.
#[derive(Debug, Clone)]
pub struct Name {
    labels: Vec<Vec<u8>>,
}

impl Name {
    /// Build a name from a dotted string.
    ///
    /// A trailing dot is accepted and ignored: `local` and `local.` are the
    /// same name, and both spellings appear in the documents this implements.
    pub fn parse(text: &str) -> Result<Self, NameError> {
        let trimmed = text.strip_suffix('.').unwrap_or(text);
        if trimmed.is_empty() {
            return Ok(Self { labels: Vec::new() });
        }
        let mut labels = Vec::new();
        for part in trimmed.split('.') {
            if part.is_empty() {
                return Err(NameError::EmptyLabel);
            }
            if part.len() > MAX_LABEL {
                return Err(NameError::LabelTooLong { len: part.len() });
            }
            labels.push(part.as_bytes().to_vec());
        }
        let me = Self { labels };
        let len = me.wire_len();
        if len > MAX_NAME {
            return Err(NameError::TooLong { len });
        }
        Ok(me)
    }

    /// Build a name from labels that are already bytes.
    pub fn from_labels(labels: Vec<Vec<u8>>) -> Result<Self, NameError> {
        for label in &labels {
            if label.is_empty() {
                return Err(NameError::EmptyLabel);
            }
            if label.len() > MAX_LABEL {
                return Err(NameError::LabelTooLong { len: label.len() });
            }
        }
        let me = Self { labels };
        let len = me.wire_len();
        if len > MAX_NAME {
            return Err(NameError::TooLong { len });
        }
        Ok(me)
    }

    /// The labels, in order.
    #[must_use]
    pub fn labels(&self) -> &[Vec<u8>] {
        &self.labels
    }

    /// How many bytes this takes on the wire, uncompressed.
    #[must_use]
    pub fn wire_len(&self) -> usize {
        self.labels
            .iter()
            .map(|l| l.len().saturating_add(1))
            .sum::<usize>()
            .saturating_add(1)
    }

    /// Whether this name ends with `suffix`, label by label.
    ///
    /// Used to tell an instance of our service from anything else on the
    /// segment, which is a suffix question rather than a string one.
    #[must_use]
    pub fn ends_with(&self, suffix: &Self) -> bool {
        if suffix.labels.len() > self.labels.len() {
            return false;
        }
        let skip = self.labels.len().saturating_sub(suffix.labels.len());
        self.labels
            .iter()
            .skip(skip)
            .zip(suffix.labels.iter())
            .all(|(a, b)| equal_ignoring_case(a, b))
    }

    /// Append labels, keeping the length rules.
    pub fn join(&self, other: &Self) -> Result<Self, NameError> {
        let mut labels = self.labels.clone();
        labels.extend(other.labels.iter().cloned());
        Self::from_labels(labels)
    }

    /// Write this name, uncompressed.
    ///
    /// Compression is not used when writing. It would save perhaps a hundred
    /// bytes in a packet with a thousand to spare, and it is the part of this
    /// protocol most able to produce a message another implementation reads
    /// differently from how it was meant.
    pub fn write(&self, out: &mut Vec<u8>) {
        for label in &self.labels {
            // The length rules were checked when the name was built, so this
            // cast cannot lose anything.
            let len = u8::try_from(label.len()).unwrap_or(0);
            out.push(len);
            out.extend_from_slice(label);
        }
        out.push(0);
    }

    /// Read a name starting at `at` in `packet`, returning it and the offset
    /// just past it — **in the original stream**, which is what a caller has to
    /// continue from when the name ended in a pointer.
    pub fn read(packet: &[u8], at: usize) -> Result<(Self, usize), NameError> {
        let mut labels: Vec<Vec<u8>> = Vec::new();
        let mut cursor = at;
        let mut total = 1usize;
        // Where the caller carries on from: the first pointer ends the name in
        // the original stream even though reading continues elsewhere.
        let mut after: Option<usize> = None;
        // Every jump must be strictly backwards from the last one, which is
        // what makes a loop impossible.
        let mut furthest = packet.len();

        loop {
            let Some(&len) = packet.get(cursor) else {
                return Err(NameError::Truncated);
            };
            if len & POINTER_TAG == POINTER_TAG {
                let Some(&low) = packet.get(cursor.saturating_add(1)) else {
                    return Err(NameError::Truncated);
                };
                let to = usize::from((u16::from(len) << 8 | u16::from(low)) & POINTER_MASK);
                // Two conditions, and both matter. Before the pointer itself,
                // because the protocol says a pointer names a *prior*
                // occurrence — a forward one is malformed even though it
                // cannot loop. And before every earlier jump, which is what
                // makes a loop impossible rather than merely unlikely.
                if to >= cursor || to >= furthest {
                    return Err(NameError::PointerNotBackwards { at: cursor, to });
                }
                furthest = to;
                if after.is_none() {
                    after = Some(cursor.saturating_add(2));
                }
                cursor = to;
                continue;
            }
            if len as usize > MAX_LABEL {
                return Err(NameError::LabelTooLong { len: len as usize });
            }
            if len == 0 {
                let end = after.unwrap_or(cursor.saturating_add(1));
                return Ok((Self { labels }, end));
            }
            let start = cursor.saturating_add(1);
            let stop = start.saturating_add(len as usize);
            let Some(label) = packet.get(start..stop) else {
                return Err(NameError::Truncated);
            };
            total = total.saturating_add(label.len().saturating_add(1));
            if total > MAX_NAME {
                return Err(NameError::TooLong { len: total });
            }
            labels.push(label.to_vec());
            cursor = stop;
        }
    }
}

impl PartialEq for Name {
    fn eq(&self, other: &Self) -> bool {
        self.labels.len() == other.labels.len()
            && self
                .labels
                .iter()
                .zip(other.labels.iter())
                .all(|(a, b)| equal_ignoring_case(a, b))
    }
}

impl Eq for Name {}

impl fmt::Display for Name {
    /// Dotted, for a person to read.
    ///
    /// **Not the inverse of [`Name::parse`], and not meant to be.** A label
    /// may contain a dot or a backslash, which are escaped here so the name
    /// still reads as the right number of labels — but a space is left as a
    /// space, because an instance name is shown to somebody choosing a machine
    /// from a list and `elythion\032(c6a8baa2)` is not a thing to show them. This
    /// is presentation; nothing parses it back.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut first = true;
        for label in &self.labels {
            if !first {
                f.write_str(".")?;
            }
            first = false;
            for &b in label {
                match b {
                    b'.' | b'\\' => write!(f, "\\{}", b as char)?,
                    0x20..=0x7E => write!(f, "{}", b as char)?,
                    other => write!(f, "\\{other:03}")?,
                }
            }
        }
        Ok(())
    }
}

/// ASCII case-insensitive comparison, which is what DNS specifies — and
/// deliberately not Unicode case folding, which would make two different names
/// compare equal.
fn equal_ignoring_case(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len()
        && a.iter()
            .zip(b.iter())
            .all(|(x, y)| x.eq_ignore_ascii_case(y))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    fn wire(name: &str) -> Vec<u8> {
        let mut out = Vec::new();
        Name::parse(name).expect("a name").write(&mut out);
        out
    }

    #[test]
    fn a_name_survives_the_round_trip() {
        for text in [
            "local",
            "_ownview._udp.local",
            "elythion (c6a8baa2)._ownview._udp.local",
            "a.b.c.d.e.f",
        ] {
            let bytes = wire(text);
            let (read, end) = Name::read(&bytes, 0).expect("read");
            assert_eq!(read.to_string(), text);
            assert_eq!(end, bytes.len(), "the whole name was consumed");
        }
    }

    #[test]
    fn the_root_is_one_zero_byte() {
        let mut out = Vec::new();
        Name::parse("").expect("the root").write(&mut out);
        assert_eq!(out, vec![0]);
        let (read, end) = Name::read(&out, 0).expect("read");
        assert!(read.labels().is_empty());
        assert_eq!(end, 1);
    }

    #[test]
    fn names_compare_without_regard_to_case() {
        assert_eq!(
            Name::parse("_OwnView._UDP.Local").expect("a name"),
            Name::parse("_ownview._udp.local").expect("a name")
        );
        assert!(
            Name::parse("KIWI._ownview._udp.LOCAL")
                .expect("a name")
                .ends_with(&Name::parse("_ownview._udp.local").expect("a name"))
        );
    }

    /// **The loop a packet from the network would otherwise cause.**
    ///
    /// A pointer to itself, or to anything at or after where it sits, is how a
    /// reader is made to follow pointers for ever. Refusing anything that is
    /// not strictly backwards rules out every loop rather than catching the
    /// obvious ones.
    ///
    /// **Removing the bound does not make this test fail — it makes it
    /// hang**, which is what the bound is for. The 255-byte limit is no help
    /// here either: a pointer that points at a pointer accumulates no labels,
    /// so there is nothing for a length limit to notice.
    #[test]
    fn a_pointer_that_does_not_point_backwards_is_refused() {
        // Points at itself.
        let self_pointer = vec![0xC0, 0x00];
        assert!(matches!(
            Name::read(&self_pointer, 0),
            Err(NameError::PointerNotBackwards { .. })
        ));

        // Two pointers pointing at each other, which a bound on the number of
        // jumps would allow for a while and this refuses at once.
        let pair = vec![0xC0, 0x02, 0xC0, 0x00];
        assert!(matches!(
            Name::read(&pair, 0),
            Err(NameError::PointerNotBackwards { .. })
        ));
        assert!(matches!(
            Name::read(&pair, 2),
            Err(NameError::PointerNotBackwards { .. })
        ));

        // Forwards, into a legal name that sits later in the packet.
        let mut forwards = vec![0xC0, 0x04, 0x00, 0x00];
        forwards.extend(wire("local"));
        assert!(matches!(
            Name::read(&forwards, 0),
            Err(NameError::PointerNotBackwards { .. })
        ));
    }

    /// A pointer chain that goes backwards every time is legal, and has to
    /// work: it is what every other implementation writes.
    #[test]
    fn a_backwards_chain_of_pointers_is_followed() {
        // "local" at 0, then "_udp" -> pointer to 0, then "_ownview" ->
        // pointer to that.
        let mut packet = wire("local");
        let udp_at = packet.len();
        packet.push(4);
        packet.extend_from_slice(b"_udp");
        packet.extend_from_slice(&[0xC0, 0x00]);
        let own_at = packet.len();
        packet.push(8);
        packet.extend_from_slice(b"_ownview");
        packet.extend_from_slice(&[0xC0, u8::try_from(udp_at).expect("a small offset")]);

        let (name, end) = Name::read(&packet, own_at).expect("read");
        assert_eq!(name.to_string(), "_ownview._udp.local");
        assert_eq!(end, packet.len(), "the caller continues past the pointer");
    }

    #[test]
    fn a_truncated_name_is_refused() {
        // A length byte with nothing behind it.
        assert!(matches!(Name::read(&[5], 0), Err(NameError::Truncated)));
        // A label that claims more bytes than are there.
        assert!(matches!(
            Name::read(&[5, b'a', b'b'], 0),
            Err(NameError::Truncated)
        ));
        // A pointer's second byte missing.
        assert!(matches!(Name::read(&[0xC0], 0), Err(NameError::Truncated)));
        // Nothing at all.
        assert!(matches!(Name::read(&[], 0), Err(NameError::Truncated)));
    }

    #[test]
    fn a_name_longer_than_the_protocol_allows_is_refused() {
        let long = (0..40).map(|_| "abcdef").collect::<Vec<_>>().join(".");
        assert!(matches!(Name::parse(&long), Err(NameError::TooLong { .. })));

        // And on the way in, built out of legal labels that add up to too much.
        let mut packet = Vec::new();
        for _ in 0..40 {
            packet.push(6);
            packet.extend_from_slice(b"abcdef");
        }
        packet.push(0);
        assert!(matches!(
            Name::read(&packet, 0),
            Err(NameError::TooLong { .. })
        ));
    }

    #[test]
    fn a_label_longer_than_the_protocol_allows_is_refused() {
        let label = "a".repeat(64);
        assert!(matches!(
            Name::parse(&label),
            Err(NameError::LabelTooLong { len: 64 })
        ));
        assert!(matches!(
            Name::from_labels(vec![vec![b'a'; 64]]),
            Err(NameError::LabelTooLong { len: 64 })
        ));
    }

    #[test]
    fn an_empty_label_is_refused() {
        assert!(matches!(Name::parse("a..b"), Err(NameError::EmptyLabel)));
        assert!(matches!(
            Name::from_labels(vec![b"a".to_vec(), Vec::new()]),
            Err(NameError::EmptyLabel)
        ));
    }

    #[test]
    fn a_trailing_dot_is_the_same_name() {
        assert_eq!(
            Name::parse("local.").expect("a name"),
            Name::parse("local").expect("a name")
        );
    }

    /// A shorter name cannot end with a longer one, and a suffix has to match
    /// whole labels: `xownview` does not end with `ownview`.
    #[test]
    fn ends_with_compares_labels_not_characters() {
        let service = Name::parse("_ownview._udp.local").expect("a name");
        assert!(!Name::parse("local").expect("a name").ends_with(&service));
        assert!(
            !Name::parse("x_ownview._udp.local")
                .expect("a name")
                .ends_with(&service)
        );
        assert!(
            Name::parse("elythion._ownview._udp.local")
                .expect("a name")
                .ends_with(&service)
        );
    }

    #[test]
    fn display_escapes_what_would_change_the_shape() {
        let name = Name::from_labels(vec![b"a.b".to_vec(), b"c\\d".to_vec()]).expect("labels");
        assert_eq!(name.to_string(), r"a\.b.c\\d");
        let control = Name::from_labels(vec![vec![b'a', 0x01]]).expect("labels");
        assert_eq!(control.to_string(), r"a\001");
    }
}
