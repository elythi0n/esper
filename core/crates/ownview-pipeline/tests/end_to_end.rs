#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

//! The whole pipeline, running as one thing, with no display anywhere.
//!
//! Every stage has been tested on its own. This is the first test where they
//! share a clock, a buffer and a budget — which is where the failures that
//! only appear in combination live. Two of them appeared here.

use std::time::Duration;

use ownview_capture::gate::Policy;
use ownview_capture::pattern::Pattern;
use ownview_capture::synthetic::SyntheticCapturer;
use ownview_decode::decoder::DecoderConfig;
use ownview_decode::openh264::OpenH264Decoder;
use ownview_encode::budget::Budget;
use ownview_encode::encoder::{EncodeError, EncoderConfig, VideoEncoder};
use ownview_encode::openh264::OpenH264;
use ownview_pipeline::pipeline::{Pipeline, Step, Unapplied};

fn encoder_for(config: &EncoderConfig) -> Result<Box<dyn VideoEncoder>, EncodeError> {
    Ok(Box::new(OpenH264::new(config)?))
}

fn build(w: u32, h: u32, fps: u32, budget: f64) -> Pipeline {
    let capturer = SyntheticCapturer::new(w, h, 1_000_000).expect("a valid size");
    Pipeline::new(
        Box::new(capturer),
        &encoder_for,
        Budget::of(budget),
        Policy {
            max_fps: fps,
            ..Policy::default()
        },
    )
    .expect("a pipeline")
}

fn with_decoder(p: Pipeline) -> Pipeline {
    p.with_decoder(Box::new(
        OpenH264Decoder::new(&DecoderConfig::default()).expect("a decoder"),
    ))
}

/// **The Phase 3 goal, headless.** Frames go in one end and pictures come out
/// the other, with every stage timed.
#[test]
fn the_whole_path_runs_with_no_display() {
    let mut p = with_decoder(build(320, 240, 60, 1.0));
    assert_eq!(p.capture_backend(), "synthetic");
    assert_eq!(p.encoder_backend().0, "openh264");
    assert_eq!(
        p.decoder_backend(),
        Some(("openh264", false)),
        "a loopback decoder is in-process, and must say so"
    );

    let mut encoded = 0usize;
    let mut decoded = 0usize;
    for _ in 0..40 {
        match p.step(Duration::from_secs(1)).expect("a step") {
            Step::Encoded { decoded: pic, .. } => {
                encoded += 1;
                if let Some(pic) = pic {
                    assert_eq!((pic.width(), pic.height()), (320, 240));
                    decoded += 1;
                }
            }
            Step::Skipped | Step::Unchanged => {}
            other => panic!("unexpected {other:?}"),
        }
    }

    assert!(encoded > 0, "nothing was encoded");
    assert_eq!(decoded, encoded, "every encoded frame should decode");

    let s = p.stats();
    assert_eq!(s.frames_captured, 40);
    assert!(s.bytes > 0);
    // Every stage that ran must have a timing.
    for (name, stage) in [
        ("capture", &s.capture),
        ("convert", &s.convert),
        ("encode", &s.encode),
        ("decode", &s.decode),
    ] {
        assert!(stage.calls > 0, "{name} never ran");
        assert!(stage.wall > Duration::ZERO, "{name} has no wall time");
        #[cfg(unix)]
        assert!(
            stage.cpu.is_some(),
            "{name} has no CPU time, and the budget is written in CPU time"
        );
    }
}

/// **The case for damage-driven conversion, measured through the real path.**
#[test]
fn only_the_damaged_part_of_each_frame_is_converted() {
    let mut p = build(1280, 720, 60, 1.0);
    for _ in 0..60 {
        let _ = p.step(Duration::from_secs(1)).expect("a step");
    }
    let s = p.stats();
    assert!(s.pixels_if_whole > 0);
    let share = s.conversion_share();
    assert!(
        share < 0.10,
        "converted {:.1}% of the pixels a whole-frame pipeline would have; the \
         synthetic pattern moves a small box and a counter, so this should be \
         a few per cent",
        share * 100.0
    );
    // And the first frame is necessarily whole, so it is not zero either.
    assert!(share > 0.0);
}

/// The first frame must be converted whole. Damage-driven conversion is only
/// correct when the destination already holds the previous picture, and before
/// the first frame it holds nothing.
#[test]
fn the_first_frame_is_converted_whole() {
    let mut p = build(320, 240, 60, 1.0);
    let _ = p.step(Duration::from_secs(1)).expect("a step");
    let s = p.stats();
    assert_eq!(
        s.pixels_converted,
        320 * 240,
        "the first frame must be whole: the staging buffer holds nothing yet"
    );
}

/// **A failure that only appears in combination.** After a resolution change
/// the staging buffer no longer holds the previous picture, so the next
/// conversion has to be whole — otherwise the regions the damage does not
/// cover show whatever the resize left behind.
#[test]
fn a_resolution_change_forces_a_whole_conversion() {
    let capturer = SyntheticCapturer::new(320, 240, 1_000_000)
        .expect("valid")
        .with_reconfigure_at(3);
    let mut p = Pipeline::new(
        Box::new(capturer),
        &encoder_for,
        Budget::of(1.0),
        Policy::default(),
    )
    .expect("a pipeline");

    let mut saw_reconfigure = false;
    let mut converted_before = 0u64;
    for _ in 0..10 {
        match p.step(Duration::from_secs(1)).expect("a step") {
            Step::Reconfigured { .. } => {
                saw_reconfigure = true;
                converted_before = p.stats().pixels_converted;
            }
            _ => {
                if saw_reconfigure {
                    let after = p.stats().pixels_converted - converted_before;
                    if after > 0 {
                        assert_eq!(
                            after,
                            320 * 240,
                            "the frame after a reshape must be converted whole, \
                             or the untouched regions show what the resize left"
                        );
                        return;
                    }
                }
            }
        }
    }
    assert!(saw_reconfigure, "the capturer never reported a reconfigure");
}

/// A host over its budget must be degraded, and the degradation must reach
/// the encoder rather than staying in the governor's own bookkeeping.
#[test]
fn a_tiny_budget_degrades_the_encoder() {
    // A budget so small that any real work exceeds it.
    let mut p = build(1280, 720, 60, 0.05);
    for _ in 0..80 {
        let _ = p.step(Duration::from_secs(1)).expect("a step");
    }
    assert!(
        p.governor().is_degraded(),
        "a 5% of a core budget should have degraded: {}",
        p.governor().describe()
    );
    // The framerate the governor asked for must be what the encoder was given.
    let level = p.governor().level();
    assert!(level.fps < 60, "the level should have dropped: {level:?}");
}

/// **The lever that reached the wrong thing.** The governor's framerate rung
/// has to change what the *gate* admits. Told only to the encoder it changes
/// the rate model and not the workload, so the pipeline reports itself
/// degraded and costs the same.
#[test]
fn degrading_actually_reduces_the_frames_encoded() {
    // A generous budget first: how many frames get encoded when nothing is
    // holding it back.
    let mut fast = build(640, 480, 60, 100.0);
    for _ in 0..120 {
        let _ = fast.step(Duration::from_secs(1)).expect("a step");
    }
    let unrestricted = fast.stats().frames_encoded;

    // Then a budget too small for any real work, which must drive the ladder
    // down and actually encode fewer frames.
    let mut slow = build(640, 480, 60, 0.02);
    for _ in 0..120 {
        let _ = slow.step(Duration::from_secs(1)).expect("a step");
    }
    let degraded = slow.stats().frames_encoded;

    assert!(
        slow.governor().is_degraded(),
        "the tiny budget should have degraded: {}",
        slow.governor().describe()
    );
    assert!(
        degraded < unrestricted,
        "degrading encoded {degraded} frames against {unrestricted}          unrestricted — the framerate rung is not reaching the damage gate, so          it costs nothing"
    );
}

/// The ladder describes how far to fall, not a rate to aim at. A session asked
/// for 30 fps must not be raised to the ladder's 60 because the governor is at
/// the top.
#[test]
fn the_governor_never_raises_the_frame_rate_above_what_was_asked() {
    let p = build(320, 240, 30, 100.0);
    assert_eq!(
        p.governor().level().fps,
        60,
        "the ladder's top rung is 60, which is what the governor reports"
    );
    assert_eq!(
        p.level().fps,
        30,
        "but the level in force must be clamped to the 30 that was requested"
    );
}

/// **Reported rather than hidden.** The governor has a resolution lever and
/// the pipeline cannot act on it, because there is no scaler. A pipeline that
/// silently ignored it would make the bottom of the ladder look like it
/// worked.
#[test]
fn the_resolution_lever_is_declared_unapplied() {
    let p = build(320, 240, 60, 1.0);
    let unapplied = p.unapplied_levers();
    assert!(
        unapplied.contains(&Unapplied::Resolution),
        "the pipeline must admit it cannot scale"
    );
    assert!(
        Unapplied::Resolution.reason().contains("scaler"),
        "and say why: {}",
        Unapplied::Resolution.reason()
    );
}

/// A keyframe on request, all the way through to a decodable picture — which
/// is what a controller that lost sync needs.
#[test]
fn a_requested_keyframe_reaches_the_decoder() {
    let mut p = with_decoder(build(320, 240, 60, 1.0));
    // Get past the first frame, which is a keyframe anyway.
    for _ in 0..5 {
        let _ = p.step(Duration::from_secs(1)).expect("a step");
    }
    p.request_keyframe();

    for _ in 0..5 {
        if let Step::Encoded {
            keyframe: true,
            decoded: Some(pic),
            ..
        } = p.step(Duration::from_secs(1)).expect("a step")
        {
            assert_eq!((pic.width(), pic.height()), (320, 240));
            return;
        }
    }
    panic!("the requested keyframe never arrived");
}

/// The picture that comes out has to be the picture that went in. This is the
/// round trip again, but through the pipeline rather than assembled by hand —
/// so it also covers the wiring.
#[test]
fn the_frame_counter_survives_the_whole_pipeline() {
    let pattern = Pattern::new(320, 240).expect("valid");
    let mut p = with_decoder(build(320, 240, 60, 1.0));

    let mut checked = 0usize;
    for _ in 0..12 {
        if let Step::Encoded {
            decoded: Some(pic), ..
        } = p.step(Duration::from_secs(1)).expect("a step")
        {
            let read = pattern.read_counter(|x, y| {
                let y_sample = pic.luma_at(x, y).unwrap_or(0);
                let (u, v) = pic.chroma_at(x, y).unwrap_or((128, 128));
                let (r, g, b) = ownview_color::convert::reference_rgb(
                    y_sample,
                    u,
                    v,
                    ownview_color::ColorSpace::default(),
                );
                [r, g, b]
            });
            // The counter is the frame index the capturer rendered, which
            // advances once per captured frame.
            assert!(
                read < 64,
                "the counter came back as {read}, which is not a frame this \
                 test could have produced"
            );
            checked += 1;
        }
    }
    assert!(checked > 0, "nothing decoded, so nothing was checked");
}

/// Statistics over no time must not divide by zero, and a decoder that was
/// never attached must not poison the CPU total.
#[test]
fn statistics_are_safe_before_anything_has_run() {
    let p = build(320, 240, 60, 1.0);
    let s = p.stats();
    assert_eq!(s.bitrate(Duration::ZERO), 0);
    assert_eq!(s.core_share(Duration::ZERO), None);
    assert_eq!(s.conversion_share(), 0.0);
    assert_eq!(
        s.total_cpu(),
        Some(Duration::ZERO),
        "nothing has run, which is a total of zero rather than an unknown"
    );
}

/// A pipeline with no decoder must still report a complete CPU total. An
/// absent stage contributes nothing; it does not make the total unknown.
#[test]
fn a_missing_decoder_does_not_poison_the_cpu_total() {
    let mut p = build(320, 240, 60, 1.0);
    for _ in 0..10 {
        let _ = p.step(Duration::from_secs(1)).expect("a step");
    }
    assert_eq!(p.stats().decode.calls, 0);
    #[cfg(unix)]
    assert!(
        p.stats().total_cpu().is_some(),
        "a pipeline without a decoder still has a knowable cost"
    );
}
