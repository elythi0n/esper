#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! **Does the resolution rung pay for itself?**
//!
//! The rung exists to save CPU. Scaling is not free, so it only helps if the
//! scaler costs less than the conversion and encode it takes away. Measured
//! against the conversion alone it loses badly; the conversion is the smaller
//! half of the work, and a partial comparison would have retired a lever that
//! is in fact worth having.
//!
//! So this measures the whole path, through the real pipeline, in **CPU time**
//! — which is what the budget is written in, and which stops agreeing with
//! wall clock the moment the encoder uses more than one thread.
//!
//! `#[ignore]`d, because a timing assertion on a shared CI runner measures the
//! runner. Run it deliberately:
//!
//! ```text
//! cargo test --release -p esper-pipeline --test cost -- --ignored --nocapture
//! ```

use std::time::Duration;

use esper_capture::gate::Policy;
use esper_capture::synthetic::SyntheticCapturer;
use esper_encode::budget::Budget;
use esper_encode::encoder::{EncodeError, EncoderConfig, VideoEncoder};
use esper_encode::openh264::OpenH264;
use esper_pipeline::pipeline::Pipeline;
use esper_scale::Ceiling;

fn encoder_for(config: &EncoderConfig) -> Result<Box<dyn VideoEncoder>, EncodeError> {
    Ok(Box::new(OpenH264::new(config)?))
}

/// Per-frame CPU cost of the whole path, at a given cap.
struct Measured {
    encoding: (u32, u32),
    convert_ms: f64,
    encode_ms: f64,
    total_ms: f64,
    /// The slowest single scale-and-convert, which is a keyframe: `Everything`
    /// damage means the whole frame is resampled rather than a region.
    worst_convert_ms: f64,
    frames: u64,
}

fn run(w: u32, h: u32, ceiling: Ceiling, frames: usize) -> Measured {
    let capturer = SyntheticCapturer::new(w, h, 1_000_000).expect("a valid size");
    // A budget large enough that the governor never moves, so this measures
    // the cap and not the ladder reacting to the measurement.
    let mut p = Pipeline::new(
        Box::new(capturer),
        &encoder_for,
        Budget::of(1000.0),
        Policy {
            max_fps: 60,
            ..Policy::default()
        },
    )
    .expect("a pipeline")
    .with_ceiling(ceiling);

    // One step to settle the size and get past the first keyframe.
    let _ = p.step(Duration::from_secs(5)).expect("a step");
    let warm = *p.stats();

    for _ in 0..frames {
        let _ = p.step(Duration::from_secs(5)).expect("a step");
    }
    let s = *p.stats();

    let ms = |now: Option<Duration>, before: Option<Duration>| {
        now.unwrap_or_default()
            .saturating_sub(before.unwrap_or_default())
            .as_secs_f64()
            * 1000.0
    };
    let n = s.frames_encoded.saturating_sub(warm.frames_encoded).max(1);
    let convert = ms(s.convert.cpu, warm.convert.cpu);
    let encode = ms(s.encode.cpu, warm.encode.cpu);
    Measured {
        encoding: p.encode_size(),
        convert_ms: convert / n as f64,
        encode_ms: encode / n as f64,
        // Capture is deliberately **excluded**. The synthetic capturer draws
        // its pattern pixel by pixel, which costs more than everything else
        // put together and is not work any real backend does — a real one
        // hands over a buffer the compositor already filled. Including it
        // buried the difference the rung makes under a harness artefact, and
        // made a rung that saves 4.6x on the encoder look like a loss.
        total_ms: (convert + encode) / n as f64,
        worst_convert_ms: s.convert.worst.as_secs_f64() * 1000.0,
        frames: n,
    }
}

#[test]
#[ignore = "a measurement, not an assertion: run it deliberately"]
fn measure_whether_the_resolution_rung_pays_for_itself() {
    println!("\nper-frame CPU through the whole path, full-motion synthetic content:");
    println!(
        "{:>13}  {:>13}  {:>10}  {:>10}  {:>10}  {:>8}  {:>10}",
        "display", "encoding", "scale+conv", "encode", "the rung", "saved", "worst s+c"
    );
    for (w, h) in [(1920u32, 1080u32), (2560, 1440), (3840, 2160)] {
        let native = run(w, h, Ceiling::none(), 40);
        println!(
            "{w:>6}x{h:<6} {:>6}x{:<6}  {:>10.3}  {:>10.3}  {:>10.3}  {:>8}  {:>10.3}   ({} frames)",
            native.encoding.0,
            native.encoding.1,
            native.convert_ms,
            native.encode_ms,
            native.total_ms,
            "-",
            native.worst_convert_ms,
            native.frames
        );
        for cap in [1920u32, 1280, 960] {
            if cap >= w.max(h) {
                continue;
            }
            let capped = run(w, h, Ceiling::of(cap), 40);
            let saved = (native.total_ms - capped.total_ms) / native.total_ms * 100.0;
            println!(
                "{:>13}  {:>6}x{:<6}  {:>10.3}  {:>10.3}  {:>10.3}  {saved:>7.1}%  {:>10.3}   ({} frames)",
                "",
                capped.encoding.0,
                capped.encoding.1,
                capped.convert_ms,
                capped.encode_ms,
                capped.total_ms,
                capped.worst_convert_ms,
                capped.frames
            );
        }
    }
    println!(
        "\nThe scale+conv column is one stage: scaling happens inside the \
         conversion's timing. Both are damage-driven, which is why they stay \
         small — the scaler only resamples what changed. Capture is excluded; \
         see `run`."
    );
}
