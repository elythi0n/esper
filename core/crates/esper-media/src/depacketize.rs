//! RTP packets back into frames, RFC 6184.
//!
//! # A frame is abandoned rather than waited for
//!
//! This is the decision that makes a remote desktop feel like one. A lost
//! fragment means the frame it belonged to can never be completed, and there
//! are two things to do about it: wait, in case the packet is merely late, or
//! give up and ask for a fresh picture.
//!
//! Waiting is what a video player does, because a player has a buffer measured
//! in seconds and nothing else to show. A remote desktop has neither. A frame
//! held for a late packet delays every frame behind it, and the screen it is
//! delaying is one somebody is trying to *use* — so the frame is dropped, the
//! caller asks for a keyframe, and the picture catches up in one round trip
//! instead of stuttering for the length of a buffer.
//!
//! # What bounds this
//!
//! Everything a peer controls. Reassembly holds at most one frame, that frame
//! is capped in both packets and bytes, and both caps are checked *before* the
//! allocation rather than after. A peer that sends fragments which never
//! complete costs one frame's worth of memory and nothing more; a peer that
//! claims a frame of a gigabyte is refused at the first packet past the cap.
//!
//! # Reordering, and why it is not loss
//!
//! QUIC datagrams arrive out of order. A receiver that treated the first
//! out-of-order packet as loss would abandon frames constantly on a healthy
//! network. So packets are collected by the frame they belong to and sorted
//! into place, and a frame is complete when the run from its first packet to
//! its marked last packet has no holes — which is a statement about what
//! arrived, not about what order it arrived in.

use std::collections::BTreeMap;

use crate::annexb::{START_CODE, is_keyframe};
use crate::rtp::{RtpError, RtpHeader, seq_distance};

/// FU-A, the fragmentation unit type.
const FU_A: u8 = 28;

/// STAP-A and the other aggregation and fragmentation types.
///
/// Recognised only so they can be refused by name: nothing on this link writes
/// them, so a packet carrying one is either a bug at the far end or not a peer.
const STAP_A: u8 = 24;
const STAP_B: u8 = 25;
const MTAP16: u8 = 26;
const MTAP24: u8 = 27;
const FU_B: u8 = 29;

/// The largest frame this will reassemble.
///
/// Two megabytes, matching the cap the decoder itself enforces on a packet.
/// A 1080p keyframe at a generous quality is a couple of hundred kilobytes, so
/// this is an order of magnitude of headroom — and it is the number that stops
/// a peer from turning a lost marker bit into unbounded memory.
pub const MAX_FRAME_BYTES: usize = 2 * 1024 * 1024;

/// The largest number of packets one frame may be split into.
///
/// At the minimum MTU, two megabytes is about eighteen hundred packets, so this
/// is the same bound expressed in the other unit. Both are checked, because a
/// peer sending two thousand one-byte fragments costs nothing in bytes and
/// plenty in bookkeeping.
pub const MAX_PACKETS_PER_FRAME: usize = 2048;

/// Why a packet or a frame was rejected.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ReassemblyError {
    /// The RTP header did not parse.
    #[error(transparent)]
    Rtp(#[from] RtpError),
    /// A payload type this stream does not carry.
    #[error("payload type {got} is not the {want} this stream carries")]
    WrongPayloadType { got: u8, want: u8 },
    /// A packet from a different stream.
    #[error("SSRC {got:#010x} is not this stream's {want:#010x}")]
    WrongStream { got: u32, want: u32 },
    /// An RFC 6184 mode this link does not write.
    #[error("NAL type {0} is an aggregation or fragmentation mode this link does not use")]
    UnsupportedMode(u8),
    /// A fragment arrived with nothing in it.
    #[error("a fragmentation unit with no payload")]
    EmptyFragment,
    /// The frame would exceed [`MAX_FRAME_BYTES`] or [`MAX_PACKETS_PER_FRAME`].
    #[error("a frame of {what} exceeds the {limit} this accepts")]
    FrameTooLarge { what: String, limit: usize },
}

/// Why a frame was given up on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Abandoned {
    /// A packet was missing and the next frame had already started.
    ///
    /// The common case, and the one a keyframe request answers.
    Incomplete,
    /// The frame's first packet never arrived, so its head is missing.
    TruncatedHead,
    /// The fragments did not form a whole NAL unit.
    BrokenFragments,
    /// It grew past what this accepts.
    TooLarge,
}

impl Abandoned {
    /// Why, for the log and the session's stats.
    #[must_use]
    pub const fn reason(self) -> &'static str {
        match self {
            Self::Incomplete => "a packet was lost and the next frame had started",
            Self::TruncatedHead => "the frame's first packet never arrived",
            Self::BrokenFragments => "the fragments did not form a whole NAL unit",
            Self::TooLarge => "the frame exceeded what this accepts",
        }
    }
}

/// What reassembly produced.
///
/// A list rather than a single value, because one packet can produce two
/// results: the packet that starts a new frame is often the same packet that
/// reveals the previous one will never finish. An earlier version returned one
/// of them and dropped the other on the floor — and the one it dropped was the
/// loss, so the caller never learned to ask for a keyframe and the picture
/// stayed broken until the next scheduled one.
#[derive(Debug, PartialEq, Eq)]
pub enum Reassembled {
    /// A complete frame, in Annex-B framing, ready for a decoder.
    Frame {
        /// The bitstream, with start codes, exactly as an encoder would emit.
        bitstream: Vec<u8>,
        /// The frame's presentation time, at 90 kHz.
        timestamp: u32,
        /// Whether a decoder can start here — derived from the NAL types.
        keyframe: bool,
    },
    /// A frame was given up on. **The caller should request a keyframe.**
    Lost {
        /// The abandoned frame's timestamp.
        timestamp: u32,
        why: Abandoned,
    },
}

/// Running totals, for the stats a session reports.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Stats {
    pub packets: u64,
    pub frames: u64,
    pub abandoned: u64,
    /// Frames given up on, which is the number a keyframe request answers.
    pub keyframes_needed: u64,
    /// Packets that arrived out of order but in time to be used.
    pub reordered: u64,
    /// Packets refused before reassembly: wrong stream, wrong type, malformed.
    pub rejected: u64,
}

/// One frame's packets, waiting to be complete.
#[derive(Debug)]
struct Pending {
    timestamp: u32,
    /// The sequence the first packet seen for this frame carried. Offsets are
    /// relative to it, so the wrap never enters the bookkeeping.
    anchor: u16,
    /// Offset from `anchor` to the RTP payload.
    packets: BTreeMap<i32, Vec<u8>>,
    /// The offset of the packet carrying the marker bit, once it arrives.
    end: Option<i32>,
    /// The sequence this frame should have begun at, when the previous
    /// marker was seen. Captured when the frame starts, because the marker
    /// that arrives *during* this frame describes the next one.
    expected_first: Option<u16>,
    bytes: usize,
}

/// Turns RTP packets back into frames.
#[derive(Debug)]
pub struct Depacketizer {
    ssrc: u32,
    payload_type: u8,
    current: Option<Pending>,
    /// The sequence the *next* new frame should start at: one past the last
    /// marker seen.
    ///
    /// Set from the **marker bit**, not from a frame completing. The marker is
    /// a frame's last packet, so it says where the next frame begins whether
    /// or not the frame it ended was any good — and an earlier version only
    /// recorded it on success, throwing away information it already had. That
    /// left the receiver guessing frame boundaries after every loss, and its
    /// guess emitted frames with slices missing.
    ///
    /// `None` only at the start of a session, and after a marker is itself
    /// lost.
    next_frame_start: Option<u16>,
    /// Whether a keyframe is still needed to make output decodable.
    needs_keyframe: bool,
    stats: Stats,
}

impl Depacketizer {
    /// A depacketizer for one stream.
    #[must_use]
    pub fn new(ssrc: u32) -> Self {
        Self {
            ssrc,
            payload_type: crate::PAYLOAD_TYPE_H264,
            current: None,
            next_frame_start: None,
            needs_keyframe: true,
            stats: Stats::default(),
        }
    }

    /// The stream this accepts.
    #[must_use]
    pub const fn ssrc(&self) -> u32 {
        self.ssrc
    }

    /// Running totals.
    #[must_use]
    pub const fn stats(&self) -> Stats {
        self.stats
    }

    /// Whether a frame is part-assembled.
    #[must_use]
    pub const fn is_assembling(&self) -> bool {
        self.current.is_some()
    }

    /// Whether a keyframe is needed before what comes out is worth decoding.
    ///
    /// Set at the start of a session and after any frame is abandoned, and
    /// cleared by the next keyframe that reassembles.
    ///
    /// # Why this is reported rather than enforced
    ///
    /// A frame this hands back is one it can *prove complete*. Whether it is
    /// *decodable* is a different question — a delta frame references pictures
    /// before it, and after a loss one of those is gone — and answering it is
    /// the session's business, not the parser's.
    ///
    /// Layering it this way is not tidiness. The consumer of these frames is a
    /// large C decoder, and a session that skips frames it knows to be
    /// undecodable feeds it strictly less attacker-shaped input than one that
    /// hands everything over and lets it complain.
    #[must_use]
    pub const fn needs_keyframe(&self) -> bool {
        self.needs_keyframe
    }

    /// Bytes currently held for the frame being assembled.
    ///
    /// Exposed so the fuzz target can assert the bound after every packet
    /// rather than trusting that the checks are in the right places.
    #[must_use]
    pub fn held_bytes(&self) -> usize {
        self.current.as_ref().map_or(0, |p| p.bytes)
    }

    /// Packets currently held for the frame being assembled.
    #[must_use]
    pub fn held_packets(&self) -> usize {
        self.current.as_ref().map_or(0, |p| p.packets.len())
    }

    /// Feed one datagram, appending whatever it produced to `out`.
    ///
    /// Appends rather than clears, so a caller can feed a batch of datagrams
    /// and handle the results together.
    pub fn push(
        &mut self,
        packet: &[u8],
        out: &mut Vec<Reassembled>,
    ) -> Result<(), ReassemblyError> {
        let (header, payload) = match RtpHeader::parse(packet) {
            Ok(v) => v,
            Err(e) => {
                self.stats.rejected = self.stats.rejected.saturating_add(1);
                return Err(e.into());
            }
        };
        if header.ssrc != self.ssrc {
            self.stats.rejected = self.stats.rejected.saturating_add(1);
            return Err(ReassemblyError::WrongStream {
                got: header.ssrc,
                want: self.ssrc,
            });
        }
        if header.payload_type != self.payload_type {
            self.stats.rejected = self.stats.rejected.saturating_add(1);
            return Err(ReassemblyError::WrongPayloadType {
                got: header.payload_type,
                want: self.payload_type,
            });
        }
        // Refuse the modes this link does not write before any of it is
        // stored, so an unsupported packet cannot occupy a frame's budget.
        let kind = payload.first().copied().unwrap_or(0) & 0x1F;
        if matches!(kind, STAP_A | STAP_B | MTAP16 | MTAP24 | FU_B) {
            self.stats.rejected = self.stats.rejected.saturating_add(1);
            return Err(ReassemblyError::UnsupportedMode(kind));
        }
        if kind == FU_A && payload.len() < 3 {
            self.stats.rejected = self.stats.rejected.saturating_add(1);
            return Err(ReassemblyError::EmptyFragment);
        }

        // A packet for a frame that has already finished is too late to be
        // useful. Dropping it is not loss — the loss was whatever made that
        // frame finish without it.
        if let Some(p) = self
            .current
            .as_ref()
            .filter(|p| p.timestamp != header.timestamp)
        {
            {
                if seq_distance_ts(p.timestamp, header.timestamp) < 0 {
                    self.stats.rejected = self.stats.rejected.saturating_add(1);
                    return Ok(());
                }
                // A newer frame has started, so the one in hand will get
                // nothing more. This is the moment it can be judged
                // permissively: whatever arrived is all there is.
                match self.try_complete(Bound::FrameEnded) {
                    Some(done) => out.push(done),
                    None => out.push(self.abandon(Abandoned::Incomplete)),
                }
            }
        }
        self.stats.packets = self.stats.packets.saturating_add(1);

        let expected_first = self.next_frame_start;
        let pending = self.current.get_or_insert_with(|| Pending {
            timestamp: header.timestamp,
            anchor: header.sequence,
            packets: BTreeMap::new(),
            end: None,
            expected_first,
            bytes: 0,
        });

        if pending.packets.len() >= MAX_PACKETS_PER_FRAME {
            let _ = self.abandon(Abandoned::TooLarge);
            self.stats.rejected = self.stats.rejected.saturating_add(1);
            return Err(ReassemblyError::FrameTooLarge {
                what: format!("{} packets", MAX_PACKETS_PER_FRAME.saturating_add(1)),
                limit: MAX_PACKETS_PER_FRAME,
            });
        }
        if pending.bytes.saturating_add(payload.len()) > MAX_FRAME_BYTES {
            let held = pending.bytes.saturating_add(payload.len());
            let _ = self.abandon(Abandoned::TooLarge);
            self.stats.rejected = self.stats.rejected.saturating_add(1);
            return Err(ReassemblyError::FrameTooLarge {
                what: format!("{held} bytes"),
                limit: MAX_FRAME_BYTES,
            });
        }

        let offset = seq_distance(pending.anchor, header.sequence);
        if header.marker {
            pending.end = Some(offset);
        }
        // The marker bounds the *next* frame, and is recorded here rather
        // than when this frame completes: a marker says where the following
        // frame begins whether or not the one it ended was any good.
        if header.marker {
            self.next_frame_start = Some(header.sequence.wrapping_add(1));
        }
        let latest = pending.packets.keys().next_back().copied();
        if pending.packets.insert(offset, payload.to_vec()).is_none() {
            pending.bytes = pending.bytes.saturating_add(payload.len());
        }
        // Arriving before a packet already held means the network reordered
        // them, which is normal and worth counting apart from loss.
        if latest.is_some_and(|l| offset < l) {
            self.stats.reordered = self.stats.reordered.saturating_add(1);
        }

        if let Some(done) = self.try_complete(Bound::MoreMayCome) {
            out.push(done);
        }
        Ok(())
    }

    /// Decide the frame in hand now, without waiting for the next one.
    ///
    /// Returns the frame if what arrived is enough, the loss if it is not, and
    /// nothing if there was no frame in hand.
    ///
    /// Two callers need this. One is the end of a stream, where there is no
    /// next frame to bound the last one. The other is a jitter buffer: this
    /// reassembler is deliberately correct rather than eager — a frame whose
    /// start is not yet known waits — and *how long* to wait is a latency
    /// decision that belongs with the buffer rather than in here.
    pub fn flush(&mut self) -> Option<Reassembled> {
        self.current.as_ref()?;
        match self.try_complete(Bound::FrameEnded) {
            Some(done) => Some(done),
            None => Some(self.abandon(Abandoned::Incomplete)),
        }
    }

    /// Give up on the frame in hand.
    fn abandon(&mut self, why: Abandoned) -> Reassembled {
        let timestamp = self.current.as_ref().map_or(0, |p| p.timestamp);
        self.current = None;
        self.stats.abandoned = self.stats.abandoned.saturating_add(1);
        self.stats.keyframes_needed = self.stats.keyframes_needed.saturating_add(1);
        self.needs_keyframe = true;
        // `expected_start` is deliberately left alone. It came from a marker
        // bit, and a marker still says where the next frame begins even though
        // the frame it ended is being thrown away.
        Reassembled::Lost { timestamp, why }
    }

    /// Emit the frame if everything it needs has arrived.
    ///
    /// # The two questions, and why one of them cannot be answered early
    ///
    /// "Is the run from the first held packet to the marked last one
    /// complete?" is answerable at any time. "Does that run *start where the
    /// frame started*?" is not, unless the previous frame's marker was seen —
    /// then it is known exactly, and a frame is emitted the instant its last
    /// packet lands.
    ///
    /// Without it — the first frame of a session, and the frame after a loss —
    /// there is no way to tell a head that has not arrived from one that never
    /// will. The fallback used to be "the first held packet looks like the
    /// beginning of a NAL unit", and **that is a different question**: on a
    /// keyframe delivered back to front, the IDR's first fragment satisfies it
    /// while the SPS and PPS are still in flight, and the frame is emitted
    /// without them. openh264 rejected the result, which is how it was found.
    ///
    /// So when the start is unknown, the frame waits for the next one to
    /// begin, and is judged then. That costs one frame of latency exactly
    /// twice: at the start of a session, and while recovering from loss.
    fn try_complete(&mut self, bound: Bound) -> Option<Reassembled> {
        let pending = self.current.as_ref()?;
        let end = pending.end?;
        let &first = pending.packets.keys().next()?;
        // Every offset from the first held packet to the marked last one must
        // be present, or something between them is still missing.
        let want = i64::from(end)
            .checked_sub(i64::from(first))?
            .checked_add(1)?;
        if want <= 0 || pending.packets.len() as i64 != want {
            return None;
        }
        // And the run has to begin where the frame began.
        let starts_here = match (pending.expected_first, bound) {
            // Known exactly: emit the moment the last packet lands.
            (Some(expected), _) => pending.anchor.wrapping_add(first as u16) == expected,
            // Unknown, and more packets may still arrive: wait.
            (None, Bound::MoreMayCome) => false,
            // Unknown, and the frame is over. There is no way to prove this
            // frame is whole — so the only frames accepted here are the ones
            // that prove it themselves: a keyframe, beginning with its
            // sequence parameter set.
            //
            // The weaker test used to be "the first held packet begins a NAL
            // unit", and under real loss that emitted frames with slices
            // missing: a frame whose first two slices were lost still has a
            // contiguous run to its marker, and its third slice still begins a
            // unit. openh264 is configured to refuse rather than conceal, so
            // it rejected them — but a truncated frame should never have
            // reached it.
            //
            // Discarding until a keyframe is also what the protocol already
            // does about loss: the receiver asks for one, and the periodic
            // keyframe arrives even if the ask is ignored.
            (None, Bound::FrameEnded) => begins_a_keyframe(
                pending
                    .packets
                    .get(&first)
                    .map(Vec::as_slice)
                    .unwrap_or_default(),
            ),
        };
        if !starts_here {
            return None;
        }

        let pending = self.current.take()?;
        let timestamp = pending.timestamp;
        match assemble(&pending) {
            // A frame accepted without a known start has to be a keyframe;
            // anything else cannot be shown to be whole. Checked on the
            // assembled bitstream rather than on the first packet alone, so a
            // keyframe missing its picture is caught as well as one missing
            // its parameter set.
            Some(bitstream)
                if bound == Bound::FrameEnded
                    && pending.expected_first.is_none()
                    && !is_keyframe(&bitstream) =>
            {
                self.stats.abandoned = self.stats.abandoned.saturating_add(1);
                self.stats.keyframes_needed = self.stats.keyframes_needed.saturating_add(1);
                Some(Reassembled::Lost {
                    timestamp,
                    why: Abandoned::TruncatedHead,
                })
            }
            Some(bitstream) => {
                self.stats.frames = self.stats.frames.saturating_add(1);
                let keyframe = is_keyframe(&bitstream);
                if keyframe {
                    self.needs_keyframe = false;
                }
                Some(Reassembled::Frame {
                    bitstream,
                    timestamp,
                    keyframe,
                })
            }
            None => {
                self.stats.abandoned = self.stats.abandoned.saturating_add(1);
                self.stats.keyframes_needed = self.stats.keyframes_needed.saturating_add(1);
                self.needs_keyframe = true;
                Some(Reassembled::Lost {
                    timestamp,
                    why: Abandoned::BrokenFragments,
                })
            }
        }
    }
}

/// Whether more packets for the frame in hand could still arrive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Bound {
    /// Mid-frame: a hole may yet be filled by a packet in flight.
    MoreMayCome,
    /// A newer frame has started, so nothing more is coming for this one.
    FrameEnded,
}

/// The NAL unit type a payload begins, if it begins one at all.
///
/// `None` for a continuation fragment, which has no header of its own.
fn begins_unit_of_kind(payload: &[u8]) -> Option<u8> {
    let first = payload.first().copied()?;
    let kind = first & 0x1F;
    if kind == FU_A {
        let fu = payload.get(1).copied()?;
        // The start bit says this is the first fragment of its unit, and the
        // original type lives in the FU header.
        return (fu & 0x80 != 0).then_some(fu & 0x1F);
    }
    // A single NAL unit packet is by definition a whole unit.
    (1..=23).contains(&kind).then_some(kind)
}

/// Whether a payload is the first packet of a *keyframe*.
///
/// The parameter set comes first in a keyframe, and it is the one NAL type
/// that can only be at the beginning of one. That makes it the only
/// self-evident frame boundary available to a receiver that has lost track of
/// where frames start.
fn begins_a_keyframe(payload: &[u8]) -> bool {
    begins_unit_of_kind(payload) == Some(crate::annexb::nal::SPS)
}

/// Turn a complete frame's payloads into an Annex-B bitstream.
///
/// `None` when the fragments do not form whole units — a fragmented unit whose
/// start or end is missing, or a continuation with nothing before it.
fn assemble(pending: &Pending) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(pending.bytes.saturating_add(64));
    // Whether a unit is currently being rebuilt from fragments.
    let mut building = false;

    for payload in pending.packets.values() {
        let first = payload.first().copied()?;
        let kind = first & 0x1F;
        if kind == FU_A {
            let fu = payload.get(1).copied()?;
            let body = payload.get(2..)?;
            if body.is_empty() {
                return None;
            }
            let start = fu & 0x80 != 0;
            let end = fu & 0x40 != 0;
            if start {
                if building {
                    // A new unit began before the last one ended.
                    return None;
                }
                out.extend_from_slice(&START_CODE);
                // The NAL header is rebuilt from the two: the forbidden bit
                // and importance from the indicator, the type from the FU
                // header. Neither packet carries the byte itself.
                out.push((first & 0xE0) | (fu & 0x1F));
                building = true;
            } else if !building {
                // A continuation with no beginning.
                return None;
            }
            out.extend_from_slice(body);
            if end {
                if !building {
                    return None;
                }
                building = false;
            }
        } else {
            if building {
                // A whole unit in the middle of a fragmented one.
                return None;
            }
            out.extend_from_slice(&START_CODE);
            out.extend_from_slice(payload);
        }
    }
    // A fragmented unit whose end never came is not a frame.
    if building {
        return None;
    }
    (!out.is_empty()).then_some(out)
}

/// Distance between two RTP timestamps, wrap-aware.
///
/// Timestamps wrap at 2^32, which at 90 kHz is thirteen hours — long enough
/// that a session can reach it, and short enough that treating them as
/// unsigned would make a frame after the wrap look thirteen hours stale.
const fn seq_distance_ts(earlier: u32, later: u32) -> i64 {
    (later.wrapping_sub(earlier) as i32) as i64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::packetize::Packetizer;

    const SSRC: u32 = 0x1234_5678;
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

    fn send(p: &mut Packetizer, bitstream: &[u8], ts: u32) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        p.packetize(bitstream, ts, &mut out).expect("packetizes");
        out
    }

    /// Feed every packet and return whatever came out.
    fn feed(d: &mut Depacketizer, packets: &[Vec<u8>]) -> Vec<Reassembled> {
        let mut out = Vec::new();
        for packet in packets {
            d.push(packet, &mut out)
                .unwrap_or_else(|e| panic!("a packet this crate wrote was refused: {e}"));
        }
        out
    }

    /// Feed every packet, then decide whatever is still in hand.
    ///
    /// A frame whose start is not yet known waits for the next frame to bound
    /// it, so a test sending one frame in isolation has to say that there is
    /// no next frame coming — which is what a stream ending, or a jitter
    /// buffer's deadline, says.
    fn feed_and_flush(d: &mut Depacketizer, packets: &[Vec<u8>]) -> Vec<Reassembled> {
        let mut out = feed(d, packets);
        out.extend(d.flush());
        out
    }

    /// A realistic keyframe: parameter sets then a picture, which is what an
    /// encoder actually emits and what a session opens with.
    fn keyframe(slice: usize) -> Vec<u8> {
        stream(&[
            &unit(crate::annexb::nal::SPS, 3, 12),
            &unit(crate::annexb::nal::PPS, 3, 4),
            &unit(crate::annexb::nal::IDR, 3, slice),
        ])
    }

    /// Get the receiver in sync, the way a session does: one keyframe.
    ///
    /// A session's first frame has no previous marker to say where it begins,
    /// so the only frame the receiver can prove is whole is a keyframe with
    /// its parameter set. After that the marker bits carry the boundaries and
    /// every frame is emitted the instant its last packet lands.
    fn sync(p: &mut Packetizer, d: &mut Depacketizer, ts: u32) {
        let first = send(p, &keyframe(300), ts);
        let got = feed_and_flush(d, &first);
        assert!(
            matches!(got.first(), Some(Reassembled::Frame { keyframe: true, .. })),
            "the opening keyframe did not establish sync: {got:?}"
        );
    }

    /// Feed one packet and return whatever came out.
    fn one(d: &mut Depacketizer, packet: &[u8]) -> Vec<Reassembled> {
        let mut out = Vec::new();
        d.push(packet, &mut out).expect("accepted");
        out
    }

    /// **The round trip.** What the packetizer wrote, the depacketizer must
    /// give back byte for byte — not merely something equivalent.
    #[test]
    fn a_frame_survives_the_round_trip_exactly() {
        for units in [
            vec![unit(5, 3, 100)],
            vec![unit(7, 3, 20), unit(8, 3, 8), unit(5, 3, 500)],
            vec![unit(1, 2, 3000)],
            vec![unit(7, 3, 12), unit(5, 3, 40_000)],
            vec![unit(1, 0, 1)],
        ] {
            let refs: Vec<&[u8]> = units.iter().map(Vec::as_slice).collect();
            let s = stream(&refs);
            let mut p = Packetizer::new(SSRC, MTU).expect("valid");
            let mut d = Depacketizer::new(SSRC);
            sync(&mut p, &mut d, 3000);
            let packets = send(&mut p, &s, 9000);
            let got = feed(&mut d, &packets);
            assert_eq!(got.len(), 1, "{} packets produced {got:?}", packets.len());
            match got.into_iter().next() {
                Some(Reassembled::Frame {
                    bitstream,
                    timestamp,
                    keyframe,
                }) => {
                    assert_eq!(bitstream, s, "the bitstream changed in transit");
                    assert_eq!(timestamp, 9000);
                    assert_eq!(keyframe, crate::annexb::is_keyframe(&s));
                }
                other => panic!("expected a frame, got {other:?}"),
            }
        }
    }

    #[test]
    fn many_frames_in_a_row_all_arrive() {
        let mut p = Packetizer::new(SSRC, MTU).expect("valid");
        let mut d = Depacketizer::new(SSRC);
        let key = stream(&[&unit(7, 3, 20), &unit(8, 3, 8), &unit(5, 3, 4000)]);
        let delta = stream(&[&unit(1, 2, 600)]);

        // Sent, in order, so what comes back can be compared against it.
        let mut expected: Vec<(u32, Vec<u8>, bool)> = Vec::new();
        let mut got: Vec<Reassembled> = Vec::new();
        for i in 0..30u32 {
            let s = if i % 10 == 0 { &key } else { &delta };
            expected.push((i * 3000, s.clone(), i % 10 == 0));
            let packets = send(&mut p, s, i * 3000);
            got.extend(feed(&mut d, &packets));
        }
        // The last frame has no next frame to bound it.
        got.extend(d.flush());

        // **The first frame lags by one**, because a session's first frame has
        // no previous marker to say where it starts, so it waits for the next
        // frame to begin. Every frame after that is emitted the instant its
        // last packet lands. The order and the contents must still be exact.
        assert_eq!(got.len(), expected.len(), "{} frames came back", got.len());
        for (r, (ts, bitstream, keyframe)) in got.iter().zip(expected.iter()) {
            match r {
                Reassembled::Frame {
                    bitstream: b,
                    timestamp,
                    keyframe: k,
                } => {
                    assert_eq!(timestamp, ts);
                    assert_eq!(b, bitstream);
                    assert_eq!(k, keyframe);
                }
                other => panic!("expected the frame at {ts}, got {other:?}"),
            }
        }
        assert_eq!(d.stats().frames, 30);
        assert_eq!(d.stats().abandoned, 0);
        assert_eq!(d.stats().keyframes_needed, 0);
    }

    /// **Reordering is not loss.** A receiver that abandoned a frame on the
    /// first out-of-order packet would give up constantly on a healthy link.
    #[test]
    fn a_reordered_frame_still_arrives() {
        let s = stream(&[&unit(5, 3, 6000)]);

        // Every rotation of the packet order, which covers arriving last-first.
        let probe = {
            let mut p = Packetizer::new(SSRC, MTU).expect("valid");
            send(&mut p, &s, 12_000).len()
        };
        assert!(probe >= 6, "need several packets to reorder");
        for shift in 0..probe {
            let mut p = Packetizer::new(SSRC, MTU).expect("valid");
            let mut d = Depacketizer::new(SSRC);
            sync(&mut p, &mut d, 9000);
            let mut order: Vec<Vec<u8>> = send(&mut p, &s, 12_000);
            order.rotate_left(shift);
            let got = feed(&mut d, &order);
            assert_eq!(
                got.len(),
                1,
                "rotation by {shift} produced {} results",
                got.len()
            );
            match got.into_iter().next() {
                Some(Reassembled::Frame { bitstream, .. }) => {
                    assert_eq!(bitstream, s, "rotation by {shift} corrupted the frame");
                }
                other => panic!("rotation by {shift}: {other:?}"),
            }
        }
        // And fully reversed.
        let mut p = Packetizer::new(SSRC, MTU).expect("valid");
        let mut d = Depacketizer::new(SSRC);
        sync(&mut p, &mut d, 9000);
        let mut reversed = send(&mut p, &s, 12_000);
        reversed.reverse();
        let got = feed(&mut d, &reversed);
        assert!(
            matches!(got.first(), Some(Reassembled::Frame { .. })),
            "reversed: {got:?}"
        );
        assert!(d.stats().reordered > 0, "reordering was not counted");
    }

    /// **The decision this module is named for.** A lost fragment means the
    /// frame is given up on, and the caller is told so it can ask for a
    /// keyframe — not held in the hope the packet is late.
    #[test]
    fn a_frame_missing_a_fragment_is_abandoned_when_the_next_one_starts() {
        let big = stream(&[&unit(5, 3, 6000)]);
        let next = stream(&[&unit(1, 2, 200)]);
        let mut p = Packetizer::new(SSRC, MTU).expect("valid");
        let mut d = Depacketizer::new(SSRC);

        let first = send(&mut p, &big, 0);
        assert!(first.len() >= 3);
        // Drop the middle packet.
        for (i, packet) in first.iter().enumerate() {
            if i == 1 {
                continue;
            }
            assert!(
                one(&mut d, packet).is_empty(),
                "packet {i} should not complete a frame with a hole in it"
            );
        }

        // The next frame's first packet is what reveals the loss — and it is
        // also the packet that may complete the *next* frame, so both results
        // have to come back. Reporting one and dropping the other is how the
        // caller stops asking for keyframes it needs.
        let second = send(&mut p, &next, 3000);
        let results = feed(&mut d, &second);
        assert!(
            results.contains(&Reassembled::Lost {
                timestamp: 0,
                why: Abandoned::Incomplete
            }),
            "the incomplete frame must be reported so a keyframe can be asked for: {results:?}"
        );
        // And the frame that revealed the loss arrives in the same breath.
        // The abandoned frame's *marker* did arrive, and a marker says where
        // the next frame begins whether or not the frame it ended survived —
        // so no extra latency is paid for the recovery.
        assert!(
            results.iter().any(|r| matches!(
                r,
                Reassembled::Frame {
                    timestamp: 3000,
                    ..
                }
            )),
            "the frame after the loss must arrive immediately: {results:?}"
        );
        assert_eq!(d.stats().abandoned, 1);
        assert_eq!(d.stats().keyframes_needed, 1);
    }

    /// A frame whose first packet never arrived has a head missing, and the
    /// rest of it is not a frame — it is the tail of one.
    #[test]
    fn a_frame_missing_its_head_is_abandoned_rather_than_decoded() {
        let s = stream(&[&unit(5, 3, 6000)]);
        let mut p = Packetizer::new(SSRC, MTU).expect("valid");
        let mut d = Depacketizer::new(SSRC);
        let packets = send(&mut p, &s, 0);
        assert!(packets.len() >= 3);
        // Everything except the first fragment.
        let held = feed(&mut d, packets.get(1..).expect("a tail"));
        assert!(
            held.is_empty(),
            "a head that has not arrived is indistinguishable from one that \
             never will, so this must wait rather than guess: {held:?}"
        );
        assert!(d.is_assembling());

        // The next frame starting is what settles it.
        let next = send(&mut p, &stream(&[&unit(1, 2, 100)]), 3000);
        let results = feed(&mut d, &next);
        assert!(
            results.iter().any(|r| matches!(
                r,
                Reassembled::Lost {
                    why: Abandoned::Incomplete | Abandoned::TruncatedHead,
                    ..
                }
            )),
            "a tail without a head must never be handed to a decoder: {results:?}"
        );
        assert!(
            !results
                .iter()
                .any(|r| matches!(r, Reassembled::Frame { timestamp: 0, .. })),
            "the truncated frame must not be emitted: {results:?}"
        );
    }

    /// Loss between frames must not let the *next* frame be mistaken for
    /// complete when part of it is missing too.
    #[test]
    fn loss_does_not_leak_into_the_frame_after_it() {
        let a = stream(&[&unit(5, 3, 4000)]);
        let b = stream(&[&unit(1, 2, 4000)]);
        let c = stream(&[&unit(1, 2, 300)]);
        let mut p = Packetizer::new(SSRC, MTU).expect("valid");
        let mut d = Depacketizer::new(SSRC);

        // Frame A complete, so the expected start is known.
        let pa = send(&mut p, &a, 0);
        // The first frame of a session waits for the next one to bound it.
        assert!(feed(&mut d, &pa).is_empty());

        // Frame B loses its first packet.
        let pb = send(&mut p, &b, 3000);
        let _ = feed(&mut d, pb.get(1..).expect("a tail"));
        // Frame C is whole and must arrive, which it can only do if the
        // depacketizer recovered rather than staying confused.
        let pc = send(&mut p, &c, 6000);
        let mut got = feed(&mut d, &pc);
        got.extend(d.flush());
        assert!(
            got.iter().any(|r| matches!(
                r,
                Reassembled::Frame { bitstream, .. } if bitstream == &c
            )),
            "the frame after the loss did not arrive: {got:?}"
        );
    }

    /// **Joining a session mid-stream.** A receiver that starts listening
    /// between keyframes cannot prove any frame is whole until it has either a
    /// keyframe or a marker to anchor on, and the frames it can prove are
    /// still undecodable until a keyframe arrives.
    ///
    /// Both halves are here. The session's *first* frame must be a keyframe or
    /// it is refused — nothing else can be shown to be complete. After that,
    /// marker bits carry the boundaries, so complete delta frames are handed
    /// back; whether they are worth decoding is what `needs_keyframe` is for.
    #[test]
    fn a_receiver_joining_mid_stream_says_it_needs_a_keyframe() {
        let mut p = Packetizer::new(SSRC, MTU).expect("valid");
        let mut d = Depacketizer::new(SSRC);
        let delta = stream(&[&unit(crate::annexb::nal::SLICE, 2, 900)]);
        assert!(
            d.needs_keyframe(),
            "a fresh receiver has no reference picture"
        );

        // The very first frame is a delta, and cannot be proven whole.
        let first = feed(&mut d, &send(&mut p, &delta, 0));
        assert!(
            !first.iter().any(|r| matches!(r, Reassembled::Frame { .. })),
            "the first frame of a session must be a keyframe or be refused: {first:?}"
        );

        // From its marker onwards the boundaries are known, so later delta
        // frames *are* complete and are handed back — still flagged.
        let second = feed(&mut d, &send(&mut p, &delta, 3000));
        assert!(
            second.iter().any(|r| matches!(
                r,
                Reassembled::Frame {
                    timestamp: 3000,
                    ..
                }
            )),
            "a provably complete frame must be handed back: {second:?}"
        );
        assert!(
            d.needs_keyframe(),
            "delta frames must not clear the need for a keyframe"
        );

        // And a keyframe clears it.
        let key = feed(&mut d, &send(&mut p, &keyframe(900), 6000));
        assert!(
            key.iter().any(|r| matches!(
                r,
                Reassembled::Frame {
                    keyframe: true,
                    timestamp: 6000,
                    ..
                }
            )),
            "the keyframe did not arrive: {key:?}"
        );
        assert!(!d.needs_keyframe(), "a keyframe must clear the flag");
    }

    /// And a loss sets it again, which is what makes the request happen.
    #[test]
    fn a_loss_sets_the_keyframe_flag_again() {
        let mut p = Packetizer::new(SSRC, MTU).expect("valid");
        let mut d = Depacketizer::new(SSRC);
        sync(&mut p, &mut d, 0);
        assert!(!d.needs_keyframe());

        // A frame with a hole, then the frame after it to reveal the loss.
        let holed = send(&mut p, &stream(&[&unit(5, 3, 6000)]), 3000);
        for (i, packet) in holed.iter().enumerate() {
            if i == 1 {
                continue;
            }
            let _ = one(&mut d, packet);
        }
        let _ = feed(&mut d, &send(&mut p, &stream(&[&unit(1, 2, 100)]), 6000));
        assert!(
            d.needs_keyframe(),
            "a lost frame must leave the receiver asking for a keyframe"
        );
    }

    /// A keyframe whose parameter set was lost is not a starting point either,
    /// however much of the rest of it arrived.
    #[test]
    fn a_keyframe_missing_its_parameter_set_is_not_a_starting_point() {
        let mut p = Packetizer::new(SSRC, MTU).expect("valid");
        let mut d = Depacketizer::new(SSRC);
        let packets = send(&mut p, &keyframe(900), 0);
        assert!(packets.len() >= 3, "SPS, PPS and a picture");
        // Everything but the SPS.
        let got = feed_and_flush(&mut d, packets.get(1..).expect("a tail"));
        assert!(
            !got.iter().any(|r| matches!(r, Reassembled::Frame { .. })),
            "a keyframe without its parameter set was emitted: {got:?}"
        );
    }

    #[test]
    fn a_packet_from_another_stream_is_refused() {
        let s = stream(&[&unit(5, 3, 40)]);
        let mut p = Packetizer::new(0xAAAA_AAAA, MTU).expect("valid");
        let mut d = Depacketizer::new(SSRC);
        let packets = send(&mut p, &s, 0);
        let mut out = Vec::new();
        assert_eq!(
            d.push(packets.first().expect("a packet"), &mut out),
            Err(ReassemblyError::WrongStream {
                got: 0xAAAA_AAAA,
                want: SSRC
            })
        );
        assert!(out.is_empty());
        assert_eq!(d.stats().rejected, 1);
        assert_eq!(d.stats().packets, 0, "a refused packet is not a packet");
    }

    #[test]
    fn a_packet_of_another_payload_type_is_refused() {
        let s = stream(&[&unit(5, 3, 40)]);
        let mut p = Packetizer::new(SSRC, MTU).expect("valid");
        let mut d = Depacketizer::new(SSRC);
        let mut packets = send(&mut p, &s, 0);
        if let Some(b) = packets.first_mut().and_then(|p| p.get_mut(1)) {
            *b = (*b & 0x80) | 97;
        }
        let mut out = Vec::new();
        assert_eq!(
            d.push(packets.first().expect("a packet"), &mut out),
            Err(ReassemblyError::WrongPayloadType { got: 97, want: 96 })
        );
    }

    /// The aggregation modes this link never writes are refused by name, so an
    /// unsupported packet cannot occupy a frame's budget on the way to being
    /// rejected.
    #[test]
    fn aggregation_modes_are_refused() {
        let mut d = Depacketizer::new(SSRC);
        for kind in [STAP_A, STAP_B, MTAP16, MTAP24, FU_B] {
            let mut packet = vec![0u8; RtpHeaderLen::LEN + 4];
            RtpHeader {
                marker: true,
                payload_type: 96,
                sequence: 0,
                timestamp: 0,
                ssrc: SSRC,
            }
            .write(&mut packet)
            .expect("writes");
            if let Some(b) = packet.get_mut(RtpHeaderLen::LEN) {
                *b = kind;
            }
            let mut out = Vec::new();
            assert_eq!(
                d.push(&packet, &mut out),
                Err(ReassemblyError::UnsupportedMode(kind)),
                "NAL type {kind} was accepted"
            );
        }
    }

    struct RtpHeaderLen;
    impl RtpHeaderLen {
        const LEN: usize = crate::rtp::RTP_HEADER_LEN;
    }

    /// **The bound a peer cannot spend past.** Fragments that never complete
    /// must cost one frame's worth of memory, not a gigabyte.
    #[test]
    fn a_frame_that_grows_too_large_is_refused_rather_than_buffered() {
        let mut d = Depacketizer::new(SSRC);
        let body = vec![0xABu8; MTU - RtpHeaderLen::LEN - 2];
        let mut seq = 0u16;
        let mut accepted = 0usize;
        // Fragments of one enormous unit, none of them ever ending it.
        loop {
            let mut packet = vec![0u8; RtpHeaderLen::LEN + 2 + body.len()];
            RtpHeader {
                marker: false,
                payload_type: 96,
                sequence: seq,
                timestamp: 0,
                ssrc: SSRC,
            }
            .write(&mut packet)
            .expect("writes");
            if let Some(b) = packet.get_mut(RtpHeaderLen::LEN) {
                *b = 0x60 | FU_A;
            }
            if let Some(b) = packet.get_mut(RtpHeaderLen::LEN + 1) {
                *b = if seq == 0 { 0x80 | 5 } else { 5 };
            }
            if let Some(dst) = packet.get_mut(RtpHeaderLen::LEN + 2..) {
                dst.copy_from_slice(&body);
            }
            let mut out = Vec::new();
            match d.push(&packet, &mut out) {
                Ok(()) => accepted += 1,
                Err(ReassemblyError::FrameTooLarge { .. }) => break,
                Err(e) => panic!("unexpected {e}"),
            }
            seq = seq.wrapping_add(1);
            assert!(
                accepted <= MAX_PACKETS_PER_FRAME + 1,
                "the packet cap did not bite after {accepted} packets"
            );
        }
        // One of the two caps stopped it, and the held bytes are bounded.
        assert!(
            accepted * body.len() <= MAX_FRAME_BYTES + body.len(),
            "{accepted} packets of {} bytes were buffered",
            body.len()
        );
    }

    #[test]
    fn fragments_that_do_not_form_a_unit_are_not_handed_on() {
        // A frame whose only packet is a *continuation* fragment: it has no
        // start bit, so there is no NAL header to rebuild.
        let mut d = Depacketizer::new(SSRC);
        let mut packet = vec![0u8; RtpHeaderLen::LEN + 5];
        RtpHeader {
            marker: true,
            payload_type: 96,
            sequence: 0,
            timestamp: 0,
            ssrc: SSRC,
        }
        .write(&mut packet)
        .expect("writes");
        if let Some(b) = packet.get_mut(RtpHeaderLen::LEN) {
            *b = 0x60 | FU_A;
        }
        // No start bit, no end bit.
        if let Some(b) = packet.get_mut(RtpHeaderLen::LEN + 1) {
            *b = 5;
        }
        let r = one(&mut d, &packet);
        assert!(
            r.is_empty(),
            "a lone continuation fragment must wait, not be decoded: {r:?}"
        );
    }

    #[test]
    fn a_fragmentation_unit_with_no_body_is_refused() {
        let mut d = Depacketizer::new(SSRC);
        for extra in [0usize, 1] {
            let mut packet = vec![0u8; RtpHeaderLen::LEN + 1 + extra];
            RtpHeader {
                marker: true,
                payload_type: 96,
                sequence: 0,
                timestamp: 0,
                ssrc: SSRC,
            }
            .write(&mut packet)
            .expect("writes");
            if let Some(b) = packet.get_mut(RtpHeaderLen::LEN) {
                *b = 0x60 | FU_A;
            }
            let mut out = Vec::new();
            assert_eq!(
                d.push(&packet, &mut out),
                Err(ReassemblyError::EmptyFragment)
            );
        }
    }

    /// The sequence space wraps, and a session runs long enough to reach it.
    #[test]
    fn frames_keep_arriving_across_the_sequence_wrap() {
        let s = stream(&[&unit(1, 2, 2500)]);
        let mut p = Packetizer::new(SSRC, MTU).expect("valid");
        // Start just short of the wrap.
        for _ in 0..u16::MAX - 3 {
            let mut sink = Vec::new();
            let _ = p.packetize(&stream(&[&unit(1, 2, 1)]), 0, &mut sink);
        }
        let mut d = Depacketizer::new(SSRC);
        let mut frames = 0;
        for i in 0..20u32 {
            let packets = send(&mut p, &s, i * 3000);
            for r in feed(&mut d, &packets) {
                match r {
                    Reassembled::Frame { bitstream, .. } => {
                        assert_eq!(bitstream, s);
                        frames += 1;
                    }
                    // The first frame after the wrap may be reported lost if
                    // it straddles it; what must not happen is every frame
                    // after the wrap being lost.
                    Reassembled::Lost { .. } => {}
                }
            }
        }
        assert!(
            frames >= 18,
            "only {frames} of 20 frames survived the sequence wrap"
        );
    }

    /// Anything at all, without panicking. This is the parser between the
    /// network and a large C decoder.
    #[test]
    fn arbitrary_packets_never_panic() {
        let mut state = 0x9E37_79B9_7F4A_7C15u64;
        let mut byte = || {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
            (state >> 33) as u8
        };
        let mut d = Depacketizer::new(SSRC);
        for len in 0..80usize {
            for _ in 0..60 {
                let mut packet: Vec<u8> = (0..len).map(|_| byte()).collect();
                // Half the time make it look like ours, so it gets past the
                // early checks and into reassembly.
                if len >= crate::rtp::RTP_HEADER_LEN && byte() % 2 == 0 {
                    if let Some(b) = packet.first_mut() {
                        *b = 0x80;
                    }
                    if let Some(b) = packet.get_mut(1) {
                        *b = 96 | (*b & 0x80);
                    }
                    if let Some(s) = packet.get_mut(8..12) {
                        s.copy_from_slice(&SSRC.to_be_bytes());
                    }
                }
                let mut out = Vec::new();
                let _ = d.push(&packet, &mut out);
                for r in out {
                    if let Reassembled::Frame { bitstream, .. } = r {
                        assert!(bitstream.len() <= MAX_FRAME_BYTES + 64);
                    }
                }
            }
        }
    }
}
