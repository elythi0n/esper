#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

//! What walking the degradation ladder costs in keyframes.
//!
//! The ladder is spent on links that are already struggling, and a keyframe is
//! the largest frame there is — so the frames a rung change *forces* are the
//! ones a slow link is least able to carry. This walks the real `LADDER` down
//! and back up and counts them, because "a rung need not cost a keyframe" is a
//! claim about a number.

use esper_color::i420::I420;
use esper_encode::budget::LADDER;
use esper_encode::{EncoderConfig, OpenH264, VideoEncoder};
use std::time::Duration;

/// Screen-shaped content: smooth ground, hard edges, a little text-like
/// detail, and something moving so there is real work to do.
fn picture(w: u32, h: u32, shift: u32) -> I420 {
    let mut img = I420::new(w, h).expect("a valid size");
    let stride = img.y_stride();
    let (y, u, v) = img.planes_mut();
    for row in 0..h as usize {
        for col in 0..w as usize {
            let c = (col + shift as usize) % w as usize;
            let ground = if (row / 16 + c / 16).is_multiple_of(2) {
                28
            } else {
                34
            };
            let edge = if c % 64 < 2 || row % 48 < 2 {
                210
            } else {
                ground
            };
            let glyph = if row % 48 >= 8 && row % 48 < 20 && (c / 3 + row).is_multiple_of(7) {
                190
            } else {
                edge
            };
            if let Some(p) = y.get_mut(row * stride + col) {
                *p = glyph;
            }
        }
    }
    u.fill(124);
    v.fill(132);
    img
}

/// Walk the ladder, encoding at each rung, and report what it cost.
///
/// The keyframe interval is deliberately far longer than the walk. There are
/// two reasons a keyframe appears — the periodic one the interval asks for,
/// and the one a rung change forces — and a measurement that cannot tell them
/// apart measures neither. At five seconds and fifteen frames a second a
/// periodic keyframe lands every seventy-five pictures, which is inside this
/// walk; pushed out of reach, every keyframe after the first is a rung being
/// paid for.
fn walk(native: (u32, u32), rungs: &[usize], per_rung: u32) -> (usize, usize) {
    let out_of_reach = Duration::from_secs(3600);
    let first = LADDER.first().expect("a ladder with rungs");
    let mut config = EncoderConfig {
        keyframe_interval: out_of_reach,
        ..EncoderConfig::for_size(native.0, native.1)
    }
    .with_level(first);
    let mut enc = OpenH264::new(&config).expect("an encoder");

    let mut keyframes = 0usize;
    let mut bytes = 0usize;
    let mut shift = 0u32;

    for &rung in rungs {
        let level = LADDER.get(rung).expect("a rung in the ladder");
        let (w, h) = level.size(native.0, native.1);
        config = EncoderConfig {
            keyframe_interval: out_of_reach,
            ..EncoderConfig::for_size(w, h)
        }
        .with_level(level);
        enc.reconfigure(&config).expect("reconfigure");

        for _ in 0..per_rung {
            shift = shift.wrapping_add(3);
            let out = enc.encode(&picture(w, h, shift), false).expect("encode");
            bytes += out.bitstream.len();
            if out.keyframe {
                keyframes += 1;
            }
        }
    }
    (keyframes, bytes)
}

/// **The top of the ladder is free now, in both directions.**
///
/// Rungs 0..=3 are 60, 30, 20 and 15 fps at full size and full fidelity —
/// framerate and nothing else. Falling through all four and recovering back to
/// the top is six transitions, and every one of them used to rebuild the
/// encoder and force an IDR.
///
/// The periodic keyframe is pushed out of reach by `walk`, so the only
/// keyframe that may appear is the first — the one a decoder holding no
/// reference cannot do without.
#[test]
fn falling_and_recovering_through_the_framerate_rungs_costs_one_keyframe() {
    let (keyframes, _) = walk((640, 480), &[0, 1, 2, 3, 2, 1, 0], 20);
    assert_eq!(
        keyframes, 1,
        "only the very first picture should be a keyframe: six framerate \
         transitions and none of them changes anything a reference picture \
         describes"
    );
}

/// And the rungs that genuinely change the picture still pay, because they
/// have to: a smaller picture invalidates every reference the decoder holds,
/// and openh264 will not move the quantiser on a running encoder.
#[test]
fn the_rungs_that_change_the_picture_still_cost_a_keyframe() {
    // 3 → 4 is fidelity, 4 → 6 reaches a scale change. Four transitions that
    // each have to rebuild, plus the first picture.
    let (keyframes, _) = walk((640, 480), &[3, 4, 5, 6, 3], 10);
    assert!(
        keyframes >= 5,
        "{keyframes} keyframe(s): a fidelity or resolution change has to \
         rebuild the encoder, and a rebuilt encoder owes an IDR"
    );
}

/// The saving, printed rather than asserted, so the number is visible when
/// somebody runs the suite with `--nocapture`.
#[test]
fn what_the_framerate_rungs_cost_in_bytes() {
    let (keys, bytes) = walk((640, 480), &[0, 1, 2, 3, 2, 1, 0], 20);
    println!("  ladder 60->15->60, 140 pictures: {keys} keyframe(s), {bytes} bytes");
    assert!(bytes > 0);
}
