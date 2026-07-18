#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! What the conversion actually costs.
//!
//! The pipeline's budget gives colour conversion one millisecond of a 16.6 ms
//! frame. That number is a target taken from what the rest of the pipeline
//! needs, not a measurement, and a target nobody has measured is a wish.
//!
//! These are `#[ignore]`d, because a timing assertion on a shared CI runner
//! measures the runner. Run them deliberately:
//!
//! ```text
//! cargo test --release -p esper-color -- --ignored --nocapture
//! ```
//!
//! and put the result in the project's findings with the machine it came from.
//! A number without a machine is not a measurement.

use std::time::Instant;

use esper_capture::damage::Damage;
use esper_capture::frame::{Frame, PixelFormat};
use esper_capture::geometry::Rect;
use esper_capture::pattern::Pattern;
use esper_capture::pool::Pixels;
use esper_color::convert::{regions, whole_frame};
use esper_color::{ColorSpace, I420};

/// A realistic frame: the synthetic pattern, in the layout X11 hands over,
/// with a padded stride.
fn frame_of(w: u32, h: u32, index: u64) -> Frame {
    let pattern = Pattern::new(w, h).expect("valid size");
    let format = PixelFormat::Bgrx8;
    let bpp = format.bytes_per_pixel();
    // A stride padded to 64 bytes, as a real backend produces.
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
    Frame::new(w, h, stride, format, Damage::Everything, Pixels::owned(buf)).expect("a valid frame")
}

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    v.get(v.len() / 2).copied().unwrap_or(0.0)
}

/// Whole-frame conversion, at the sizes that matter.
#[test]
#[ignore = "a measurement, not an assertion: run it deliberately"]
fn measure_whole_frame_conversion() {
    let space = ColorSpace::default();
    println!("\nwhole-frame conversion, BT.709 limited, BGRX in:");
    println!(
        "  {:>11}  {:>10}  {:>12}  {:>10}",
        "size", "ms/frame", "Mpixel/s", "budget"
    );

    for (w, h, label) in [
        (1280u32, 720u32, "720p"),
        (1920, 1080, "1080p"),
        (2560, 1440, "1440p"),
        (3840, 2160, "4K"),
    ] {
        let frame = frame_of(w, h, 0);
        let mut img = I420::new(w, h).expect("valid");
        // Warm the buffers and the branch predictor.
        for _ in 0..3 {
            let _ = whole_frame(&frame, space, &mut img).expect("converted");
        }

        let mut samples = Vec::new();
        for _ in 0..30 {
            let t = Instant::now();
            let _ = whole_frame(&frame, space, &mut img).expect("converted");
            samples.push(t.elapsed().as_secs_f64() * 1000.0);
        }
        let ms = median(samples);
        let mpix = (f64::from(w) * f64::from(h)) / (ms / 1000.0) / 1e6;
        let verdict = if ms <= 1.0 { "within" } else { "OVER" };
        println!("  {label:>11}  {ms:>10.3}  {mpix:>12.1}  {verdict:>10}");
    }
}

/// The damage-driven case, which is what the pipeline actually does.
#[test]
#[ignore = "a measurement, not an assertion: run it deliberately"]
fn measure_damage_driven_conversion() {
    let space = ColorSpace::default();
    let (w, h) = (1920u32, 1080u32);
    let frame = frame_of(w, h, 1);
    let mut img = I420::new(w, h).expect("valid");
    let _ = whole_frame(&frame, space, &mut img).expect("converted");

    println!("\ndamage-driven conversion at 1080p:");
    println!(
        "  {:>22}  {:>9}  {:>10}  {:>9}",
        "what changed", "pixels", "ms", "of whole"
    );

    let whole = {
        let mut samples = Vec::new();
        for _ in 0..20 {
            let t = Instant::now();
            let _ = whole_frame(&frame, space, &mut img).expect("converted");
            samples.push(t.elapsed().as_secs_f64() * 1000.0);
        }
        median(samples)
    };

    for (label, rects) in [
        ("a text cursor", vec![Rect::new(400, 300, 2, 18)]),
        ("a word of text", vec![Rect::new(400, 300, 60, 18)]),
        ("a line of text", vec![Rect::new(0, 300, 1920, 18)]),
        ("a scrolling window", vec![Rect::new(200, 100, 1200, 800)]),
        ("the whole screen", vec![Rect::sized(w, h)]),
    ] {
        let mut samples = Vec::new();
        let mut converted = 0u64;
        for _ in 0..20 {
            let t = Instant::now();
            converted = regions(&frame, space, &mut img, &rects).expect("converted");
            samples.push(t.elapsed().as_secs_f64() * 1000.0);
        }
        let ms = median(samples);
        let share = if whole > 0.0 { ms / whole * 100.0 } else { 0.0 };
        println!("  {label:>22}  {converted:>9}  {ms:>10.4}  {share:>8.1}%");
    }
    println!("  (whole frame for reference: {whole:.3} ms)");
}

/// Whether the pixel layout costs anything. It should not — the channel
/// offsets are read once per region, not per pixel — and if it does, the loop
/// is doing something per pixel that it should be doing once.
#[test]
#[ignore = "a measurement, not an assertion: run it deliberately"]
fn measure_the_cost_of_each_pixel_layout() {
    let space = ColorSpace::default();
    let (w, h) = (1920u32, 1080u32);
    println!("\ncost per pixel layout at 1080p:");
    for format in [
        PixelFormat::Bgra8,
        PixelFormat::Bgrx8,
        PixelFormat::Rgba8,
        PixelFormat::Rgbx8,
    ] {
        // Rebuild the frame in this layout.
        let pattern = Pattern::new(w, h).expect("valid");
        let bpp = format.bytes_per_pixel();
        let stride = ((w as usize) * bpp).div_ceil(64) * 64;
        let mut buf = vec![0u8; stride * h as usize];
        let (r_at, g_at, b_at) = format.rgb_offsets();
        for y in 0..h {
            for x in 0..w {
                let [r, g, b] = pattern.pixel(x, y, 0);
                let at = y as usize * stride + x as usize * bpp;
                if let Some(px) = buf.get_mut(at..at + bpp) {
                    if let Some(c) = px.get_mut(r_at) {
                        *c = r;
                    }
                    if let Some(c) = px.get_mut(g_at) {
                        *c = g;
                    }
                    if let Some(c) = px.get_mut(b_at) {
                        *c = b;
                    }
                }
            }
        }
        let frame = Frame::new(w, h, stride, format, Damage::Everything, Pixels::owned(buf))
            .expect("valid");

        let mut img = I420::new(w, h).expect("valid");
        for _ in 0..3 {
            let _ = whole_frame(&frame, space, &mut img).expect("converted");
        }
        let mut samples = Vec::new();
        for _ in 0..20 {
            let t = Instant::now();
            let _ = whole_frame(&frame, space, &mut img).expect("converted");
            samples.push(t.elapsed().as_secs_f64() * 1000.0);
        }
        println!("  {:>8}  {:>8.3} ms", format.name(), median(samples));
    }
}
