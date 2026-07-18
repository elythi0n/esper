//! Fuzz the frame decoder — the first parser attacker-controlled bytes reach.
//!
//! The contract this asserts is deliberately narrow and absolute: **decoding
//! arbitrary bytes must return, never panic and never hang.** It does not care
//! whether the result is Ok or Err. A panic reachable from the network is a
//! denial of service, and this is the target that proves there isn't one.
//!
//! Run locally:
//!     cargo +nightly fuzz run frame_decode
//!
//! The always-on cheap version of this lives in the crate's unit tests, which
//! sweep every prefix and every single-byte corruption of a valid frame.

#![no_main]

use libfuzzer_sys::fuzz_target;
use esper_proto::{Hello, read_frame};

fuzz_target!(|data: &[u8]| {
    let mut cursor = data;
    let _ = read_frame::<Hello, _>(&mut cursor);
});
