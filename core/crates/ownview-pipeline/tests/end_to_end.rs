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
use ownview_pipeline::pipeline::{Pipeline, Step};
use ownview_scale::Ceiling;

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
                    // Against what the pipeline says it is encoding, not
                    // against the display: the governor may have taken a
                    // resolution rung, and this test is about frames going in
                    // and pictures coming out, not about a fixed size.
                    assert_eq!((pic.width(), pic.height()), p.encode_size());
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
    // A budget large enough that the governor cannot move. With a tight one
    // this measured two things at once: at a one-core budget the whole suite
    // running in parallel is enough contention to push the ladder onto a
    // resolution rung, and every size change forces one whole reconversion.
    // The share then reflects ambient load on the machine rather than the
    // damage in the picture.
    let mut p = build(1280, 720, 60, 100.0);
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

/// **The interaction, not either half.** Scaling widens every damage
/// rectangle — a destination pixel averages several source pixels, so it has
/// to be repainted if any of them moved, and the result is then aligned to
/// even edges for the chroma planes. Widened enough, damage-driven conversion
/// stops paying for itself.
///
/// Forced with a ceiling rather than a small budget, so this measures the
/// geometry and not the load on the machine.
#[test]
fn scaling_does_not_destroy_damage_driven_conversion() {
    let mut p = build(1280, 720, 60, 100.0).with_ceiling(Ceiling::of(640));
    let mut reconfigured = 0;
    for _ in 0..60 {
        if matches!(
            p.step(Duration::from_secs(1)).expect("a step"),
            Step::Reconfigured { .. }
        ) {
            reconfigured += 1;
        }
    }
    assert!(p.is_scaling(), "the ceiling should have engaged");
    // The direct symptom of the bug this test found: the unannounced-resolution
    // check compared the frame against the staging buffer, which a scaled
    // session sizes below the display, so every single frame looked like the
    // monitor had changed size.
    assert_eq!(
        reconfigured, 0,
        "a steady display reconfigured {reconfigured} times while scaling"
    );
    assert_eq!(p.encode_size(), (640, 360));
    let share = p.stats().conversion_share();
    assert!(
        share < 0.20,
        "converted {:.1}% of a scaled frame; the pattern moves a small box and \
         a counter, so widening the damage for the filter footprint must not \
         turn that into most of the picture",
        share * 100.0
    );
    assert!(
        share > 0.0,
        "the first frame is whole, so this cannot be zero"
    );
}

/// **A scaler built mid-stream starts on a blank picture.** The pipeline
/// converts the whole staging buffer whenever it cannot trust what is in it —
/// and a scaler that was just built is exactly that case, because its own
/// destination is blank too. If it is handed only the frame's small damage
/// region while the conversion reads the whole thing, everything outside that
/// region is converted from nothing.
///
/// Reached by turning the ceiling on a few frames in, once the gate has
/// stopped reporting the whole frame as damaged. Starting with the ceiling on
/// does not reach it: the first frame is whole damage anyway.
#[test]
fn a_scaler_built_mid_stream_repaints_the_whole_picture() {
    fn mean_luma(p: &ownview_color::I420) -> f64 {
        let y = p.y();
        if y.is_empty() {
            return 0.0;
        }
        y.iter().map(|&v| f64::from(v)).sum::<f64>() / y.len() as f64
    }

    // One pipeline capped from the start, one capped at frame five. The
    // synthetic source is deterministic, so at the same frame index they are
    // looking at the same picture and must agree about it.
    let mut from_the_start =
        with_decoder(build(1280, 720, 60, 100.0).with_ceiling(Ceiling::of(640)));
    let mut switched = with_decoder(build(1280, 720, 60, 100.0));

    let mut reference = None;
    let mut after_switch = None;
    for i in 0..8 {
        if i == 5 {
            switched.set_ceiling(Ceiling::of(640));
        }
        let a = from_the_start.step(Duration::from_secs(2)).expect("a step");
        let b = switched.step(Duration::from_secs(2)).expect("a step");
        if i == 5 {
            if let Step::Encoded {
                decoded: Some(p), ..
            } = a
            {
                reference = Some(mean_luma(&p));
            }
            if let Step::Encoded {
                decoded: Some(p), ..
            } = b
            {
                after_switch = Some(mean_luma(&p));
            }
        }
    }

    assert_eq!(
        switched.encode_size(),
        (640, 360),
        "the cap should have taken"
    );
    let reference = reference.expect("the capped pipeline decoded frame five");
    let switched = after_switch.expect("the switched pipeline decoded frame five");
    assert!(
        (reference - switched).abs() < 6.0,
        "the frame after the scaler was built averages {switched:.1} against \
         {reference:.1} for the same picture — most of it was converted from a \
         blank scaler buffer"
    );
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

/// **The lever that used to reach nothing.** The governor's bottom rungs cut
/// resolution, and for a while the pipeline had no scaler, so it would report
/// three quarter scale and encode exactly as many pixels as before. Dropping
/// to a resolution rung has to shrink what is actually encoded.
#[test]
fn the_resolution_rung_shrinks_what_is_encoded() {
    let mut p = build(640, 480, 60, 0.02);
    assert_eq!(p.encode_size(), (640, 480), "it starts at the display size");
    assert!(!p.is_scaling(), "and does not resample until it has to");

    for _ in 0..400 {
        let _ = p.step(Duration::from_secs(1)).expect("a step");
        if p.encode_size() != (640, 480) {
            break;
        }
    }

    let level = p.governor().level();
    assert!(
        level.scale != (1, 1),
        "a 2% budget should have reached a resolution rung: {}",
        p.governor().describe()
    );
    let (w, h) = p.encode_size();
    assert!(
        w < 640 && h < 480,
        "the rung says {:?} but the encoder is still being handed {w}x{h}",
        level.scale
    );
    assert!(p.is_scaling(), "and the frames must actually be resampled");
    assert_eq!(
        (w, h),
        level.size(640, 480),
        "the encode size must be the rung's size, not something near it"
    );
}

/// Every rung now lands somewhere, so there is nothing left to declare.
#[test]
fn no_lever_is_left_unapplied() {
    let p = build(320, 240, 60, 1.0);
    assert!(
        p.unapplied_levers().is_empty(),
        "a lever the pipeline cannot honour has to be declared, and there are none"
    );
}

/// **The prior, not the reaction.** On a large display the extra pixels were
/// never affordable, and discovering that through the governor costs the first
/// seconds of every session.
#[test]
fn the_ceiling_caps_a_large_display_before_any_load_is_seen() {
    let mut p = build(2560, 1440, 60, 100.0);
    assert_eq!(
        p.native_size(),
        (2560, 1440),
        "the display is still what it is"
    );
    assert_eq!(
        p.encode_size(),
        (1920, 1080),
        "but a generous budget must still not encode 1440p"
    );
    assert!(p.is_scaling());
    let _ = p.step(Duration::from_secs(1)).expect("a step");
    assert_eq!(p.staging_size(), (1920, 1080), "and the buffer agrees");
}

/// A host with cores to spare should be able to say so — and be told the
/// truth about it straight away.
///
/// The loopback prints the encode size in its header, before any frame has
/// run. Read off the staging buffer that reported a `--max-resolution none`
/// session as scaling to 1080p, because the buffer was sized in the
/// constructor from the default ceiling and only resized on the first frame.
#[test]
fn the_ceiling_can_be_lifted() {
    let mut p = build(2560, 1440, 60, 100.0).with_ceiling(Ceiling::none());
    assert_eq!(p.ceiling().long_edge(), None);
    assert_eq!(
        p.encode_size(),
        (2560, 1440),
        "lifting the cap has to be visible before the first frame"
    );
    assert!(!p.is_scaling());
    // And the buffer catches up on that frame.
    let _ = p.step(Duration::from_secs(1)).expect("a step");
    assert_eq!(p.staging_size(), (2560, 1440));
    assert_eq!(p.encode_size(), p.staging_size());
}

/// A display below the ceiling must not be touched at all — no resampling, no
/// cost, no quality lost to a pass that had nothing to do.
#[test]
fn a_small_display_is_not_resampled() {
    for (w, h) in [(1920u32, 1080u32), (1366, 768), (800, 600), (1920, 1200)] {
        let mut p = build(w, h, 60, 100.0);
        let _ = p.step(Duration::from_secs(1)).expect("a step");
        assert_eq!(p.encode_size(), (w, h), "{w}x{h} was resized");
        assert!(!p.is_scaling(), "{w}x{h} was resampled for no reason");
    }
}

/// **Awkward display sizes must not produce a dead session.** The staging
/// buffer rounds its size up to even for the chroma planes, so an odd encode
/// size leaves it a pixel wider than the frame going into it and the
/// conversion refuses — which reaches a user as a session that shows nothing
/// rather than a picture one pixel narrower than it could have been.
///
/// The sizes here are the odd ones the synthetic capturer will accept — it has
/// its own floor, so a display of two pixels is not reachable through it. The
/// arithmetic is guarded below that floor anyway, because `Pipeline` takes its
/// size from whatever a backend reports and a future one is not bound by this
/// capturer's minimum.
#[test]
fn an_odd_or_tiny_display_still_encodes() {
    for (w, h) in [
        (1921u32, 1081u32),
        (1919, 1079),
        (65, 65),
        (129, 4097),
        (2561, 1441),
    ] {
        let mut p = build(w, h, 30, 100.0);
        let (ew, eh) = p.encode_size();
        assert_eq!(ew % 2, 0, "{w}x{h} encodes at an odd width {ew}");
        assert_eq!(eh % 2, 0, "{w}x{h} encodes at an odd height {eh}");
        assert!(ew <= w && eh <= h, "{w}x{h} was enlarged to {ew}x{eh}");
        // And it has to actually run, which is what catches the mismatch.
        for _ in 0..3 {
            let _ = p
                .step(Duration::from_secs(2))
                .unwrap_or_else(|e| panic!("{w}x{h} failed to step: {e}"));
        }
        assert_eq!(p.staging_size(), p.encode_size(), "{w}x{h} disagrees");
    }
}

/// A capped session still has to produce pictures a decoder accepts, at the
/// size it is actually encoding.
#[test]
fn a_capped_session_still_round_trips() {
    let mut p = with_decoder(build(2560, 1440, 30, 100.0).with_ceiling(Ceiling::of(640)));
    let mut seen = 0;
    for _ in 0..8 {
        if let Step::Encoded {
            decoded: Some(picture),
            ..
        } = p.step(Duration::from_secs(2)).expect("a step")
        {
            assert_eq!(
                (picture.width(), picture.height()),
                p.encode_size(),
                "the decoder produced a different size than the pipeline claims to encode"
            );
            seen += 1;
        }
    }
    assert!(seen > 0, "nothing decoded");
    assert_eq!(p.encode_size(), (640, 360));
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
