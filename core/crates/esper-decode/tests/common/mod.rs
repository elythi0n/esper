//! What the three decode tests all need: a frame of the synthetic pattern.
//!
//! Shared because it is the same twenty lines in each of them, and because
//! *how* a capture backend hands a frame over is part of what these tests
//! exercise — a padded stride and BGRX byte order, not a tidy buffer.

#![allow(clippy::indexing_slicing)]

use esper_capture::damage::Damage;
use esper_capture::frame::{Frame, PixelFormat};
use esper_capture::pattern::Pattern;
use esper_capture::pool::Pixels;

/// Render the pattern into a frame, the way a capture backend would.
///
/// **A padded stride, because no real backend is tightly packed.** A test that
/// used `width * 4` would pass with a stride bug in place.
pub fn captured(pattern: &Pattern, index: u64) -> Frame {
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
    Frame::new(w, h, stride, format, Damage::Everything, Pixels::owned(buf)).expect("a valid frame")
}
