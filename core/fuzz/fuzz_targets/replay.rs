//! Fuzz the replay window.
//!
//! A peer chooses the sequence numbers, so the window sees arbitrary `u64`
//! values in arbitrary order — including jumps of nearly the whole range, which
//! is where a bitmap shift goes wrong.
//!
//! The contract: **`check` must return, never panic.** Its verdict is not under
//! test here; the unit tests cover correctness. This covers survival.
//!
//! Run:
//!     cargo +nightly fuzz run replay

#![no_main]

use libfuzzer_sys::fuzz_target;
use esper_proto::ReplayWindow;

fuzz_target!(|data: &[u8]| {
    let mut w = ReplayWindow::new();
    // Eight bytes per sequence number, so the fuzzer can steer the exact
    // values — including adjacent ones that differ only near a word boundary.
    for chunk in data.chunks_exact(8) {
        let mut n = [0u8; 8];
        n.copy_from_slice(chunk);
        let _ = w.check(u64::from_be_bytes(n));
    }
});
