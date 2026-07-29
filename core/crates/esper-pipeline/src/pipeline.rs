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
//! It has three levers, and all three now reach something:
//!
//! | Lever | Reaches | Status |
//! |---|---|---|
//! | Framerate | the damage gate's rate ceiling | applied |
//! | Fidelity | the encoder's quantiser range | applied |
//! | Resolution | the scaler, and through it the conversion and the encoder | applied |
//!
//! Resolution was the last to land, and for a while it reached nothing at
//! all: the governor would drop to its bottom rung, report three quarter
//! scale, and encode exactly as many pixels as before. That was reported by
//! `unapplied_levers` rather than hidden, which is the only reason it stayed
//! visible long enough to get built.
//!
//! # Where the size is decided
//!
//! Three things narrow it, in order:
//!
//! 1. The **display** says how big it is.
//! 2. The **ceiling** caps it before any load is observed, because on a 4K
//!    host the extra pixels were never affordable and discovering that at
//!    runtime costs the first seconds of every session.
//! 3. The **governor's rung** takes a fraction of what is left, reactively.
//!
//! [`Pipeline::retarget`] is the single place those three meet. Staging
//! buffer, encoder and scaler are resized together or not at all: a rung
//! change that resized two of the three would hand the converter a frame the
//! destination is not shaped for.
//!
//! Two of the three levers arrived by being caught rather than by being built.
//! Fidelity had nowhere to land at all — the encoder had no quantiser setting.
//! And framerate reached the *encoder's rate model* but not the gate, so the
//! governor would report 15 fps while the gate went on admitting frames at 60:
//! the pipeline looked degraded and cost exactly the same. The first run of
//! `esper-engine loopback` printed "60 fps" for a session asked to run at 30,
//! which is what gave it away.

use std::time::{Duration, Instant};

use esper_capture::capturer::{Capture, CaptureError, Capturer};
use esper_capture::damage::Damage;
use esper_capture::frame::PixelFormat;
use esper_capture::gate::{EncodeGate, Policy, Verdict};
use esper_capture::geometry::Rect;
use esper_color::convert;
use esper_color::{ColorSpace, I420};
use esper_decode::decoder::{DecodeError, VideoDecoder};
use esper_encode::budget::{Budget, Fidelity, Governor, Moved};
use esper_encode::encoder::{EncodeError, EncoderConfig, VideoEncoder};
use esper_scale::{Ceiling, ScaleError, Scaler};

use crate::timing::{Stage, Timer};

/// What one turn of the pipeline did.
#[derive(Debug)]
pub enum Step {
    /// A frame was captured and encoded.
    Encoded {
        /// Whether a decoder can start from it.
        keyframe: bool,
        /// The encoded frame, in Annex-B framing.
        ///
        /// Handed out because a host has to send it, and the pipeline is the
        /// only thing that has it. Moved rather than copied — the encoder
        /// already produced an owned buffer — so a loopback that ignores it
        /// pays nothing for a sender that needs it.
        bitstream: Vec<u8>,
        /// What changed in this frame, in picture coordinates.
        ///
        /// Carried out of the pipeline because the consumers need it: a
        /// presenter repaints only these regions, and the transport will want
        /// to say what a frame covers.
        ///
        /// A caveat worth knowing rather than discovering. This is what
        /// *capture* reported. A lossy encoder can also nudge macroblocks
        /// nothing touched, so a consumer that repaints only these regions is
        /// showing very slightly older pixels elsewhere. That is what every
        /// remote desktop does, and a keyframe — where the damage is
        /// `Everything` — repaints all of it.
        damage: Damage,
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
    Buffer(#[from] esper_color::i420::I420Error),
    #[error(transparent)]
    Scale(#[from] ScaleError),
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
///
/// Empty, and deliberately still here. Every rung of the ladder now reaches
/// something, but the next backend or platform may not manage that, and a
/// pipeline that cannot honour a lever has to be able to say so rather than
/// degrade on paper. [`Pipeline::unapplied_levers`] is the channel; this is
/// the vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unapplied {}

impl Unapplied {
    /// Why, for the log and the operator.
    #[must_use]
    pub const fn reason(self) -> &'static str {
        match self {}
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

/// The size to encode at, given a display, a cap and a rung.
///
/// Two properties this has to guarantee, because the stages downstream assume
/// them rather than check them:
///
/// **Never larger than the display.** The ceiling's floor on a tiny edge and
/// the ladder's floor both round *up* to sixteen, so on a display smaller than
/// that they would ask the scaler to enlarge. It refuses, correctly, but the
/// refusal surfaces as a dead session rather than a slightly odd size.
///
/// **Always even.** The staging buffer rounds its size *up* to even for the
/// chroma planes, so an odd target leaves the buffer one pixel wider than the
/// frame being converted into it — and the conversion rejects that, again as a
/// dead session. Clamping to the display can reintroduce an odd number after
/// the other two steps have removed it, so the evening happens last.
///
/// A display under two pixels has no even size at or below it and is left
/// alone; nothing downstream can encode one either way.
fn target_size(
    ceiling: Ceiling,
    native: (u32, u32),
    level: &esper_encode::budget::Level,
) -> (u32, u32) {
    let (capped_w, capped_h) = ceiling.apply(native.0, native.1);
    let (w, h) = level.size(capped_w, capped_h);
    let fit = |want: u32, have: u32| {
        if have < 2 {
            return have;
        }
        (want.min(have) & !1).max(2)
    };
    (fit(w, native.0), fit(h, native.1))
}

/// The whole media path.
pub struct Pipeline {
    capturer: Box<dyn Capturer>,
    gate: EncodeGate,
    governor: Governor,
    space: ColorSpace,
    /// What the display is, before any cap or rung.
    native: (u32, u32),
    /// The policy cap on encode size.
    ceiling: Ceiling,
    /// Present only when the encode size differs from the capture size.
    ///
    /// It holds its own destination across frames, so it cannot be rebuilt
    /// casually: a rebuild starts from a blank picture and needs a whole
    /// frame behind it.
    scaler: Option<Scaler>,
    /// Reused across frames, and holding the previous one — which is what
    /// makes damage-driven conversion correct. Sized to the **encode** size,
    /// not the display.
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
    applied_level: Option<esper_encode::budget::Level>,
    /// The frame rate the caller asked for.
    ///
    /// The governor's ladder is **relative to this, not absolute**: its top
    /// rung is 60 fps, so a session asked for 30 must not be raised to 60 the
    /// moment the governor recovers. The effective ceiling is the lower of the
    /// two.
    requested_fps: u32,
    /// The coarsest quantiser the caller will accept, if it has said. See
    /// [`Pipeline::set_requested_fidelity`].
    requested_fidelity: Option<Fidelity>,
}

impl std::fmt::Debug for Pipeline {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pipeline")
            .field("capture", &self.capturer.backend())
            .field("encoder", &self.encoder.backend())
            .field("decoder", &self.decoder.as_ref().map(|d| d.backend()))
            .field("level", &self.governor.level().describe())
            .field("native", &self.native)
            .field("staging", &(self.staging.width(), self.staging.height()))
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
            .map_or((1920, 1080), esper_capture::capturer::MonitorInfo::size);

        let governor = Governor::new(budget, esper_encode::budget::Reflexes::default());
        // Clamped to the requested rate from the start, for the same reason
        // the recovery path is: the ladder's top rung is 60 fps and the caller
        // may have asked for 30.
        let mut level = governor.level();
        level.fps = level.fps.min(policy.max_fps.max(1)).max(1);

        let ceiling = Ceiling::default();
        let (enc_w, enc_h) = target_size(ceiling, (width, height), &level);
        let staging = I420::new(enc_w, enc_h)?;
        let config = EncoderConfig::for_budget(staging.width(), staging.height(), &budget)
            .with_level(&level);
        let encoder = encoder_for(&config)?;

        Ok(Self {
            capturer,
            gate: EncodeGate::new(Rect::sized(width, height), policy),
            requested_fps: policy.max_fps.max(1),
            requested_fidelity: None,
            governor,
            space: ColorSpace::default(),
            native: (width, height),
            ceiling,
            scaler: None,
            staging,
            encoder,
            decoder: None,
            needs_whole_frame: true,
            stats: PipelineStats::default(),
            started: Instant::now(),
            applied_level: Some(level),
        })
    }

    /// Set the policy cap on encode size.
    ///
    /// Takes effect on the next frame, through the same path a rung change
    /// takes, so there is one implementation of "resize everything".
    #[must_use]
    pub fn with_ceiling(mut self, ceiling: Ceiling) -> Self {
        self.ceiling = ceiling;
        self
    }

    /// Attach a decoder, so a loopback can check its own output.
    ///
    /// For a loopback this is safe **in process**: the bitstream is our own
    /// encoder's, not a peer's, so the large C parser is not being pointed at
    /// anything hostile. A controller decoding a peer's stream must use the
    /// sandboxed decoder instead, and that is not a detail to leave to whoever
    /// wires it up — see `esper_decode::sandbox`.
    #[must_use]
    pub fn with_decoder(mut self, decoder: Box<dyn VideoDecoder>) -> Self {
        self.decoder = Some(decoder);
        self
    }

    /// Every monitor the capture source can see.
    ///
    /// Exposed because a host has to describe them to a controller, and the
    /// capturer is behind the pipeline.
    #[must_use]
    pub fn monitors(&self) -> &[esper_capture::capturer::MonitorInfo] {
        self.capturer.monitors()
    }

    /// Capture a different monitor from now on, and say what will be encoded.
    ///
    /// The reshape is the one a resolution change already takes: the gate is
    /// resized, the scaler is dropped because it was built for the old
    /// display, and the next frame is whole because the staging buffer holds a
    /// picture of somewhere else. A keyframe is requested for the same reason
    /// — every reference picture the decoder holds is of the old monitor.
    ///
    /// Selecting the monitor already selected does nothing at all, rather than
    /// costing a keyframe and a whole frame's conversion.
    pub fn select_monitor(
        &mut self,
        monitor: esper_capture::capturer::MonitorId,
    ) -> Result<(u32, u32), PipelineError> {
        if monitor == self.capturer.selected() {
            return Ok(self.encode_size());
        }
        self.capturer.select(monitor)?;
        let (width, height) = self
            .capturer
            .monitors()
            .iter()
            .find(|m| m.id == monitor)
            .map_or(self.native, esper_capture::capturer::MonitorInfo::size);
        self.reconfigure(width, height)?;
        self.request_keyframe();
        Ok(self.encode_size())
    }

    /// The monitor being captured.
    #[must_use]
    pub fn selected_monitor(&self) -> esper_capture::capturer::MonitorId {
        self.capturer.selected()
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
    pub fn level(&self) -> esper_encode::budget::Level {
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
        Vec::new()
    }

    /// What the display is, before the ceiling or a rung.
    #[must_use]
    pub const fn native_size(&self) -> (u32, u32) {
        self.native
    }

    /// What is being encoded.
    ///
    /// Derived from the display, the ceiling and the rung — not read off the
    /// staging buffer. The buffer is resized by [`Self::retarget`] on the
    /// next frame, so between changing the ceiling and that frame the buffer
    /// still has its old size. Reporting it would have told a caller that a
    /// session with the cap switched off was still scaling, which is what a
    /// `--max-resolution none` run of the loopback printed.
    #[must_use]
    pub fn encode_size(&self) -> (u32, u32) {
        target_size(self.ceiling, self.native, &self.effective_level())
    }

    /// The size of the buffer frames are converted into.
    ///
    /// Equal to [`Self::encode_size`] whenever a frame is being converted.
    /// They differ only between a policy change and the frame that applies
    /// it, which is why the two are separate questions.
    #[must_use]
    pub const fn staging_size(&self) -> (u32, u32) {
        (self.staging.width(), self.staging.height())
    }

    /// The policy cap in force.
    #[must_use]
    pub const fn ceiling(&self) -> Ceiling {
        self.ceiling
    }

    /// Change the policy cap on a running session.
    ///
    /// Takes effect on the next frame, through the same resize path a rung
    /// change takes.
    pub fn set_ceiling(&mut self, ceiling: Ceiling) {
        self.ceiling = ceiling;
    }

    /// Whether frames are being resampled on the way to the encoder.
    ///
    /// Derived from the sizes rather than from whether a scaler object exists.
    /// The scaler cannot be built until a frame has revealed which bytes hold
    /// red, so asking this before the first frame would otherwise report a
    /// capped 4K session as unscaled.
    #[must_use]
    pub fn is_scaling(&self) -> bool {
        self.encode_size() != self.native
    }

    /// Ask for a keyframe on the next frame.
    /// Change the frame rate the caller is asking for.
    ///
    /// A ceiling on the governor's ladder, not a demand on it: asking for 60
    /// on a host that can afford 15 still gets 15. That asymmetry is the whole
    /// point — a controller must not be able to make somebody else's laptop
    /// unusable by asking nicely.
    pub fn set_requested_fps(&mut self, fps: u32) {
        self.requested_fps = fps.max(1);
        let level = self.effective_level();
        self.gate.set_max_fps(level.fps);
    }

    /// Change the coarsest quantiser the caller is willing to accept.
    ///
    /// **The middle rung of the degradation order, and the one nothing could
    /// reach.** The governor spends framerate, then fidelity, then resolution
    /// when the *CPU* runs short; a caller short of *bandwidth* could ask for
    /// fewer frames and smaller ones and had no way to ask for cheaper ones.
    /// So it skipped the middle rung and went to resolution, which is the
    /// most damaging lever for a screen somebody is reading.
    ///
    /// A floor under the picture's cost, not a demand for it: `None` is no
    /// preference, and the coarser of this and the governor's own choice
    /// wins — so this can only ever make a picture cheaper, never dearer.
    /// Takes effect on the next frame, through the same path a rung change
    /// takes.
    pub fn set_requested_fidelity(&mut self, fidelity: Option<Fidelity>) {
        self.requested_fidelity = fidelity;
    }

    /// The coarsest quantiser the caller has asked for, if any.
    #[must_use]
    pub const fn requested_fidelity(&self) -> Option<Fidelity> {
        self.requested_fidelity
    }

    /// The frame rate the caller has asked for.
    #[must_use]
    pub const fn requested_fps(&self) -> u32 {
        self.requested_fps
    }

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
        //
        // Against the **display** size, not the staging buffer's. They were
        // the same thing until the scaler arrived, and comparing against
        // staging afterwards makes every frame of a scaled session look like a
        // resolution change: the gate is reconfigured and reports the whole
        // frame as damaged, the scaler is rebuilt onto a blank destination,
        // and the conversion is whole. A resolution rung meant to save CPU
        // would instead have quietly cost the most it possibly could.
        if (frame.width(), frame.height()) != self.native {
            self.reconfigure(frame.width(), frame.height())?;
        }

        let now = Instant::now();
        let verdict = self.gate.decide(frame.damage(), now);

        // Tell the governor what the path has cost, and follow it.
        self.follow_the_governor(now)?;

        // And make the encode size agree with whatever it just decided,
        // before anything is sized against it.
        self.retarget(Some(frame.format()))?;

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
        // Kept before the converter borrows it, so the step can say what the
        // frame covered.
        let reported = if whole {
            Damage::Everything
        } else {
            damage.clone()
        };
        // A scaler keeps its own destination across frames, so when the whole
        // staging buffer is being rewritten the scaler has to rewrite all of
        // its own too. Handing it a small region here and then converting the
        // whole thing would convert whatever the scaler's buffer held before.
        let for_scaler = if whole {
            Damage::Everything
        } else {
            damage.clone()
        };

        let convert = Timer::start();
        let (converted, source_pixels) = match self.scaler.as_mut() {
            Some(scaler) => {
                let scaled = scaler.scale(&frame, &for_scaler)?;
                let whole_pixels = u64::from(scaled.width()) * u64::from(scaled.height());
                let n = if whole {
                    let _ = convert::whole_frame(scaled, self.space, &mut self.staging)?;
                    whole_pixels
                } else {
                    let regions = scaled.damage().rects(&scaled.bounds());
                    convert::regions(scaled, self.space, &mut self.staging, &regions)?
                };
                (n, whole_pixels)
            }
            None => {
                let whole_pixels = u64::from(frame.width()) * u64::from(frame.height());
                let n = if whole {
                    let _ = convert::whole_frame(&frame, self.space, &mut self.staging)?;
                    whole_pixels
                } else {
                    let regions = damage.rects(&frame.bounds());
                    convert::regions(&frame, self.space, &mut self.staging, &regions)?
                };
                (n, whole_pixels)
            }
        };
        convert.stop(&mut self.stats.convert);
        self.needs_whole_frame = false;
        self.stats.pixels_converted = self.stats.pixels_converted.saturating_add(converted);
        self.stats.pixels_if_whole = self.stats.pixels_if_whole.saturating_add(source_pixels);

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
            bitstream: encoded.bitstream,
            damage: reported,
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
                // The gate decides how many frames are encoded at all, which
                // is what makes the framerate rung cost anything. The size
                // and the encoder settings are `retarget`'s, so that a rung
                // change resizes everything or nothing.
                self.gate.set_max_fps(self.effective_level().fps);
            }
        }
        Ok(())
    }

    /// Make the staging buffer, the encoder and the scaler agree on one size.
    ///
    /// The single place the encode size is decided, and the reason it is one
    /// place: three things have to be resized together. A rung change that
    /// resized the staging buffer but not the scaler would hand the converter
    /// a frame its destination is not shaped for — which the conversion does
    /// catch, but as a dead session rather than a smaller picture.
    ///
    /// `format` is the pixel layout of a frame in hand, or `None` when there
    /// is not one yet. Without a frame the size is still settled, but a
    /// scaler cannot be built, because which bytes hold red is a property of
    /// the backend and not of the geometry.
    fn retarget(&mut self, format: Option<PixelFormat>) -> Result<(), PipelineError> {
        let level = self.effective_level();
        let (want_w, want_h) = target_size(self.ceiling, self.native, &level);
        let scaling = (want_w, want_h) != self.native;

        let scaler_fits = match (&self.scaler, format) {
            // No scaler needed and none present.
            (None, _) => !scaling,
            // One present: it has to match on all three counts, because any
            // of them changing means its weights or its buffer are wrong.
            (Some(s), Some(f)) => {
                scaling
                    && s.source() == self.native
                    && s.target() == (want_w, want_h)
                    && s.input_format() == f
            }
            // One present and no frame to check it against. Left alone: it
            // will be checked against the next frame that arrives.
            (Some(s), None) => scaling && s.target() == (want_w, want_h),
        };
        let sized = self.staging.width() == want_w && self.staging.height() == want_h;
        let configured = self.applied_level == Some(level);
        if scaler_fits && sized && configured {
            return Ok(());
        }

        if !scaler_fits {
            self.scaler = match (scaling, format) {
                (true, Some(f)) => Some(Scaler::new(
                    self.native.0,
                    self.native.1,
                    want_w,
                    want_h,
                    f,
                )?),
                // Scaling is wanted but the layout is not known yet, so no
                // scaler this turn. Dropping the old one is deliberate: its
                // target is wrong and a wrong scaler is worse than none.
                (true, None) => None,
                (false, _) => None,
            };
            // A fresh scaler starts on a blank destination.
            self.needs_whole_frame = true;
        }

        if !sized {
            let _ = self.staging.resize(want_w, want_h)?;
            // **The staging buffer no longer holds the previous frame**, so
            // the next conversion has to be whole. Missing this leaves the
            // untouched regions showing whatever the resize left behind.
            self.needs_whole_frame = true;
        }

        self.gate.set_max_fps(level.fps);

        // **A rung change rebuilds the encoder, and a rebuild costs a
        // keyframe.** `reconfigure` builds a fresh one — deliberately,
        // because openh264 accepts some changes in place and silently ignores
        // others — and a fresh encoder holds no reference pictures, so its
        // first frame must be an IDR. On the links where rungs are spent that
        // is the scarcest thing there is: a keyframe is the largest frame, so
        // it is the one a slow link is least able to carry. A session walking
        // four framerate rungs and four resolution rungs pays eight of them.
        //
        // Skipping the rebuild for a framerate-only change was tried, on the
        // reasoning that the gate above is what actually admits fewer frames
        // and the encoder's `fps` is only a rate-control hint. **Measured
        // worse**, in both directions: 5% loss went from 78 pictures to 67
        // with 30% more bytes per frame. The hint is not idle — the periodic
        // keyframe interval is derived from it *in frames*, so an encoder left
        // believing in a lower framerate emits keyframes more often, and the
        // saving buys more of exactly what it was trying to avoid.
        //
        // Which leaves the cost real and unpaid. Doing better than this needs
        // the quantiser changed in place, and `reinit` is private in the
        // binding while the raw `SetOption` is `unsafe`, which this workspace
        // forbids.
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
    fn effective_level(&self) -> esper_encode::budget::Level {
        let mut level = self.governor.level();
        level.fps = level.fps.min(self.requested_fps).max(1);
        // **The coarser of the two wins, which is the *larger*.** `Fidelity`
        // is declared sharpest-first, so `High < Balanced < Low` — and an
        // inverted comparison here would let a viewer ask a host to spend
        // more on the picture than the host had chosen, which is the one
        // thing a request from the far end must never be able to do.
        if let Some(asked) = self.requested_fidelity {
            level.fidelity = level.fidelity.max(asked);
        }
        level
    }

    /// Take a new display size.
    ///
    /// Only the display and the gate are set here. Everything downstream is
    /// sized by [`Self::retarget`], because the encode size depends on the
    /// ceiling and the rung as well, and computing it twice is how the two
    /// copies drift apart.
    fn reconfigure(&mut self, width: u32, height: u32) -> Result<(), PipelineError> {
        self.native = (width, height);
        self.gate.reconfigure(Rect::sized(width, height));
        // The old scaler was built for the old display.
        self.scaler = None;
        self.needs_whole_frame = true;
        self.retarget(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use esper_encode::budget::LADDER;

    /// A pipeline over the synthetic screen, for the tests that need a real
    /// one rather than a table of numbers.
    fn pipeline_for_test() -> Pipeline {
        fn encoder_for(
            config: &EncoderConfig,
        ) -> Result<Box<dyn esper_encode::encoder::VideoEncoder>, esper_encode::encoder::EncodeError>
        {
            Ok(Box::new(esper_encode::openh264::OpenH264::new(config)?))
        }
        let capturer =
            esper_capture::synthetic::SyntheticCapturer::new(640, 360, 1_000_000).expect("a size");
        Pipeline::new(
            Box::new(capturer),
            &encoder_for,
            Budget::of(100.0),
            esper_capture::gate::Policy::default(),
        )
        .expect("a pipeline")
    }

    /// **A request from the far end may only ever make a picture cheaper.**
    ///
    /// The one property this whole mechanism has to have, and the one an
    /// inverted comparison would silently destroy. `Fidelity` is declared
    /// sharpest-first, so the *coarser* of two answers is the larger — which
    /// makes the clamp a `max` where every other cap here is a `min`, and
    /// reading it the wrong way round would let a viewer ask somebody else's
    /// laptop to spend more on the picture than its own governor chose.
    ///
    /// Driven through [`Pipeline::effective_level`] rather than asserted as
    /// arithmetic. The first version of this test compared `chosen.max(asked)`
    /// against itself and passed with the comparison inverted, which is a test
    /// of `Ord` and not of this file.
    #[test]
    fn a_request_can_only_ever_coarsen() {
        let mut p = pipeline_for_test();
        let chosen = p.governor.level().fidelity;
        for asked in [
            Fidelity::High,
            Fidelity::Balanced,
            Fidelity::Low,
            Fidelity::Coarse,
        ] {
            p.set_requested_fidelity(Some(asked));
            let effective = p.effective_level().fidelity;
            assert!(
                effective >= chosen,
                "a host at {} asked for {} ended at {}, which is sharper than it chose",
                chosen.name(),
                asked.name(),
                effective.name()
            );
            assert_eq!(
                effective,
                if chosen > asked { chosen } else { asked },
                "asking for {} should give the coarser of it and {}",
                asked.name(),
                chosen.name()
            );
        }
        // And asking for nothing leaves the governor's own choice alone.
        p.set_requested_fidelity(None);
        assert_eq!(p.effective_level().fidelity, chosen);
    }

    /// And a coarser rung really is cheaper, or the lever is decoration.
    ///
    /// Measured on the synthetic pattern — the numbers are on
    /// [`Fidelity::Coarse`] for a screen and on [`Fidelity`] itself for
    /// video — and pinned here as an ordering so a future change to the
    /// ranges cannot silently invert it.
    #[test]
    fn the_rungs_are_ordered_by_what_they_cost() {
        let ranges = [
            Fidelity::High.qp_range(),
            Fidelity::Balanced.qp_range(),
            Fidelity::Low.qp_range(),
            Fidelity::Coarse.qp_range(),
        ];
        for pair in ranges.windows(2) {
            if let [(lo, hi), (next_lo, next_hi)] = pair {
                assert!(
                    next_lo > lo && next_hi > hi,
                    "({lo},{hi}) then ({next_lo},{next_hi}): a coarser rung must \
                     quantise more coarsely at both ends, or it saves nothing"
                );
            }
        }
        // H.264's quantiser stops at 51, and openh264's `QpRange::new`
        // **panics** past it rather than refusing.
        for (lo, hi) in ranges {
            assert!(lo < hi && hi <= 51, "({lo},{hi}) is not a usable range");
        }
    }

    /// The two properties everything downstream assumes of the encode size,
    /// over every rung and a range of displays including ones smaller than any
    /// capture backend will report.
    ///
    /// Odd is the dangerous one. The staging buffer rounds *up* to even for
    /// the chroma planes, so an odd target leaves it a pixel wider than the
    /// frame going into it, and the colour conversion refuses — a session that
    /// shows nothing instead of a picture one pixel narrower. Clamping to the
    /// display can put an odd number back after the ceiling and the rung have
    /// both removed it, which is why the evening happens last.
    #[test]
    fn the_encode_size_is_always_even_and_never_larger_than_the_display() {
        let ceilings = [Ceiling::none(), Ceiling::default(), Ceiling::of(640)];
        let sizes = [
            (2u32, 2u32),
            (3, 3),
            (9, 100),
            (17, 33),
            (31, 15),
            (1919, 1079),
            (1920, 1080),
            (1921, 1081),
            (2560, 1440),
            (3440, 1440),
            (3840, 2160),
            (5120, 2880),
        ];
        for ceiling in ceilings {
            for (w, h) in sizes {
                for level in LADDER {
                    let (tw, th) = target_size(ceiling, (w, h), level);
                    assert_eq!(tw % 2, 0, "{w}x{h} {ceiling:?} {level:?} gave width {tw}");
                    assert_eq!(th % 2, 0, "{w}x{h} {ceiling:?} {level:?} gave height {th}");
                    assert!(
                        tw <= w && th <= h,
                        "{w}x{h} {ceiling:?} {level:?} enlarged to {tw}x{th}"
                    );
                    assert!(tw >= 2 && th >= 2, "{w}x{h} collapsed to {tw}x{th}");
                }
            }
        }
    }

    /// A display below two pixels has no even size at or below it. It is left
    /// as it is rather than rounded to something larger, because asking the
    /// scaler to enlarge fails louder and no further along.
    #[test]
    fn a_display_too_small_to_snap_is_left_alone() {
        let level = LADDER.first().copied().expect("the ladder is not empty");
        assert_eq!(target_size(Ceiling::default(), (1, 1), &level), (1, 1));
        assert_eq!(target_size(Ceiling::default(), (1, 1080), &level).0, 1);
    }
}
