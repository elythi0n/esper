//! Fuzz the incremental frame reader on a control stream.
//!
//! Two contracts, both absolute:
//!
//! 1. **Arbitrary bytes must return, never panic and never hang.** A panic
//!    reachable from the network is a denial of service, and every byte here
//!    arrives from a peer.
//!
//! 2. **How the bytes were split must not change the frames that come out.**
//!    This is the one that matters. `Control::recv` is called inside
//!    `tokio::select!`, which drops its future whenever the pseudo-terminal
//!    wins the race — so the reader is handed the stream in pieces whose
//!    boundaries it does not choose and cannot predict. A reader whose output
//!    depended on those boundaries would desynchronise the stream, and the
//!    failure surfaces as a nonsense frame length a long way from its cause.
//!
//! Run locally:
//!     cargo +nightly fuzz run control_frames
//!
//! The always-on cheap version lives in the crate's unit tests, which sweep
//! every split of a fixed input.

#![no_main]

use libfuzzer_sys::fuzz_target;
use esper_transport::FrameReader;

/// Feed `data` in pieces of `step` bytes and collect what comes out.
///
/// A reader that errors is finished: a malformed frame is fatal to the stream,
/// so stopping is the honest continuation rather than resetting and pretending
/// the rest is well-formed.
fn drain(data: &[u8], step: usize) -> (Vec<Vec<u8>>, bool) {
    let mut r = FrameReader::new();
    let mut out = Vec::new();
    for chunk in data.chunks(step.max(1)) {
        r.push(chunk);
        loop {
            match r.take() {
                Ok(Some(frame)) => out.push(frame),
                Ok(None) => break,
                Err(_) => return (out, true),
            }
        }
    }
    (out, false)
}

fuzz_target!(|data: &[u8]| {
    // Whole-input-at-once is the reference.
    let reference = drain(data, usize::MAX);

    // Every other split must agree with it. Byte-at-a-time is the worst case
    // and the one a cancelled read produces most often.
    for step in [1usize, 2, 3, 4, 5, 7, 13, 64, 1024] {
        let got = drain(data, step);
        assert_eq!(
            got, reference,
            "splitting the same bytes into {step}-byte pieces changed the \
             frames that came out — a cancelled read would desynchronise the \
             stream"
        );
    }
});
