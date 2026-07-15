//! Splitting an Annex-B bitstream into NAL units.
//!
//! An H.264 encoder writing to a file or a pipe emits Annex-B: NAL units
//! separated by a start code of `00 00 01`, usually with an extra leading zero.
//! RTP wants the NAL units themselves, without start codes, so this is the
//! first thing the packetizer does.
//!
//! # Why scanning for start codes is safe
//!
//! It looks like a framing scheme that can be confused by its own payload, and
//! it is not, because H.264 forbids the confusion at the source. An encoder
//! applies *emulation prevention*: anywhere the payload would contain
//! `00 00 00`, `00 00 01`, `00 00 02` or `00 00 03`, it inserts a `03` byte —
//! so `00 00 01` cannot occur inside a NAL unit at all. The decoder removes
//! those bytes again.
//!
//! This module therefore does not need to understand the bitstream to find its
//! boundaries, which is the entire reason Annex-B exists.
//!
//! # What it does with a malformed stream
//!
//! Returns what it found and stops. A stream with no start code produces no
//! NAL units rather than one unit that is the whole buffer: guessing that a
//! bitstream without framing is a single NAL is how a corrupt frame becomes a
//! decoder's problem instead of a parser's.

/// NAL unit types this needs to recognise by name.
///
/// The full list is long and most of it never appears in a stream from a
/// real-time encoder. These are the ones a decision is made about.
pub mod nal {
    /// A non-IDR coded slice: a picture that depends on earlier ones.
    pub const SLICE: u8 = 1;
    /// An IDR coded slice: a picture a decoder can start from.
    pub const IDR: u8 = 5;
    /// Supplemental enhancement information.
    pub const SEI: u8 = 6;
    /// Sequence parameter set.
    pub const SPS: u8 = 7;
    /// Picture parameter set.
    pub const PPS: u8 = 8;
}

/// One NAL unit, without its start code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NalUnit<'a> {
    /// The unit, beginning with its one-byte NAL header.
    pub bytes: &'a [u8],
}

impl<'a> NalUnit<'a> {
    /// The NAL unit type, from the low five bits of the header byte.
    #[must_use]
    pub fn kind(&self) -> u8 {
        self.bytes.first().copied().unwrap_or(0) & 0x1F
    }

    /// The `nal_ref_idc` field, which says how much a decoder loses by
    /// dropping this unit. Carried through fragmentation unchanged.
    #[must_use]
    pub fn importance(&self) -> u8 {
        (self.bytes.first().copied().unwrap_or(0) >> 5) & 0x03
    }

    /// The header byte, forbidden bit included.
    #[must_use]
    pub fn header(&self) -> u8 {
        self.bytes.first().copied().unwrap_or(0)
    }

    /// Everything after the header byte.
    #[must_use]
    pub fn payload(&self) -> &'a [u8] {
        self.bytes.get(1..).unwrap_or_default()
    }

    /// Whether a decoder can begin at this unit.
    #[must_use]
    pub fn starts_a_picture(&self) -> bool {
        matches!(self.kind(), nal::IDR | nal::SPS | nal::PPS)
    }
}

/// The NAL units of an Annex-B bitstream, in order.
#[derive(Debug, Clone)]
pub struct NalUnits<'a> {
    rest: &'a [u8],
}

impl<'a> NalUnits<'a> {
    /// Walk `bitstream`.
    #[must_use]
    pub fn new(bitstream: &'a [u8]) -> Self {
        Self {
            rest: after_start_code(bitstream).unwrap_or_default(),
        }
    }
}

impl<'a> Iterator for NalUnits<'a> {
    type Item = NalUnit<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.rest.is_empty() {
            return None;
        }
        match find_start_code(self.rest) {
            Some((at, past)) => {
                let unit = self.rest.get(..at)?;
                self.rest = self.rest.get(past..).unwrap_or_default();
                // A start code immediately followed by another is not a unit.
                if unit.is_empty() {
                    return self.next();
                }
                Some(NalUnit { bytes: unit })
            }
            None => {
                // Whatever is left is the last unit, trailing zeros included.
                //
                // Annex-B does allow zero padding after a unit, and an earlier
                // version of this stripped it — which quietly corrupted any
                // unit whose own last byte was zero. That is a *bitstream*
                // assumption inside what is meant to be pure framing, and this
                // module's whole claim is that it finds boundaries without
                // understanding the payload. A decoder discards trailing zeros
                // itself under the RBSP trailing rules, so leaving them in
                // costs nothing and keeps the split exactly reversible.
                let unit = std::mem::take(&mut self.rest);
                (!unit.is_empty()).then_some(NalUnit { bytes: unit })
            }
        }
    }
}

/// Whether a frame contains a picture a decoder can start from.
///
/// Derived rather than taken from a flag on the wire. A sender's flag is
/// something a sender can get wrong or lie about; the NAL unit types are what
/// the decoder will actually act on.
#[must_use]
pub fn is_keyframe(bitstream: &[u8]) -> bool {
    NalUnits::new(bitstream).any(|n| n.kind() == nal::IDR)
}

/// The byte after the first start code, or `None` if there is not one.
fn after_start_code(bytes: &[u8]) -> Option<&[u8]> {
    find_start_code(bytes).and_then(|(_, past)| bytes.get(past..))
}

/// Where the next start code begins and ends: `(at, past)`.
///
/// `at` is the index of the first zero, so the caller can take everything
/// before it as a unit; `past` is the first byte after the code.
fn find_start_code(bytes: &[u8]) -> Option<(usize, usize)> {
    let mut i = 0usize;
    while i + 2 < bytes.len() {
        let (a, b, c) = (
            bytes.get(i).copied()?,
            bytes.get(i + 1).copied()?,
            bytes.get(i + 2).copied()?,
        );
        if a == 0 && b == 0 && c == 1 {
            // A four-byte code is a three-byte code with a zero in front, and
            // that zero belongs to the code rather than to the unit before it.
            let at = if i > 0 && bytes.get(i - 1) == Some(&0) {
                i - 1
            } else {
                i
            };
            return Some((at, i + 3));
        }
        // `00 00 01` cannot straddle a non-zero byte, so a non-zero at i+2
        // means the next possible code starts at i+3.
        i += if c != 0 { 3 } else { 1 };
    }
    None
}

/// The Annex-B start code this writes when reassembling a frame.
///
/// Four bytes rather than three. Both are legal and decoders take either; the
/// four-byte form is what openh264 emits, so a reassembled frame is
/// byte-identical to the one that was sent rather than merely equivalent —
/// which makes a round-trip test an equality rather than a judgement.
pub const START_CODE: [u8; 4] = [0, 0, 0, 1];

#[cfg(test)]
mod tests {
    use super::*;

    fn stream(units: &[&[u8]]) -> Vec<u8> {
        let mut out = Vec::new();
        for u in units {
            out.extend_from_slice(&START_CODE);
            out.extend_from_slice(u);
        }
        out
    }

    #[test]
    fn units_come_back_in_order_without_their_start_codes() {
        let a: &[u8] = &[0x67, 1, 2, 3];
        let b: &[u8] = &[0x68, 4, 5];
        let c: &[u8] = &[0x65, 6, 7, 8, 9];
        let s = stream(&[a, b, c]);
        let got: Vec<&[u8]> = NalUnits::new(&s).map(|n| n.bytes).collect();
        assert_eq!(got, vec![a, b, c]);
    }

    #[test]
    fn a_three_byte_start_code_works_too() {
        // 00 00 01 without the leading zero, which is also legal Annex-B.
        let s = vec![0, 0, 1, 0x67, 1, 2, 0, 0, 1, 0x65, 3];
        let got: Vec<&[u8]> = NalUnits::new(&s).map(|n| n.bytes).collect();
        assert_eq!(got, vec![&[0x67u8, 1, 2][..], &[0x65, 3][..]]);
    }

    #[test]
    fn the_leading_zero_of_a_four_byte_code_is_not_part_of_the_unit_before_it() {
        // The classic off-by-one: unit A must not end with a stray 0x00.
        let s = stream(&[&[0x67, 0xAA], &[0x65, 0xBB]]);
        let got: Vec<&[u8]> = NalUnits::new(&s).map(|n| n.bytes).collect();
        assert_eq!(got, vec![&[0x67u8, 0xAA][..], &[0x65, 0xBB][..]]);
    }

    #[test]
    fn the_header_byte_is_decoded_into_its_fields() {
        // 0x65: forbidden 0, nal_ref_idc 3, type 5 (IDR).
        let s = stream(&[&[0x65, 1]]);
        let n = NalUnits::new(&s).next().expect("a unit");
        assert_eq!(n.kind(), nal::IDR);
        assert_eq!(n.importance(), 3);
        assert_eq!(n.header(), 0x65);
        assert_eq!(n.payload(), &[1]);
        assert!(n.starts_a_picture());

        // 0x41: nal_ref_idc 2, type 1 (a non-IDR slice).
        let s = stream(&[&[0x41, 2, 3]]);
        let n = NalUnits::new(&s).next().expect("a unit");
        assert_eq!(n.kind(), nal::SLICE);
        assert_eq!(n.importance(), 2);
        assert!(!n.starts_a_picture());
    }

    #[test]
    fn a_stream_with_no_start_code_yields_nothing() {
        // Not one unit that is the whole buffer: a bitstream without framing
        // is not a NAL unit, and guessing makes it the decoder's problem.
        assert_eq!(NalUnits::new(&[0x67, 1, 2, 3]).count(), 0);
        assert_eq!(NalUnits::new(&[]).count(), 0);
        assert_eq!(NalUnits::new(&[0, 0]).count(), 0);
        assert_eq!(NalUnits::new(&[0, 0, 0]).count(), 0);
    }

    #[test]
    fn a_start_code_with_nothing_after_it_is_not_a_unit() {
        let mut s = stream(&[&[0x67, 1]]);
        s.extend_from_slice(&START_CODE);
        s.extend_from_slice(&START_CODE);
        s.extend_from_slice(&[0x65, 2]);
        let got: Vec<&[u8]> = NalUnits::new(&s).map(|n| n.bytes).collect();
        assert_eq!(got, vec![&[0x67u8, 1][..], &[0x65, 2][..]]);
    }

    /// **The split has to be exactly reversible.** Trailing zeros stay with
    /// the last unit rather than being taken for padding, because a unit whose
    /// own last byte is zero is indistinguishable from one with padding after
    /// it — and guessing wrong corrupts the frame.
    #[test]
    fn a_unit_ending_in_zero_keeps_its_last_byte() {
        for tail in [
            vec![0x01u8, 0x00],
            vec![0x41, 0x10, 0x00, 0x00],
            vec![0x65, 0x00],
        ] {
            let s = stream(&[&[0x67, 1], &tail]);
            let got: Vec<&[u8]> = NalUnits::new(&s).map(|n| n.bytes).collect();
            assert_eq!(
                got,
                vec![&[0x67u8, 1][..], tail.as_slice()],
                "the trailing zeros of {tail:?} were lost"
            );
        }
    }

    /// A payload that *looks* like a start code cannot occur, because the
    /// encoder prevents it — but a payload full of zeros and ones that stops
    /// short of the pattern must still split correctly.
    #[test]
    fn zero_heavy_payloads_split_correctly() {
        for payload in [
            vec![0x41u8, 0, 0, 2, 0, 0, 3],
            vec![0x41, 0, 0, 0, 3, 0, 0, 2],
            vec![0x41, 0, 3, 0, 3, 0, 3],
        ] {
            let s = stream(&[&payload, &[0x65, 0xFF]]);
            let got: Vec<&[u8]> = NalUnits::new(&s).map(|n| n.bytes).collect();
            assert_eq!(
                got,
                vec![payload.as_slice(), &[0x65, 0xFF][..]],
                "payload {payload:?} split wrongly"
            );
        }
    }

    #[test]
    fn a_keyframe_is_recognised_by_its_units_rather_than_a_flag() {
        let idr = stream(&[&[0x67, 1], &[0x68, 2], &[0x65, 3]]);
        assert!(is_keyframe(&idr));
        let delta = stream(&[&[0x41, 3]]);
        assert!(!is_keyframe(&delta));
        // Parameter sets alone are not a picture.
        let params = stream(&[&[0x67, 1], &[0x68, 2]]);
        assert!(!is_keyframe(&params));
    }

    /// Anything at all, without panicking. This is a parser on the path from
    /// the network to a decoder.
    #[test]
    fn arbitrary_bytes_never_panic() {
        let mut state = 0x1234_5678u64;
        let mut next = || {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
            (state >> 33) as u8
        };
        for len in 0..200usize {
            for _ in 0..20 {
                let bytes: Vec<u8> = (0..len).map(|_| next() % 4).collect();
                let mut total = 0usize;
                for n in NalUnits::new(&bytes) {
                    total += n.bytes.len();
                    let _ = (n.kind(), n.importance(), n.header(), n.payload());
                }
                assert!(total <= bytes.len(), "units are larger than the input");
                let _ = is_keyframe(&bytes);
            }
        }
    }
}
