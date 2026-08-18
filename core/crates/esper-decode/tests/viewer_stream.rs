#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! A continuous bitstream for the viewer, from the product's own encoder.
//!
//! The webview has to decode *this* encoder's output, not a sample from
//! somewhere else: the profile, the level, the parameter sets and the way
//! access units are cut are all ours. The fuzz corpus next door holds single
//! pictures, which answer whether a parser accepts one — not whether a
//! decoder can be fed a stream at the rate a session runs at.
//!
//! `#[ignore]`d because it writes a file. What it was written for: feeding the
//! webview's `VideoDecoder` to find out whether a WebCodecs presenter is
//! viable. It is — 1920x1080 decoded at 542 frames a second, 151 with every
//! frame drawn, on software rendering with no GPU — and the detail worth
//! keeping next to the generator is that **WebKit refuses Annex B**: the same
//! stream decodes only when each access unit is length-prefixed AVCC with an
//! `avcC` description assembled from the SPS and PPS. Run it the same way:
//!
//! ```text
//! ESPER_SPIKE_OUT=/tmp/stream.h264 ESPER_SPIKE_W=1920 ESPER_SPIKE_H=1080 \
//!   cargo test -p esper-decode --test viewer_stream -- --ignored --nocapture
//! ```
//!
//! The sidecar it writes beside the bitstream holds each access unit's length,
//! so a reader splits the file exactly rather than parsing for boundaries.

use std::path::PathBuf;

use esper_capture::damage::Damage;
use esper_capture::frame::{Frame, PixelFormat};
use esper_capture::pattern::Pattern;
use esper_capture::pool::Pixels;
use esper_color::convert::whole_frame;
use esper_color::{ColorSpace, I420};
use esper_encode::encoder::{EncoderConfig, Tuning, VideoEncoder};
use esper_encode::openh264::OpenH264;

#[test]
#[ignore = "writes a bitstream; run it by hand, see the module note"]
fn write_a_stream_for_the_viewer() {
    let out = PathBuf::from(std::env::var("ESPER_SPIKE_OUT").expect("ESPER_SPIKE_OUT"));
    let (w, h): (u32, u32) = (
        std::env::var("ESPER_SPIKE_W")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(1280),
        std::env::var("ESPER_SPIKE_H")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(720),
    );
    let frames: u64 = std::env::var("ESPER_SPIKE_FRAMES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(120);

    let pattern = Pattern::new(w, h).expect("valid");
    let mut staging = I420::new(w, h).expect("valid");
    let mut enc = OpenH264::new(&EncoderConfig {
        fps: 30,
        tuning: Tuning::ScreenContent,
        ..EncoderConfig::for_size(w, h)
    })
    .expect("encoder");

    let mut stream = Vec::new();
    let mut sizes = Vec::new();
    for index in 0..frames {
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
        let frame = Frame::new(w, h, stride, format, Damage::Everything, Pixels::owned(buf))
            .expect("valid");
        let _ = whole_frame(&frame, ColorSpace::default(), &mut staging).expect("converted");
        let encoded = enc.encode(&staging, index == 0).expect("encoded");
        sizes.push(encoded.bitstream.len());
        stream.extend_from_slice(&encoded.bitstream);
    }

    std::fs::write(&out, &stream).expect("write");
    let sidecar = out.with_extension("sizes.json");
    std::fs::write(
        &sidecar,
        format!(
            "{{\"width\":{w},\"height\":{h},\"fps\":30,\"frames\":{},\"sizes\":{:?}}}",
            sizes.len(),
            sizes
        ),
    )
    .expect("write sizes");
    println!(
        "wrote {} bytes, {} frames to {}",
        stream.len(),
        sizes.len(),
        out.display()
    );
}
