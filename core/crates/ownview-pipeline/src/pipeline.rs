//! The media path as one thing.
//!
//! Capture, the damage gate, colour conversion, encode — and optionally a
//! decode, for a loopback that can check its own output. Each stage is timed
//! in both wall clock and CPU, and the governor is told what it cost.
//!
//! # Why this exists as a type rather than a function
//!
//! Every stage has been measured on its own. Running them together is the
//! first time they share a clock, a buffer and a budget, and that is where the
//! interesting failures are: a conversion that is fast in isolation and starves
//! the encoder of cache, a gate that skips a frame whose damage the converter
//! needed, a governor told a cost that excludes half the pipeline.
//!
//! # What the governor can and cannot do here
//!
//! It has three levers, and two of them now reach something:
//!
//! | Lever | Reaches | Status |
//! |---|---|---|
//! | Framerate | the damage gate's rate ceiling | applied |
//! | Fidelity | the encoder's quantiser range | applied |
//! | **Resolution** | nothing | **not applied** |
//!
//! Resolution needs a scaler and there is not one. That is stated here, and
//! reported by [`Pipeline::unapplied_levers`], rather than silently ignored —
//! because a governor that appears to reduce resolution and does not would
//! make the bottom of its ladder look like it worked.
//!
//! Two of those three arrived by being caught rather than by being built.
//! Fidelity had nowhere to land at all — the encoder had no quantiser setting.
//! And framerate reached the *encoder's rate model* but not the gate, so the
//! governor would report 15 fps while the gate went on admitting frames at 60:
//! the pipeline looked degraded and cost exactly the same. The first run of
//! `ownview-engine loopback` printed "60 fps" for a session asked to run at 30,
//! which is what gave it away.

use std::time::{Duration, Instant};

use ownview_capture::capturer::{Capture, CaptureError, Capturer};
use ownview_capture::damage::Damage;
use ownview_capture::gate::{EncodeGate, Policy, Verdict};
use ownview_capture::geometry::Rect;
use ownview_color::convert;
use ownview_color::{ColorSpace, I420};
use ownview_decode::decoder::{DecodeError, VideoDecoder};
use ownview_encode::budget::{Budget, Governor, Moved};
use ownview_encode::encoder::{EncodeError, EncoderConfig, VideoEncoder};

use crate::timing::{Stage, Timer};

/// What one turn of the pipeline did.
#[derive(Debug)]
pub enum Step {
    /// A frame was captured and encoded.
    Encoded {
        /// Whether a decoder can start from it.
        keyframe: bool,
        /// Bytes on the wire.
        bytes: usize,
        /// The decoded picture, when a decoder is attached.
        decoded: Option<I420>,
    },
    /// A frame was captured and the gate skipped it.
    Skipped,
    /// Nothing changed before the deadline.
    Unchanged,
    /// The monitor's geometry changed and the pipeline reconfigured.
    Reconfigured { width: u32, height: u32 },
    /// The capture source has finished.
    Finished,
}

/// What went wrong.
#[derive(Debug, thiserror::Error)]
pub enum PipelineError {
    #[error(transparent)]
    Capture(#[from] CaptureError),
    #[error(transparent)]
    Convert(#[from] convert::ConvertError),
    #[error(transparent)]
    Encode(#[from] EncodeError),
    #[error(transparent)]
    Decode(#[from] DecodeError),
    #[error(transparent)]
    Buffer(#[from] ownview_color::i420::I420Error),
}

/// Running totals for the whole path.
#[derive(Debug, Clone, Copy, Default)]
pub struct PipelineStats {
    pub capture: Stage,
    pub convert: Stage,
    pub encode: Stage,
    pub decode: Stage,
    pub frames_captured: u64,
    pub frames_encoded: u64,
    pub frames_skipped: u64,
    pub keyframes: u64,
    pub bytes: u64,
    /// Luma samples actually converted.
    ///
    /// Reported against what a whole frame would have cost, because the ratio
    /// is the entire case for damage-driven conversion and a single number
    /// hides it.
    pub pixels_converted: u64,
    pub pixels_if_whole: u64,
}

impl PipelineStats {
    /// CPU time across every stage, where every stage could be measured.
    #[must_use]
    pub fn total_cpu(&self) -> Option<Duration> {
        let mut total = Duration::ZERO;
        for stage in [&self.capture, &self.convert, &self.encode, &self.decode] {
            // A stage that never ran contributes nothing rather than nothing
            // *known*: a decoder that was not attached must not poison the
            // total.
            if stage.calls == 0 {
                continue;
            }
            total = total.saturating_add(stage.cpu?);
        }
        Some(total)
    }

    /// Share of one core the whole path used over a period.
    #[must_use]
    pub fn core_share(&self, over: Duration) -> Option<f64> {
        let secs = over.as_secs_f64();
        if secs <= 0.0 {
            return None;
        }
        Some(self.total_cpu()?.as_secs_f64() / secs)
    }

    /// Bits per second produced.
    #[must_use]
    pub fn bitrate(&self, over: Duration) -> u64 {
        let secs = over.as_secs_f64();
        if secs <= 0.0 {
            return 0;
        }
        ((self.bytes as f64) * 8.0 / secs) as u64
    }

    /// Fraction of pixels that had to be converted.
    #[must_use]
    pub fn conversion_share(&self) -> f64 {
        if self.pixels_if_whole == 0 {
            return 0.0;
        }
        self.pixels_converted as f64 / self.pixels_if_whole as f64
    }
}

/// A lever the governor has that the pipeline cannot act on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unapplied {
    /// Resolution, because there is no scaler.
    Resolution,
}

impl Unapplied {
    /// Why, for the log and the operator.
    #[must_use]
    pub const fn reason(self) -> &'static str {
        match self {
            Self::Resolution => {
                "the resolution rung needs a scaler, and there is not one yet — \
                 so the bottom of the CPU budget's ladder reduces framerate and \
                 quality but not size"
            }
        }
    }
}

/// Builds an encoder for a configuration.
///
/// A function rather than a concrete type, so the pipeline can be handed a
/// software encoder now and a hardware one in v1.x without changing shape —
/// and so a test can hand it one that fails on demand.
///
/// It takes a configuration each time because the pipeline **rebuilds** the
/// encoder when the governor moves a rung, and a factory that captured its
/// settings once could not follow.
pub type EncoderFactory<'a> =
    &'a dyn Fn(&EncoderConfig) -> Result<Box<dyn VideoEncoder>, EncodeError>;

/// The whole media path.
pub struct Pipeline {
    capturer: Box<dyn Capturer>,
    gate: EncodeGate,
    governor: Governor,
    space: ColorSpace,
    /// Reused across frames, and holding the previous one — which is what
    /// makes damage-driven conversion correct.
    staging: I420,
    encoder: Box<dyn VideoEncoder>,
    decoder: Option<Box<dyn VideoDecoder>>,
    /// Set when the staging buffer cannot be trusted to hold the previous
    /// frame: the first frame, and after any reshape.
    needs_whole_frame: bool,
    stats: PipelineStats,
    started: Instant,
    /// The level last pushed to the encoder, so it is only reconfigured when
    /// something actually changed.
    applied_level: Option<ownview_encode::budget::Level>,
    /// The frame rate the caller asked for.
    ///
    /// The governor's ladder is **relative to this, not absolute**: its top
    /// rung is 60 fps, so a session asked for 30 must not be raised to 60 the
    /// moment the governor recovers. The effective ceiling is the lower of the
    /// two.
    requested_fps: u32,
}

impl std::fmt::Debug for Pipeline {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pipeline")
            .field("capture", &self.capturer.backend())
            .field("encoder", &self.encoder.backend())
            .field("decoder", &self.decoder.as_ref().map(|d| d.backend()))
            .field("level", &self.governor.level().describe())
            .finish_non_exhaustive()
    }
}

impl Pipeline {
    /// Build a pipeline around a capture source.
    ///
    /// The encoder is configured from the budget, so its thread count and
    /// slicing follow from what the host may spend rather than from what the
    /// machine happens to have.
    pub fn new(
        capturer: Box<dyn Capturer>,
        encoder_for: EncoderFactory<'_>,
        budget: Budget,
        policy: Policy,
    ) -> Result<Self, PipelineError> {
        let monitor = capturer.selected();
        let (width, height) = capturer
            .monitors()
            .iter()
            .find(|m| m.id == monitor)
            .map_or((1920, 1080), ownview_capture::capturer::MonitorInfo::size);

        let staging = I420::new(width, height)?;
        let governor = Governor::new(budget, ownview_encode::budget::Reflexes::default());
        // Clamped to the requested rate from the start, for the same reason
        // the recovery path is: the ladder's top rung is 60 fps and the caller
        // may have asked for 30.
        let mut level = governor.level();
        level.fps = level.fps.min(policy.max_fps.max(1)).max(1);
        let config = EncoderConfig::for_budget(staging.width(), staging.height(), &budget)
            .with_level(&level);
        let encoder = encoder_for(&config)?;

        Ok(Self {
            capturer,
            gate: EncodeGate::new(Rect::sized(width, height), policy),
            requested_fps: policy.max_fps.max(1),
            governor,
            space: ColorSpace::default(),
            staging,
            encoder,
            decoder: None,
            needs_whole_frame: true,
            stats: PipelineStats::default(),
            started: Instant::now(),
            applied_level: Some(level),
        })
    }

    /// Attach a decoder, so a loopback can check its own output.
    ///
    /// For a loopback this is safe **in process**: the bitstream is our own
    /// encoder's, not a peer's, so the large C parser is not being pointed at
    /// anything hostile. A controller decoding a peer's stream must use the
    /// sandboxed decoder instead, and that is not a detail to leave to whoever
    /// wires it up — see `ownview_decode::sandbox`.
    #[must_use]
    pub fn with_decoder(mut self, decoder: Box<dyn VideoDecoder>) -> Self {
        self.decoder = Some(decoder);
        self
    }

    /// Which capture backend is in use.
    #[must_use]
    pub fn capture_backend(&self) -> &'static str {
        self.capturer.backend()
    }

    /// Which encoder, and whether it has its fast path.
    #[must_use]
    pub fn encoder_backend(&self) -> (&'static str, bool) {
        (self.encoder.backend(), self.encoder.is_fast_path())
    }

    /// Which decoder, and whether it is sandboxed.
    #[must_use]
    pub fn decoder_backend(&self) -> Option<(&'static str, bool)> {
        self.decoder
            .as_ref()
            .map(|d| (d.backend(), d.is_sandboxed()))
    }

    /// Running totals.
    #[must_use]
    pub const fn stats(&self) -> &PipelineStats {
        &self.stats
    }

    /// The governor.
    #[must_use]
    pub const fn governor(&self) -> &Governor {
        &self.governor
    }

    /// The level actually in force — the governor's, clamped to the requested
    /// frame rate.
    ///
    /// Reported separately from [`Pipeline::governor`] because the two differ,
    /// and a report that showed only the governor's said 60 fps for a session
    /// running at 30.
    #[must_use]
    pub fn level(&self) -> ownview_encode::budget::Level {
        self.effective_level()
    }

    /// The damage gate's counts.
    #[must_use]
    pub fn gate_counts(&self) -> (u64, u64) {
        self.gate.counts()
    }

    /// How long the pipeline has been running.
    #[must_use]
    pub fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }

    /// Levers the governor has that this pipeline cannot act on.
    ///
    /// Reported rather than hidden. A governor that appears to reduce
    /// resolution and does not would make the bottom of its ladder look like
    /// it worked.
    #[must_use]
    pub fn unapplied_levers(&self) -> Vec<Unapplied> {
        vec![Unapplied::Resolution]
    }

    /// Ask for a keyframe on the next frame.
    pub fn request_keyframe(&mut self) {
        self.gate.request_keyframe();
    }

    /// Run one turn.
    pub fn step(&mut self, timeout: Duration) -> Result<Step, PipelineError> {
        let capture = Timer::start();
        let captured = self.capturer.next_frame(timeout);
        capture.stop(&mut self.stats.capture);

        let frame = match captured? {
            Capture::Frame(f) => f,
            Capture::Unchanged => return Ok(Step::Unchanged),
            Capture::Reconfigured(monitors) => {
                let (w, h) = monitors
                    .iter()
                    .find(|m| m.id == self.capturer.selected())
                    .map_or((self.staging.width(), self.staging.height()), |m| m.size());
                self.reconfigure(w, h)?;
                return Ok(Step::Reconfigured {
                    width: w,
                    height: h,
                });
            }
        };
        self.stats.frames_captured = self.stats.frames_captured.saturating_add(1);

        // A resolution change the backend did not announce. Caught here
        // because a converter handed a differently-sized frame would resize
        // the staging buffer and silently lose the previous picture that
        // damage-driven conversion depends on.
        if frame.width() != self.staging.width() || frame.height() != self.staging.height() {
            self.reconfigure(frame.width(), frame.height())?;
        }

        let now = Instant::now();
        let verdict = self.gate.decide(frame.damage(), now);

        // Tell the governor what the path has cost, and follow it.
        self.follow_the_governor(now)?;

        let (keyframe, damage) = match verdict {
            Verdict::Skip => {
                self.stats.frames_skipped = self.stats.frames_skipped.saturating_add(1);
                return Ok(Step::Skipped);
            }
            Verdict::Encode {
                keyframe, damage, ..
            } => (keyframe, damage),
        };

        // Convert. Only the damaged regions, unless the staging buffer cannot
        // be trusted to hold the previous frame.
        let whole = self.needs_whole_frame || matches!(damage, Damage::Everything);
        let convert = Timer::start();
        let converted = if whole {
            let _ = convert::whole_frame(&frame, self.space, &mut self.staging)?;
            u64::from(frame.width()) * u64::from(frame.height())
        } else {
            let regions = damage.rects(&frame.bounds());
            convert::regions(&frame, self.space, &mut self.staging, &regions)?
        };
        convert.stop(&mut self.stats.convert);
        self.needs_whole_frame = false;
        self.stats.pixels_converted = self.stats.pixels_converted.saturating_add(converted);
        self.stats.pixels_if_whole = self
            .stats
            .pixels_if_whole
            .saturating_add(u64::from(frame.width()) * u64::from(frame.height()));

        let encode = Timer::start();
        let encoded = self.encoder.encode(&self.staging, keyframe);
        encode.stop(&mut self.stats.encode);
        let encoded = encoded?;

        self.stats.frames_encoded = self.stats.frames_encoded.saturating_add(1);
        if encoded.keyframe {
            self.stats.keyframes = self.stats.keyframes.saturating_add(1);
        }
        self.stats.bytes = self.stats.bytes.saturating_add(encoded.len() as u64);

        let decoded = match self.decoder.as_mut() {
            Some(decoder) => {
                let decode = Timer::start();
                let out = decoder.decode(&encoded.bitstream);
                decode.stop(&mut self.stats.decode);
                out?
            }
            None => None,
        };

        Ok(Step::Encoded {
            keyframe: encoded.keyframe,
            bytes: encoded.len(),
            decoded,
        })
    }

    /// Report cost to the governor and apply whatever it decides.
    fn follow_the_governor(&mut self, now: Instant) -> Result<(), PipelineError> {
        let _ = now;
        let elapsed = self.started.elapsed();
        if let Some(cpu) = self.stats.total_cpu() {
            let moved = self.governor.observe(cpu, elapsed);
            if matches!(moved, Moved::Down | Moved::Up) {
                self.apply_level()?;
            }
        }
        Ok(())
    }

    /// Push the governor's current level to the encoder **and the gate**.
    ///
    /// The gate is the half that was missing, and its absence made the whole
    /// framerate rung decorative: the governor would report 15 fps, the
    /// encoder's rate model would be told 15, and the gate would carry on
    /// admitting frames at 60. The pipeline looked degraded and cost the same.
    fn apply_level(&mut self) -> Result<(), PipelineError> {
        let level = self.effective_level();
        if self.applied_level == Some(level) {
            return Ok(());
        }

        // The gate decides how many frames are encoded at all, which is what
        // makes the rung cost anything.
        self.gate.set_max_fps(level.fps);

        // The resolution part is deliberately not applied; see
        // `unapplied_levers`. Framerate and fidelity are.
        let config = EncoderConfig::for_budget(
            self.staging.width(),
            self.staging.height(),
            &self.governor.budget(),
        )
        .with_level(&level);
        self.encoder.reconfigure(&config)?;
        self.applied_level = Some(level);
        Ok(())
    }

    /// The governor's level, clamped to what the caller asked for.
    ///
    /// The ladder's top rung is 60 fps. A session asked for 30 must not be
    /// raised to 60 because the governor recovered — the ladder describes how
    /// far to fall, not a rate to aim at.
    fn effective_level(&self) -> ownview_encode::budget::Level {
        let mut level = self.governor.level();
        level.fps = level.fps.min(self.requested_fps).max(1);
        level
    }

    /// Resize everything for a new picture size.
    fn reconfigure(&mut self, width: u32, height: u32) -> Result<(), PipelineError> {
        let _ = self.staging.resize(width, height)?;
        self.gate.reconfigure(Rect::sized(width, height));
        // **The staging buffer no longer holds the previous frame**, so the
        // next conversion has to be whole. Missing this leaves the untouched
        // regions of the first frame after a resize showing whatever the
        // resize left behind.
        self.needs_whole_frame = true;
        let level = self.effective_level();
        self.gate.set_max_fps(level.fps);
        let config = EncoderConfig::for_budget(
            self.staging.width(),
            self.staging.height(),
            &self.governor.budget(),
        )
        .with_level(&level);
        self.encoder.reconfigure(&config)?;
        self.applied_level = Some(level);
        Ok(())
    }
}
