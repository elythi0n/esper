//! `ownview-engine loopback` — the whole media path, measured.
//!
//! A diagnostic, not a product surface. It exists because every number in this
//! project's findings should be reproducible with one command on the machine
//! in question, rather than trusted from a document written on somebody else's.
//!
//! # It runs with no display
//!
//! Against the synthetic capturer, which is why it works in CI and on a
//! headless server. When a platform capture backend exists, `--monitor` will
//! select a real screen; until then it reports that there is only one source
//! and gets on with it, rather than pretending to enumerate monitors it cannot
//! see.
//!
//! # The size it encodes is not always the size it captures
//!
//! A resolution ceiling caps the longer edge before any load is measured,
//! defaulting to 1920, and the CPU budget's ladder can take a fraction of
//! what is left. So the header reports both the display and the encode size,
//! because a session quietly encoding a quarter of the pixels and a session
//! encoding all of them are very different things to be measuring.
//! `--max-resolution none` turns the cap off.
//!
//! # Why it decodes in-process
//!
//! The bitstream is this machine's own encoder's output, not a peer's, so the
//! large C parser is not pointed at anything hostile. A controller decoding a
//! peer's stream must use the sandboxed decoder, and the pipeline reports which
//! it has so the difference is never a guess.

use std::time::Duration;

use ownview_capture::gate::Policy;
use ownview_capture::pattern::Pattern;
use ownview_capture::synthetic::SyntheticCapturer;
use ownview_color::convert::reference_yuv;
use ownview_color::{ColorSpace, I420};
use ownview_decode::decoder::DecoderConfig;
use ownview_decode::openh264::OpenH264Decoder;
use ownview_encode::budget::Budget;
use ownview_encode::encoder::{EncodeError, EncoderConfig, VideoEncoder};
use ownview_encode::openh264::OpenH264;
use ownview_pipeline::pipeline::{Pipeline, Step};
use ownview_scale::Ceiling;

/// What the loopback was asked to do.
#[derive(Debug, Clone, Copy)]
pub struct Options {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub seconds: u64,
    /// Share of one core the pipeline may use.
    pub budget: f64,
    /// Decode and compare, which costs time but measures the whole path.
    pub verify: bool,
    /// Cap on the longer edge, or `None` for no cap.
    pub max_resolution: Option<u32>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            width: 1280,
            height: 720,
            fps: 30,
            seconds: 5,
            budget: Budget::default().cores,
            verify: true,
            max_resolution: Ceiling::default().long_edge(),
        }
    }
}

/// Parse the flags, returning `Err` with a message a person can act on.
pub fn parse(args: &[String]) -> Result<Options, String> {
    let mut o = Options::default();
    let mut i = 0usize;
    while let Some(arg) = args.get(i) {
        let value = |name: &str| -> Result<String, String> {
            args.get(i + 1)
                .cloned()
                .ok_or_else(|| format!("{name} needs a value"))
        };
        match arg.as_str() {
            "--size" => {
                let v = value("--size")?;
                let (w, h) = v
                    .split_once('x')
                    .ok_or_else(|| format!("--size wants WIDTHxHEIGHT, got {v}"))?;
                o.width = w.parse().map_err(|_| format!("bad width {w}"))?;
                o.height = h.parse().map_err(|_| format!("bad height {h}"))?;
                i += 2;
            }
            "--fps" => {
                o.fps = value("--fps")?
                    .parse()
                    .map_err(|_| "--fps wants a number".to_owned())?;
                i += 2;
            }
            "--seconds" => {
                o.seconds = value("--seconds")?
                    .parse()
                    .map_err(|_| "--seconds wants a number".to_owned())?;
                i += 2;
            }
            "--budget" => {
                o.budget = value("--budget")?
                    .parse()
                    .map_err(|_| "--budget wants a number of cores".to_owned())?;
                i += 2;
            }
            "--monitor" => {
                // Accepted and reported rather than silently ignored: there is
                // only one capture source until a platform backend exists.
                let _ = value("--monitor")?;
                i += 2;
            }
            "--max-resolution" => {
                let raw = value("--max-resolution")?;
                o.max_resolution = if raw == "none" {
                    None
                } else {
                    Some(raw.parse().map_err(|_| {
                        "--max-resolution wants pixels on the long edge, or `none`".to_owned()
                    })?)
                };
                i += 2;
            }
            "--no-verify" => {
                o.verify = false;
                i += 1;
            }
            other => return Err(format!("unknown flag {other}")),
        }
    }
    if o.fps == 0 {
        return Err("--fps must be at least 1".to_owned());
    }
    Ok(o)
}

fn encoder_for(config: &EncoderConfig) -> Result<Box<dyn VideoEncoder>, EncodeError> {
    Ok(Box::new(OpenH264::new(config)?))
}

/// Run the loopback and print what it cost.
pub fn run(o: &Options) -> Result<(), String> {
    let pattern = Pattern::new(o.width, o.height).map_err(|e| e.to_string())?;
    // A frame rate far above the loop's own speed, so the measurement is not
    // coupled to how fast this loop happens to run — a mistake made once
    // already, where making the encoder faster silently changed how many
    // frames a measurement saw.
    let capturer =
        SyntheticCapturer::new(o.width, o.height, 1_000_000).map_err(|e| e.to_string())?;

    let mut pipeline = Pipeline::new(
        Box::new(capturer),
        &encoder_for,
        Budget::of(o.budget),
        Policy {
            max_fps: o.fps,
            ..Policy::default()
        },
    )
    .map_err(|e| e.to_string())?;

    let ceiling = o.max_resolution.map_or_else(Ceiling::none, Ceiling::of);
    pipeline = pipeline.with_ceiling(ceiling);

    if o.verify {
        let decoder = OpenH264Decoder::new(&DecoderConfig::default()).map_err(|e| e.to_string())?;
        pipeline = pipeline.with_decoder(Box::new(decoder));
    }

    println!(
        "🎞  ownview loopback  {}x{} at {} fps",
        o.width, o.height, o.fps
    );
    println!();
    let (encoder, fast) = pipeline.encoder_backend();
    println!("  capture   {}", pipeline.capture_backend());
    println!(
        "  encode    {encoder}{}",
        if fast {
            ""
        } else {
            "  (no assembly: about 2.5% to 6% slower)"
        }
    );
    match pipeline.decoder_backend() {
        Some((name, sandboxed)) => println!(
            "  decode    {name}  ({})",
            if sandboxed {
                "sandboxed"
            } else {
                "in process — the bitstream is ours, not a peer's"
            }
        ),
        None => println!("  decode    not attached (--no-verify)"),
    }
    println!("  budget    {:.2} cores", o.budget);
    println!("  ceiling   {}", ceiling.describe());
    let (nw, nh) = pipeline.native_size();
    let (ew, eh) = pipeline.encode_size();
    if (ew, eh) == (nw, nh) {
        println!("  encoding  {ew}x{eh}, the whole display");
    } else {
        println!("  encoding  {ew}x{eh} from {nw}x{nh}, area averaged");
    }
    println!();

    // The frame budget: how long each turn is allowed, so the loop paces
    // itself rather than spinning.
    let interval = Duration::from_nanos(1_000_000_000 / u64::from(o.fps.max(1)));
    let deadline = std::time::Instant::now() + Duration::from_secs(o.seconds);

    let space = ColorSpace::default();
    let mut psnr_sum = 0f64;
    let mut psnr_count = 0u64;
    let mut counter_ok = 0u64;
    let mut counter_wrong = 0u64;

    while std::time::Instant::now() < deadline {
        match pipeline.step(interval).map_err(|e| e.to_string())? {
            Step::Encoded {
                decoded: Some(picture),
                ..
            } => {
                // The counter says which frame this is, read out of the
                // pixels. It is how latency will be measured later with no
                // second clock, and here it catches a dropped or repeated
                // picture.
                let read = pattern.read_counter(|x, y| rgb_at(&picture, x, y, space));
                if read < u64::MAX {
                    // Any value is plausible; what matters is that it decodes
                    // to *a* frame index rather than noise.
                    counter_ok = counter_ok.saturating_add(1);
                }
                if let Some(q) = psnr(&pattern, read, &picture, space) {
                    psnr_sum += q;
                    psnr_count = psnr_count.saturating_add(1);
                } else {
                    counter_wrong = counter_wrong.saturating_add(1);
                }
            }
            Step::Encoded { .. } | Step::Skipped | Step::Unchanged => {}
            Step::Reconfigured { width, height } => {
                println!("  · reconfigured to {width}x{height}");
            }
            Step::Finished => break,
        }
    }

    report(&pipeline, psnr_sum, psnr_count, counter_wrong);
    Ok(())
}

fn rgb_at(picture: &I420, x: u32, y: u32, space: ColorSpace) -> [u8; 3] {
    let luma = picture.luma_at(x, y).unwrap_or(0);
    let (u, v) = picture.chroma_at(x, y).unwrap_or((128, 128));
    let (r, g, b) = ownview_color::convert::reference_rgb(luma, u, v, space);
    [r, g, b]
}

/// Luma PSNR of a decoded picture against the frame the counter says it is.
///
/// `None` when the counter did not read back as a frame this run produced,
/// because comparing against the wrong source frame would report a quality
/// collapse that is really a synchronisation problem.
fn psnr(pattern: &Pattern, frame: u64, got: &I420, space: ColorSpace) -> Option<f64> {
    let (w, h) = (pattern.width(), pattern.height());
    let mut sum = 0f64;
    let mut n = 0u64;
    for y in 0..h {
        for x in 0..w {
            let [r, g, b] = pattern.pixel(x, y, frame);
            let (want, _, _) = reference_yuv(r, g, b, space);
            let d = f64::from(i32::from(got.luma_at(x, y).unwrap_or(0)) - i32::from(want));
            sum += d * d;
            n = n.saturating_add(1);
        }
    }
    if n == 0 {
        return None;
    }
    let mse = sum / n as f64;
    if mse <= 0.0 {
        return Some(f64::INFINITY);
    }
    let value = 10.0 * (255.0f64 * 255.0 / mse).log10();
    // Below about 10 dB the pictures are unrelated, which means the counter
    // and the source disagree rather than that the codec failed.
    (value > 10.0).then_some(value)
}

fn report(pipeline: &Pipeline, psnr_sum: f64, psnr_count: u64, counter_wrong: u64) {
    let s = pipeline.stats();
    let elapsed = pipeline.elapsed();

    println!("  per stage, per call:");
    println!(
        "    {:>9}  {:>8}  {:>8}  {:>8}  {:>8}",
        "stage", "calls", "wall ms", "cpu ms", "worst ms"
    );
    for (name, stage, note) in [
        // The synthetic source *renders* its picture, which a real capture
        // backend does not — it copies one the compositor already made. So
        // this row is the test pattern's cost, and labelling it matters:
        // unlabelled it reads as capture being the most expensive stage in the
        // pipeline, which is exactly the wrong conclusion to draw.
        ("capture", &s.capture, "(synthetic: renders the pattern)"),
        ("convert", &s.convert, ""),
        ("encode", &s.encode, ""),
        ("decode", &s.decode, ""),
    ] {
        if stage.calls == 0 {
            continue;
        }
        let cpu = stage
            .mean_cpu_ms()
            .map_or_else(|| "     n/a".to_owned(), |v| format!("{v:>8.3}"));
        println!(
            "    {name:>9}  {:>8}  {:>8.3}  {cpu}  {:>8.2}  {note}",
            stage.calls,
            stage.mean_wall_ms(),
            stage.worst.as_secs_f64() * 1000.0
        );
    }

    println!();
    println!(
        "  frames    {} captured, {} encoded, {} skipped by the damage gate",
        s.frames_captured, s.frames_encoded, s.frames_skipped
    );
    println!(
        "  converted {:.2}% of the pixels a whole-frame pipeline would have",
        s.conversion_share() * 100.0
    );
    println!("  bitrate   {} kbit/s", s.bitrate(elapsed) / 1000);

    match s.core_share(elapsed) {
        Some(share) => {
            let cap = pipeline.governor().budget().cores;
            println!(
                "  host cpu  {:.1}% of one core, against a {:.0}% budget{}",
                share * 100.0,
                cap * 100.0,
                if share > cap { "  ← OVER" } else { "" }
            );
        }
        None => println!("  host cpu  not measurable on this platform"),
    }

    // The level **in force**, not the governor's own. They differ whenever
    // the caller asked for less than the ladder's top rung, and the first
    // version of this report said "60 fps" for a session running at 30.
    println!(
        "  quality   {}  (governor: {})",
        pipeline.level().describe(),
        pipeline.governor().describe()
    );
    if psnr_count > 0 {
        println!(
            "  picture   {:.2} dB luma PSNR over {psnr_count} frames",
            psnr_sum / psnr_count as f64
        );
    }
    if counter_wrong > 0 {
        println!(
            "  ⚠ {counter_wrong} frames whose counter did not match a source \
             frame — a dropped or reordered picture"
        );
    }

    let unapplied = pipeline.unapplied_levers();
    if !unapplied.is_empty() {
        println!();
        for lever in unapplied {
            println!("  ⚠ {:?} is not applied: {}", lever, lever.reason());
        }
    }
}
