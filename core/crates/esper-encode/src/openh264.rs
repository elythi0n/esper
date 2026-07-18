//! The openh264 backend.
//!
//! The only encoder in v1, and the reasons are in the project's codec design:
//! one code path instead of five, and identical behaviour on every host —
//! including virtual machines, containers, and machines whose GPU driver does
//! not work, which is a large fraction of real hosts.
//!
//! # Three settings worth explaining
//!
//! **The tuning is a choice, not a default.** openh264 has a screen-content
//! mode, and picking it for a remote desktop looks obvious. Measured, it costs
//! six times the CPU for 40% fewer bytes — so the default is the cheap one.
//! See [`crate::encoder::Tuning`], which carries the numbers and the two
//! caveats that could change the answer.
//!
//! **Frame skipping is off, and that weakens the bitrate ceiling.** openh264
//! says so itself, in a warning on startup: without frame skip it cannot
//! *guarantee* a bitrate in any of its rate-control modes, only aim at one.
//!
//! That is the right trade here and it is deliberate. Which frames are sent is
//! the damage gate's decision, made from what actually changed on screen; an
//! encoder silently dropping frames underneath it would be a second, invisible
//! policy fighting the first. The consequence is that congestion control has
//! to act on the bitrate **measured coming out** rather than trusting the
//! encoder to have hit its target.
//!
//! **The VUI is written.** The colour space this crate converted into is
//! described in the bitstream, so a decoder inverts what was actually done
//! rather than what it assumed. Getting this wrong shifts every colour
//! slightly, which looks like a driver fault rather than an arithmetic one.

use esper_color::i420::I420;
use esper_color::space::{Matrix, Range};
use openh264::OpenH264API;
use openh264::encoder::{
    BitRate, Complexity, Encoder as Inner, EncoderConfig as InnerConfig, FrameRate, FrameType,
    IntraFramePeriod, Level, Profile, QpRange, RateControlMode, UsageType, VuiConfig,
};
use openh264::formats::YUVSource;

use crate::encoder::{
    EncodeError, EncodeStats, Encoded, EncoderConfig, Tuning, VideoEncoder, has_assembly, timed,
};

/// The backend name, for the HUD and the log.
pub const BACKEND: &str = "openh264";

/// A software H.264 encoder.
pub struct OpenH264 {
    inner: Inner,
    config: EncoderConfig,
    stats: EncodeStats,
    /// Set when the next picture must be a keyframe regardless of what the
    /// caller asks for — after a reconfigure, because every reference the
    /// decoder holds describes a different picture size.
    owed_keyframe: bool,
}

impl std::fmt::Debug for OpenH264 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenH264")
            .field("size", &(self.config.width, self.config.height))
            .field("bitrate", &self.config.bitrate)
            .field("threads", &self.config.threads)
            .field("assembly", &has_assembly())
            .finish_non_exhaustive()
    }
}

impl OpenH264 {
    /// An encoder for this configuration.
    pub fn new(config: &EncoderConfig) -> Result<Self, EncodeError> {
        let inner = build(config)?;
        Ok(Self {
            inner,
            config: *config,
            stats: EncodeStats::default(),
            // The first picture has to be a keyframe: a decoder holding no
            // reference cannot use anything else.
            owed_keyframe: true,
        })
    }
}

/// Build the underlying encoder for a configuration.
fn build(config: &EncoderConfig) -> Result<Inner, EncodeError> {
    if !config.is_well_formed() {
        return Err(EncodeError::BadConfig {
            width: config.width,
            height: config.height,
            fps: config.fps,
            bitrate: config.bitrate,
        });
    }

    let vui = match (config.space.matrix, config.space.range) {
        (Matrix::Bt709, Range::Limited) => VuiConfig::bt709(),
        (Matrix::Bt709, Range::Full) => VuiConfig::bt709_full(),
        // The wrapper offers no full-range BT.601 constructor. Limited is
        // what BT.601 content is in practice, and the conversion side
        // defaults to BT.709 anyway — so rather than hand-building a VUI to
        // support a combination nothing asks for, this is the one place the
        // range is not honoured, and it is stated rather than silent.
        (Matrix::Bt601, _) => VuiConfig::bt601(),
    };

    let usage = match config.tuning {
        Tuning::CameraVideo => UsageType::CameraVideoRealTime,
        Tuning::ScreenContent => UsageType::ScreenContentRealTime,
    };

    let inner_config = InnerConfig::new()
        .usage_type(usage)
        // Frame skipping off: which frames are sent is the damage gate's
        // decision, and an encoder dropping them underneath it would be a
        // second policy fighting the first. openh264 warns that this stops it
        // guaranteeing a bitrate — see the module documentation.
        .skip_frames(false)
        // Main profile: the design's ceiling, decodable by every webview.
        .profile(Profile::Main)
        .level(Level::Level_4_0)
        // **Quality mode, not bitrate mode**, and this is the single most
        // consequential setting in the file.
        //
        // Bitrate mode was the obvious choice — congestion control drives the
        // rate, so tell the encoder the rate. Measured on an unchanged
        // 320x240 picture after a keyframe:
        //
        //   bitrate mode      958 bytes
        //   quality mode       20 bytes
        //
        // Forty-eight times. Bitrate mode aims *at* its target, so it spends
        // its budget refining a picture that did not change. At 30 fps that
        // is 230 kbit/s for a static screen against 4.8 — and the design's
        // claim is that reading a document costs single-digit kilobits per
        // second, which bitrate mode simply does not deliver.
        //
        // Congestion control still drives quality; it does it through the QP
        // range and the framerate and resolution ladder rather than by
        // pinning a rate. That is also the order the design degrades in:
        // framerate first, then chroma, then resolution.
        //
        // Neither mode guarantees a ceiling anyway — openh264 says so on
        // startup, because frame skipping is off — so congestion control has
        // to act on the bitrate measured coming out. See the module
        // documentation.
        .rate_control_mode(RateControlMode::Quality)
        .bitrate(BitRate::from_bps(config.bitrate))
        .max_frame_rate(FrameRate::from_hz(config.fps as f32))
        .intra_frame_period(IntraFramePeriod::from_num_frames(keyframe_period_frames(
            config,
        )))
        // Bounded by the CPU budget rather than the core count. A host is
        // usually not the viewer's machine.
        .num_threads(config.threads)
        .complexity(Complexity::Low)
        .vui(vui)
        .debug(false);

    // The fidelity rung of the CPU budget's ladder.
    let inner_config = match config.qp {
        Some((min, max)) => inner_config.qp(QpRange::new(min, max)),
        None => inner_config,
    };

    // Slicing, which is what makes `threads` mean anything. Applied
    // conditionally so the default stays one slice per picture — the most
    // efficient way to code a frame.
    let inner_config = match config.max_slice_bytes {
        Some(bytes) => inner_config.max_slice_len(bytes),
        None => inner_config,
    };

    Inner::with_api_config(OpenH264API::from_source(), inner_config).map_err(|e| {
        EncodeError::Backend {
            backend: BACKEND,
            detail: e.to_string(),
        }
    })
}

/// The keyframe interval, in frames, which is what openh264 wants.
///
/// Clamped to at least one: a period of zero would ask for a keyframe every
/// frame, which is the opposite of what a zero-length interval means to a
/// person setting it.
fn keyframe_period_frames(config: &EncoderConfig) -> u32 {
    let frames = config
        .keyframe_interval
        .as_secs_f64()
        .max(0.0)
        .mul_add(f64::from(config.fps), 0.5) as u32;
    frames.max(1)
}

/// Adapts an [`I420`] to what openh264 reads.
///
/// A borrow rather than a copy: the planes are 1.5 bytes per pixel and this
/// runs every frame.
struct Planes<'a>(&'a I420);

impl YUVSource for Planes<'_> {
    fn dimensions(&self) -> (usize, usize) {
        (self.0.width() as usize, self.0.height() as usize)
    }

    fn strides(&self) -> (usize, usize, usize) {
        (self.0.y_stride(), self.0.uv_stride(), self.0.uv_stride())
    }

    fn y(&self) -> &[u8] {
        self.0.y()
    }

    fn u(&self) -> &[u8] {
        self.0.u()
    }

    fn v(&self) -> &[u8] {
        self.0.v()
    }
}

impl VideoEncoder for OpenH264 {
    fn backend(&self) -> &'static str {
        BACKEND
    }

    fn is_fast_path(&self) -> bool {
        has_assembly()
    }

    fn config(&self) -> &EncoderConfig {
        &self.config
    }

    fn reconfigure(&mut self, config: &EncoderConfig) -> Result<(), EncodeError> {
        // A fresh encoder rather than `SetOption`. openh264 accepts some
        // changes in place and silently ignores others, and the difference is
        // version-dependent — so the encoder is rebuilt, which is
        // unambiguous and happens at most a few times a session.
        self.inner = build(config)?;
        self.config = *config;
        // Every reference the decoder holds describes a different picture
        // size now, so the next frame cannot be a delta against any of them.
        self.owed_keyframe = true;
        Ok(())
    }

    fn encode(&mut self, picture: &I420, keyframe: bool) -> Result<Encoded, EncodeError> {
        if picture.width() != self.config.width || picture.height() != self.config.height {
            return Err(EncodeError::SizeMismatch {
                want_w: self.config.width,
                want_h: self.config.height,
                got_w: picture.width(),
                got_h: picture.height(),
            });
        }

        if keyframe || self.owed_keyframe {
            self.inner.force_intra_frame();
            self.owed_keyframe = false;
        }

        let planes = Planes(picture);
        let (result, took) = timed(|| self.inner.encode(&planes));
        let stream = result.map_err(|e| EncodeError::Backend {
            backend: BACKEND,
            detail: e.to_string(),
        })?;

        // Whether a decoder can start here, taken from the encoder rather
        // than guessed from the NAL types. `Skip` is a real answer — the
        // encoder decided this picture needed no bits — and treating it as a
        // keyframe would tell a late-joining viewer it could start on a frame
        // that contains nothing.
        let is_key = matches!(stream.frame_type(), FrameType::IDR | FrameType::I);
        let encoded = Encoded {
            bitstream: stream.to_vec(),
            keyframe: is_key,
            took,
        };
        self.stats.record(&encoded);
        Ok(encoded)
    }

    fn stats(&self) -> EncodeStats {
        self.stats
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use esper_color::space::ColorSpace;

    /// A picture with something in it, so the encoder has work to do.
    fn picture(w: u32, h: u32, seed: u8) -> I420 {
        let mut img = I420::new(w, h).expect("valid");
        let (y, u, v) = img.planes_mut();
        for (i, p) in y.iter_mut().enumerate() {
            *p = ((i / 13) as u8).wrapping_add(seed);
        }
        u.fill(110u8.wrapping_add(seed));
        v.fill(140u8.wrapping_add(seed));
        img
    }

    fn small() -> EncoderConfig {
        EncoderConfig {
            fps: 30,
            ..EncoderConfig::for_size(320, 240)
        }
    }

    #[test]
    fn it_produces_a_keyframe_first() {
        let mut enc = OpenH264::new(&small()).expect("encoder");
        let out = enc.encode(&picture(320, 240, 0), false).expect("encode");
        assert!(
            out.keyframe,
            "the first picture must be a keyframe however it was asked for — a \
             decoder holding no reference cannot use anything else"
        );
        assert!(!out.is_empty());
        // Annex-B framing: a start code, then a NAL header.
        assert_eq!(
            out.bitstream.get(..4),
            Some(&[0, 0, 0, 1][..]),
            "expected an Annex-B start code"
        );
        // NAL type 7 is a sequence parameter set, which a keyframe carries.
        let nal_type = out.bitstream.get(4).copied().unwrap_or(0) & 0x1F;
        assert_eq!(nal_type, 7, "a keyframe should begin with an SPS");
    }

    /// The property the whole product rests on: a screen that has not changed
    /// costs almost nothing to send.
    ///
    /// Deliberately the *same* picture twice. An earlier version shifted every
    /// pixel between frames and then asserted the delta was smaller than the
    /// keyframe — which is not true in general and stopped being true the
    /// moment the encoder tuning changed. A delta of a picture that moved
    /// entirely can legitimately exceed an I-frame of a simple one; a delta of
    /// a picture that did not move cannot.
    #[test]
    fn an_unchanged_picture_costs_almost_nothing() {
        let mut enc = OpenH264::new(&small()).expect("encoder");
        let same = picture(320, 240, 0);

        let first = enc.encode(&same, false).expect("encode");
        assert!(first.keyframe);

        let second = enc.encode(&same, false).expect("encode");
        assert!(!second.keyframe, "the second picture should be a delta");
        assert!(
            second.len() * 10 < first.len(),
            "an unchanged picture cost {} bytes against a keyframe's {} — the \
             product's central claim is that this is nearly free",
            second.len(),
            first.len()
        );
    }

    /// And a picture that *did* change costs more than one that did not, which
    /// is the other half of the same property.
    #[test]
    fn a_changed_picture_costs_more_than_an_unchanged_one() {
        let mut enc = OpenH264::new(&small()).expect("encoder");
        let a = picture(320, 240, 0);
        let _ = enc.encode(&a, false).expect("encode");

        let unchanged = enc.encode(&a, false).expect("encode").len();
        let changed = enc
            .encode(&picture(320, 240, 90), false)
            .expect("encode")
            .len();

        assert!(
            changed > unchanged,
            "a changed picture cost {changed} bytes and an unchanged one \
             {unchanged} — the encoder is not tracking the difference"
        );
    }

    /// A decoder that has lost sync is stuck until it gets a keyframe.
    #[test]
    fn a_keyframe_can_be_demanded() {
        let mut enc = OpenH264::new(&small()).expect("encoder");
        let _ = enc.encode(&picture(320, 240, 0), false).expect("encode");
        let _ = enc.encode(&picture(320, 240, 1), false).expect("encode");

        let forced = enc.encode(&picture(320, 240, 2), true).expect("encode");
        assert!(forced.keyframe, "a demanded keyframe must be delivered");
    }

    /// A picture of the wrong size would produce a bitstream a decoder cannot
    /// use, so it is refused rather than encoded.
    #[test]
    fn a_picture_of_the_wrong_size_is_refused() {
        let mut enc = OpenH264::new(&small()).expect("encoder");
        let err = enc
            .encode(&picture(640, 480, 0), false)
            .expect_err("wrong size");
        match err {
            EncodeError::SizeMismatch {
                want_w,
                want_h,
                got_w,
                got_h,
            } => {
                assert_eq!((want_w, want_h), (320, 240));
                assert_eq!((got_w, got_h), (640, 480));
            }
            other => panic!("{other:?}"),
        }
    }

    /// A resolution change invalidates every reference the decoder holds, so
    /// the next picture has to be a keyframe whether or not anyone asked.
    #[test]
    fn a_reconfigure_forces_the_next_picture_to_be_a_keyframe() {
        let mut enc = OpenH264::new(&small()).expect("encoder");
        let _ = enc.encode(&picture(320, 240, 0), false).expect("encode");
        let delta = enc.encode(&picture(320, 240, 1), false).expect("encode");
        assert!(!delta.keyframe);

        enc.reconfigure(&EncoderConfig {
            fps: 30,
            ..EncoderConfig::for_size(640, 480)
        })
        .expect("reconfigure");
        assert_eq!(enc.config().width, 640);

        let after = enc.encode(&picture(640, 480, 2), false).expect("encode");
        assert!(
            after.keyframe,
            "every reference describes a 320x240 picture now"
        );
    }

    #[test]
    fn an_unusable_configuration_is_refused() {
        let err = OpenH264::new(&EncoderConfig {
            fps: 0,
            ..EncoderConfig::for_size(320, 240)
        })
        .expect_err("zero fps");
        assert!(matches!(err, EncodeError::BadConfig { .. }), "{err:?}");

        assert!(OpenH264::new(&EncoderConfig::for_size(319, 240)).is_err());
    }

    #[test]
    fn the_statistics_track_what_was_encoded() {
        let mut enc = OpenH264::new(&small()).expect("encoder");
        for i in 0..5u8 {
            let _ = enc.encode(&picture(320, 240, i), false).expect("encode");
        }
        let s = enc.stats();
        assert_eq!(s.frames, 5);
        assert_eq!(s.keyframes, 1, "only the first");
        assert!(s.bytes > 0);
        assert!(s.mean_ms() > 0.0);
        assert!(s.worst >= std::time::Duration::ZERO);
    }

    /// The keyframe period is in frames, and a zero-length interval must not
    /// mean "every frame" — which is the opposite of what someone setting it
    /// to zero intends.
    #[test]
    fn the_keyframe_period_is_frames_and_never_zero() {
        let c = EncoderConfig {
            fps: 30,
            keyframe_interval: std::time::Duration::from_secs(5),
            ..EncoderConfig::default()
        };
        assert_eq!(keyframe_period_frames(&c), 150);

        let zero = EncoderConfig {
            keyframe_interval: std::time::Duration::ZERO,
            ..c
        };
        assert_eq!(keyframe_period_frames(&zero), 1);

        // And a fractional second rounds rather than truncating to zero.
        let short = EncoderConfig {
            keyframe_interval: std::time::Duration::from_millis(100),
            ..c
        };
        assert_eq!(keyframe_period_frames(&short), 3);
    }

    /// Every colour space must produce a working encoder, because the VUI is
    /// built from a match and a missing arm would be a runtime failure rather
    /// than a compile error.
    #[test]
    fn every_colour_space_configures() {
        for space in esper_color::convert::all_spaces() {
            let mut enc = OpenH264::new(&EncoderConfig {
                space,
                fps: 30,
                ..EncoderConfig::for_size(320, 240)
            })
            .unwrap_or_else(|e| panic!("{}: {e}", space.describe()));
            let out = enc
                .encode(&picture(320, 240, 0), false)
                .unwrap_or_else(|e| panic!("{}: {e}", space.describe()));
            assert!(out.keyframe, "{}", space.describe());
        }
    }

    #[test]
    fn the_backend_reports_itself_and_whether_it_is_fast() {
        let enc = OpenH264::new(&small()).expect("encoder");
        assert_eq!(enc.backend(), "openh264");
        assert_eq!(enc.is_fast_path(), has_assembly());
        // Debug must say the useful things and not dump a bitstream.
        let d = format!("{enc:?}");
        assert!(d.contains("320") && d.contains("assembly"), "{d}");
        assert!(d.len() < 200, "{} characters", d.len());
    }

    /// The colour space defaults must reach the encoder rather than being
    /// dropped on the way.
    #[test]
    fn the_default_colour_space_is_the_conservative_one() {
        let enc = OpenH264::new(&small()).expect("encoder");
        assert_eq!(enc.config().space, ColorSpace::default());
        assert_eq!(enc.config().space.matrix, Matrix::Bt709);
        assert_eq!(enc.config().space.range, Range::Limited);
    }
}
