#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

//! Does screen-content tuning buy quality for its six times the CPU?
//!
//! This is the question that was left open when the tuning was chosen: the
//! decision was made on CPU and bitrate, because comparing *quality* needs a
//! decoder and there was not one. There is now.
//!
//! Measures luma PSNR of the decoded picture against the source, per tuning,
//! along with the bytes each spent. A tuning that is more expensive in CPU has
//! to be buying something for it.

use std::time::Instant;

use esper_capture::damage::Damage;
use esper_capture::frame::{Frame, PixelFormat};
use esper_capture::pattern::Pattern;
use esper_capture::pool::Pixels;
use esper_color::convert::{reference_yuv, whole_frame};
use esper_color::{ColorSpace, I420};
use esper_decode::decoder::{DecoderConfig, VideoDecoder};
use esper_decode::openh264::OpenH264Decoder;
use esper_encode::encoder::{EncoderConfig, Tuning, VideoEncoder};
use esper_encode::openh264::OpenH264;

fn source(p: &Pattern, i: u64, out: &mut I420) {
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

/// Luma PSNR of a decoded picture against the pattern it came from.
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

/// **Closes the question the tuning decision was left open on.**
#[test]
#[ignore = "a measurement: run it deliberately"]
fn measure_what_screen_content_tuning_buys_in_quality() {
    let (w, h) = (1280u32, 720u32);
    println!(
        "\nquality per tuning at {w}x{h}, screen-like content (assembly: {})",
        esper_encode::encoder::has_assembly()
    );
    println!(
        "  {:>16}  {:>9}  {:>12}  {:>10}  {:>14}",
        "tuning", "ms/frame", "bytes/frame", "PSNR dB", "dB per ms"
    );

    for tuning in [Tuning::CameraVideo, Tuning::ScreenContent] {
        let p = Pattern::new(w, h).unwrap();
        let mut staging = I420::new(w, h).unwrap();
        let mut enc = OpenH264::new(&EncoderConfig {
            fps: 30,
            tuning,
            ..EncoderConfig::for_size(w, h)
        })
        .unwrap();
        let mut dec = OpenH264Decoder::new(&DecoderConfig::default()).unwrap();

        let mut ms = Vec::new();
        let mut bytes = 0usize;
        let mut quality = Vec::new();
        for i in 0..24u64 {
            source(&p, i, &mut staging);
            let t = Instant::now();
            let e = enc.encode(&staging, i == 0).unwrap();
            ms.push(t.elapsed().as_secs_f64() * 1000.0);
            bytes += e.len();
            if let Ok(Some(picture)) = dec.decode(&e.bitstream) {
                quality.push(psnr(&p, i, &picture));
            }
        }
        ms.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let median_ms = ms[ms.len() / 2];
        let mean_psnr = quality.iter().sum::<f64>() / quality.len().max(1) as f64;
        println!(
            "  {:>16}  {median_ms:>9.2}  {:>12}  {mean_psnr:>10.2}  {:>14.2}",
            tuning.name(),
            bytes / 24,
            mean_psnr / median_ms
        );
        assert!(!quality.is_empty(), "{}: nothing decoded", tuning.name());
    }
    println!("  (higher PSNR is better; the last column is quality per unit of CPU)");
}
