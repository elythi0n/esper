#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! What the encoder actually costs, and what the screen-content tuning buys.
//!
//! The design targets 15 ms per 1080p frame and under one core for full-motion
//! 1080p30. Those are targets; these are measurements.
//!
//! `#[ignore]`d, because a timing assertion on a shared runner measures the
//! runner. Run deliberately:
//!
//! ```text
//! cargo test --release -p esper-encode -- --ignored --nocapture
//! ```
//!
//! Every run prints whether the build has openh264's assembly, because a
//! number should say which build it came from. Measured, that is worth 2.5% to
//! 6% — small, but the label costs nothing and an unlabelled number is how a
//! wrong claim gets repeated.

use std::time::{Duration, Instant};

use esper_capture::damage::Damage;
use esper_capture::frame::{Frame, PixelFormat};
use esper_capture::pattern::Pattern;
use esper_capture::pool::Pixels;
use esper_color::convert::whole_frame;
use esper_color::{ColorSpace, I420};
use esper_encode::encoder::{EncoderConfig, VideoEncoder, has_assembly};
use esper_encode::openh264::OpenH264;

/// A realistic picture: the synthetic pattern, converted the way the pipeline
/// converts it.
fn picture(w: u32, h: u32, index: u64, out: &mut I420) {
    let pattern = Pattern::new(w, h).expect("valid");
    let format = PixelFormat::Bgrx8;
    let bpp = format.bytes_per_pixel();
    let stride = (w as usize) * bpp;
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
    let frame =
        Frame::new(w, h, stride, format, Damage::Everything, Pixels::owned(buf)).expect("valid");
    let _ = whole_frame(&frame, ColorSpace::default(), out).expect("converted");
}

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    v.get(v.len() / 2).copied().unwrap_or(0.0)
}

fn path() -> &'static str {
    if has_assembly() {
        "with openh264 assembly"
    } else {
        "C-only path — no nasm at build time, so about 2.5% to 6% slower"
    }
}

/// Cost per frame, and the share of a core it implies at 30 fps.
#[test]
#[ignore = "a measurement, not an assertion: run it deliberately"]
fn measure_encode_cost() {
    println!(
        "\nsoftware H.264 encode, screen-content tuning ({})",
        path()
    );
    println!(
        "  {:>8}  {:>10}  {:>10}  {:>12}  {:>10}",
        "size", "ms/frame", "worst ms", "core @30fps", "kbit/s"
    );

    for (w, h, label) in [
        (1280u32, 720u32, "720p"),
        (1920, 1080, "1080p"),
        (2560, 1440, "1440p"),
    ] {
        let config = EncoderConfig {
            fps: 30,
            ..EncoderConfig::for_size(w, h)
        };
        assert_eq!(
            config.tuning,
            esper_encode::encoder::Tuning::CameraVideo,
            "the default tuning is what these numbers describe"
        );
        let mut enc = OpenH264::new(&config).expect("encoder");
        let mut img = I420::new(w, h).expect("valid");

        let mut samples = Vec::new();
        let started = Instant::now();
        for f in 0..60u64 {
            picture(w, h, f, &mut img);
            let t = Instant::now();
            let _ = enc.encode(&img, false).expect("encode");
            samples.push(t.elapsed().as_secs_f64() * 1000.0);
        }
        let stats = enc.stats();
        let ms = median(samples);
        // The share of one core this would take at 30 fps, as a percentage —
        // which is the number the design's ceiling is written in. An earlier
        // version printed the fraction with no decimals, so 70% of a core
        // read as "1%".
        let share = ms * 30.0 / 1000.0 * 100.0;
        let elapsed = started.elapsed();
        println!(
            "  {label:>8}  {ms:>10.2}  {:>10.2}  {share:>10.0}%   {:>10}",
            stats.worst.as_secs_f64() * 1000.0,
            stats.bitrate(elapsed) / 1000
        );
    }
    println!("  (design targets: under 15 ms at 1080p, under 1 core at 1080p30)");
}

/// **What telling the encoder it is looking at a screen buys.**
///
/// openh264 defaults to tuning for camera video. A screen is mostly static,
/// has hard edges, large flat areas and text, and no sensor noise. This is the
/// measurement behind choosing `ScreenContentRealTime`, and it is here because
/// a setting chosen without a number is a guess.
#[test]
#[ignore = "a measurement, not an assertion: run it deliberately"]
fn measure_what_screen_content_tuning_buys() {
    use openh264::OpenH264API;
    use openh264::encoder::{
        BitRate, Encoder as Inner, EncoderConfig as InnerConfig, FrameRate, UsageType,
    };
    use openh264::formats::YUVSource;

    struct Planes<'a>(&'a I420);
    impl YUVSource for Planes<'_> {
        fn dimensions(&self) -> (usize, usize) {
            (self.0.width() as usize, self.0.height() as usize)
        }
        fn strides(&self) -> (usize, usize, usize) {
            (self.0.y_stride(), self.0.uv_stride(), self.0.uv_stride())
        }
        fn y(&self) -> &[u8] {
            self.0.y()
        }
        fn u(&self) -> &[u8] {
            self.0.u()
        }
        fn v(&self) -> &[u8] {
            self.0.v()
        }
    }

    let (w, h) = (1920u32, 1080u32);
    println!(
        "\nscreen-content tuning against the camera default, 1080p ({})",
        path()
    );
    println!(
        "  {:>26}  {:>10}  {:>12}",
        "usage type", "ms/frame", "bytes/frame"
    );

    for (usage, label) in [
        (UsageType::CameraVideoRealTime, "camera (the default)"),
        (UsageType::ScreenContentRealTime, "screen content"),
    ] {
        let inner_config = InnerConfig::new()
            .usage_type(usage)
            .bitrate(BitRate::from_bps(4_000_000))
            .max_frame_rate(FrameRate::from_hz(30.0))
            .debug(false);
        let mut enc =
            Inner::with_api_config(OpenH264API::from_source(), inner_config).expect("encoder");
        let mut img = I420::new(w, h).expect("valid");

        let mut samples = Vec::new();
        let mut bytes = 0usize;
        for f in 0..60u64 {
            picture(w, h, f, &mut img);
            let t = Instant::now();
            let out = enc.encode(&Planes(&img)).expect("encode");
            samples.push(t.elapsed().as_secs_f64() * 1000.0);
            bytes += out.to_vec().len();
        }
        println!(
            "  {label:>26}  {:>10.2}  {:>12}",
            median(samples),
            bytes / 60
        );
    }
    println!("  (the pattern is deliberately screen-like: text, flat areas, hard edges)");
}

/// **How much the content matters, which is more than anything else here.**
///
/// This exists because getting it wrong produced a false claim that reached a
/// build script, a README and a CI workflow. An early probe measured 20.6 ms
/// for a 1080p frame on a high-entropy synthetic gradient, on a build without
/// openh264's assembly, and the slowness was attributed to the missing
/// assembly. It was the content: high-motion content measures 47 ms *with* the
/// assembly, and screen-like content measures 4.
///
/// So both axes are measured together now, and neither gets quoted without the
/// other.
#[test]
#[ignore = "a measurement, not an assertion: run it deliberately"]
fn measure_how_much_the_content_matters() {
    let (w, h) = (1920u32, 1080u32);
    println!("\nencode cost by content and tuning at 1080p ({})", path());
    println!(
        "  {:>16}  {:>14}  {:>9}  {:>12}  {:>12}",
        "tuning", "content", "ms/frame", "bytes/frame", "core @30fps"
    );

    for tuning in [
        esper_encode::encoder::Tuning::CameraVideo,
        esper_encode::encoder::Tuning::ScreenContent,
    ] {
        for (label, high_entropy) in [("screen-like", false), ("high motion", true)] {
            let mut enc = OpenH264::new(&EncoderConfig {
                fps: 30,
                tuning,
                ..EncoderConfig::for_size(w, h)
            })
            .expect("encoder");
            let mut img = I420::new(w, h).expect("valid");

            let mut samples = Vec::new();
            let mut bytes = 0usize;
            for f in 0..40u64 {
                if high_entropy {
                    fill_with_motion(&mut img, f);
                } else {
                    picture(w, h, f, &mut img);
                }
                let t = Instant::now();
                let e = enc.encode(&img, f == 0).expect("encode");
                samples.push(t.elapsed().as_secs_f64() * 1000.0);
                bytes += e.len();
            }
            let ms = median(samples);
            println!(
                "  {:>16}  {label:>14}  {ms:>9.2}  {:>12}  {:>11.0}%",
                tuning.name(),
                bytes / 40,
                ms * 3.0
            );
        }
    }
    println!(
        "  (the design's ceiling is one core for full-motion 1080p30; high \
         motion exceeds it, which is what the CPU budget governor is for)"
    );
}

/// A picture that changes everywhere every frame: the worst case for an
/// encoder, and what full-motion video actually looks like.
fn fill_with_motion(out: &mut I420, frame: u64) {
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

/// The pipeline as it will actually run: capture, damage gate, damage-driven
/// conversion, encode.
///
/// **Not a static screen** — the synthetic pattern moves a box and a counter
/// every frame by design, so every frame really does have damage and the gate
/// correctly encodes all of them. An earlier version of this called itself a
/// static-screen measurement and reported zero skipped frames as though that
/// were a finding; it was measuring a screen that changes constantly.
///
/// What it does measure is the thing that matters: how little of the frame has
/// to be *converted* when only a small region changed.
#[test]
#[ignore = "a measurement, not an assertion: run it deliberately"]
fn measure_a_screen_where_only_a_small_region_changes() {
    use esper_capture::gate::{EncodeGate, Policy, Verdict};
    use esper_capture::geometry::Rect;
    use esper_capture::synthetic::SyntheticCapturer;
    use esper_capture::{Capture, Capturer};

    let (w, h) = (1920u32, 1080u32);
    let config = EncoderConfig {
        fps: 30,
        ..EncoderConfig::for_size(w, h)
    };
    let mut enc = OpenH264::new(&config).expect("encoder");
    let mut img = I420::new(w, h).expect("valid");
    let mut gate = EncodeGate::new(Rect::sized(w, h), Policy::default());
    // A frame rate far above the loop's speed, so the capturer always has one
    // ready and the measurement is not coupled to how fast this loop happens
    // to run.
    //
    // It was, and the coupling was invisible: with the slow encoder the loop
    // took long enough that real time advanced past each frame's due moment,
    // and 205 of 300 polls returned frames. Making the encoder six times
    // faster dropped that to 2 — the same code measuring a different thing,
    // with nothing to say it had changed. Measurement harnesses need the same
    // care as the code they measure.
    let mut cap = SyntheticCapturer::new(w, h, 1_000_000).expect("capturer");

    // Ten seconds of wall clock, simulated, at 30 fps.
    let mut converted_total = 0u64;
    let mut now = Instant::now();
    let session_length = Duration::from_millis(33 * 300);
    for _ in 0..300 {
        now += Duration::from_millis(33);
        let Ok(Capture::Frame(frame)) = cap.next_frame(Duration::from_secs(1)) else {
            continue;
        };
        match gate.decide(frame.damage(), now) {
            Verdict::Skip => {}
            Verdict::Encode {
                keyframe, damage, ..
            } => {
                let rects = damage.rects(&frame.bounds());
                converted_total +=
                    esper_color::convert::regions(&frame, ColorSpace::default(), &mut img, &rects)
                        .expect("converted");
                let _ = enc.encode(&img, keyframe).expect("encode");
            }
        }
    }
    let (encodes, skips) = gate.counts();
    let stats = enc.stats();

    println!(
        "\nten seconds of 1080p where only a small region changes each frame ({})",
        path()
    );
    println!("  frames captured        {}", encodes + skips);
    println!("  frames encoded         {encodes}");
    println!("  frames skipped         {skips}");
    println!(
        "  pixels converted       {converted_total} (of {} if every frame were whole)",
        u64::from(w) * u64::from(h) * (encodes + skips)
    );
    println!(
        "  encode time total      {:.1} ms over a simulated {:.0} s session",
        stats.spent.as_secs_f64() * 1000.0,
        session_length.as_secs_f64()
    );
    println!(
        "  share of one core      {:.1}%",
        stats.core_share(session_length) * 100.0
    );
    println!(
        "  bitrate                {} kbit/s",
        stats.bitrate(session_length) / 1000
    );
    println!(
        "  (design targets: idle 4K under 50 kbit/s, reading a document under \
         10% of one core)"
    );
}
