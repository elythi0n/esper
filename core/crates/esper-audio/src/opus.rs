//! Opus, in and out.
//!
//! # Why Opus and nothing else
//!
//! It is the only codec that is good at speech *and* music, which matters
//! because a desktop's sound is both — a video call in one window and a video
//! in another. Every browser decodes it, which is what the viewer being a
//! WebRTC surface later depends on. And it is royalty-free, which a product
//! people self-host cannot do without.
//!
//! # Where each half runs
//!
//! The **encoder** runs in the host process. It is fed the host's own captured
//! samples, so its input is not attacker-controlled and it needs no sandbox of
//! its own.
//!
//! The **decoder** is fed a peer's packets, which makes it exactly the shape of
//! thing this product does not run in its own address space: a C library
//! parsing bytes from the network. It runs in the sandboxed worker beside the
//! video decoder — a separate process, seccomp and Landlock applied before the
//! first byte arrives. See `esper-decode`.
//!
//! Nothing here enforces that. It is stated here so that whoever wires up a
//! decoder somewhere else knows what they are doing.

use esper_proto::AudioConfig;

use crate::{AudioError, Format, Frame};

/// The largest packet this will produce or accept.
///
/// **A bound, not a buffer size.** Opus at 48 kHz stereo and a sane bitrate
/// makes packets of 50-200 bytes; 1275 is the largest a single Opus frame can
/// be by its own specification, and a packet claiming more than that is not
/// Opus. Bounded rather than trusted because the number arrives from a peer:
/// see `esper-decode`'s framing for the same rule applied to pictures.
pub const MAX_PACKET: usize = 1275;

/// What the encoder aims at, in bits per second.
///
/// **96 kbit/s for stereo.** Opus is transparent for most material by 128 and
/// good by 96; a remote desktop is sharing a link with a screen that wants
/// every kilobit it can get, and 96 is where the sound stops being the thing
/// you would trade away. It is a *target*: Opus varies the rate frame by frame
/// and silence costs almost nothing.
pub const BITRATE: i32 = 96_000;

/// Sound in, packets out.
#[derive(Debug)]
pub struct Encoder {
    inner: opus::Encoder,
    format: Format,
    /// Reused, so a frame's packet is not a fresh allocation every 20 ms.
    scratch: Vec<u8>,
    packets: u64,
    bytes: u64,
}

impl Encoder {
    /// An encoder for this shape of sound.
    ///
    /// # Errors
    ///
    /// If the shape is not one Opus has, or the codec refuses to start.
    pub fn new(format: Format) -> Result<Self, AudioError> {
        if !format.is_supported() {
            return Err(AudioError::Unsupported {
                sample_rate_hz: format.sample_rate_hz,
                channels: format.channels,
                frame_ms: format.frame_ms,
            });
        }
        // **`Audio`, not `Voip`.** The application hint changes how Opus
        // spends its bits: `Voip` optimises for speech intelligibility and is
        // the wrong answer for a desktop, where half of what is heard is
        // music, an alert or a video.
        let mut inner = opus::Encoder::new(
            format.sample_rate_hz,
            channels_of(format.channels)?,
            opus::Application::Audio,
        )
        .map_err(codec)?;
        inner
            .set_bitrate(opus::Bitrate::Bits(BITRATE))
            .map_err(codec)?;
        Ok(Self {
            inner,
            format,
            scratch: vec![0; MAX_PACKET],
            packets: 0,
            bytes: 0,
        })
    }

    /// The shape it expects.
    #[must_use]
    pub const fn format(&self) -> Format {
        self.format
    }

    /// Packets and bytes produced so far.
    #[must_use]
    pub const fn sent(&self) -> (u64, u64) {
        (self.packets, self.bytes)
    }

    /// Encode one frame.
    ///
    /// # Errors
    ///
    /// If the frame does not hold exactly one frame's worth of samples, or the
    /// codec refuses it.
    pub fn encode(&mut self, frame: &Frame) -> Result<&[u8], AudioError> {
        let want = self.format.values_per_frame();
        if frame.samples.len() != want {
            return Err(AudioError::WrongSize {
                got: frame.samples.len(),
                want,
            });
        }
        let n = self
            .inner
            .encode(&frame.samples, &mut self.scratch)
            .map_err(codec)?;
        let packet = self.scratch.get(..n).ok_or_else(|| AudioError::Codec {
            detail: format!("the encoder reported {n} bytes into a {MAX_PACKET}-byte buffer"),
        })?;
        self.packets = self.packets.saturating_add(1);
        self.bytes = self.bytes.saturating_add(n as u64);
        Ok(packet)
    }
}

/// Packets in, sound out.
#[derive(Debug)]
pub struct Decoder {
    inner: opus::Decoder,
    format: Format,
    frames: u64,
    /// Frames reconstructed from nothing, because a packet did not arrive.
    filled: u64,
}

impl Decoder {
    /// A decoder for a config a host announced.
    ///
    /// # Errors
    ///
    /// If the config is not one Opus has, or the codec refuses to start.
    pub fn new(config: &AudioConfig) -> Result<Self, AudioError> {
        let format = Format::of(config);
        if !config.is_well_formed() {
            return Err(AudioError::Unsupported {
                sample_rate_hz: format.sample_rate_hz,
                channels: format.channels,
                frame_ms: format.frame_ms,
            });
        }
        let inner = opus::Decoder::new(format.sample_rate_hz, channels_of(format.channels)?)
            .map_err(codec)?;
        Ok(Self {
            inner,
            format,
            frames: 0,
            filled: 0,
        })
    }

    /// The shape it produces.
    #[must_use]
    pub const fn format(&self) -> Format {
        self.format
    }

    /// Frames decoded, and of those how many were reconstructed from nothing.
    #[must_use]
    pub const fn decoded(&self) -> (u64, u64) {
        (self.frames, self.filled)
    }

    /// Decode one packet into one frame.
    ///
    /// `at` is the frame's position in the stream, carried through rather than
    /// derived here: the decoder does not know what was lost before it.
    ///
    /// # Errors
    ///
    /// If the packet is longer than any Opus packet can be, or the codec
    /// refuses it.
    pub fn decode(&mut self, packet: &[u8], at: u64) -> Result<Frame, AudioError> {
        if packet.len() > MAX_PACKET {
            return Err(AudioError::Codec {
                detail: format!(
                    "a {}-byte packet, where no Opus packet exceeds {MAX_PACKET}",
                    packet.len()
                ),
            });
        }
        let mut samples = vec![0i16; self.format.values_per_frame()];
        let produced = self
            .inner
            .decode(packet, &mut samples, false)
            .map_err(codec)?;
        finish(&mut samples, produced, &self.format)?;
        self.frames = self.frames.saturating_add(1);
        Ok(Frame { samples, at })
    }

    /// Reconstruct the frame that should have arrived, having lost it.
    ///
    /// **Opus does this better than silence does.** Its own loss concealment
    /// continues the previous frame's spectrum, which a listener hears as a
    /// blur rather than as a click — and a click is what a frame of zeros
    /// sounds like when it lands in the middle of a note.
    ///
    /// # Errors
    ///
    /// If the codec refuses.
    pub fn conceal(&mut self, at: u64) -> Result<Frame, AudioError> {
        let mut samples = vec![0i16; self.format.values_per_frame()];
        let produced = self.inner.decode(&[], &mut samples, false).map_err(codec)?;
        finish(&mut samples, produced, &self.format)?;
        self.frames = self.frames.saturating_add(1);
        self.filled = self.filled.saturating_add(1);
        Ok(Frame { samples, at })
    }
}

impl crate::Decode for Decoder {
    fn backend(&self) -> &'static str {
        "opus"
    }

    /// **No.** This one runs in the caller's own process, which is right for a
    /// loopback and for a test and wrong for a peer's packets. See
    /// `esper_decode::audio::SandboxedAudio`.
    fn is_sandboxed(&self) -> bool {
        false
    }

    fn format(&self) -> Format {
        self.format
    }

    fn decode(&mut self, packet: &[u8], at: u64) -> Result<Frame, AudioError> {
        Self::decode(self, packet, at)
    }

    fn conceal(&mut self, at: u64) -> Result<Frame, AudioError> {
        Self::conceal(self, at)
    }

    fn decoded(&self) -> (u64, u64) {
        Self::decoded(self)
    }
}

/// Check what the decoder produced against what was announced.
///
/// **A frame that is not the announced length is refused.** Opus itself allows
/// a sender to change frame length packet by packet, and a packet of junk is
/// often *read* as a shorter frame — which is how this rule was found: random
/// bytes decoded to a valid 10 ms frame in a stream announced as 20 ms.
///
/// Accepting it would mean every buffer downstream is sized for a length the
/// stream does not keep to, which is heard as a stutter rather than seen as an
/// error. Both ends of this protocol are this product, the announced length is
/// on the wire precisely so a receiver can size for it, and a sender that
/// contradicts its own announcement is either broken or hostile. So the length
/// is part of the contract and this is where it is enforced.
fn finish(samples: &mut [i16], per_channel: usize, format: &Format) -> Result<(), AudioError> {
    let values = per_channel.saturating_mul(usize::from(format.channels.max(1)));
    if values != samples.len() {
        return Err(AudioError::WrongSize {
            got: values,
            want: samples.len(),
        });
    }
    Ok(())
}

/// The codec's own channel type, refusing anything it does not have.
fn channels_of(channels: u8) -> Result<opus::Channels, AudioError> {
    match channels {
        1 => Ok(opus::Channels::Mono),
        2 => Ok(opus::Channels::Stereo),
        n => Err(AudioError::Unsupported {
            sample_rate_hz: 0,
            channels: n,
            frame_ms: 0,
        }),
    }
}

/// The codec's errors, in this crate's terms.
fn codec(e: opus::Error) -> AudioError {
    AudioError::Codec {
        detail: e.to_string(),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;
    use crate::{SENT, Source, SyntheticTone};

    fn tone() -> SyntheticTone {
        SyntheticTone::new().unpaced()
    }

    fn frame(source: &mut SyntheticTone) -> Frame {
        match source
            .next_frame(std::time::Duration::from_secs(1))
            .expect("a frame")
        {
            crate::Captured::Frame(f) => f,
            other => panic!("expected a frame, got {other:?}"),
        }
    }

    /// **Sound goes in and sound comes out**, at the size the format says and
    /// loud enough to be the tone rather than the silence a broken path
    /// produces.
    #[test]
    fn a_tone_survives_a_round_trip_through_the_codec() {
        let mut source = tone();
        let mut encoder = Encoder::new(SENT).expect("an encoder");
        let mut decoder = Decoder::new(&SENT.as_config(1)).expect("a decoder");

        // The first few frames are the encoder settling: Opus needs a little
        // history before its output resembles its input, which is why this
        // checks a later frame rather than the first.
        let mut last = None;
        for _ in 0..10 {
            let f = frame(&mut source);
            let packet = encoder.encode(&f).expect("encoded");
            assert!(
                !packet.is_empty() && packet.len() <= MAX_PACKET,
                "a {}-byte packet",
                packet.len()
            );
            last = Some(decoder.decode(packet, f.at).expect("decoded"));
        }
        let out = last.expect("ten frames");
        assert!(out.fits(&SENT), "{} values", out.samples.len());
        assert!(
            out.peak() > 5_000,
            "the tone came back at a peak of {}, which is silence with extra steps",
            out.peak()
        );
    }

    /// **A packet is small.** The point of a codec is that sound costs
    /// kilobits and not megabits, and the number is worth asserting: 20 ms of
    /// 48 kHz stereo is 3840 bytes of samples, and a packet an order of
    /// magnitude larger than the target bitrate would mean the bitrate was
    /// never applied.
    #[test]
    fn a_packet_is_about_the_bitrate_that_was_asked_for() {
        let mut source = tone();
        let mut encoder = Encoder::new(SENT).expect("an encoder");
        let mut total = 0usize;
        let rounds = 50;
        for _ in 0..rounds {
            let f = frame(&mut source);
            total += encoder.encode(&f).expect("encoded").len();
        }
        // 96 kbit/s is 240 bytes per 20 ms frame. A tone is much cheaper than
        // that, so this bounds it above and checks it is not empty below.
        let average = total / rounds;
        assert!(
            (10..=300).contains(&average),
            "an average packet of {average} bytes against a 240-byte budget"
        );
        let (packets, bytes) = encoder.sent();
        assert_eq!(packets, rounds as u64);
        assert_eq!(bytes as usize, total);
    }

    /// A frame that is not one frame is refused, rather than encoded as
    /// whatever the codec makes of it.
    #[test]
    fn a_frame_of_the_wrong_size_is_refused() {
        let mut encoder = Encoder::new(SENT).expect("an encoder");
        let short = Frame {
            samples: vec![0; 100],
            at: 0,
        };
        assert!(matches!(
            encoder.encode(&short),
            Err(AudioError::WrongSize {
                got: 100,
                want: 1920
            })
        ));
    }

    /// **A packet that decodes to a different frame length is refused.**
    ///
    /// Opus lets a sender change frame length packet by packet, and a receiver
    /// has sized every buffer downstream from the length that was announced.
    /// So a 10 ms packet arriving in a stream announced as 20 ms is refused —
    /// and the check has to be real, because without it the frame is the right
    /// *size* with only half of it decoded and the rest left as zeros. That is
    /// a click in the middle of every such frame, and nothing further down
    /// would notice.
    #[test]
    fn a_packet_of_a_different_frame_length_is_refused() {
        let short = Format {
            frame_ms: 10,
            ..SENT
        };
        let mut source = SyntheticTone::new().in_format(short).unpaced();
        let mut encoder = Encoder::new(short).expect("a 10 ms encoder");
        let mut decoder = Decoder::new(&SENT.as_config(1)).expect("a 20 ms decoder");

        let f = frame(&mut source);
        let packet = encoder.encode(&f).expect("encoded").to_vec();
        match decoder.decode(&packet, 0) {
            Err(AudioError::WrongSize { got, want }) => {
                assert_eq!((got, want), (960, 1920));
            }
            Ok(f) => panic!(
                "a 10 ms packet was accepted into a 20 ms stream as {} values",
                f.samples.len()
            ),
            Err(e) => panic!("unexpected error: {e}"),
        }
    }

    /// **A packet longer than any Opus packet is refused before the codec sees
    /// it.** It arrives from a peer, and the bound is the specification's own.
    #[test]
    fn a_packet_too_long_to_be_opus_is_refused() {
        let mut decoder = Decoder::new(&SENT.as_config(1)).expect("a decoder");
        let huge = vec![0u8; MAX_PACKET + 1];
        assert!(matches!(
            decoder.decode(&huge, 0),
            Err(AudioError::Codec { .. })
        ));
    }

    /// Rubbish in is an error, not a panic and not a frame of noise presented
    /// as sound. This is the path a hostile peer takes.
    #[test]
    fn a_packet_that_is_not_opus_is_an_error_rather_than_a_panic() {
        let mut decoder = Decoder::new(&SENT.as_config(1)).expect("a decoder");
        for junk in [
            vec![0xFFu8; 64],
            vec![0x00u8; 1],
            (0..200u16).map(|i| (i % 251) as u8).collect::<Vec<u8>>(),
        ] {
            // Either an error or a frame of exactly the announced size; never
            // a panic, and never a frame of a length nothing sized a buffer
            // for. Junk really does decode as a valid shorter frame — that is
            // what `finish` refuses, and this is the test that found it.
            match decoder.decode(&junk, 0) {
                Ok(f) => assert!(f.fits(&SENT), "{} values", f.samples.len()),
                Err(AudioError::Codec { .. } | AudioError::WrongSize { .. }) => {}
                Err(e) => panic!("unexpected error: {e}"),
            }
        }
    }

    /// **A lost packet is concealed rather than replaced with a click.** The
    /// concealed frame is the right size and is counted separately, so a
    /// session can say how much of what was heard was reconstructed.
    #[test]
    fn a_lost_packet_is_concealed_and_counted() {
        let mut source = tone();
        let mut encoder = Encoder::new(SENT).expect("an encoder");
        let mut decoder = Decoder::new(&SENT.as_config(1)).expect("a decoder");
        for _ in 0..10 {
            let f = frame(&mut source);
            let packet = encoder.encode(&f).expect("encoded");
            let _ = decoder.decode(packet, f.at).expect("decoded");
        }
        let concealed = decoder.conceal(10 * 960).expect("concealed");
        assert!(concealed.fits(&SENT));
        let (frames, filled) = decoder.decoded();
        assert_eq!((frames, filled), (11, 1));
        // Concealment continues the tone, so it is not silence — which is the
        // whole reason to use it rather than a frame of zeros.
        assert!(
            concealed.peak() > 1_000,
            "concealment produced a peak of {}, which is a click",
            concealed.peak()
        );
    }

    /// Mono and stereo both work, because a backend may only offer one.
    #[test]
    fn mono_encodes_and_decodes_as_mono() {
        let mono = Format {
            channels: 1,
            ..SENT
        };
        let mut source = SyntheticTone::new().in_format(mono).unpaced();
        let mut encoder = Encoder::new(mono).expect("an encoder");
        let mut decoder = Decoder::new(&mono.as_config(1)).expect("a decoder");
        for _ in 0..10 {
            let f = frame(&mut source);
            assert_eq!(f.samples.len(), 960);
            let packet = encoder.encode(&f).expect("encoded");
            let out = decoder.decode(packet, f.at).expect("decoded");
            assert_eq!(out.samples.len(), 960);
        }
    }

    /// A shape Opus does not have is refused when the codec is built, not when
    /// the first frame arrives.
    #[test]
    fn an_unsupported_shape_is_refused_at_construction() {
        let bad = Format {
            sample_rate_hz: 44_100,
            ..SENT
        };
        assert!(matches!(
            Encoder::new(bad),
            Err(AudioError::Unsupported { .. })
        ));
        assert!(matches!(
            Decoder::new(&bad.as_config(1)),
            Err(AudioError::Unsupported { .. })
        ));
    }
}
