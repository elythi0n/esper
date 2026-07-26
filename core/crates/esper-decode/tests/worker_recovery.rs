#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! **A keyframe has to rescue the sandboxed decoder, not just the C library.**
//!
//! The whole loss-recovery path rests on this. A controller that loses a frame
//! asks for a keyframe, and the point of a keyframe is that it refers to
//! nothing — so it can be decoded by a decoder holding no reference. If the
//! worker in front of that decoder will not pass the keyframe on, every ask is
//! wasted and the session stays black however politely it asks.
//!
//! It did. The parent treated a frame the child could not decode as the end of
//! the stream and refused everything after it without sending it, so the first
//! lost reference — the ordinary event on any lossy link — ended the video for
//! good. Measured on a link losing five per cent of datagrams: 39 of 41
//! keyframes that arrived whole were never looked at, and the session showed 8
//! pictures in six seconds where it now shows about 70.
//!
//! # Why this test has no harness
//!
//! The worker *is* this binary, re-executed — that is what makes the two
//! halves of the protocol impossible to get out of step. libtest owns `argv`
//! and would read the worker's argument as a test filter, so this target sets
//! `harness = false` and dispatches on `argv` itself, exactly as the engine
//! does.

#[cfg(target_os = "linux")]
use esper_capture::pattern::Pattern;
#[cfg(target_os = "linux")]
use esper_color::convert::whole_frame;
#[cfg(target_os = "linux")]
use esper_color::{ColorSpace, I420};
#[cfg(target_os = "linux")]
use esper_decode::decoder::{DecoderConfig, VideoDecoder};
#[cfg(target_os = "linux")]
use esper_decode::{SandboxedDecoder, WORKER_ARG, WORKER_ENV};
#[cfg(target_os = "linux")]
use esper_encode::encoder::{EncoderConfig, VideoEncoder};
#[cfg(target_os = "linux")]
use esper_encode::openh264::OpenH264;

#[cfg(target_os = "linux")]
mod common;
#[cfg(target_os = "linux")]
use common::captured;

/// **The jail is Linux-only, and so is this — but `main` is not.** A
/// `harness = false` target has to have one on every platform the workspace is
/// checked for, or a cross-target `clippy` fails to compile a test that was
/// never going to run there.
#[cfg(not(target_os = "linux"))]
fn main() {
    println!("worker_recovery: skipped, the sandbox is Linux-only");
}

#[cfg(target_os = "linux")]
fn main() {
    // The child half, reached only by both the argument and the environment
    // variable — the same pair the engine checks.
    if std::env::args().nth(1).as_deref() == Some(WORKER_ARG)
        && std::env::var_os(WORKER_ENV).is_some()
    {
        match esper_decode::child::run_on_stdio(&DecoderConfig::default()) {
            Ok(()) => return,
            Err(e) => {
                eprintln!("decode-worker: {e}");
                std::process::exit(1);
            }
        }
    }

    a_frame_the_decoder_cannot_decode_does_not_end_the_session();
    println!("test a_frame_the_decoder_cannot_decode_does_not_end_the_session ... ok");
}

#[cfg(target_os = "linux")]
fn a_frame_the_decoder_cannot_decode_does_not_end_the_session() {
    let (w, h) = (320u32, 180u32);
    let pattern = Pattern::new(w, h).expect("a valid size");
    let space = ColorSpace::default();
    let mut staging = I420::new(w, h).expect("a valid size");
    let mut encoder = OpenH264::new(&EncoderConfig {
        fps: 15,
        ..EncoderConfig::for_size(staging.width(), staging.height())
    })
    .expect("an encoder");
    let mut decoder = SandboxedDecoder::spawn_self(&DecoderConfig::default()).expect("a worker");
    assert!(
        decoder.is_sandboxed(),
        "the point of this decoder is the jail"
    );

    let mut encode = |index: u64, keyframe: bool| {
        let frame = captured(&pattern, index);
        let _ = whole_frame(&frame, space, &mut staging).expect("converted");
        encoder
            .encode(&staging, keyframe)
            .expect("encoded")
            .bitstream
    };

    // A clean start: a keyframe, and a picture out of it.
    let first = encode(0, true);
    assert!(
        decoder.decode(&first).expect("the first frame").is_some(),
        "a keyframe on a fresh decoder must produce a picture"
    );

    // The link loses frame 1 outright. Frames 2, 3 and 4 arrive whole and
    // refer to what the decoder does not have, so each is refused — which is
    // correct, and is what makes a controller ask for a keyframe.
    let _lost_in_flight = encode(1, false);
    for index in 2..5u64 {
        let orphan = encode(index, false);
        assert!(
            decoder.decode(&orphan).is_err(),
            "frame {index} refers to a lost picture and must not be concealed"
        );
    }

    // **And now the answer to that ask.** Nothing in a keyframe refers to
    // anything, so a decoder holding no reference can use it — and the worker
    // has to let it.
    let rescue = encode(5, true);
    let picture = decoder
        .decode(&rescue)
        .expect("a keyframe must rescue a decoder that lost sync")
        .expect("a keyframe must produce a picture");
    assert_eq!((picture.width(), picture.height()), (w, h));

    // The session carries on from there, which is the part that matters: not
    // one picture, but the stream continuing.
    for index in 6..12u64 {
        let next = encode(index, false);
        assert!(
            decoder
                .decode(&next)
                .expect("a frame after recovery")
                .is_some(),
            "frame {index} follows a decoded keyframe and should decode"
        );
    }

    let stats = decoder.stats();
    assert_eq!(
        stats.undecodable, 3,
        "the three orphaned frames should be counted as undecodable"
    );
    assert_eq!(
        stats.pictures, 8,
        "two keyframes and six frames after recovery are eight pictures"
    );
}
