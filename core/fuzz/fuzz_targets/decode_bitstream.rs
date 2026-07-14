//! Fuzz the H.264 decoder's input path.
//!
//! **This is the most valuable fuzz target in the project**, because it is the
//! only place where attacker-controlled bytes reach a large C parser. The
//! others check Rust that forbids unsafe code; this one checks 155 000 lines
//! of vendored C that a peer can feed arbitrary input to.
//!
//! # The flags this target needs, and why the Makefile sets them
//!
//! `cargo fuzz` sets `RUSTFLAGS` and **not** `CFLAGS`. Left alone, that means
//! openh264's C is compiled with neither sanitiser nor coverage
//! instrumentation, and the consequences are worse than they sound:
//!
//! - **No ASan on the C** means a heap overflow inside the decoder is not
//!   detected. The fuzzer would find the input that triggers it, run it, and
//!   report nothing.
//! - **No coverage on the C** means libFuzzer gets no feedback from inside the
//!   decoder at all. Every mutation looks equally good, so it never learns to
//!   build a valid parameter set — which is the gate every interesting code
//!   path is behind. That is not weak fuzzing, it is *blind* fuzzing: a
//!   million executions that never get past the first header check.
//!
//! So `make fuzz` sets `CFLAGS` and `CXXFLAGS` for both. A run of this target
//! without them is not a shorter version of the same thing; it is a different,
//! much weaker thing that looks identical from the outside.
//!
//! # The contract
//!
//! Arbitrary bytes must return or error. Never panic, never hang, and never
//! produce a picture larger than the configured maximum — because a caller
//! that trusted the dimensions would then allocate whatever a peer asked for.
//!
//! Run locally:
//!     make fuzz FUZZ_TARGETS=decode_bitstream FUZZ_SECONDS=600

#![no_main]

use libfuzzer_sys::fuzz_target;
use ownview_decode::decoder::{DecoderConfig, VideoDecoder};
use ownview_decode::openh264::OpenH264Decoder;

/// A modest ceiling, so a picture the fuzzer talks the decoder into is caught
/// here rather than by the machine running out of memory.
const MAX_PIXELS: u64 = 1920 * 1080;

fuzz_target!(|data: &[u8]| {
    let Ok(mut decoder) = OpenH264Decoder::new(&DecoderConfig {
        max_pixels: MAX_PIXELS,
        threads: 1,
    }) else {
        return;
    };

    // Split on Annex-B start codes, so the fuzzer's mutations land on NAL
    // boundaries some of the time rather than always mid-unit. A decoder is
    // fed access units in sequence, and feeding one blob per run would never
    // exercise the state a parameter set leaves behind for the slices after
    // it — which is where the interesting parsing happens.
    for packet in split_annex_b(data) {
        match decoder.decode(packet) {
            Ok(Some(picture)) => {
                let pixels = u64::from(picture.width()) * u64::from(picture.height());
                assert!(
                    pixels <= MAX_PIXELS,
                    "decoded a {}x{} picture past the {MAX_PIXELS} pixel limit — \
                     a caller trusting these dimensions would allocate whatever \
                     a peer asked for",
                    picture.width(),
                    picture.height()
                );
                // The planes must match the dimensions, or a consumer indexing
                // by stride reads out of bounds.
                assert_eq!(
                    picture.y().len(),
                    picture.y_stride() * picture.height() as usize
                );
                assert_eq!(picture.u().len(), picture.v().len());
                assert_eq!(picture.u().len() * 4, picture.y().len());
            }
            Ok(None) => {}
            // Every error is fatal to the stream, so a real caller stops here.
            // The fuzzer keeps going, which exercises the decoder's own
            // recovery state — a place a real peer can also reach by sending
            // a second stream after a bad one.
            Err(_) => {}
        }
    }
});

/// Split a buffer on Annex-B start codes, keeping each code with its unit.
///
/// Returns the whole buffer as one packet when there is no start code, so
/// input the fuzzer has not yet learned to shape is still fed in.
fn split_annex_b(data: &[u8]) -> Vec<&[u8]> {
    let mut starts = Vec::new();
    let mut i = 0usize;
    while i + 3 <= data.len() {
        let three = data.get(i..i + 3);
        if three == Some(&[0, 0, 1][..]) {
            starts.push(i);
            i += 3;
        } else if data.get(i..i + 4) == Some(&[0, 0, 0, 1][..]) {
            starts.push(i);
            i += 4;
        } else {
            i += 1;
        }
    }

    if starts.is_empty() {
        return if data.is_empty() { Vec::new() } else { vec![data] };
    }

    let mut out = Vec::with_capacity(starts.len());
    for (n, from) in starts.iter().enumerate() {
        let to = starts.get(n + 1).copied().unwrap_or(data.len());
        if let Some(slice) = data.get(*from..to) {
            out.push(slice);
        }
    }
    out
}
