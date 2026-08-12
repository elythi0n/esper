#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

//! **What does the host's desktop background cost on the wire?**
//!
//! A measurement, not a gate — the same shape as `keyframe_size.rs`, and the
//! reason `host.plain_background` exists.
//!
//! The question it answers: a desktop is windows over a background, and the
//! windows are what the person at the far end came for. If the background is a
//! photograph, what is it costing them?
//!
//! Answered as a **keyframe**, because that is where the whole cost lands: a
//! still background is sent once and never again — capture is damage-driven —
//! so what a wallpaper costs is the first picture and every keyframe after a
//! change of size. Which is precisely where this product measured its worst
//! behaviour: a link whose buffer could not hold one keyframe showed *no
//! picture at all* for as long as a session ran.
//!
//! Run it with `ESPER_MEASURE=1`.

use esper_capture::damage::Damage;
use esper_capture::frame::{Frame, PixelFormat};
use esper_capture::pattern::Pattern;
use esper_capture::pool::Pixels;
use esper_color::convert::whole_frame;
use esper_color::{ColorSpace, I420};
use esper_encode::budget::Fidelity;
use esper_encode::encoder::{EncoderConfig, VideoEncoder};
use esper_encode::openh264::OpenH264;

/// What is behind the windows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Behind {
    /// A photograph, or anything else with detail in it.
    Busy,
    /// One colour.
    Plain,
}

/// A desktop: windows covering `area` per cent of the screen, over `behind`.
///
/// **Centred, and one rectangle.** Several windows in the same total area
/// encode differently — more edges — and the point here is what the
/// *background* costs, so the foreground is held still while the background
/// changes.
fn desktop(pattern: &Pattern, w: u32, h: u32, area: u32, behind: Behind) -> Frame {
    let format = PixelFormat::Bgrx8;
    let bpp = format.bytes_per_pixel();
    let stride = (w as usize) * bpp + 64;
    let mut buf = vec![0u8; stride * h as usize];
    // A rectangle of the right area, in the middle, with the screen's shape.
    let scale = f64::from(area).max(0.0).sqrt() / 10.0;
    let (ww, wh) = ((f64::from(w) * scale) as u32, (f64::from(h) * scale) as u32);
    let (wx, wy) = ((w - ww.min(w)) / 2, (h - wh.min(h)) / 2);
    // A desktop grey, which is what a plain background actually looks like.
    let plain = [40u8, 42, 48];
    for y in 0..h {
        for x in 0..w {
            let in_window = x >= wx && x < wx + ww && y >= wy && y < wy + wh;
            let [r, g, b] = if in_window || behind == Behind::Busy {
                pattern.pixel(x, y, 0)
            } else {
                plain
            };
            let at = y as usize * stride + x as usize * bpp;
            buf[at] = b;
            buf[at + 1] = g;
            buf[at + 2] = r;
        }
    }
    Frame::new(w, h, stride, format, Damage::Everything, Pixels::owned(buf)).expect("a frame")
}

/// One keyframe of that desktop, in bytes.
fn keyframe(pattern: &Pattern, w: u32, h: u32, area: u32, behind: Behind, f: Fidelity) -> usize {
    let mut staging = I420::new(w, h).expect("a size");
    let mut encoder = OpenH264::new(&EncoderConfig {
        fps: 30,
        qp: Some(f.qp_range()),
        ..EncoderConfig::for_size(w, h)
    })
    .expect("an encoder");
    let frame = desktop(pattern, w, h, area, behind);
    let _ = whole_frame(&frame, ColorSpace::default(), &mut staging).expect("converted");
    encoder
        .encode(&staging, true)
        .expect("encoded")
        .bitstream
        .len()
}

#[test]
fn what_the_desktop_background_costs() {
    if std::env::var_os("ESPER_MEASURE").is_none() {
        println!("skipped: set ESPER_MEASURE=1 to run this measurement");
        return;
    }
    println!("\n  size       fidelity   windows   busy behind   plain behind   saved");
    for (w, h) in [(1280u32, 720u32), (640, 360)] {
        let pattern = Pattern::new(w, h).expect("a size");
        for fidelity in [Fidelity::Balanced, Fidelity::Coarse] {
            for area in [10u32, 25, 50] {
                let busy = keyframe(&pattern, w, h, area, Behind::Busy, fidelity);
                let plain = keyframe(&pattern, w, h, area, Behind::Plain, fidelity);
                let saved = busy.saturating_sub(plain);
                println!(
                    "  {w}x{h}   {fidelity:?}   {area:>5}%   {busy:>10}   {plain:>11}   \
                     {saved:>6} ({:>2}%)",
                    saved
                        .checked_mul(100)
                        .and_then(|n| n.checked_div(busy))
                        .unwrap_or(0)
                );
            }
        }
    }
    println!();
}
