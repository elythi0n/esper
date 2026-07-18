//! The RTP header: twelve bytes, RFC 3550.
//!
//! ```text
//!  0                   1                   2                   3
//!  0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1
//! ┌─┬─┬─┬───────┬─┬─────────────┬───────────────────────────────┐
//! │V│P│X│  CC   │M│      PT     │        sequence number        │
//! ├─┴─┴─┴───────┴─┴─────────────┴───────────────────────────────┤
//! │                           timestamp                         │
//! ├─────────────────────────────────────────────────────────────┤
//! │                             SSRC                            │
//! └─────────────────────────────────────────────────────────────┘
//! ```
//!
//! # What is deliberately not supported
//!
//! **Contributing sources, extensions and padding.** A packet declaring any of
//! them is rejected rather than skipped over. All three are legitimate RTP and
//! none of them can arrive from an Esper peer, because this crate is the only
//! thing that writes these packets — so accepting them would mean parsing
//! variable-length structures on behalf of a sender that does not exist. The
//! shortest path between attacker-controlled bytes and a decoder should have as
//! few branches in it as possible.
//!
//! That is a statement about *this* link, not about RTP. The loopback that
//! re-offers media to a webview writes its own packets; if it ever needs a
//! header extension it is the one adding it, and it will be parsing its own
//! output rather than a peer's.

/// Length of a header with no contributing sources and no extension.
pub const RTP_HEADER_LEN: usize = 12;

/// The only RTP version this speaks.
const VERSION: u8 = 2;

/// Why a header could not be read.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum RtpError {
    /// Fewer than twelve bytes.
    #[error("an RTP packet needs at least {RTP_HEADER_LEN} bytes, got {0}")]
    TooShort(usize),
    /// Not RTP version 2.
    #[error("RTP version {0} is not version {VERSION}")]
    WrongVersion(u8),
    /// A feature this link does not use, so a peer sending it is not a peer.
    #[error("this link carries no {what}; a packet declaring it is refused rather than skipped")]
    Unsupported { what: &'static str },
    /// The header is there but nothing follows it.
    #[error("an RTP packet with no payload")]
    Empty,
}

/// A parsed RTP header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RtpHeader {
    /// Set on the last packet of a frame, so a receiver knows when it is done
    /// without waiting for the next frame to start.
    pub marker: bool,
    /// Which payload format. Checked, not assumed.
    pub payload_type: u8,
    /// Wraps at 65536, and is meant to: the distance between two sequence
    /// numbers is what matters, never their order as integers.
    pub sequence: u16,
    /// Presentation time, at 90 kHz for video. Every packet of one frame
    /// carries the same value, which is how they are grouped.
    pub timestamp: u32,
    /// The stream's identity. One monitor, one SSRC.
    pub ssrc: u32,
}

impl RtpHeader {
    /// Write the header into the front of `out`.
    ///
    /// Returns how many bytes were written, so a caller can fill the payload
    /// after it without recomputing the offset.
    pub fn write(&self, out: &mut [u8]) -> Option<usize> {
        let head = out.get_mut(..RTP_HEADER_LEN)?;
        let mut b = [0u8; RTP_HEADER_LEN];
        // V=2, P=0, X=0, CC=0.
        *b.first_mut()? = VERSION << 6;
        *b.get_mut(1)? = (u8::from(self.marker) << 7) | (self.payload_type & 0x7F);
        let seq = self.sequence.to_be_bytes();
        *b.get_mut(2)? = *seq.first()?;
        *b.get_mut(3)? = *seq.get(1)?;
        b.get_mut(4..8)?
            .copy_from_slice(&self.timestamp.to_be_bytes());
        b.get_mut(8..12)?.copy_from_slice(&self.ssrc.to_be_bytes());
        head.copy_from_slice(&b);
        Some(RTP_HEADER_LEN)
    }

    /// Read a header, returning it and the payload that follows.
    pub fn parse(packet: &[u8]) -> Result<(Self, &[u8]), RtpError> {
        let head = packet
            .get(..RTP_HEADER_LEN)
            .ok_or(RtpError::TooShort(packet.len()))?;
        let b0 = *head.first().ok_or(RtpError::TooShort(packet.len()))?;
        let version = b0 >> 6;
        if version != VERSION {
            return Err(RtpError::WrongVersion(version));
        }
        if b0 & 0x20 != 0 {
            return Err(RtpError::Unsupported { what: "padding" });
        }
        if b0 & 0x10 != 0 {
            return Err(RtpError::Unsupported {
                what: "header extensions",
            });
        }
        if b0 & 0x0F != 0 {
            return Err(RtpError::Unsupported {
                what: "contributing sources",
            });
        }
        let b1 = *head.get(1).ok_or(RtpError::TooShort(packet.len()))?;
        let be16 = |a: usize| -> Result<u16, RtpError> {
            let hi = *head.get(a).ok_or(RtpError::TooShort(packet.len()))?;
            let lo = *head.get(a + 1).ok_or(RtpError::TooShort(packet.len()))?;
            Ok(u16::from_be_bytes([hi, lo]))
        };
        let be32 = |a: usize| -> Result<u32, RtpError> {
            let s = head.get(a..a + 4).ok_or(RtpError::TooShort(packet.len()))?;
            let mut v = [0u8; 4];
            v.copy_from_slice(s);
            Ok(u32::from_be_bytes(v))
        };

        let header = Self {
            marker: b1 & 0x80 != 0,
            payload_type: b1 & 0x7F,
            sequence: be16(2)?,
            timestamp: be32(4)?,
            ssrc: be32(8)?,
        };
        let payload = packet
            .get(RTP_HEADER_LEN..)
            .ok_or(RtpError::TooShort(packet.len()))?;
        if payload.is_empty() {
            return Err(RtpError::Empty);
        }
        Ok((header, payload))
    }
}

/// How far `later` is ahead of `earlier`, accounting for the wrap.
///
/// RTP sequence numbers wrap at 65536 and comparing them as integers is the
/// classic mistake: after a wrap, packet 1 looks 65534 *behind* packet 65535
/// when it is one ahead. Interpreting the difference as a signed 16-bit value
/// gives the right answer for any gap smaller than half the space, which is
/// every gap that is not already a lost session.
#[must_use]
pub const fn seq_distance(earlier: u16, later: u16) -> i32 {
    (later.wrapping_sub(earlier) as i16) as i32
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header() -> RtpHeader {
        RtpHeader {
            marker: true,
            payload_type: crate::PAYLOAD_TYPE_H264,
            sequence: 0xBEEF,
            timestamp: 0x1234_5678,
            ssrc: 0xDEAD_C0DE,
        }
    }

    #[test]
    fn a_header_round_trips() {
        let mut buf = [0u8; RTP_HEADER_LEN + 3];
        let n = header().write(&mut buf).expect("writes");
        assert_eq!(n, RTP_HEADER_LEN);
        if let Some(p) = buf.get_mut(RTP_HEADER_LEN..) {
            p.copy_from_slice(&[1, 2, 3]);
        }
        let (got, payload) = RtpHeader::parse(&buf).expect("parses");
        assert_eq!(got, header());
        assert_eq!(payload, &[1, 2, 3]);
    }

    #[test]
    fn the_first_two_bytes_are_the_bits_the_rfc_says() {
        let mut buf = [0u8; RTP_HEADER_LEN + 1];
        let mut h = header();
        h.marker = false;
        h.payload_type = 96;
        h.write(&mut buf).expect("writes");
        // V=2 in the top two bits, nothing else set.
        assert_eq!(buf.first(), Some(&0x80));
        assert_eq!(buf.get(1), Some(&96));
        h.marker = true;
        h.write(&mut buf).expect("writes");
        assert_eq!(
            buf.get(1),
            Some(&(0x80 | 96)),
            "the marker is the top bit of byte 1"
        );
    }

    #[test]
    fn every_field_survives_every_value() {
        // A sweep rather than a handful: a byte-order mistake in one field is
        // invisible if the test only uses palindromic values.
        for (seq, ts, ssrc) in [
            (0u16, 0u32, 0u32),
            (1, 1, 1),
            (u16::MAX, u32::MAX, u32::MAX),
            (0x0102, 0x0102_0304, 0x0a0b_0c0d),
            (0x00FF, 0x0000_00FF, 0xFF00_0000),
        ] {
            for marker in [false, true] {
                for pt in [0u8, 96, 127] {
                    let h = RtpHeader {
                        marker,
                        payload_type: pt,
                        sequence: seq,
                        timestamp: ts,
                        ssrc,
                    };
                    let mut buf = [0u8; RTP_HEADER_LEN + 1];
                    h.write(&mut buf).expect("writes");
                    if let Some(b) = buf.get_mut(RTP_HEADER_LEN) {
                        *b = 0x42;
                    }
                    let (got, payload) = RtpHeader::parse(&buf).expect("parses");
                    assert_eq!(got, h);
                    assert_eq!(payload, &[0x42]);
                }
            }
        }
    }

    #[test]
    fn a_short_packet_is_refused_before_anything_is_read() {
        for len in 0..RTP_HEADER_LEN {
            let buf = vec![0x80u8; len];
            assert_eq!(RtpHeader::parse(&buf), Err(RtpError::TooShort(len)));
        }
    }

    #[test]
    fn a_header_with_no_payload_is_refused() {
        let mut buf = [0u8; RTP_HEADER_LEN];
        header().write(&mut buf).expect("writes");
        assert_eq!(RtpHeader::parse(&buf), Err(RtpError::Empty));
    }

    #[test]
    fn the_wrong_version_is_refused() {
        let mut buf = [0u8; RTP_HEADER_LEN + 1];
        header().write(&mut buf).expect("writes");
        for version in [0u8, 1, 3] {
            if let Some(b) = buf.first_mut() {
                *b = (version << 6) | (*b & 0x3F);
            }
            assert_eq!(RtpHeader::parse(&buf), Err(RtpError::WrongVersion(version)));
        }
    }

    /// Padding, extensions and contributing sources all mean a
    /// variable-length header. Refusing them keeps the path to the decoder as
    /// short as it can be.
    #[test]
    fn variable_length_headers_are_refused_rather_than_skipped() {
        for (bit, what) in [
            (0x20u8, "padding"),
            (0x10, "header extensions"),
            (0x01, "contributing sources"),
        ] {
            let mut buf = [0u8; RTP_HEADER_LEN + 1];
            header().write(&mut buf).expect("writes");
            if let Some(b) = buf.first_mut() {
                *b |= bit;
            }
            assert_eq!(
                RtpHeader::parse(&buf),
                Err(RtpError::Unsupported { what }),
                "bit {bit:#04x} was accepted"
            );
        }
    }

    /// **The wrap.** Comparing sequence numbers as integers is the classic RTP
    /// mistake and it only appears after sixty-five thousand packets, which on
    /// a 30 fps stream is half an hour in.
    #[test]
    fn sequence_distance_is_wrap_aware() {
        assert_eq!(seq_distance(10, 11), 1);
        assert_eq!(seq_distance(11, 10), -1);
        assert_eq!(seq_distance(0, 0), 0);
        // Across the wrap, which is the whole point.
        assert_eq!(seq_distance(65535, 0), 1);
        assert_eq!(seq_distance(65535, 1), 2);
        assert_eq!(seq_distance(0, 65535), -1);
        assert_eq!(seq_distance(65530, 5), 11);
        assert_eq!(seq_distance(5, 65530), -11);
        // And it is consistent for every pair a session could produce.
        for start in [0u16, 1, 30000, 65000, 65535] {
            for step in 1..=1000u16 {
                let later = start.wrapping_add(step);
                assert_eq!(seq_distance(start, later), i32::from(step));
                assert_eq!(seq_distance(later, start), -i32::from(step));
            }
        }
    }
}
