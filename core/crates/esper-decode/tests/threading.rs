#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

//! What slice-parallel encoding buys, and what it costs.
//!
//! The measurement that made this urgent: full-motion 1080p30 needs 51 ms a
//! frame single-threaded, which is 153% of one core and does not sustain 30 fps
//! at all. Threading is the only lever that changes that without degrading the
//! picture.
//!
//! But it is not free, and the cost is not just overhead. openh264 parallelises
//! by **splitting each picture into slices**, and a slice is coded
//! independently — no prediction across its boundary. More slices means less
//! compression efficiency, which shows up as more bytes for the same quality
//! or less quality for the same bytes.
//!
//! So this measures four things together: wall-clock per frame, **CPU time
//! consumed**, bytes, and the quality actually delivered through a real
//! decoder.
//!
//! # Wall clock and CPU time are not the same number
//!
//! The distinction matters more than anything else here. Wall-clock per frame
//! answers "does this sustain 30 fps"; CPU time answers "does this fit the
//! budget", and the design's ceiling is written in CPU time. They are equal
//! only for single-threaded work.
//!
//! An earlier version of this measurement reported wall-clock share as though
//! it were core share, and would have announced that four threads took
//! full-motion 1080p from 147% of a core to 44% — a claim wrong in the most
//! flattering possible direction.

use std::time::Instant;

use esper_capture::damage::Damage;
use esper_capture::frame::{Frame, PixelFormat};
use esper_capture::pattern::Pattern;
use esper_capture::pool::Pixels;
use esper_color::convert::{reference_yuv, whole_frame};
use esper_color::{ColorSpace, I420};
use esper_decode::decoder::{DecoderConfig, VideoDecoder};
use esper_decode::openh264::OpenH264Decoder;
use esper_encode::encoder::{EncoderConfig, VideoEncoder};
use esper_encode::openh264::OpenH264;

/// Process CPU time, which is what the budget is written in.
///
/// Sums across threads, so a four-thread encode that finishes in a quarter of
/// the wall clock still reports roughly the same total — which is exactly the
/// thing wall-clock timing hides.
fn cpu_time() -> f64 {
    let t = rustix::time::clock_gettime(rustix::time::ClockId::ProcessCPUTime);
    t.tv_sec as f64 * 1000.0 + t.tv_nsec as f64 / 1e6
}

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[v.len() / 2]
}

fn screen_source(p: &Pattern, i: u64, out: &mut I420) {
    let (w, h) = (p.width(), p.height());
    let bpp = 4usize;
    let stride = w as usize * bpp;
    let mut buf = vec![0u8; stride * h as usize];
    for y in 0..h {
        for x in 0..w {
            let [r, g, b] = p.pixel(x, y, i);
            let at = y as usize * stride + x as usize * bpp;
            buf[at] = b;
            buf[at + 1] = g;
            buf[at + 2] = r;
        }
    }
    let f = Frame::new(
        w,
        h,
        stride,
        PixelFormat::Bgrx8,
        Damage::Everything,
        Pixels::owned(buf),
    )
    .unwrap();
    let _ = whole_frame(&f, ColorSpace::default(), out).unwrap();
}

/// Full-motion content: the case that misses the CPU ceiling.
fn motion_source(out: &mut I420, frame: u64) {
    let (y, u, v) = out.planes_mut();
    for (n, p) in y.iter_mut().enumerate() {
        *p = ((n / 7 + frame as usize * 3) % 256) as u8;
    }
    for (n, p) in u.iter_mut().enumerate() {
        *p = ((n * 5 + frame as usize * 11) % 256) as u8;
    }
    for (n, p) in v.iter_mut().enumerate() {
        *p = ((n * 3 + frame as usize * 7) % 256) as u8;
    }
}

fn psnr(p: &Pattern, i: u64, got: &I420) -> f64 {
    let space = ColorSpace::default();
    let mut sum = 0f64;
    let mut n = 0u64;
    for y in 0..p.height() {
        for x in 0..p.width() {
            let [r, g, b] = p.pixel(x, y, i);
            let (want, _, _) = reference_yuv(r, g, b, space);
            let d = f64::from(i32::from(got.luma_at(x, y).unwrap_or(0)) - i32::from(want));
            sum += d * d;
            n += 1;
        }
    }
    let mse = sum / n as f64;
    if mse <= 0.0 {
        return f64::INFINITY;
    }
    10.0 * (255.0f64 * 255.0 / mse).log10()
}

/// **What threading buys on the content that needs it.**
#[test]
#[ignore = "a measurement: run it deliberately"]
fn measure_what_slice_threading_buys() {
    let (w, h) = (1920u32, 1080u32);
    let cores = std::thread::available_parallelism().map_or(1, std::num::NonZero::get);
    println!("\nslice-parallel encode at {w}x{h} ({cores} logical cores available)");

    for (label, motion) in [("screen-like", false), ("full motion", true)] {
        println!("\n  {label}:");
        println!(
            "    {:>7}  {:>8}  {:>9}  {:>9}  {:>12}  {:>12}  {:>12}",
            "threads", "slice", "wall ms", "cpu ms", "bytes/frame", "core @30fps", "sustains 30?"
        );
        let mut single = 0f64;
        for (threads, slice) in [
            (1u16, None),
            (1, Some(64 * 1024u32)),
            (2, Some(64 * 1024)),
            (4, Some(32 * 1024)),
            (4, Some(8 * 1024)),
            (8, Some(8 * 1024)),
            (8, Some(2 * 1024)),
        ] {
            if usize::from(threads) > cores {
                continue;
            }
            let p = Pattern::new(w, h).unwrap();
            let mut img = I420::new(w, h).unwrap();
            let mut enc = OpenH264::new(&EncoderConfig {
                fps: 30,
                threads,
                max_slice_bytes: slice,
                ..EncoderConfig::for_size(w, h)
            })
            .unwrap();

            let mut ms = Vec::new();
            let mut bytes = 0usize;
            let mut cpu = 0f64;
            for i in 0..40u64 {
                if motion {
                    motion_source(&mut img, i);
                } else {
                    screen_source(&p, i, &mut img);
                }
                let t = Instant::now();
                let c0 = cpu_time();
                let e = enc.encode(&img, i == 0).unwrap();
                cpu += cpu_time() - c0;
                ms.push(t.elapsed().as_secs_f64() * 1000.0);
                bytes += e.len();
            }
            let cpu_per_frame = cpu / 40.0;
            let m = median(ms);
            if threads == 1 && slice.is_none() {
                single = m;
            }
            let slice_label = slice.map_or_else(|| "one".to_owned(), |b| format!("{}K", b / 1024));
            println!(
                "    {threads:>7}  {slice_label:>8}  {m:>9.2}  {cpu_per_frame:>9.2}  \
                 {:>12}  {:>11.0}%  {:>12}",
                bytes / 40,
                // CPU time, not wall clock: 30 frames a second of `cpu` ms
                // each, as a percentage of one core.
                cpu_per_frame * 3.0,
                if m <= 33.3 { "yes" } else { "NO" }
            );
        }
        if single > 0.0 {
            println!("    (single-threaded was {single:.2} ms)");
        }
    }
}

/// **What slicing costs the picture.** A speedup that quietly loses quality is
/// not a speedup, and openh264 parallelises by cutting each frame into
/// independently coded slices — so this is the number that decides how many
/// threads are worth using.
#[test]
#[ignore = "a measurement: run it deliberately"]
fn measure_what_slicing_costs_the_picture() {
    let (w, h) = (1280u32, 720u32);
    let cores = std::thread::available_parallelism().map_or(1, std::num::NonZero::get);
    println!("\nquality against thread count at {w}x{h}, screen-like content");
    println!(
        "  {:>7}  {:>8}  {:>9}  {:>12}  {:>10}",
        "threads", "slice", "ms/frame", "bytes/frame", "PSNR dB"
    );

    for (threads, slice) in [
        (1u16, None),
        (2, Some(32 * 1024u32)),
        (4, Some(16 * 1024)),
        (8, Some(4 * 1024)),
    ] {
        if usize::from(threads) > cores {
            continue;
        }
        let p = Pattern::new(w, h).unwrap();
        let mut staging = I420::new(w, h).unwrap();
        let mut enc = OpenH264::new(&EncoderConfig {
            fps: 30,
            threads,
            max_slice_bytes: slice,
            ..EncoderConfig::for_size(w, h)
        })
        .unwrap();
        let mut dec = OpenH264Decoder::new(&DecoderConfig::default()).unwrap();

        let mut ms = Vec::new();
        let mut bytes = 0usize;
        let mut quality = Vec::new();
        for i in 0..24u64 {
            screen_source(&p, i, &mut staging);
            let t = Instant::now();
            let e = enc.encode(&staging, i == 0).unwrap();
            ms.push(t.elapsed().as_secs_f64() * 1000.0);
            bytes += e.len();
            if let Ok(Some(picture)) = dec.decode(&e.bitstream) {
                quality.push(psnr(&p, i, &picture));
            }
        }
        let mean = quality.iter().sum::<f64>() / quality.len().max(1) as f64;
        let slice_label = slice.map_or_else(|| "one".to_owned(), |b| format!("{}K", b / 1024));
        println!(
            "  {threads:>7}  {slice_label:>8}  {:>9.2}  {:>12}  {mean:>10.2}",
            median(ms),
            bytes / 24
        );
        assert!(!quality.is_empty(), "{threads} threads: nothing decoded");
    }
    println!("  (a thread count that costs quality is not worth the speed)");
}
