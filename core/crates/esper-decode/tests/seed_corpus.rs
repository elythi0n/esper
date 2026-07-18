#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Generate a seed corpus for the decoder's fuzz target.
//!
//! A fuzzer starting from random bytes spends its whole budget learning that
//! H.264 begins with a start code and a parameter set. Everything interesting
//! is behind that gate, so a corpus of real bitstreams is worth more than any
//! amount of extra runtime — it starts the fuzzer inside the parser instead of
//! outside it.
//!
//! The encoder is right here, so the seeds are real output rather than
//! captured files that would have to be committed, kept in step with the
//! encoder's settings, and explained.
//!
//! `#[ignore]`d because it writes files. Run it through `make fuzz-corpus`.

use std::path::PathBuf;

use esper_capture::damage::Damage;
use esper_capture::frame::{Frame, PixelFormat};
use esper_capture::pattern::Pattern;
use esper_capture::pool::Pixels;
use esper_color::convert::whole_frame;
use esper_color::{ColorSpace, I420};
use esper_encode::encoder::{EncoderConfig, Tuning, VideoEncoder};
use esper_encode::openh264::OpenH264;

fn corpus_dir() -> PathBuf {
    std::env::var_os("ESPER_FUZZ_CORPUS")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .ancestors()
                .nth(3)
                .expect("crate is at <root>/core/crates/<name>")
                .join("core/fuzz/corpus/decode_bitstream")
        })
}

#[test]
#[ignore = "writes a fuzz corpus; run it through `make fuzz-corpus`"]
fn write_seed_corpus() {
    let dir = corpus_dir();
    std::fs::create_dir_all(&dir).expect("create the corpus directory");

    let mut written = 0usize;
    // A spread of sizes and settings, because the parser's paths depend on
    // dimensions, on whether a picture is a keyframe, and on which tuning the
    // encoder used — and a corpus of one shape teaches the fuzzer one shape.
    for (w, h) in [(64u32, 48u32), (320, 240), (640, 480), (1280, 720)] {
        for tuning in [Tuning::CameraVideo, Tuning::ScreenContent] {
            let pattern = Pattern::new(w, h).expect("valid");
            let mut staging = I420::new(w, h).expect("valid");
            let mut enc = OpenH264::new(&EncoderConfig {
                fps: 30,
                tuning,
                ..EncoderConfig::for_size(staging.width(), staging.height())
            })
            .expect("encoder");

            for index in 0..4u64 {
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
                    Frame::new(w, h, stride, format, Damage::Everything, Pixels::owned(buf))
                        .expect("valid");
                let _ =
                    whole_frame(&frame, ColorSpace::default(), &mut staging).expect("converted");
                let encoded = enc.encode(&staging, index == 0).expect("encoded");

                let name = format!(
                    "{w}x{h}-{}-{index}{}.h264",
                    tuning.name(),
                    if encoded.keyframe { "-key" } else { "" }
                );
                std::fs::write(dir.join(&name), &encoded.bitstream)
                    .unwrap_or_else(|e| panic!("writing {name}: {e}"));
                written += 1;
            }
        }
    }

    // A few deliberately broken ones, so the fuzzer starts with examples of
    // the shapes that matter: a bare start code, a truncated parameter set, a
    // slice with no parameter set before it.
    let broken: [(&str, Vec<u8>); 4] = [
        ("bare-start-code", vec![0, 0, 0, 1]),
        ("truncated-sps", vec![0, 0, 0, 1, 0x67, 0x42]),
        (
            "slice-with-no-sps",
            vec![0, 0, 0, 1, 0x65, 0x88, 0x84, 0x00],
        ),
        (
            "two-start-codes-nothing-between",
            vec![0, 0, 0, 1, 0, 0, 0, 1],
        ),
    ];
    for (name, bytes) in broken {
        std::fs::write(dir.join(format!("{name}.h264")), &bytes)
            .unwrap_or_else(|e| panic!("writing {name}: {e}"));
        written += 1;
    }

    println!("wrote {written} seeds to {}", dir.display());
    assert!(written > 30, "only {written} seeds");
}
