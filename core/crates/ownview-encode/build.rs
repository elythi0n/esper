// A build script's only way to fail is to panic, so the workspace's ban on
// panicking does not apply here. The ban exists because a panic reachable from
// the network is a denial of service; this code runs at build time on a
// developer's own machine and has no other way to say no.
#![allow(clippy::panic)]

//! Refuse to build an encoder that is quietly slow.
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
//! Measured on this project's development machine, that fallback costs
//! **20.6 ms per 1080p frame against a target of 15**. Nothing reports it. A
//! host built without `nasm` would encode 37% slower than every number in the
//! project's own findings, and the first sign would be a user saying the
//! session feels slow.
//!
//! That is not a warning-shaped problem. The product's central promise is that
//! it does not take over someone else's machine, and a build that silently
//! breaks the number that promise rests on should not succeed.
//!
//! Set `OWNVIEW_ALLOW_SLOW_ENCODER=1` to build anyway. It is deliberately
//! wordy, and the binary reports the slow path at run time so a session that
//! feels wrong can be explained rather than guessed at.

use std::process::Command;

fn main() {
    println!("cargo:rerun-if-env-changed=OWNVIEW_ALLOW_SLOW_ENCODER");
    println!("cargo:rustc-check-cfg=cfg(slow_encoder)");

    let arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    // Only x86 has the assembly. On anything else the C path is all there is,
    // so demanding an assembler would be a build failure with no remedy.
    let needs_nasm = arch == "x86" || arch == "x86_64";
    if !needs_nasm {
        return;
    }

    if has_nasm() {
        return;
    }

    if std::env::var("OWNVIEW_ALLOW_SLOW_ENCODER").is_ok() {
        println!("cargo:rustc-cfg=slow_encoder");
        println!(
            "cargo:warning=building without nasm: the H.264 encoder will use \
             the C-only path, measured at 20.6 ms per 1080p frame against a \
             target of 15. The binary will report this at run time."
        );
        return;
    }

    // A build script's only way to fail is to panic, and the message is what
    // the developer sees, so it carries the whole explanation.
    // A build script's only way to fail is to panic, and the message is what
    // the developer sees, so it carries the whole explanation.
    panic!(
        "{}",
        [
            "",
            "  nasm is not installed, and openh264 needs it.",
            "",
            "  Without nasm, openh264 silently compiles its C-only path. That is",
            "  37% slower - 20.6 ms per 1080p frame against a target of 15 - and",
            "  nothing at build time says so. A host built this way would be",
            "  slower than every measurement in this project's findings.",
            "",
            "  Install it:",
            "",
            "    Arch      sudo pacman -S nasm",
            "    Debian    sudo apt install nasm",
            "    macOS     brew install nasm",
            "    Windows   winget install nasm",
            "",
            "  Or, if you genuinely cannot, build the slow encoder on purpose:",
            "",
            "    OWNVIEW_ALLOW_SLOW_ENCODER=1 cargo build",
            "",
            "  The binary reports the slow path at run time, so a session that",
            "  feels wrong can be explained rather than guessed at.",
            "",
        ]
        .join("\n")
    );
}

/// Whether a usable `nasm` is on the path.
///
/// Runs it rather than looking for the file: a `nasm` that is present but
/// cannot execute — the wrong architecture, a broken symlink, a shim — would
/// pass a file check and fail the build later, or worse, fall back silently.
fn has_nasm() -> bool {
    Command::new(std::env::var("NASM").unwrap_or_else(|_| "nasm".to_owned()))
        .arg("-v")
        .output()
        .is_ok_and(|out| out.status.success())
}
