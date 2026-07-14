// A build script's only way to fail is to panic, so the workspace's ban on
// panicking does not apply here. The ban exists because a panic reachable from
// the network is a denial of service; this code runs at build time on a
// developer's own machine and has no other way to say no.
#![allow(clippy::panic)]

//! Notice, loudly, when the encoder is built without its assembly.
//!
//! `openh264-sys2` compiles Cisco's C, and the x86 parts of it have hand
//! written assembly that needs `nasm`. When `nasm` is missing it **silently
//! falls back to the C-only path** — upstream's own build script says so:
//!
//! ```text
//! // TODO: is it a good idea to "silently" disable assembly if this fails?
//! let Ok(object_files) = nasm_build.files(...).compile_objects()
//! ```
//!
//! # What that actually costs, measured
//!
//! **Between 2.5% and 6%**, on this project's development machine, at 1080p:
//!
//! | Tuning | Content | With assembly | C-only |
//! |---|---|---:|---:|
//! | camera | screen-like | 4.30 ms | 4.56 ms |
//! | camera | high motion | 47.4 ms | 49.7 ms |
//! | screen content | screen-like | 25.3 ms | 25.9 ms |
//!
//! Byte counts are identical, so it is purely a speed question.
//!
//! # This used to refuse to build, and that was wrong
//!
//! An earlier version of this script panicked, on the grounds that the C-only
//! path is "37% slower — 20.6 ms per 1080p frame against a target of 15".
//!
//! **That claim was unfounded.** The 20.6 ms came from an early probe against
//! a high-entropy synthetic gradient, and the slowness was a property of the
//! *content*, not of the missing assembly — the same content measures 47 ms
//! with the assembly present. Comparing like for like, on identical content
//! and identical builds, the assembly is worth a few per cent.
//!
//! A hard build failure over 5% is disproportionate: it blocks anyone without
//! an assembler from building at all, for a gain most users would not notice.
//! So it warns instead. What has not changed is that the fallback must not be
//! *silent* — the warning fires and the binary reports the slow path at run
//! time, so a session that feels wrong can be explained rather than guessed at.

use std::process::Command;

fn main() {
    println!("cargo:rerun-if-env-changed=NASM");
    println!("cargo:rustc-check-cfg=cfg(slow_encoder)");

    let arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    // Only x86 has the assembly. On anything else the C path is all there is,
    // so complaining about an assembler would be noise with no remedy.
    if arch != "x86" && arch != "x86_64" {
        return;
    }

    if has_nasm() {
        return;
    }

    println!("cargo:rustc-cfg=slow_encoder");
    println!(
        "cargo:warning=nasm was not found, so openh264 is being built without \
         its assembly. Measured cost: 2.5% to 6% slower encoding, with \
         identical output size. Install nasm to get it back (pacman -S nasm, \
         apt install nasm, brew install nasm, winget install nasm). The binary \
         reports the slow path at run time."
    );
}

/// Whether a usable `nasm` is on the path.
///
/// Runs it rather than looking for the file: a `nasm` that is present but
/// cannot execute — the wrong architecture, a broken symlink, a shim — would
/// pass a file check and then fall back silently anyway.
///
/// Reads the same `NASM` variable `nasm-rs` does, so this agrees with what
/// openh264 will actually use rather than guessing.
fn has_nasm() -> bool {
    Command::new(std::env::var("NASM").unwrap_or_else(|_| "nasm".to_owned()))
        .arg("-v")
        .output()
        .is_ok_and(|out| out.status.success())
}
