//! A frame to RTP packets, RFC 6184.
//!
//! # Two of the three payload modes, and why the third is left out
//!
//! RFC 6184 defines three ways to carry H.264 in RTP. This writes two:
//!
//! - **Single NAL unit**, when a unit fits in a packet. The NAL unit *is* the
//!   payload, with no extra bytes at all.
//! - **FU-A**, when it does not. The unit is split across packets with a
//!   two-byte header on each saying which piece it is.
//!
//! The third is **STAP-A**, which packs several small units into one packet.
//! Its whole benefit is saving datagrams on the parameter sets — an SPS and a
//! PPS are a few dozen bytes each, so a keyframe sends two nearly-empty
//! packets that STAP-A would combine into one. That is two datagrams per
//! keyframe, against another aggregation format for a receiver to parse on the
//! way to a decoder. Every WebRTC receiver accepts single-NAL packets, so the
//! trade is a packet nobody notices against a parser everybody has to trust.
//!
//! # The MTU is a budget, not a target
//!
//! Fragmenting to the path MTU means the *whole datagram* must fit: the QUIC
//! framing, the RTP header, and for FU-A its two extra bytes. A packetizer
//! that fragments to the MTU and then adds a header produces datagrams one
//! byte too large, which is a failure that only appears on paths whose MTU is
//! exactly wrong and not on a loopback.

use crate::annexb::{NalUnits, START_CODE};
use crate::rtp::{RTP_HEADER_LEN, RtpHeader};

/// FU-A, the fragmentation unit type.
const FU_A: u8 = 28;

/// FU-A adds an indicator byte and a header byte to every fragment.
const FU_HEADER_LEN: usize = 2;

/// The smallest datagram worth attempting.
///
/// # This is datagram payload, not path MTU, and the difference bit
///
/// QUIC's floor is 1200 bytes of *UDP payload*, and IPv6 guarantees 1280 bytes
/// end to end. Neither is what a sender gets to fill: the QUIC packet header,
/// the AEAD tag and the DATAGRAM frame header come out of it first, leaving
/// about 1162 bytes on a connection at QUIC's minimum.
///
/// So a floor of 1200 expressed against `max_datagram_size` is a floor no
/// connection clears until path MTU discovery has raised it — which happens
/// after a connection is established, not before. The first version of this
/// was 1200, and it refused **every real connection at startup** while passing
/// a test that happened to ask later.
///
/// 1100 leaves sixty bytes of margin under the worst case a conforming QUIC
/// peer can offer, and is still large enough that RTP's twelve-byte header is
/// noise.
pub const MIN_MTU: usize = 1100;

/// Why a frame could not be packetized.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PacketizeError {
    /// The MTU leaves no room for a payload.
    #[error("an MTU of {0} is below the {MIN_MTU}-byte floor this link needs")]
    MtuTooSmall(usize),
    /// The bitstream held no NAL units.
    ///
    /// An encoder producing a frame with no units means something upstream is
    /// wrong, and sending nothing would leave a receiver waiting for a frame
    /// that is never completed.
    #[error("the bitstream contains no NAL units, so there is nothing to send")]
    NoNalUnits,
}

/// Turns encoded frames into RTP packets.
///
/// Holds the sequence number and the SSRC, because those are properties of the
/// *stream* rather than of a frame: a packetizer rebuilt per frame would
/// restart its sequence numbering every time, which a receiver reads as
/// catastrophic loss.
#[derive(Debug)]
pub struct Packetizer {
    ssrc: u32,
    payload_type: u8,
    sequence: u16,
    mtu: usize,
    /// Total datagrams written, for the stats the session reports.
    packets: u64,
}

impl Packetizer {
    /// A packetizer for one stream.
    ///
    /// `ssrc` identifies the stream, and is the monitor's identity on the
    /// wire — see the crate documentation.
    pub fn new(ssrc: u32, mtu: usize) -> Result<Self, PacketizeError> {
        if mtu < MIN_MTU {
            return Err(PacketizeError::MtuTooSmall(mtu));
        }
        Ok(Self {
            ssrc,
            payload_type: crate::PAYLOAD_TYPE_H264,
            // A random start is what RFC 3550 asks for, and this is not that:
            // the sequence space is not a secret here because the link is
            // already authenticated and encrypted by QUIC. Starting at zero
            // makes a capture readable, which is worth more.
            sequence: 0,
            mtu,
            packets: 0,
        })
    }

    /// The stream's identity.
    #[must_use]
    pub const fn ssrc(&self) -> u32 {
        self.ssrc
    }

    /// The next sequence number that will be used.
    #[must_use]
    pub const fn next_sequence(&self) -> u16 {
        self.sequence
    }

    /// How many datagrams have been produced.
    #[must_use]
    pub const fn packets(&self) -> u64 {
        self.packets
    }

    /// The largest payload a single-NAL packet can carry.
    #[must_use]
    pub const fn max_single_payload(&self) -> usize {
        self.mtu.saturating_sub(RTP_HEADER_LEN)
    }

    /// The datagram size this is packetizing for.
    ///
    /// So a caller following a path that changes can tell whether it has.
    #[must_use]
    pub const fn mtu(&self) -> usize {
        self.mtu
    }

    /// Continue an interrupted stream's numbering.
    ///
    /// For a session resumed on a new transport: the packets are the same
    /// stream, so a capture of the traffic should read as one stream
    /// interrupted rather than two unrelated ones. The receiver is new and has
    /// no expectation either way, which is why this is about legibility rather
    /// than correctness.
    pub const fn set_sequence(&mut self, sequence: u16) {
        self.sequence = sequence;
    }

    /// Become a different stream.
    ///
    /// For a monitor switch: RTP says a synchronisation source is one
    /// timeline, and pixels of a different screen at a different size are not
    /// that. The sequence numbering restarts because a receiver keyed on the
    /// new SSRC has no history to be consistent with — and continuing the old
    /// count would suggest a continuity that does not exist.
    pub const fn set_ssrc(&mut self, ssrc: u32) {
        self.ssrc = ssrc;
        self.sequence = 0;
    }

    /// Change the MTU, for a path that turned out to be different.
    pub fn set_mtu(&mut self, mtu: usize) -> Result<(), PacketizeError> {
        if mtu < MIN_MTU {
            return Err(PacketizeError::MtuTooSmall(mtu));
        }
        self.mtu = mtu;
        Ok(())
    }

    /// Packetize one encoded frame.
    ///
    /// `timestamp` is the frame's presentation time at 90 kHz. Every packet of
    /// the frame carries it, which is how a receiver groups them, and the last
    /// packet carries the marker bit, which is how it knows the frame is done.
    ///
    /// `into` is cleared and filled with complete datagrams.
    pub fn packetize(
        &mut self,
        bitstream: &[u8],
        timestamp: u32,
        into: &mut Vec<Vec<u8>>,
    ) -> Result<(), PacketizeError> {
        into.clear();
        for unit in NalUnits::new(bitstream) {
            if unit.bytes.len() <= self.max_single_payload() {
                self.single(unit.bytes, timestamp, into);
            } else {
                self.fragmented(unit.header(), unit.payload(), timestamp, into);
            }
        }
        if into.is_empty() {
            return Err(PacketizeError::NoNalUnits);
        }
        // The marker says "this is the last packet of this frame", so it can
        // only be set once every unit has been written.
        if let Some(b) = into.last_mut().and_then(|last| last.get_mut(1)) {
            *b |= 0x80;
        }
        Ok(())
    }

    fn header(&mut self, timestamp: u32) -> RtpHeader {
        let h = RtpHeader {
            marker: false,
            payload_type: self.payload_type,
            sequence: self.sequence,
            timestamp,
            ssrc: self.ssrc,
        };
        self.sequence = self.sequence.wrapping_add(1);
        self.packets = self.packets.saturating_add(1);
        h
    }

    fn single(&mut self, unit: &[u8], timestamp: u32, into: &mut Vec<Vec<u8>>) {
        let mut packet = vec![0u8; RTP_HEADER_LEN.saturating_add(unit.len())];
        let h = self.header(timestamp);
        if h.write(&mut packet).is_none() {
            return;
        }
        if let Some(body) = packet.get_mut(RTP_HEADER_LEN..) {
            body.copy_from_slice(unit);
        }
        into.push(packet);
    }

    fn fragmented(
        &mut self,
        nal_header: u8,
        payload: &[u8],
        timestamp: u32,
        into: &mut Vec<Vec<u8>>,
    ) {
        // The FU indicator keeps the original unit's forbidden bit and
        // importance, and replaces the type with FU-A. The original type moves
        // into the FU header, which is how the receiver rebuilds the byte.
        let indicator = (nal_header & 0xE0) | FU_A;
        let kind = nal_header & 0x1F;
        let room = self
            .mtu
            .saturating_sub(RTP_HEADER_LEN)
            .saturating_sub(FU_HEADER_LEN);
        if room == 0 {
            return;
        }

        let mut offset = 0usize;
        while offset < payload.len() {
            let take = room.min(payload.len().saturating_sub(offset));
            let Some(chunk) = payload.get(offset..offset.saturating_add(take)) else {
                return;
            };
            let start = offset == 0;
            let end = offset.saturating_add(take) >= payload.len();
            let mut packet = vec![
                0u8;
                RTP_HEADER_LEN
                    .saturating_add(FU_HEADER_LEN)
                    .saturating_add(take)
            ];
            let h = self.header(timestamp);
            if h.write(&mut packet).is_none() {
                return;
            }
            if let Some(b) = packet.get_mut(RTP_HEADER_LEN) {
                *b = indicator;
            }
            if let Some(b) = packet.get_mut(RTP_HEADER_LEN + 1) {
                *b = (u8::from(start) << 7) | (u8::from(end) << 6) | kind;
            }
            if let Some(body) = packet.get_mut(RTP_HEADER_LEN + FU_HEADER_LEN..) {
                body.copy_from_slice(chunk);
            }
            into.push(packet);
            offset = offset.saturating_add(take);
        }
    }
}

/// Rebuild an Annex-B bitstream from NAL units.
///
/// The inverse of what the packetizer took apart, used by the depacketizer and
/// available on its own so a caller holding units can produce something a
/// decoder or a file will take.
#[must_use]
pub fn to_annexb(units: &[Vec<u8>]) -> Vec<u8> {
    let total: usize = units
        .iter()
        .map(|u| u.len().saturating_add(START_CODE.len()))
        .sum();
    let mut out = Vec::with_capacity(total);
    for unit in units {
        out.extend_from_slice(&START_CODE);
        out.extend_from_slice(unit);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::annexb::NalUnits;
    use crate::rtp::RtpHeader;

    const MTU: usize = 1200;

    fn stream(units: &[&[u8]]) -> Vec<u8> {
        let mut out = Vec::new();
        for u in units {
            out.extend_from_slice(&START_CODE);
            out.extend_from_slice(u);
        }
        out
    }

    fn unit(kind: u8, importance: u8, len: usize) -> Vec<u8> {
        let mut v = vec![(importance << 5) | kind];
        v.extend((0..len).map(|i| (i % 251) as u8));
        v
    }

    #[test]
    fn a_small_frame_becomes_one_packet_per_unit() {
        let mut p = Packetizer::new(7, MTU).expect("valid");
        let a = unit(7, 3, 20);
        let b = unit(8, 3, 8);
        let c = unit(5, 3, 100);
        let s = stream(&[&a, &b, &c]);
        let mut out = Vec::new();
        p.packetize(&s, 900, &mut out).expect("packetizes");
        assert_eq!(out.len(), 3);
        for (i, packet) in out.iter().enumerate() {
            let (h, payload) = RtpHeader::parse(packet).expect("parses");
            assert_eq!(h.ssrc, 7);
            assert_eq!(h.timestamp, 900);
            assert_eq!(h.sequence, i as u16);
            assert_eq!(h.payload_type, crate::PAYLOAD_TYPE_H264);
            assert_eq!(h.marker, i == 2, "only the last packet carries the marker");
            let expected: &[u8] = match i {
                0 => &a,
                1 => &b,
                _ => &c,
            };
            assert_eq!(payload, expected, "packet {i} payload");
        }
    }

    #[test]
    fn every_datagram_fits_the_mtu() {
        // The mistake this catches is fragmenting to the MTU and then adding a
        // header, which overshoots by exactly the header.
        for mtu in [MIN_MTU, 1201, 1280, 1400, 1500, 9000] {
            for size in [1usize, 100, 1187, 1188, 1189, 5000, 100_000] {
                let mut p = Packetizer::new(1, mtu).expect("valid");
                let s = stream(&[&unit(5, 3, size)]);
                let mut out = Vec::new();
                p.packetize(&s, 0, &mut out).expect("packetizes");
                for packet in &out {
                    assert!(
                        packet.len() <= mtu,
                        "mtu {mtu}, unit {size}: a {}-byte datagram",
                        packet.len()
                    );
                }
                assert!(!out.is_empty());
            }
        }
    }

    #[test]
    fn a_large_unit_is_fragmented_and_the_pieces_rebuild_it() {
        let mut p = Packetizer::new(3, MTU).expect("valid");
        let big = unit(5, 2, 5000);
        let s = stream(&[&big]);
        let mut out = Vec::new();
        p.packetize(&s, 4500, &mut out).expect("packetizes");
        assert!(out.len() > 1, "5000 bytes should not fit one packet");

        // Rebuild by hand, the way a receiver does, and check the bytes.
        let mut rebuilt = Vec::new();
        for (i, packet) in out.iter().enumerate() {
            let (h, payload) = RtpHeader::parse(packet).expect("parses");
            assert_eq!(h.timestamp, 4500);
            assert_eq!(h.sequence, i as u16);
            let indicator = payload.first().copied().expect("an indicator");
            let fu = payload.get(1).copied().expect("an FU header");
            assert_eq!(indicator & 0x1F, FU_A, "packet {i} is not FU-A");
            assert_eq!(
                indicator & 0xE0,
                big.first().copied().unwrap_or(0) & 0xE0,
                "the importance and forbidden bits must survive fragmentation"
            );
            assert_eq!(fu & 0x1F, 5, "the original type lives in the FU header");
            assert_eq!(fu & 0x80 != 0, i == 0, "start bit on packet {i}");
            assert_eq!(fu & 0x40 != 0, i == out.len() - 1, "end bit on packet {i}");
            if i == 0 {
                // The receiver rebuilds the NAL header from the two.
                rebuilt.push((indicator & 0xE0) | (fu & 0x1F));
            }
            rebuilt.extend_from_slice(payload.get(2..).expect("a fragment"));
        }
        assert_eq!(rebuilt, big, "the fragments do not rebuild the unit");
    }

    #[test]
    fn only_the_very_last_packet_of_a_frame_is_marked() {
        // A frame with one small unit and one that fragments: the marker must
        // be on the last fragment, not on the last unit's first packet.
        let mut p = Packetizer::new(1, MTU).expect("valid");
        let s = stream(&[&unit(7, 3, 10), &unit(5, 3, 4000)]);
        let mut out = Vec::new();
        p.packetize(&s, 0, &mut out).expect("packetizes");
        assert!(out.len() >= 3);
        for (i, packet) in out.iter().enumerate() {
            let (h, _) = RtpHeader::parse(packet).expect("parses");
            assert_eq!(
                h.marker,
                i == out.len() - 1,
                "packet {i} of {}: marker {}",
                out.len(),
                h.marker
            );
        }
    }

    #[test]
    fn the_sequence_continues_across_frames() {
        // Restarting per frame reads to a receiver as catastrophic loss.
        let mut p = Packetizer::new(1, MTU).expect("valid");
        let s = stream(&[&unit(5, 3, 50)]);
        let mut out = Vec::new();
        let mut seen = Vec::new();
        for frame in 0..5u32 {
            p.packetize(&s, frame * 3000, &mut out).expect("packetizes");
            for packet in &out {
                let (h, _) = RtpHeader::parse(packet).expect("parses");
                seen.push(h.sequence);
            }
        }
        assert_eq!(seen, vec![0, 1, 2, 3, 4]);
        assert_eq!(p.next_sequence(), 5);
        assert_eq!(p.packets(), 5);
    }

    #[test]
    fn the_sequence_wraps_rather_than_stopping() {
        let mut p = Packetizer::new(1, MTU).expect("valid");
        p.sequence = u16::MAX;
        let s = stream(&[&unit(5, 3, 20)]);
        let mut out = Vec::new();
        p.packetize(&s, 0, &mut out).expect("packetizes");
        p.packetize(&s, 3000, &mut out).expect("packetizes");
        let (h, _) = RtpHeader::parse(out.first().expect("a packet")).expect("parses");
        assert_eq!(h.sequence, 0, "the sequence must wrap to zero");
    }

    #[test]
    fn an_mtu_below_the_floor_is_refused() {
        assert_eq!(
            Packetizer::new(1, MIN_MTU - 1).err(),
            Some(PacketizeError::MtuTooSmall(MIN_MTU - 1))
        );
        assert_eq!(
            Packetizer::new(1, 0).err(),
            Some(PacketizeError::MtuTooSmall(0))
        );
        let mut p = Packetizer::new(1, MTU).expect("valid");
        assert!(p.set_mtu(20).is_err());
        assert!(p.set_mtu(1500).is_ok());
    }

    #[test]
    fn a_bitstream_with_no_units_is_refused_rather_than_sent_as_nothing() {
        // Sending zero packets leaves a receiver waiting for a frame that
        // never completes.
        let mut p = Packetizer::new(1, MTU).expect("valid");
        let mut out = Vec::new();
        assert_eq!(
            p.packetize(&[], 0, &mut out),
            Err(PacketizeError::NoNalUnits)
        );
        assert_eq!(
            p.packetize(&[0x65, 1, 2], 0, &mut out),
            Err(PacketizeError::NoNalUnits),
            "no start code means no units"
        );
        assert!(out.is_empty());
    }

    #[test]
    fn the_boundary_between_single_and_fragmented_is_exact() {
        let mut p = Packetizer::new(1, MTU).expect("valid");
        let max = p.max_single_payload();
        // A unit of exactly the maximum is one packet; one byte more is two.
        for (len, want) in [(max, 1usize), (max + 1, 2)] {
            let mut body = vec![0x65u8];
            body.extend(std::iter::repeat_n(0xAB, len - 1));
            assert_eq!(body.len(), len);
            let s = stream(&[&body]);
            let mut out = Vec::new();
            p.packetize(&s, 0, &mut out).expect("packetizes");
            assert_eq!(out.len(), want, "a {len}-byte unit at MTU {MTU}");
        }
    }

    #[test]
    fn annexb_rebuilding_is_the_inverse_of_splitting() {
        let a = unit(7, 3, 12);
        let b = unit(5, 3, 300);
        let s = stream(&[&a, &b]);
        let units: Vec<Vec<u8>> = NalUnits::new(&s).map(|n| n.bytes.to_vec()).collect();
        assert_eq!(to_annexb(&units), s);
    }
}
