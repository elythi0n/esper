//! Sound, from the host's speakers to the controller's.
//!
//! # What this is for
//!
//! A remote desktop with no sound is a remote desktop where a video is a
//! silent slideshow, an alert is invisible, and a support call needs a
//! telephone alongside it. This crate is the sound half: what a frame of audio
//! is, where frames come from, and how they are turned into packets and back.
//!
//! # One direction, deliberately
//!
//! **Host to controller only.** The microphone the other way is a
//! conferencing feature: it doubles the pipeline, it needs echo cancellation
//! to be usable, and it is the part of a conferencing product that this is
//! not. The design documents said so before any of this was written, and this
//! crate has no path for it — not a disabled one, none.
//!
//! # What is captured
//!
//! A *monitor of the output*, never an input device. What the far end hears is
//! what somebody standing at the host would hear, which is the whole point —
//! and it means nothing here ever opens a microphone, so nothing here can turn
//! a session into a listening device. That is a property of the capture
//! backend and it is stated here because it is the reason the trait below has
//! no way to ask for an input.

#![forbid(unsafe_code)]

use esper_proto::{AudioCodec, AudioConfig};

pub mod opus;
pub mod tone;

pub use tone::SyntheticTone;

/// What this product sends: 48 kHz stereo in 20 ms frames.
///
/// **48 kHz** because it is Opus's own internal rate, so anything else costs a
/// resample for no gain. **Stereo** because a desktop's sound is mixed for two
/// speakers and folding it to mono loses information a person notices in
/// music. **20 ms** because it is the Opus default and the balance everybody
/// else settled on: 10 ms halves the buffering delay and adds about half again
/// to the bitrate in header overhead, and 60 ms saves little and costs three
/// times as much when a packet is lost.
pub const SENT: Format = Format {
    sample_rate_hz: 48_000,
    channels: 2,
    frame_ms: 20,
};

/// The shape of a stream of sound, without the stream.
///
/// The same three numbers as [`AudioConfig`], which is the wire form — this is
/// the domain form, and it deliberately has no SSRC: a sound card does not
/// have one, and a capture backend should not be handed a field it has no
/// business filling in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Format {
    /// Samples per second, per channel.
    pub sample_rate_hz: u32,
    /// 1 for mono, 2 for stereo.
    pub channels: u8,
    /// How much sound one frame holds, in milliseconds.
    pub frame_ms: u8,
}

impl Format {
    /// The wire form of this, for a stream with the given identity.
    #[must_use]
    pub const fn as_config(&self, ssrc: u32) -> AudioConfig {
        AudioConfig {
            codec: AudioCodec::Opus,
            sample_rate_hz: self.sample_rate_hz,
            channels: self.channels,
            frame_ms: self.frame_ms,
            ssrc,
        }
    }

    /// The domain form of a config a host announced.
    #[must_use]
    pub const fn of(config: &AudioConfig) -> Self {
        Self {
            sample_rate_hz: config.sample_rate_hz,
            channels: config.channels,
            frame_ms: config.frame_ms,
        }
    }

    /// Samples per channel in one frame.
    #[must_use]
    pub const fn samples_per_frame(&self) -> usize {
        (self.sample_rate_hz as usize).saturating_mul(self.frame_ms as usize) / 1000
    }

    /// Values in one frame, counting both channels.
    ///
    /// **This is the number, not the one above.** Samples are interleaved, so a
    /// stereo frame holds twice as many values as it holds samples — and a
    /// buffer sized by the wrong one of the two is either half a frame or a
    /// decode that fails on every packet. The distinction is why both exist and
    /// why they are named differently.
    #[must_use]
    pub const fn values_per_frame(&self) -> usize {
        self.samples_per_frame()
            .saturating_mul(self.channels as usize)
    }

    /// Whether this is a shape Opus can be built for.
    #[must_use]
    pub fn is_supported(&self) -> bool {
        self.as_config(0).is_well_formed()
    }
}

/// One frame of sound: interleaved 16-bit samples, in [`Format`] order.
///
/// **16-bit rather than float.** It is what every sound server hands over by
/// default, what Opus takes without conversion, and it is exact: a float path
/// would put two conversions between the host's speakers and the encoder for
/// precision nothing downstream can use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    /// Interleaved samples: left, right, left, right for stereo.
    pub samples: Vec<i16>,
    /// When this frame's first sample was captured, as a count of samples
    /// since the stream began.
    ///
    /// **A sample count, not a clock reading.** It is what the RTP timestamp
    /// is, it advances by exactly one frame each time, and it cannot drift
    /// against the sound it describes — where a clock read at capture time
    /// can, and then the far end resamples to a rate nothing is actually
    /// producing.
    pub at: u64,
}

impl Frame {
    /// Whether this frame holds exactly one frame's worth of `format`.
    #[must_use]
    pub fn fits(&self, format: &Format) -> bool {
        self.samples.len() == format.values_per_frame()
    }

    /// A frame of silence, for a gap that has to be filled with something.
    #[must_use]
    pub fn silence(format: &Format, at: u64) -> Self {
        Self {
            samples: vec![0; format.values_per_frame()],
            at,
        }
    }

    /// The loudest sample in the frame, as an absolute value.
    ///
    /// For saying whether anything is actually being heard — a session that
    /// reports "audio on" while sending silence is a session somebody will
    /// spend an hour debugging at the wrong end.
    #[must_use]
    pub fn peak(&self) -> u16 {
        self.samples
            .iter()
            .map(|s| s.unsigned_abs())
            .max()
            .unwrap_or(0)
    }
}

/// What a capture attempt produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Captured {
    /// One frame of sound.
    Frame(Frame),
    /// Nothing arrived within the timeout.
    ///
    /// **Not the same as silence.** Silence is sound that happens to be quiet
    /// and still costs a packet; this is the backend having nothing to say,
    /// and a caller that turned it into silence would be inventing sound the
    /// host was not making.
    Nothing,
    /// The source ended and will produce nothing more.
    Finished,
}

/// Where frames of sound come from.
///
/// The trait has no way to select an input device, and that is the point: see
/// this crate's own documentation.
pub trait Source: std::fmt::Debug + Send {
    /// Which backend this is, for the log and the session summary.
    fn backend(&self) -> &'static str;

    /// The shape of what it produces.
    fn format(&self) -> Format;

    /// Wait up to `timeout` for the next frame.
    fn next_frame(&mut self, timeout: std::time::Duration) -> Result<Captured, AudioError>;
}

/// Somewhere for sound to come out.
///
/// The controller's half. Separate from [`Source`] because the two ends of a
/// session use one each and neither should link the other's backend.
pub trait Sink: std::fmt::Debug + Send {
    /// Which backend this is.
    fn backend(&self) -> &'static str;

    /// The shape it expects.
    fn format(&self) -> Format;

    /// Play one frame.
    ///
    /// **Blocking, and that is the flow control.** A sink that buffered
    /// without bound would turn a slow speaker into unbounded memory and an
    /// ever-growing delay; one that blocks makes the caller wait for the sound
    /// card, which is exactly as fast as the sound is supposed to go.
    fn play(&mut self, frame: &Frame) -> Result<(), AudioError>;
}

/// A decoder, wherever it runs.
///
/// **The point of the trait is where the implementation lives.** A session
/// decoding a peer's packets must use the sandboxed one — a C library on a
/// peer's bytes belongs in its own process — while a loopback checking its own
/// output, or a test, wants the one in this crate. That is a choice the
/// wiring-up makes, so it is a trait here and a decision there.
pub trait Decode: std::fmt::Debug + Send {
    /// Which codec, for the log.
    fn backend(&self) -> &'static str;

    /// Whether this one is confined.
    ///
    /// A session says so in its summary, because "sandboxed" is a claim
    /// somebody should be able to check rather than trust.
    fn is_sandboxed(&self) -> bool;

    /// The shape it produces.
    fn format(&self) -> Format;

    /// Decode one packet.
    ///
    /// # Errors
    ///
    /// If the packet is not one this decoder accepts.
    fn decode(&mut self, packet: &[u8], at: u64) -> Result<Frame, AudioError>;

    /// Produce the frame that should have arrived, having lost it.
    ///
    /// # Errors
    ///
    /// If the decoder refuses.
    fn conceal(&mut self, at: u64) -> Result<Frame, AudioError>;

    /// Frames produced, and of those how many were reconstructed.
    fn decoded(&self) -> (u64, u64);
}

/// What can go wrong with sound.
#[derive(Debug, thiserror::Error)]
pub enum AudioError {
    /// No backend works on this machine.
    ///
    /// Names what was tried: "no audio" on a server tells an operator nothing
    /// they can act on, and the usual cause is a package that is not installed.
    #[error("no audio backend is available ({tried})")]
    NoBackend { tried: String },

    /// The shape asked for is not one this can produce.
    #[error("{sample_rate_hz} Hz, {channels} channel(s), {frame_ms} ms frames is not supported")]
    Unsupported {
        sample_rate_hz: u32,
        channels: u8,
        frame_ms: u8,
    },

    /// A frame that does not hold what its format says it holds.
    #[error("a frame of {got} values where {want} were expected")]
    WrongSize { got: usize, want: usize },

    /// The codec refused.
    #[error("opus: {detail}")]
    Codec { detail: String },

    /// The backend failed.
    #[error("{backend}: {detail}")]
    Backend {
        backend: &'static str,
        detail: String,
    },
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    /// **The wire form and the domain form must agree about the arithmetic**,
    /// because a buffer sized by one and filled by the other is a decode that
    /// fails on every packet. Two statements of one rule, held to each other.
    #[test]
    fn the_two_forms_of_a_format_agree_at_every_legal_shape() {
        for rate in [8_000u32, 12_000, 16_000, 24_000, 48_000] {
            for channels in [1u8, 2] {
                for frame_ms in [10u8, 20, 40, 60] {
                    let format = Format {
                        sample_rate_hz: rate,
                        channels,
                        frame_ms,
                    };
                    let config = format.as_config(7);
                    assert!(format.is_supported(), "{format:?}");
                    assert!(config.is_well_formed(), "{config:?}");
                    assert_eq!(
                        format.samples_per_frame(),
                        config.samples_per_frame(),
                        "{format:?}"
                    );
                    assert_eq!(Format::of(&config), format);
                    assert_eq!(
                        format.values_per_frame(),
                        format.samples_per_frame() * channels as usize
                    );
                }
            }
        }
    }

    /// What this product actually sends, spelled out so a change to it is a
    /// change to a test rather than a surprise on somebody's machine.
    #[test]
    fn what_is_sent_is_forty_eight_kilohertz_stereo_in_twenty_millisecond_frames() {
        assert_eq!(SENT.sample_rate_hz, 48_000);
        assert_eq!(SENT.channels, 2);
        assert_eq!(SENT.frame_ms, 20);
        assert_eq!(SENT.samples_per_frame(), 960);
        assert_eq!(SENT.values_per_frame(), 1920);
        assert!(SENT.is_supported());
    }

    /// A shape Opus cannot be built for is refused rather than approximated.
    #[test]
    fn a_shape_opus_does_not_have_is_not_supported() {
        for bad in [
            Format {
                sample_rate_hz: 44_100,
                channels: 2,
                frame_ms: 20,
            },
            Format {
                sample_rate_hz: 48_000,
                channels: 6,
                frame_ms: 20,
            },
            Format {
                sample_rate_hz: 48_000,
                channels: 2,
                frame_ms: 33,
            },
        ] {
            assert!(!bad.is_supported(), "{bad:?}");
        }
    }

    /// Silence is a frame of the right size, because it goes out as a packet
    /// like any other and everything downstream sizes buffers from the format.
    #[test]
    fn silence_is_a_frame_of_the_right_size_and_is_quiet() {
        let f = Frame::silence(&SENT, 960);
        assert!(f.fits(&SENT));
        assert_eq!(f.peak(), 0);
        assert_eq!(f.at, 960);
    }

    /// And a frame of the wrong size says so, which is what stops it reaching
    /// an encoder that would refuse it less clearly.
    #[test]
    fn a_frame_that_does_not_fit_its_format_says_so() {
        let f = Frame {
            samples: vec![0; 100],
            at: 0,
        };
        assert!(!f.fits(&SENT));
    }
}
