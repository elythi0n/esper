//! What an encoder is, from the pipeline's side.
//!
//! One trait, because v1 ships one encoder and v1.x adds hardware backends
//! behind the same shape. The trait is what makes that a cost reduction rather
//! than a rewrite, so it is written for what a hardware backend will also
//! need — a configure step separate from encoding, a keyframe that can be
//! demanded, and per-frame timing — rather than for what openh264 happens to
//! expose.
//!
//! # What is deliberately not here
//!
//! **No bitrate control.** That belongs to congestion control, which lives
//! with the transport and has the RTT and loss signals. An encoder that
//! decided its own bitrate would be guessing at a network it cannot see.
//!
//! **No probing by capability query.** When hardware backends arrive they are
//! probed by an actual test encode. Capability queries lie, particularly on
//! Linux, where the device node exists and the driver does not work.

use std::time::{Duration, Instant};

use ownview_color::i420::I420;
use ownview_color::space::ColorSpace;

/// Whether this build has openh264's assembly.
///
/// `false` means the C-only path, which is **2.5% to 6% slower** with
/// identical output size — measured on identical content in both builds. An
/// earlier version of this comment said 37%, which was wrong: that figure came
/// from an early probe on high-entropy content and was a property of the
/// content, not of the missing assembly.
///
/// Reported rather than hidden anyway, because the design's honest signal is
/// that the session HUD shows *cost*, and a host should be able to say why it
/// is a few per cent off the published numbers rather than leave a user
/// guessing.
#[must_use]
pub const fn has_assembly() -> bool {
    !cfg!(slow_encoder)
}

/// What the encoder should assume it is looking at.
///
/// # The measurement that made this a choice rather than a default
///
/// openh264 has a screen-content mode, and choosing it looks obvious: a screen
/// is not camera video. It is mostly static, has hard edges and large flat
/// areas, contains text, and has no sensor noise.
///
/// Measured on this project's development machine, with the assembly present,
/// against a deliberately screen-like pattern — and it is **not a trade-off**:
///
/// | Tuning | ms/frame | bytes/frame | Luma PSNR |
/// |---|---:|---:|---:|
/// | Camera (openh264's default) | 2.2 | 2 805 | **47.9 dB** |
/// | Screen content | 11.8 | 1 773 | 44.6 dB |
///
/// **5.4 times the CPU, 37% fewer bytes, and 3.3 dB worse quality.** Camera
/// tuning wins on cost *and* on picture, and loses only on bitrate — the one
/// dimension with room to spare, since 2.8 KB a frame is under 700 kbit/s at
/// 30 fps.
///
/// The likely reason is in openh264's own startup warnings: screen-content
/// mode auto-disables adaptive quantisation and background detection, and
/// those are what were buying the quality.
///
/// # Why it is still configurable
///
/// The measurement is one pattern on one processor. A real desktop — actual
/// text rendering, large flat regions of interface, a specific compositor's
/// output — may behave differently, and the honest position is that the
/// default is well justified rather than that the question is closed for all
/// content. The CPU budget governor degrades whichever is chosen rather than
/// letting it take the machine.
///
/// Both earlier caveats on this decision are now resolved: the numbers above
/// are with the assembly, and quality was measured once there was a decoder to
/// measure it with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Tuning {
    /// openh264's default. Six times cheaper on the measurement above.
    #[default]
    CameraVideo,
    /// Screen-content mode. Fewer bytes, far more CPU.
    ScreenContent,
}

impl Tuning {
    /// A name for the session HUD and the log.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::CameraVideo => "camera",
            Self::ScreenContent => "screen-content",
        }
    }
}

/// How the encoder should behave.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EncoderConfig {
    /// Picture size. Must be even in both directions; an [`I420`] already is.
    pub width: u32,
    pub height: u32,
    /// The frame rate the encoder should assume for its rate model.
    ///
    /// Not a promise to deliver it — the damage gate decides that — but the
    /// encoder's own rate control needs a number, and telling it 60 while
    /// sending 2 makes it allocate bits as though motion were coming.
    pub fps: u32,
    /// Target bits per second. Set by congestion control, not by the encoder.
    pub bitrate: u32,
    /// Seconds between keyframes at most. The gate also asks for them
    /// explicitly; this is the encoder's own floor.
    pub keyframe_interval: Duration,
    /// The colour space the planes are in, so the bitstream can describe them.
    pub space: ColorSpace,
    /// How many threads the encoder may use.
    ///
    /// Bounded by the CPU budget rather than by the core count. A host is
    /// usually not the viewer's machine, and an encoder that took every core
    /// would be a product failure even with a perfect picture.
    pub threads: u16,
    /// What the encoder should assume it is looking at. See [`Tuning`] — the
    /// obvious answer is six times more expensive than the default.
    pub tuning: Tuning,
}

impl Default for EncoderConfig {
    fn default() -> Self {
        Self {
            width: 1920,
            height: 1080,
            fps: 30,
            // 4 Mbit/s: within the design's "< 5 Mbit/s at balanced" for
            // full-motion 1080p30, and congestion control moves it at once.
            bitrate: 4_000_000,
            keyframe_interval: Duration::from_secs(5),
            space: ColorSpace::default(),
            threads: 1,
            tuning: Tuning::default(),
        }
    }
}

impl EncoderConfig {
    /// A configuration for a picture of this size, keeping other defaults.
    #[must_use]
    pub fn for_size(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            ..Self::default()
        }
    }

    /// Whether this can be acted on.
    #[must_use]
    pub const fn is_well_formed(&self) -> bool {
        self.width >= 16
            && self.height >= 16
            && self.width.is_multiple_of(2)
            && self.height.is_multiple_of(2)
            && self.fps > 0
            && self.bitrate > 0
            && self.threads > 0
    }
}

/// One encoded picture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Encoded {
    /// The bitstream, in Annex-B framing — NAL units separated by start codes.
    ///
    /// Annex-B rather than length-prefixed because that is what openh264
    /// produces and what a file or a pipe expects. Packetising for QUIC is a
    /// separate step with its own decisions about how a lost datagram is
    /// recovered, and doing it here would bake one answer into the encoder.
    pub bitstream: Vec<u8>,
    /// Whether a decoder can start from this picture alone.
    pub keyframe: bool,
    /// How long the encode took.
    pub took: Duration,
}

impl Encoded {
    /// Bytes on the wire.
    #[must_use]
    pub fn len(&self) -> usize {
        self.bitstream.len()
    }

    /// Whether the encoder produced nothing.
    ///
    /// A real answer, not a failure: an encoder given a picture identical to
    /// the last one may legitimately emit no slice data.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.bitstream.is_empty()
    }
}

/// What went wrong.
#[derive(Debug, thiserror::Error)]
pub enum EncodeError {
    /// The configuration cannot be acted on.
    #[error("a {width}x{height} picture at {fps} fps and {bitrate} bit/s cannot be encoded")]
    BadConfig {
        width: u32,
        height: u32,
        fps: u32,
        bitrate: u32,
    },

    /// The picture does not match what the encoder was configured for.
    ///
    /// Its own variant because it is a caller mistake with an obvious remedy —
    /// reconfigure — and because an encoder that quietly encoded a
    /// differently-sized picture would produce a bitstream a decoder cannot
    /// use.
    #[error("the encoder is configured for {want_w}x{want_h} but was given {got_w}x{got_h}")]
    SizeMismatch {
        want_w: u32,
        want_h: u32,
        got_w: u32,
        got_h: u32,
    },

    /// The backend failed.
    #[error("{backend}: {detail}")]
    Backend {
        backend: &'static str,
        detail: String,
    },
}

/// Running totals, for the session HUD and the log.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct EncodeStats {
    pub frames: u64,
    pub keyframes: u64,
    pub bytes: u64,
    /// Total time spent encoding.
    pub spent: Duration,
    /// The slowest single frame, which is what a stutter looks like.
    ///
    /// Reported alongside the mean because a mean of 8 ms hides a 60 ms frame,
    /// and it is the 60 ms frame the user noticed.
    pub worst: Duration,
}

impl EncodeStats {
    /// Fold one encode into the totals.
    pub fn record(&mut self, encoded: &Encoded) {
        self.frames = self.frames.saturating_add(1);
        if encoded.keyframe {
            self.keyframes = self.keyframes.saturating_add(1);
        }
        self.bytes = self.bytes.saturating_add(encoded.len() as u64);
        self.spent = self.spent.saturating_add(encoded.took);
        if encoded.took > self.worst {
            self.worst = encoded.took;
        }
    }

    /// Mean milliseconds per frame.
    #[must_use]
    pub fn mean_ms(&self) -> f64 {
        if self.frames == 0 {
            return 0.0;
        }
        self.spent.as_secs_f64() * 1000.0 / self.frames as f64
    }

    /// Bits per second, given how long the session has run.
    #[must_use]
    pub fn bitrate(&self, over: Duration) -> u64 {
        let secs = over.as_secs_f64();
        if secs <= 0.0 {
            return 0;
        }
        ((self.bytes as f64) * 8.0 / secs) as u64
    }

    /// Share of one core spent encoding, over a wall-clock period.
    ///
    /// The number the design's CPU ceiling is written in, and the one a host's
    /// owner cares about: not how fast a frame was, but how much of their
    /// machine this is taking.
    #[must_use]
    pub fn core_share(&self, over: Duration) -> f64 {
        let secs = over.as_secs_f64();
        if secs <= 0.0 {
            return 0.0;
        }
        self.spent.as_secs_f64() / secs
    }
}

/// Something that turns pictures into a bitstream.
pub trait VideoEncoder: std::fmt::Debug + Send {
    /// Which backend this is, for the HUD and the log.
    fn backend(&self) -> &'static str;

    /// Whether this build or device has its fast path.
    ///
    /// For openh264 that means the assembly; for a future hardware backend it
    /// would mean the device actually working. Either way a host should be
    /// able to say why it is slow.
    fn is_fast_path(&self) -> bool;

    /// The configuration in force.
    fn config(&self) -> &EncoderConfig;

    /// Change the configuration.
    ///
    /// A separate step from encoding because a resolution change invalidates
    /// every reference the decoder holds, and the caller has to know that
    /// happened. Implementations force a keyframe on the next picture.
    fn reconfigure(&mut self, config: &EncoderConfig) -> Result<(), EncodeError>;

    /// Encode one picture.
    ///
    /// `keyframe` demands one; an encoder may produce one anyway.
    fn encode(&mut self, picture: &I420, keyframe: bool) -> Result<Encoded, EncodeError>;

    /// Running totals.
    fn stats(&self) -> EncodeStats;
}

/// Time an encode, for implementations to use.
pub(crate) fn timed<T, F: FnOnce() -> T>(f: F) -> (T, Duration) {
    let at = Instant::now();
    let out = f();
    (out, at.elapsed())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_configuration_is_usable() {
        let c = EncoderConfig::default();
        assert!(c.is_well_formed());
        assert_eq!((c.width, c.height), (1920, 1080));
        assert_eq!(
            c.threads, 1,
            "an encoder must not default to taking every core of someone \
             else's machine"
        );
        assert!(
            c.bitrate <= 5_000_000,
            "the design's ceiling for balanced 1080p30 is 5 Mbit/s"
        );
    }

    #[test]
    fn an_odd_or_tiny_picture_is_not_well_formed() {
        for (w, h) in [(1919u32, 1080u32), (1920, 1079), (8, 8), (0, 0)] {
            assert!(
                !EncoderConfig::for_size(w, h).is_well_formed(),
                "{w}x{h} should be refused"
            );
        }
        assert!(EncoderConfig::for_size(16, 16).is_well_formed());
    }

    #[test]
    fn a_zero_in_any_rate_is_not_well_formed() {
        let base = EncoderConfig::default();
        assert!(!EncoderConfig { fps: 0, ..base }.is_well_formed());
        assert!(!EncoderConfig { bitrate: 0, ..base }.is_well_formed());
        assert!(!EncoderConfig { threads: 0, ..base }.is_well_formed());
    }

    fn encoded(bytes: usize, keyframe: bool, ms: u64) -> Encoded {
        Encoded {
            bitstream: vec![0u8; bytes],
            keyframe,
            took: Duration::from_millis(ms),
        }
    }

    #[test]
    fn the_statistics_add_up() {
        let mut s = EncodeStats::default();
        s.record(&encoded(20_000, true, 20));
        s.record(&encoded(3_000, false, 8));
        s.record(&encoded(3_500, false, 6));

        assert_eq!(s.frames, 3);
        assert_eq!(s.keyframes, 1);
        assert_eq!(s.bytes, 26_500);
        assert_eq!(s.spent, Duration::from_millis(34));
        assert!((s.mean_ms() - 34.0 / 3.0).abs() < 0.001);
    }

    /// A mean of 8 ms hides a 60 ms frame, and it is the 60 ms frame the user
    /// noticed.
    #[test]
    fn the_worst_frame_is_kept_not_averaged_away() {
        let mut s = EncodeStats::default();
        for _ in 0..100 {
            s.record(&encoded(3_000, false, 8));
        }
        s.record(&encoded(3_000, false, 60));
        assert_eq!(s.worst, Duration::from_millis(60));
        assert!(
            s.mean_ms() < 9.0,
            "the mean should be near 8, and that is exactly why the worst is \
             reported separately: {}",
            s.mean_ms()
        );
    }

    /// The number the CPU ceiling is written in.
    #[test]
    fn the_core_share_is_time_spent_over_time_elapsed() {
        let mut s = EncodeStats::default();
        // 30 frames of 8 ms in one second is 240 ms of work: 24% of a core.
        for _ in 0..30 {
            s.record(&encoded(3_000, false, 8));
        }
        let share = s.core_share(Duration::from_secs(1));
        assert!((share - 0.24).abs() < 0.001, "got {share}");

        // And it is a share of *one* core, so it can exceed 1 when threaded.
        assert!(s.core_share(Duration::from_millis(100)) > 1.0);
    }

    #[test]
    fn statistics_over_no_time_at_all_do_not_divide_by_zero() {
        let s = EncodeStats::default();
        assert_eq!(s.mean_ms(), 0.0);
        assert_eq!(s.bitrate(Duration::ZERO), 0);
        assert_eq!(s.core_share(Duration::ZERO), 0.0);
    }

    #[test]
    fn bitrate_is_bits_not_bytes() {
        let mut s = EncodeStats::default();
        s.record(&encoded(125_000, true, 5));
        assert_eq!(
            s.bitrate(Duration::from_secs(1)),
            1_000_000,
            "125 kB in a second is 1 Mbit/s"
        );
    }

    /// An empty bitstream is a real answer — a picture identical to the last
    /// one may produce no slice data — not a failure to report.
    #[test]
    fn an_empty_bitstream_is_a_legitimate_result() {
        let e = encoded(0, false, 1);
        assert!(e.is_empty());
        assert_eq!(e.len(), 0);
        let mut s = EncodeStats::default();
        s.record(&e);
        assert_eq!(s.frames, 1);
        assert_eq!(s.bytes, 0);
    }

    /// Whether this build has the assembly must be knowable at run time, or a
    /// host that is a few per cent off the published numbers cannot say why.
    #[test]
    fn whether_the_build_has_assembly_is_reportable() {
        // Whichever it is, it must be a definite answer.
        let fast = has_assembly();
        assert_eq!(fast, !cfg!(slow_encoder));
    }
}
