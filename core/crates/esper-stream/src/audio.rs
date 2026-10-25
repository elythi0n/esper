//! Sound on a media session: packets out at the host, frames in at the
//! controller.
//!
//! # One datagram per frame
//!
//! A 20 ms Opus packet is tens of bytes, so it never needs fragmenting — which
//! makes the whole reassembly machinery the picture needs irrelevant here. An
//! audio datagram is an RTP header and a packet, and a lost datagram is one
//! lost frame rather than a whole picture nobody can show.
//!
//! # Why sound is not delayed to match the picture
//!
//! A remote desktop is judged on how quickly the screen answers the mouse, so
//! the picture is never held back to line up with sound. That leaves the other
//! direction, and this is where it is decided: **sound that arrives too late
//! to play in order is dropped, not queued.** A queue would grow into a delay
//! and a person hears a delay as somebody talking out of step with their
//! mouth, which is worse than a click.

use esper_audio::{Decode, Format, Frame};
use esper_media::{RTP_HEADER_LEN, RtpHeader};
use esper_proto::AudioConfig;

use crate::StreamError;

/// The RTP payload type for Opus.
///
/// **111, which is what WebRTC offers.** Dynamic types are the receiver's to
/// assign, and every implementation that matters has settled on this one for
/// Opus — so a packet capture reads correctly in Wireshark and a browser
/// consuming this later needs no remapping.
pub const PAYLOAD_TYPE_OPUS: u8 = 111;

/// The RTP clock for Opus, in Hz.
///
/// **48 000 whatever the sample rate is.** RFC 7587 fixes it: an Opus stream's
/// timestamps run at 48 kHz even when the audio itself is 16 kHz, and a
/// receiver that used the sample rate instead would space frames wrongly.
pub const AUDIO_CLOCK_HZ: u32 = 48_000;

/// How many frames of concealment a gap may be filled with.
///
/// **Ten, a fifth of a second.** Beyond that, the far end has not been sending
/// — a pause, a session that stopped — and continuing to synthesise sound from
/// a decoder's memory would be inventing a fifth of a second of noise that
/// nobody played. A longer gap is silence and is counted as loss.
const MOST_CONCEALED: u16 = 10;

/// The sending half of a session's sound.
#[derive(Debug)]
pub struct HostAudio {
    encoder: esper_audio::opus::Encoder,
    config: AudioConfig,
    sequence: u16,
    packets: u64,
    bytes: u64,
}

impl HostAudio {
    /// An encoder and a packetizer for one session.
    ///
    /// # Errors
    ///
    /// If the shape is not one Opus can be built for.
    pub fn new(format: Format, ssrc: u32) -> Result<Self, StreamError> {
        let encoder = esper_audio::opus::Encoder::new(format)
            .map_err(|e| StreamError::Audio(e.to_string()))?;
        Ok(Self {
            encoder,
            config: format.as_config(ssrc),
            // **Not zero.** A session that always starts at zero makes the
            // first packets of every session identical in their headers,
            // and RFC 3550 asks for a random start so that two streams
            // spliced by accident cannot look like one.
            sequence: (ssrc >> 16) as u16,
            packets: 0,
            bytes: 0,
        })
    }

    /// What the host announces about this sound.
    #[must_use]
    pub const fn config(&self) -> AudioConfig {
        self.config
    }

    /// Packets and bytes sent so far.
    #[must_use]
    pub const fn sent(&self) -> (u64, u64) {
        (self.packets, self.bytes)
    }

    /// Encode one frame and wrap it as a datagram.
    ///
    /// # Errors
    ///
    /// If the frame is not one frame's worth of samples, or the codec refuses
    /// it.
    pub fn packetize(&mut self, frame: &Frame) -> Result<Vec<u8>, StreamError> {
        let packet = self
            .encoder
            .encode(frame)
            .map_err(|e| StreamError::Audio(e.to_string()))?;
        let header = RtpHeader {
            // **Always set.** Every audio packet is a complete frame, which is
            // exactly what the marker bit means: there is nothing more of this
            // one coming.
            marker: true,
            payload_type: PAYLOAD_TYPE_OPUS,
            sequence: self.sequence,
            // The sample count, which is what an Opus timestamp is. Truncated
            // to 32 bits as RTP requires; it wraps every 24 hours at 48 kHz
            // and the distance between two is what a receiver uses.
            timestamp: (frame.at & u64::from(u32::MAX)) as u32,
            ssrc: self.config.ssrc,
        };
        let mut datagram = vec![0u8; RTP_HEADER_LEN.saturating_add(packet.len())];
        header
            .write(&mut datagram)
            .ok_or_else(|| StreamError::Audio("an RTP header would not fit".to_owned()))?;
        let body = datagram
            .get_mut(RTP_HEADER_LEN..)
            .ok_or_else(|| StreamError::Audio("a datagram with no room for a packet".to_owned()))?;
        body.copy_from_slice(packet);
        self.sequence = self.sequence.wrapping_add(1);
        self.packets = self.packets.saturating_add(1);
        self.bytes = self.bytes.saturating_add(datagram.len() as u64);
        Ok(datagram)
    }
}

/// What has happened to a session's sound.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AudioStats {
    /// Datagrams that arrived for this stream.
    pub datagrams: u64,
    /// Frames handed to the caller to play.
    pub frames: u64,
    /// Frames the decoder reconstructed because a packet never arrived.
    pub concealed: u64,
    /// Packets that arrived after their turn had passed, and were dropped.
    ///
    /// **The A/V policy, counted.** Sound is not delayed to line up with the
    /// picture, so a packet that arrives out of order has already missed the
    /// moment it belonged to.
    pub late: u64,
    /// Datagrams refused: not ours, not Opus, or malformed.
    pub rejected: u64,
}

/// The receiving half of a session's sound.
pub struct ControllerAudio {
    /// Opus packets held for a recording, when one is being made.
    ///
    /// Off unless asked for, like the video's: a session nobody is recording
    /// pays nothing.
    kept: Vec<Vec<u8>>,
    keeping: bool,
    decoder: Box<dyn Decode>,
    config: AudioConfig,
    /// The sequence number of the last packet played, once one has been.
    played: Option<u16>,
    /// Where the stream is, in samples — so a concealed frame has a timestamp
    /// even though nothing arrived to carry one.
    at: u64,
    stats: AudioStats,
}

impl std::fmt::Debug for ControllerAudio {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ControllerAudio")
            .field("decoder", &self.decoder.backend())
            .field("sandboxed", &self.decoder.is_sandboxed())
            .field("stats", &self.stats)
            .finish_non_exhaustive()
    }
}

impl ControllerAudio {
    /// A receiver for the sound a host announced.
    ///
    /// **The decoder is passed in**, because a session decoding a peer's
    /// packets must use the sandboxed one and a loopback checking its own
    /// output should not have to. See [`esper_audio::Decode`].
    ///
    /// # Errors
    ///
    /// If the announced shape is not one a decoder can be built for, or the
    /// decoder does not produce that shape.
    pub fn new(config: &AudioConfig, decoder: Box<dyn Decode>) -> Result<Self, StreamError> {
        if !config.is_well_formed() {
            return Err(StreamError::Audio(format!(
                "{} Hz, {} channel(s), {} ms frames is not a shape a decoder can be built for",
                config.sample_rate_hz, config.channels, config.frame_ms
            )));
        }
        // **The decoder's own shape is checked against the announcement.** A
        // decoder built for something else would produce frames of a length
        // the playback path did not size for, which is heard as a stutter
        // rather than seen as an error.
        let want = Format::of(config);
        if decoder.format() != want {
            return Err(StreamError::Audio(format!(
                "a decoder for {:?} was given to a stream announced as {want:?}",
                decoder.format()
            )));
        }
        Ok(Self {
            kept: Vec::new(),
            keeping: false,
            decoder,
            config: *config,
            played: None,
            at: 0,
            stats: AudioStats::default(),
        })
    }

    /// What the host said this sound is.
    #[must_use]
    pub const fn config(&self) -> AudioConfig {
        self.config
    }

    /// Keep the Opus packets, for a recording.
    pub const fn keep_packets(&mut self, keeping: bool) {
        self.keeping = keeping;
    }

    /// Take the packets kept since the last call.
    ///
    /// Drained rather than read, for the reason the video's is: a caller that
    /// stops draining must not be able to grow this without bound.
    pub fn take_packets(&mut self) -> Vec<Vec<u8>> {
        std::mem::take(&mut self.kept)
    }

    /// Whether the decoder in use is confined.
    #[must_use]
    pub fn is_sandboxed(&self) -> bool {
        self.decoder.is_sandboxed()
    }

    /// Running totals.
    #[must_use]
    pub const fn stats(&self) -> AudioStats {
        self.stats
    }

    /// Feed one datagram, and take whatever should be played.
    ///
    /// Returns the frames to play in order: usually one, more when a gap was
    /// concealed, and none when the datagram was not ours or arrived too late
    /// to matter.
    ///
    /// # Errors
    ///
    /// Never for a datagram: a peer able to end a session by sending one bad
    /// packet is a peer with a denial of service, so a refusal is counted and
    /// the session carries on. The error type is kept for the decoder failing
    /// in a way that means the stream is over.
    pub fn receive(&mut self, datagram: &[u8]) -> Result<Vec<Frame>, StreamError> {
        self.stats.datagrams = self.stats.datagrams.saturating_add(1);
        let Ok((header, packet)) = RtpHeader::parse(datagram) else {
            self.stats.rejected = self.stats.rejected.saturating_add(1);
            return Ok(Vec::new());
        };
        if header.ssrc != self.config.ssrc || header.payload_type != PAYLOAD_TYPE_OPUS {
            self.stats.rejected = self.stats.rejected.saturating_add(1);
            return Ok(Vec::new());
        }
        if packet.is_empty() {
            // An empty payload is not a frame. It is also how this crate asks
            // a decoder to conceal, so it must not be possible for a peer to
            // ask for that by sending nothing.
            self.stats.rejected = self.stats.rejected.saturating_add(1);
            return Ok(Vec::new());
        }

        // **Kept before the jitter and concealment logic**, because what
        // belongs in a recording is the packet the host sent, not this
        // machine's reconstruction of a stream with gaps in it.
        if self.keeping {
            self.kept.push(packet.to_vec());
        }

        let mut out = Vec::new();
        if let Some(last) = self.played {
            let ahead = header.sequence.wrapping_sub(last);
            // Zero or a number near the top of the range means this packet is
            // not after the last one: a duplicate, or one that took a longer
            // path. Either way its turn has passed.
            if ahead == 0 || ahead > u16::MAX / 2 {
                self.stats.late = self.stats.late.saturating_add(1);
                return Ok(Vec::new());
            }
            // Every packet between the last one and this one is missing.
            let missing = ahead.saturating_sub(1);
            for _ in 0..missing.min(MOST_CONCEALED) {
                match self.decoder.conceal(self.at) {
                    Ok(frame) => {
                        self.at = self.at.saturating_add(self.step());
                        self.stats.concealed = self.stats.concealed.saturating_add(1);
                        self.stats.frames = self.stats.frames.saturating_add(1);
                        out.push(frame);
                    }
                    // A decoder that cannot conceal is a decoder that is gone.
                    Err(e) => return Err(StreamError::Audio(e.to_string())),
                }
            }
            if missing > MOST_CONCEALED {
                // The rest of the gap is silence rather than invention, and
                // the stream's position moves on so what follows is not
                // stretched to cover it.
                let skipped = u64::from(missing.saturating_sub(MOST_CONCEALED));
                self.at = self.at.saturating_add(skipped.saturating_mul(self.step()));
            }
        }

        match self.decoder.decode(packet, self.at) {
            Ok(frame) => {
                self.at = self.at.saturating_add(self.step());
                self.played = Some(header.sequence);
                self.stats.frames = self.stats.frames.saturating_add(1);
                out.push(frame);
                Ok(out)
            }
            // One packet the decoder would not have. Counted as a rejection
            // and not fatal: the next one usually decodes, and a peer that
            // could end a session with one packet has a denial of service.
            Err(e) => {
                tracing::debug!("a packet of audio was refused: {e}");
                self.stats.rejected = self.stats.rejected.saturating_add(1);
                self.played = Some(header.sequence);
                Ok(out)
            }
        }
    }

    /// Samples per frame, per channel — how far the stream moves each frame.
    const fn step(&self) -> u64 {
        Format::of(&self.config).samples_per_frame() as u64
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;
    use esper_audio::{Captured, SENT, Source, SyntheticTone};

    const SSRC: u32 = 0xA0DA_0001;

    fn host() -> HostAudio {
        HostAudio::new(SENT, SSRC).expect("a host")
    }

    fn controller(config: &AudioConfig) -> ControllerAudio {
        let decoder = esper_audio::opus::Decoder::new(config).expect("a decoder");
        ControllerAudio::new(config, Box::new(decoder)).expect("a controller")
    }

    fn frames(n: usize) -> Vec<Frame> {
        let mut source = SyntheticTone::new().unpaced();
        (0..n)
            .map(|_| {
                match source
                    .next_frame(std::time::Duration::from_secs(1))
                    .expect("a frame")
                {
                    Captured::Frame(f) => f,
                    other => panic!("the generator stopped: {other:?}"),
                }
            })
            .collect()
    }

    /// **Sound goes out as datagrams and comes back as frames**, through the
    /// same header format the picture uses.
    #[test]
    #[cfg_attr(miri, ignore = "needs opus, which is C; MIRI cannot call it")]
    fn a_frame_becomes_a_datagram_and_a_datagram_becomes_a_frame() {
        let mut host = host();
        let mut controller = controller(&host.config());
        let mut played = 0;
        for frame in frames(10) {
            let datagram = host.packetize(&frame).expect("packetized");
            assert!(
                datagram.len() > RTP_HEADER_LEN,
                "a datagram of {} bytes has no payload",
                datagram.len()
            );
            // Small enough that fragmenting is never a question, which is why
            // there is no reassembly here at all.
            assert!(datagram.len() < 400, "{} bytes", datagram.len());
            for out in controller.receive(&datagram).expect("received") {
                assert!(out.fits(&SENT));
                played += 1;
            }
        }
        assert_eq!(played, 10);
        let stats = controller.stats();
        assert_eq!((stats.frames, stats.concealed, stats.late), (10, 0, 0));
    }

    /// The RTP header says what it should: the marker on every packet, the
    /// Opus payload type, the session's SSRC, and a timestamp that advances by
    /// one frame's samples.
    #[test]
    #[cfg_attr(miri, ignore = "needs opus, which is C; MIRI cannot call it")]
    fn the_header_is_one_a_receiver_can_read() {
        let mut host = host();
        let mut want_timestamp = None;
        for frame in frames(3) {
            let datagram = host.packetize(&frame).expect("packetized");
            let (header, packet) = RtpHeader::parse(&datagram).expect("a header");
            assert!(header.marker, "every audio packet is a whole frame");
            assert_eq!(header.payload_type, PAYLOAD_TYPE_OPUS);
            assert_eq!(header.ssrc, SSRC);
            assert!(!packet.is_empty());
            if let Some(want) = want_timestamp {
                assert_eq!(header.timestamp, want);
            }
            want_timestamp = Some(header.timestamp.wrapping_add(960));
        }
    }

    /// **A lost packet is concealed and counted**, so the sound blurs over the
    /// gap rather than clicking, and the session can say it happened.
    #[test]
    #[cfg_attr(miri, ignore = "needs opus, which is C; MIRI cannot call it")]
    fn a_gap_is_concealed_and_counted() {
        let mut host = host();
        let mut controller = controller(&host.config());
        let all = frames(10);
        let mut played = 0;
        // The peaks of any frame that came back silent, so the assertion below
        // can say which.
        let mut quiet: Vec<u16> = Vec::new();
        for (i, frame) in all.iter().enumerate() {
            let datagram = host.packetize(frame).expect("packetized");
            // Drop the fifth and sixth packets on the way.
            if i == 5 || i == 6 {
                continue;
            }
            let out = controller.receive(&datagram).expect("received");
            // Frames beyond the first of a turn are the concealed ones: a
            // packet produces one frame, and a gap produces the frames before
            // it.
            for concealed in out.iter().take(out.len().saturating_sub(1)) {
                if concealed.peak() < 1_000 {
                    quiet.push(concealed.peak());
                }
            }
            played += out.len();
        }
        let stats = controller.stats();
        assert_eq!(stats.concealed, 2, "two lost packets, two concealed frames");
        assert_eq!(
            played, 10,
            "eight packets arrived and ten frames should be played"
        );
        // **And what filled the gap is sound, not silence.** Opus continues
        // the previous frame's spectrum, which a listener hears as a blur;
        // a frame of zeros in the middle of a tone is a click, and counting
        // frames alone would not tell the two apart.
        assert!(
            quiet.is_empty(),
            "the concealed frames were silence, which is a click: peaks {quiet:?}"
        );
    }

    /// **A gap longer than a fifth of a second is silence, not invention.**
    /// Continuing to synthesise from the decoder's memory would fill a pause
    /// with noise nobody played.
    #[test]
    #[cfg_attr(miri, ignore = "needs opus, which is C; MIRI cannot call it")]
    fn a_very_long_gap_is_not_filled_with_invented_sound() {
        let mut host = host();
        let mut controller = controller(&host.config());
        let first = frames(1);
        let datagram = host.packetize(first.first().expect("a frame")).expect("ok");
        let _ = controller.receive(&datagram).expect("received");

        // Skip a hundred packets, then send one.
        for _ in 0..100 {
            let f = frames(1);
            let _ = host.packetize(f.first().expect("a frame")).expect("ok");
        }
        let f = frames(1);
        let datagram = host.packetize(f.first().expect("a frame")).expect("ok");
        let out = controller.receive(&datagram).expect("received");
        assert!(
            out.len() <= usize::from(MOST_CONCEALED) + 1,
            "a hundred lost packets produced {} frames",
            out.len()
        );
        assert_eq!(controller.stats().concealed, u64::from(MOST_CONCEALED));
    }

    /// **A packet that arrives after its turn is dropped, not queued.** This
    /// is the A/V policy: sound is never delayed to line up with anything, so
    /// a packet that took a longer path has missed its moment.
    #[test]
    #[cfg_attr(miri, ignore = "needs opus, which is C; MIRI cannot call it")]
    fn a_packet_that_arrives_late_is_dropped_rather_than_queued() {
        let mut host = host();
        let mut controller = controller(&host.config());
        let all = frames(4);
        let mut datagrams: Vec<Vec<u8>> = all
            .iter()
            .map(|f| host.packetize(f).expect("packetized"))
            .collect();
        // Deliver them 1, 2, 3, then the one that should have been second.
        let second = datagrams.get(1).cloned().expect("a datagram");
        datagrams.push(second);
        let order = [0usize, 2, 3, 4];
        let mut played = 0;
        for i in order {
            played += controller
                .receive(datagrams.get(i).expect("a datagram"))
                .expect("received")
                .len();
        }
        let stats = controller.stats();
        assert_eq!(stats.late, 1, "the reordered packet should be dropped");
        assert!(played >= 3, "the ones in order should all have played");
    }

    /// A duplicate is dropped for the same reason, and counted the same way.
    #[test]
    #[cfg_attr(miri, ignore = "needs opus, which is C; MIRI cannot call it")]
    fn a_duplicate_is_dropped() {
        let mut host = host();
        let mut controller = controller(&host.config());
        let one = frames(1);
        let datagram = host.packetize(one.first().expect("a frame")).expect("ok");
        assert_eq!(controller.receive(&datagram).expect("received").len(), 1);
        assert_eq!(controller.receive(&datagram).expect("received").len(), 0);
        assert_eq!(controller.stats().late, 1);
    }

    /// **A datagram for another stream is refused rather than decoded.** A
    /// media channel carries the picture and the sound together, and a video
    /// packet handed to an audio decoder would be noise.
    #[test]
    #[cfg_attr(miri, ignore = "needs opus, which is C; MIRI cannot call it")]
    fn a_datagram_from_another_stream_is_refused() {
        let mut host = host();
        let mut controller = controller(&host.config());
        let one = frames(1);
        let mut datagram = host.packetize(one.first().expect("a frame")).expect("ok");

        // Same bytes, a different SSRC.
        let (mut header, _) = RtpHeader::parse(&datagram).expect("a header");
        header.ssrc = SSRC ^ 0xFFFF;
        header.write(&mut datagram).expect("written");
        assert!(controller.receive(&datagram).expect("received").is_empty());

        // And the video payload type on our own SSRC.
        let (mut header, _) = RtpHeader::parse(&datagram).expect("a header");
        header.ssrc = SSRC;
        header.payload_type = esper_media::PAYLOAD_TYPE_H264;
        header.write(&mut datagram).expect("written");
        assert!(controller.receive(&datagram).expect("received").is_empty());

        assert_eq!(controller.stats().rejected, 2);
        assert_eq!(controller.stats().frames, 0);
    }

    /// Rubbish is refused, counted, and does not end the session — the path a
    /// hostile peer takes.
    #[test]
    #[cfg_attr(miri, ignore = "needs opus, which is C; MIRI cannot call it")]
    fn rubbish_is_refused_without_ending_the_session() {
        let mut host = host();
        let mut controller = controller(&host.config());
        for junk in [vec![], vec![0u8; 4], vec![0xFFu8; 200]] {
            assert!(controller.receive(&junk).expect("received").is_empty());
        }
        // And a real packet still plays afterwards.
        let one = frames(1);
        let datagram = host.packetize(one.first().expect("a frame")).expect("ok");
        assert_eq!(controller.receive(&datagram).expect("received").len(), 1);
    }

    /// **A decoder built for a different shape is refused at construction.**
    /// It would produce frames of a length the playback path did not size for,
    /// which is heard rather than reported.
    #[test]
    #[cfg_attr(miri, ignore = "needs opus, which is C; MIRI cannot call it")]
    fn a_decoder_of_the_wrong_shape_is_refused() {
        let announced = SENT.as_config(SSRC);
        let other = Format {
            channels: 1,
            ..SENT
        };
        let decoder = esper_audio::opus::Decoder::new(&other.as_config(SSRC)).expect("a decoder");
        assert!(ControllerAudio::new(&announced, Box::new(decoder)).is_err());
    }

    /// An announcement Opus cannot be built for is refused, rather than
    /// becoming a stream that fails on every packet.
    #[test]
    #[cfg_attr(miri, ignore = "needs opus, which is C; MIRI cannot call it")]
    fn an_unsupported_announcement_is_refused() {
        let bad = AudioConfig {
            sample_rate_hz: 44_100,
            ..SENT.as_config(SSRC)
        };
        let decoder = esper_audio::opus::Decoder::new(&SENT.as_config(SSRC)).expect("a decoder");
        assert!(ControllerAudio::new(&bad, Box::new(decoder)).is_err());
    }
}
