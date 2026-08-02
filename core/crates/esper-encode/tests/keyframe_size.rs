//! A measurement, not a gate: how big is a keyframe at the small end?
//!
//! The question it answers: on a link whose buffer holds 2.5 KB, is there any
//! configuration this product can reach whose keyframe fits? The tracker says
//! no; this says by how much.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use esper_capture::damage::Damage;
use esper_capture::frame::{Frame, PixelFormat};
use esper_capture::pattern::Pattern;
use esper_capture::pool::Pixels;
use esper_color::convert::whole_frame;
use esper_color::{ColorSpace, I420};
use esper_encode::budget::Fidelity;
use esper_encode::encoder::{EncoderConfig, VideoEncoder};
use esper_encode::openh264::OpenH264;

fn captured(pattern: &Pattern, index: u64) -> Frame {
    let (w, h) = (pattern.width(), pattern.height());
    let format = PixelFormat::Bgrx8;
    let bpp = format.bytes_per_pixel();
    let stride = (w as usize) * bpp + 64;
    let mut buf = vec![0u8; stride * h as usize];
    for y in 0..h {
        for x in 0..w {
            let [r, g, b] = pattern.pixel(x, y, index);
            let at = y as usize * stride + x as usize * bpp;
            buf[at] = b;
            buf[at + 1] = g;
            buf[at + 2] = r;
        }
    }
    Frame::new(w, h, stride, format, Damage::Everything, Pixels::owned(buf)).expect("a frame")
}

#[test]
fn how_big_is_a_keyframe_at_the_small_end() {
    if std::env::var_os("ESPER_MEASURE").is_none() {
        println!("skipped: set ESPER_MEASURE=1 to run this measurement");
        return;
    }
    println!("\n  size      fidelity   keyframe   delta   datagrams(1200B)");
    for (w, h) in [(320u32, 180u32), (480, 270), (640, 360)] {
        for fidelity in [Fidelity::Balanced, Fidelity::Low, Fidelity::Coarse] {
            let mut staging = I420::new(w, h).expect("a size");
            let pattern = Pattern::new(w, h).expect("a size");
            let range = fidelity.qp_range();
            let mut encoder = OpenH264::new(&EncoderConfig {
                fps: 30,
                qp: Some(range),
                ..EncoderConfig::for_size(w, h)
            })
            .expect("an encoder");
            let mut encode = |index: u64, keyframe: bool| {
                let frame = captured(&pattern, index);
                let _ = whole_frame(&frame, ColorSpace::default(), &mut staging).expect("ok");
                encoder
                    .encode(&staging, keyframe)
                    .expect("encoded")
                    .bitstream
                    .len()
            };
            let key = encode(0, true);
            let mut delta = 0;
            for i in 1..6 {
                delta = encode(i, false);
            }
            println!(
                "  {w}x{h}   {fidelity:?}   {key:>7}   {delta:>5}   {:>3}",
                key.div_ceil(1200)
            );
        }
    }
    println!();
}
