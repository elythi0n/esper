//! Media on the wire.
//!
//! # Why this is real RTP and not a private format
//!
//! A private header would be smaller and easier. It would also make one of
//! this project's decisions false: the viewer is a WebRTC surface, and the
//! engine re-offers the session's own media to it over a loopback
//! PeerConnection. That was chosen on the basis that it is a *repacketize,
//! not a transcode* — and that is only true if what arrives off the network is
//! already what a PeerConnection eats.
//!
//! So the packets are RFC 3550 RTP carrying RFC 6184 H.264, which is what
//! every browser's decoder is fed. The cost is twelve bytes a packet against a
//! payload of a thousand-odd. The benefit is that the browser viewer stays
//! possible and the desktop app's video path is a forwarding step rather than
//! a second implementation of everything here.
//!
//! # Where Esper's own fields went
//!
//! The protocol says a media frame carries sequence, timestamp, keyframe flag
//! and monitor id. Three of those are RTP's own; the fourth is not, and rather
//! than bolt on a header extension:
//!
//! - **Sequence and timestamp** are the RTP header's.
//! - **The keyframe flag is derived**, from the NAL unit types in the frame,
//!   exactly as a WebRTC receiver derives it. A flag a peer sets is a flag a
//!   peer can lie about; the bitstream cannot disagree with itself.
//! - **The monitor id is the SSRC.** RTP's synchronisation source is the
//!   identity of a stream, which is what a monitor is here — one screen, one
//!   timeline, its own sequence space. Two monitors are two streams, which is
//!   what they are.
//!
//! # This is the second parser attacker-controlled bytes reach
//!
//! The first is the control framing. This one is worse, because what it feeds
//! is a large C decoder. So the same standard applies and then some: every
//! length is checked before anything is allocated, reassembly is bounded so a
//! peer cannot make the receiver hold memory by sending fragments that never
//! complete, and there are no panicking constructs anywhere in the path.

pub mod annexb;
pub mod depacketize;
pub mod packetize;
pub mod rtp;

pub use annexb::{NalUnit, NalUnits};

pub use depacketize::{
    Abandoned, Depacketizer, MAX_FRAME_BYTES, MAX_PACKETS_PER_FRAME, Reassembled, ReassemblyError,
};

/// The reassembly caps, under the names the fuzz target uses.
///
/// Aliases rather than a rename: the fuzz crate is outside this workspace and
/// its target reads better saying what it is asserting about.
pub use depacketize::MAX_FRAME_BYTES as MAX_FRAME_BYTES_EXPORT;
pub use depacketize::MAX_PACKETS_PER_FRAME as MAX_PACKETS_EXPORT;
pub use packetize::{MIN_MTU, PacketizeError, Packetizer, to_annexb};
pub use rtp::{RTP_HEADER_LEN, RtpError, RtpHeader};

/// The payload type for H.264, as WebRTC's default offer uses.
///
/// Dynamic — 96 to 127 is the dynamic range, and 96 is what almost every
/// implementation picks first for H.264. A receiver checks it rather than
/// assuming, because a packet for another payload type is not a frame of ours.
pub const PAYLOAD_TYPE_H264: u8 = 96;

/// The RTP clock rate for video, in Hz.
///
/// Ninety thousand, which is not a choice: it is what RFC 3551 fixes for video
/// payload formats, and a WebRTC receiver will interpret timestamps at this
/// rate whatever we intended.
pub const VIDEO_CLOCK_HZ: u32 = 90_000;
