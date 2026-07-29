//! What each measurement actually runs.
//!
//! # Two shapes
//!
//! A **component** measurement drives one piece directly — an encoder handed
//! pictures, a converter handed a frame — because that is where a target like
//! "encode, 1080p30, full motion" lives and mixing capture into it would
//! measure the wrong thing.
//!
//! A **path** measurement runs the whole pipeline against the synthetic
//! capturer, because "a session where somebody is reading costs a tenth of a
//! core" is a claim about all of it together, and the damage gate is most of
//! why it is true.
//!
//! # Warm-up, and why it is not optional
//!
//! The first encode of a session allocates, builds tables and produces a
//! keyframe. Averaging it into a per-frame figure makes a short run look slow
//! and a long one look fast, which is a measurement of the run length. Every
//! scenario here discards a warm-up and reports the rest.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use esper_capture::Capturer as _;
use esper_capture::gate::Policy;
use esper_capture::pattern::Motion;
use esper_capture::synthetic::SyntheticCapturer;
use esper_color::{ColorSpace, I420};
use esper_encode::budget::Budget;
use esper_encode::encoder::{EncodeError, EncoderConfig, VideoEncoder};
use esper_encode::openh264::OpenH264;
use esper_pipeline::pipeline::Pipeline;
use esper_scale::Ceiling;

/// The size every target is stated at.
const WIDTH: u32 = 1920;
const HEIGHT: u32 = 1080;

/// And the one the idle target is stated at.
const WIDE: u32 = 3840;
const TALL: u32 = 2160;

/// The frame rate every per-second figure is against.
const FPS: u32 = 30;

/// Frames thrown away before anything is counted.
const WARM_UP: u64 = 10;

/// Frames counted, for the component measurements.
///
/// Enough that one slow frame does not decide the answer, and few enough that
/// the whole suite runs in under a minute — a benchmark nobody waits for is a
/// benchmark nobody runs.
const FRAMES: u64 = 60;

/// How long each whole-path measurement runs.
const PATH_SECONDS: u64 = 6;

/// And how long the idle one runs.
///
/// **Longer, because the thing being measured is a steady state.** An idle
/// screen's whole cost is the periodic keyframe, one every five seconds by
/// policy — so a six second window contains one or two of them and measures
/// mostly where the window happened to fall.
const IDLE_SECONDS: u64 = 26;

/// What a scenario produced.
pub type Measured = BTreeMap<&'static str, f64>;

/// Anything that stopped a measurement being taken.
#[derive(Debug, thiserror::Error)]
pub enum BenchError {
    #[error("the encoder would not start: {0}")]
    Encoder(#[from] EncodeError),
    #[error("the pattern is not a size this can render: {0}")]
    Pattern(#[from] esper_capture::pattern::PatternError),
    #[error("the pipeline would not start: {0}")]
    Pipeline(String),
    #[error("{0}")]
    Failed(String),
}

/// Process CPU time, the same clock the pipeline's stage timers read.
fn cpu_now() -> Option<Duration> {
    #[cfg(unix)]
    {
        let t = rustix::time::clock_gettime(rustix::time::ClockId::ProcessCPUTime);
        let secs = u64::try_from(t.tv_sec).ok()?;
        let nanos = u32::try_from(t.tv_nsec).ok()?;
        Some(Duration::new(secs, nanos))
    }
    #[cfg(not(unix))]
    {
        None
    }
}

/// The colour space a screen is sent in.
fn space() -> ColorSpace {
    ColorSpace::new(esper_color::Matrix::Bt709, esper_color::Range::Limited)
}

/// One frame out of a synthetic capturer, which is where a real one comes
/// from.
///
/// Built through the actual capture path rather than by hand: a benchmark that
/// assembled its own frames would be measuring a shape no backend produces,
/// and would keep working after the real one stopped.
fn next(capturer: &mut SyntheticCapturer) -> Result<esper_capture::frame::Frame, BenchError> {
    for _ in 0..200 {
        match capturer.next_frame(Duration::from_millis(200)) {
            Ok(esper_capture::Capture::Frame(f)) => return Ok(f),
            Ok(_) => {}
            Err(e) => return Err(BenchError::Failed(e.to_string())),
        }
    }
    Err(BenchError::Failed(
        "the synthetic capturer produced no frames".to_owned(),
    ))
}

fn encoder_config(width: u32, height: u32) -> EncoderConfig {
    EncoderConfig {
        width,
        height,
        fps: FPS,
        // The figure a balanced session settles at, so the encoder's rate model
        // is working on the same problem the product gives it.
        bitrate: 4_000_000,
        ..EncoderConfig::default()
    }
}

/// Encode `FRAMES` pictures and say what each one cost.
///
/// The conversion happens outside what is counted, because the encode and the
/// conversion have separate targets and a figure that folded them together
/// would answer neither question.
fn encode_run(motion: Motion) -> Result<(f64, f64), BenchError> {
    let mut capturer = SyntheticCapturer::new(WIDTH, HEIGHT, 1_000_000)
        .map_err(BenchError::Pattern)?
        .with_motion(motion);
    let mut encoder = OpenH264::new(&encoder_config(WIDTH, HEIGHT))?;
    let mut picture = I420::new(WIDTH, HEIGHT).map_err(|e| BenchError::Failed(e.to_string()))?;

    for n in 0..WARM_UP {
        let frame = next(&mut capturer)?;
        esper_color::convert::whole_frame(&frame, space(), &mut picture)
            .map_err(|e| BenchError::Failed(e.to_string()))?;
        encoder.encode(&picture, n == 0)?;
    }

    // **Counted from here**, so the keyframe and the encoder's first
    // allocations are not averaged into a per-frame figure — which would make
    // a short run look slow and a long one look fast, and would therefore be
    // a measurement of the run length.
    let was = encoder.stats();
    let cpu_before = cpu_now();
    let mut converting = Duration::ZERO;
    for _ in 0..FRAMES {
        let frame = next(&mut capturer)?;
        let at = Instant::now();
        esper_color::convert::whole_frame(&frame, space(), &mut picture)
            .map_err(|e| BenchError::Failed(e.to_string()))?;
        converting += at.elapsed();
        encoder.encode(&picture, false)?;
    }
    let now = encoder.stats();

    let frames = now.frames.saturating_sub(was.frames).max(1);
    let spent = now.spent.saturating_sub(was.spent);
    let per_frame = spent.as_secs_f64() * 1000.0 / frames as f64;

    // **The encoder's share of the CPU, not the loop's.** The loop also
    // renders and converts, which at 1080p costs more than the encode does, so
    // the process's CPU is apportioned by where the wall time went — measured
    // rather than assumed, because rendering is most of it.
    let cpu = match (cpu_before, cpu_now()) {
        (Some(before), Some(after)) => after.saturating_sub(before).as_secs_f64(),
        _ => 0.0,
    };
    let busy = spent.as_secs_f64() + converting.as_secs_f64();
    let encoders_share = if busy > 0.0 {
        (spent.as_secs_f64() / busy).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let cores = if frames == 0 {
        0.0
    } else {
        cpu * encoders_share / frames as f64 * f64::from(FPS)
    };
    Ok((per_frame, cores))
}

/// Convert a frame, whole or in one strip.
///
/// The same frame every time: what is being measured is the converter, and a
/// fresh capture each round would put the pattern's own rendering into the
/// figure.
fn convert_run(whole: bool) -> Result<f64, BenchError> {
    let mut capturer =
        SyntheticCapturer::new(WIDTH, HEIGHT, 1_000_000).map_err(BenchError::Pattern)?;
    let source = next(&mut capturer)?;
    let mut out = I420::new(WIDTH, HEIGHT).map_err(|e| BenchError::Failed(e.to_string()))?;

    // One line of a document's worth, which is the entire case for converting
    // damage rather than frames.
    let strip = [esper_capture::geometry::Rect {
        x: 0,
        y: i32::try_from(HEIGHT / 2).unwrap_or(0),
        width: WIDTH,
        height: 20,
    }];

    let mut run = || -> Result<(), BenchError> {
        if whole {
            esper_color::convert::whole_frame(&source, space(), &mut out)
                .map(|_| ())
                .map_err(|e| BenchError::Failed(e.to_string()))
        } else {
            esper_color::convert::regions(&source, space(), &mut out, &strip)
                .map(|_| ())
                .map_err(|e| BenchError::Failed(e.to_string()))
        }
    };

    for _ in 0..WARM_UP {
        run()?;
    }
    let started = Instant::now();
    for _ in 0..FRAMES {
        run()?;
    }
    Ok(started.elapsed().as_secs_f64() * 1000.0 / FRAMES as f64)
}

fn encoder_for(config: &EncoderConfig) -> Result<Box<dyn VideoEncoder>, EncodeError> {
    Ok(Box::new(OpenH264::new(config)?))
}

/// Run the whole pipeline for a while and report what it cost.
///
/// **The capturer produces at a screen's rate, not as fast as it can.** The
/// first version asked for a million frames a second, which is what the
/// loopback diagnostic does — and the pipeline then rendered thousands of
/// synthetic frames a second and encoded thirty of them, so the figure was one
/// core spent in the *fixture*. A real backend blocks until something changes;
/// this one has to as well, or the measurement is of this file.
fn path_run(
    width: u32,
    height: u32,
    motion: Motion,
    seconds: u64,
    settle: Duration,
    fidelity: Option<esper_pipeline::Fidelity>,
) -> Result<Path, BenchError> {
    let capturer = SyntheticCapturer::new(width, height, FPS)
        .map_err(BenchError::Pattern)?
        .with_motion(motion);
    let mut pipeline = Pipeline::new(
        Box::new(capturer),
        &encoder_for,
        Budget::of(100.0),
        Policy {
            max_fps: FPS,
            ..Policy::default()
        },
    )
    .map_err(|e| BenchError::Pipeline(e.to_string()))?
    .with_ceiling(Ceiling::none());
    // **A target stated "at balanced" has to be measured at balanced.** The
    // first version applied no profile at all, so the encoder ran wide open at
    // full resolution and reported 42 Mbit/s against a 5 Mbit/s target — a
    // number about an unconstrained encoder, not about the session the target
    // describes.
    pipeline.set_requested_fidelity(fidelity);

    let deadline = Instant::now() + Duration::from_secs(seconds) + settle;
    // Warm up on the clock rather than by frames: an idle screen may produce
    // almost none, and a warm-up counted in frames would never finish.
    let counting_from = Instant::now() + settle;
    let mut counted_from = None;
    while Instant::now() < deadline {
        pipeline
            .step(Duration::from_millis(100))
            .map_err(|e| BenchError::Pipeline(e.to_string()))?;
        if counted_from.is_none() && Instant::now() >= counting_from {
            counted_from = Some((Instant::now(), *pipeline.stats()));
        }
    }
    let (since, before) = counted_from
        .ok_or_else(|| BenchError::Failed("the run was over before it started".to_owned()))?;
    let over = since.elapsed();
    let now = *pipeline.stats();

    let bytes = now.bytes.saturating_sub(before.bytes);
    let kbit = (bytes as f64) * 8.0 / 1000.0 / over.as_secs_f64().max(0.001);

    // **Convert and encode, not capture.** The synthetic capturer renders a
    // 1080p picture per frame from a pure function of position — which is far
    // more expensive than a real backend's copy out of shared memory, and it
    // is the fixture rather than the product. Counting it put the first
    // version of this figure ten times high, all of it in this file.
    let stage_cpu = |s: &esper_pipeline::pipeline::PipelineStats| {
        let convert = s.convert.cpu.unwrap_or_default();
        let encode = s.encode.cpu.unwrap_or_default();
        convert.saturating_add(encode).as_secs_f64()
    };
    let cores = (stage_cpu(&now) - stage_cpu(&before)).max(0.0) / over.as_secs_f64().max(0.001);
    let frames = now.frames_encoded.saturating_sub(before.frames_encoded) as f64
        / over.as_secs_f64().max(0.001);
    Ok(Path {
        cores,
        kbit,
        frames_per_second: frames,
    })
}

/// What a whole-path run came to.
struct Path {
    cores: f64,
    kbit: f64,
    frames_per_second: f64,
}

/// Run everything and return what each target measured.
///
/// # Errors
///
/// If any scenario could not be run at all — a missing encoder, a size the
/// pattern refuses. A measurement that simply came out badly is a number, not
/// an error.
pub fn run_all() -> Result<Measured, BenchError> {
    let mut out = Measured::new();

    let (ms, cores) = encode_run(Motion::Screen)?;
    out.insert("encode.screen.ms", ms);
    out.insert("encode.screen.core", cores);

    let screen_ms = ms;
    let (ms, cores) = encode_run(Motion::Everything)?;
    out.insert("encode.motion.ms", ms);
    out.insert("encode.motion.core", cores);
    // **The fixture checking itself.** If the full-motion content ever stopped
    // being full motion, every encode figure above would quietly improve and
    // this harness would report good news.
    if screen_ms > 0.0 {
        out.insert("encode.motion.factor", ms / screen_ms);
    }

    let whole = convert_run(true)?;
    let line = convert_run(false)?;
    out.insert("convert.whole.ms", whole);
    out.insert("convert.line.ms", line);
    // The ratio, which is the claim rather than either number — and the one
    // figure here that means the same thing on somebody else's machine.
    if line > 0.0 {
        out.insert("convert.ratio", whole / line);
    }

    let settle = Duration::from_millis(500);
    let balanced = Some(esper_pipeline::Fidelity::Balanced);

    let screen = path_run(
        WIDTH,
        HEIGHT,
        Motion::Screen,
        PATH_SECONDS,
        settle,
        balanced,
    )?;
    out.insert("path.screen.core", screen.cores);
    out.insert("path.screen.kbit", screen.kbit);

    let motion = path_run(
        WIDTH,
        HEIGHT,
        Motion::Everything,
        PATH_SECONDS,
        settle,
        balanced,
    )?;
    out.insert("path.motion.kbit", motion.kbit);

    // **An idle screen is one where nothing changes**, and it has to have been
    // idle long enough to count as it. The first version used the ordinary
    // screen pattern at 4K, which has a counter ticking thirty times a second;
    // the second counted from half a second in, which is inside the ten
    // seconds the gate takes to reach deep idle — so it averaged one frame a
    // second with a fifth of one and reported the average of two states.
    let idle = path_run(
        WIDE,
        TALL,
        Motion::Still,
        IDLE_SECONDS,
        Duration::from_secs(12),
        balanced,
    )?;
    out.insert("path.idle.kbit", idle.kbit);
    out.insert("path.idle.fps", idle.frames_per_second);

    Ok(out)
}
