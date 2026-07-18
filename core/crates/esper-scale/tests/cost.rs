#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! What downscaling costs, and whether it pays for itself.
//!
//! The resolution rung exists to *save* CPU. A scaler that costs more than
//! the pixels it removes would make the bottom of the ladder the most
//! expensive place on it, which is the kind of thing that only shows up as
//! "the picture gets worse and it still stutters".
//!
//! So the number that matters is not the scaler in isolation. It is
//! **scale-then-convert at the smaller size** against **convert at the
//! native size**, because that is the choice the pipeline actually makes.
//!
//! The scaler is single-threaded, so wall clock and CPU time are the same
//! number here. That is worth stating rather than assuming: measuring the
//! threaded encoder in wall clock once reported it as three times cheaper
//! than it was.
//!
//! `#[ignore]`d, because a timing assertion on a shared CI runner measures the
//! runner. Run them deliberately:
//!
//! ```text
//! cargo test --release -p esper-scale -- --ignored --nocapture
//! ```
//!
//! and put the result in the project's findings with the machine it came from.
//! A number without a machine is not a measurement.

use std::time::Instant;

use esper_capture::damage::{Damage, Regions};
use esper_capture::frame::{Frame, PixelFormat};
use esper_capture::geometry::Rect;
use esper_capture::pattern::Pattern;
use esper_capture::pool::Pixels;
use esper_color::convert::{regions, whole_frame};
use esper_color::{ColorSpace, I420};
use esper_scale::{Ceiling, Scaler};

/// A realistic frame: the synthetic pattern, in the layout X11 hands over,
/// with a padded stride.
fn frame_of(w: u32, h: u32, index: u64, damage: Damage) -> Frame {
    let pattern = Pattern::new(w, h).expect("valid size");
    let format = PixelFormat::Bgrx8;
    let bpp = format.bytes_per_pixel();
    let row = (w as usize) * bpp;
    let stride = row.div_ceil(64) * 64;
    let mut buf = vec![0u8; stride * h as usize];
    for y in 0..h {
        for x in 0..w {
            let [r, g, b] = pattern.pixel(x, y, index);
            let at = y as usize * stride + x as usize * bpp;
            if let Some(px) = buf.get_mut(at..at + bpp) {
                if let Some(c) = px.first_mut() {
                    *c = b;
                }
                if let Some(c) = px.get_mut(1) {
                    *c = g;
                }
                if let Some(c) = px.get_mut(2) {
                    *c = r;
                }
            }
        }
    }
    Frame::new(w, h, stride, format, damage, Pixels::owned(buf)).expect("a valid frame")
}

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    v.get(v.len() / 2).copied().unwrap_or(0.0)
}

const RUNS: usize = 21;

/// Whole-frame downscaling, at the sizes the ceiling and the ladder produce.
#[test]
#[ignore = "a measurement, not an assertion: run it deliberately"]
fn measure_whole_frame_scaling() {
    println!("\nwhole-frame downscale, BGRX in, area average:");
    println!("{:>13}  {:>13}  {:>9}  {:>10}", "from", "to", "ms", "Mpx/s");
    for (sw, sh, dw, dh, why) in [
        (
            2560u32,
            1440u32,
            1920u32,
            1080u32,
            "1440p under the ceiling",
        ),
        (3840, 2160, 1920, 1080, "4K under the ceiling"),
        (1920, 1080, 1440, 810, "the 3/4 rung"),
        (1920, 1080, 960, 540, "the 1/2 rung"),
        (3840, 2160, 1920, 1080, "4K under the ceiling"),
    ] {
        let mut scaler = Scaler::new(sw, sh, dw, dh, PixelFormat::Bgrx8).expect("a scaler");
        let frame = frame_of(sw, sh, 1, Damage::Everything);
        // Warm the buffers so the first page fault is not in the sample.
        let _ = scaler.scale(&frame, &Damage::Everything).expect("scales");
        let mut samples = Vec::new();
        for _ in 0..RUNS {
            let at = Instant::now();
            let _ = scaler.scale(&frame, &Damage::Everything).expect("scales");
            samples.push(at.elapsed().as_secs_f64() * 1000.0);
        }
        let ms = median(samples);
        let mpx = f64::from(sw) * f64::from(sh) / 1e6 / (ms / 1000.0);
        println!("{sw:>6}x{sh:<6} {dw:>6}x{dh:<6}  {ms:>9.3}  {mpx:>10.1}   {why}");
    }
}

/// Damage-driven downscaling: a caret, a line of text, a small window.
#[test]
#[ignore = "a measurement, not an assertion: run it deliberately"]
fn measure_damaged_region_scaling() {
    println!("\ndamage-driven downscale, 1920x1080 -> 960x540:");
    println!("{:>20}  {:>9}  {:>8}", "damage", "ms", "% of whole");
    let mut scaler = Scaler::new(1920, 1080, 960, 540, PixelFormat::Bgrx8).expect("a scaler");
    let bounds = Rect {
        x: 0,
        y: 0,
        width: 1920,
        height: 1080,
    };
    let whole_frame_ms = {
        let frame = frame_of(1920, 1080, 1, Damage::Everything);
        let _ = scaler.scale(&frame, &Damage::Everything).expect("scales");
        let mut s = Vec::new();
        for _ in 0..RUNS {
            let at = Instant::now();
            let _ = scaler.scale(&frame, &Damage::Everything).expect("scales");
            s.push(at.elapsed().as_secs_f64() * 1000.0);
        }
        median(s)
    };
    for (w, h, why) in [
        (2u32, 20u32, "a text caret"),
        (400, 18, "a line of text"),
        (640, 480, "a small window"),
    ] {
        let rect = Rect {
            x: 200,
            y: 300,
            width: w,
            height: h,
        };
        let damage = Regions::coalesce(&[rect], &bounds);
        let frame = frame_of(1920, 1080, 1, damage.clone());
        let _ = scaler.scale(&frame, &damage).expect("scales");
        let mut samples = Vec::new();
        for _ in 0..RUNS {
            let at = Instant::now();
            let _ = scaler.scale(&frame, &damage).expect("scales");
            samples.push(at.elapsed().as_secs_f64() * 1000.0);
        }
        let ms = median(samples);
        println!(
            "{:>20}  {ms:>9.3}  {:>8.2}   {why}",
            format!("{w}x{h}"),
            ms / whole_frame_ms * 100.0
        );
    }
    println!(
        "{:>20}  {whole_frame_ms:>9.3}  {:>8.2}",
        "the whole frame", 100.0
    );
}

/// **The question that decides whether the rung is worth having.**
///
/// Scale-then-convert against convert-at-native. If the left-hand side is not
/// smaller, the resolution rung costs CPU to reduce quality and should be
/// taken off the ladder.
#[test]
#[ignore = "a measurement, not an assertion: run it deliberately"]
fn measure_whether_scaling_pays_for_itself() {
    let space = ColorSpace::default();
    println!("\ndoes the resolution rung pay for itself? (whole frames)");
    println!(
        "{:>13}  {:>13}  {:>11}  {:>11}  {:>11}  {:>7}",
        "display", "encoding", "convert", "scale", "scale+conv", "saved"
    );
    for (sw, sh, dw, dh) in [
        (1920u32, 1080u32, 1440u32, 810u32),
        (1920, 1080, 960, 540),
        (2560, 1440, 1920, 1080),
        (3840, 2160, 1920, 1080),
    ] {
        let frame = frame_of(sw, sh, 1, Damage::Everything);

        // Convert at the display size: what the pipeline does with no rung.
        let mut native = I420::new(sw, sh).expect("a buffer");
        let _ = whole_frame(&frame, space, &mut native).expect("converts");
        let mut s = Vec::new();
        for _ in 0..RUNS {
            let at = Instant::now();
            let _ = whole_frame(&frame, space, &mut native).expect("converts");
            s.push(at.elapsed().as_secs_f64() * 1000.0);
        }
        let convert_native = median(s);

        // Scale, then convert at the smaller size.
        let mut scaler = Scaler::new(sw, sh, dw, dh, PixelFormat::Bgrx8).expect("a scaler");
        let mut small = I420::new(dw, dh).expect("a buffer");
        {
            let scaled = scaler.scale(&frame, &Damage::Everything).expect("scales");
            let _ = whole_frame(scaled, space, &mut small).expect("converts");
        }
        let mut scale_only = Vec::new();
        let mut both = Vec::new();
        for _ in 0..RUNS {
            let at = Instant::now();
            let scaled = scaler.scale(&frame, &Damage::Everything).expect("scales");
            let mid = at.elapsed().as_secs_f64() * 1000.0;
            let _ = whole_frame(scaled, space, &mut small).expect("converts");
            let all = at.elapsed().as_secs_f64() * 1000.0;
            scale_only.push(mid);
            both.push(all);
        }
        let scale = median(scale_only);
        let combined = median(both);
        let saved = (convert_native - combined) / convert_native * 100.0;
        println!(
            "{sw:>6}x{sh:<6} {dw:>6}x{dh:<6}  {convert_native:>11.3}  {scale:>11.3}  \
             {combined:>11.3}  {saved:>6.1}%"
        );
    }
    println!(
        "\n(the encoder's saving is on top of this, and is the larger half: it \
         works on the same reduced pixel count)"
    );
}

/// And the same question for the case that actually dominates a desktop: a
/// small damaged region rather than a whole frame.
#[test]
#[ignore = "a measurement, not an assertion: run it deliberately"]
fn measure_whether_scaling_pays_on_a_damaged_region() {
    let space = ColorSpace::default();
    println!("\ndoes it still pay on a damaged region? (1920x1080 display)");
    println!(
        "{:>13}  {:>20}  {:>11}  {:>11}  {:>7}",
        "encoding", "damage", "convert", "scale+conv", "saved"
    );
    let bounds = Rect {
        x: 0,
        y: 0,
        width: 1920,
        height: 1080,
    };
    for (dw, dh) in [(1440u32, 810u32), (960, 540)] {
        for (w, h, why) in [(400u32, 18u32, "a line of text"), (640, 480, "a window")] {
            let rect = Rect {
                x: 200,
                y: 300,
                width: w,
                height: h,
            };
            let damage = Regions::coalesce(&[rect], &bounds);
            let frame = frame_of(1920, 1080, 1, damage.clone());
            let rects = damage.rects(&bounds);

            let mut native = I420::new(1920, 1080).expect("a buffer");
            let _ = whole_frame(&frame, space, &mut native).expect("converts");
            let mut s = Vec::new();
            for _ in 0..RUNS {
                let at = Instant::now();
                let _ = regions(&frame, space, &mut native, &rects).expect("converts");
                s.push(at.elapsed().as_secs_f64() * 1000.0);
            }
            let convert_native = median(s);

            let mut scaler = Scaler::new(1920, 1080, dw, dh, PixelFormat::Bgrx8).expect("scaler");
            let mut small = I420::new(dw, dh).expect("a buffer");
            {
                let scaled = scaler.scale(&frame, &Damage::Everything).expect("scales");
                let _ = whole_frame(scaled, space, &mut small).expect("converts");
            }
            let mut both = Vec::new();
            for _ in 0..RUNS {
                let at = Instant::now();
                let scaled = scaler.scale(&frame, &damage).expect("scales");
                let scaled_rects = scaled.damage().rects(&scaled.bounds());
                let _ = regions(scaled, space, &mut small, &scaled_rects).expect("converts");
                both.push(at.elapsed().as_secs_f64() * 1000.0);
            }
            let combined = median(both);
            let saved = (convert_native - combined) / convert_native * 100.0;
            println!(
                "{dw:>6}x{dh:<6}  {:>20}  {convert_native:>11.3}  {combined:>11.3}  {saved:>6.1}%   {why}",
                format!("{w}x{h}")
            );
        }
    }
}

/// What the default ceiling does to the fleet's displays.
#[test]
#[ignore = "a measurement, not an assertion: run it deliberately"]
fn measure_the_ceiling_on_real_displays() {
    println!("\nthe default ceiling on displays this is built for:");
    println!(
        "{:>13}  {:>13}  {:>9}  {:>8}",
        "display", "encoding", "Mpx", "removed"
    );
    let c = Ceiling::default();
    for (w, h, why) in [
        (1366u32, 768u32, "an old laptop"),
        (1920, 1080, "1080p"),
        (1920, 1200, "a 16:10 laptop"),
        (2560, 1440, "1440p"),
        (2880, 1800, "a retina 15 inch"),
        (3440, 1440, "an ultrawide"),
        (3840, 2160, "4K"),
        (5120, 2880, "5K"),
    ] {
        let (ow, oh) = c.apply(w, h);
        let before = f64::from(w) * f64::from(h);
        let after = f64::from(ow) * f64::from(oh);
        println!(
            "{w:>6}x{h:<6} {ow:>6}x{oh:<6}  {:>9.2}  {:>7.0}%   {why}",
            after / 1e6,
            (1.0 - after / before) * 100.0
        );
    }
}
