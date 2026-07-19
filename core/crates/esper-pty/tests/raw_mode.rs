#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
// Raw mode is Unix; so is its test.
#![cfg(unix)]

//! Raw mode, and getting the terminal back.
//!
//! # Why these tests are structured oddly
//!
//! The interesting property — that a terminal is restored even when the client
//! panics — cannot be tested in-process: a panic in a test is caught by the
//! harness, and the harness's own terminal is not ours to wreck. So the panic
//! case runs a **child process** that panics deliberately, and the parent
//! checks the terminal afterwards.
//!
//! Skipped under MIRI: these query a real terminal and spawn a real process,
//! and MIRI interprets Rust rather than making syscalls.
//!
//! Under `cargo test` stdin is not a terminal, so raw mode is inert. That is
//! itself worth asserting — a client driven by a script must not fail because
//! it has no terminal — and the cases needing a real tty allocate one.

use esper_pty::{RawMode, raw};

/// With no terminal — piped stdin, CI, a test harness — the guard is inert
/// rather than an error. A remote shell driven by a script has no terminal and
/// does not need one.
#[test]
#[cfg_attr(miri, ignore = "queries a real terminal; MIRI has no syscalls")]
fn without_a_terminal_the_guard_is_inert_rather_than_failing() {
    let guard = RawMode::enable().expect("must not fail without a tty");
    assert!(
        !guard.is_active(),
        "there is no tty under the test harness, so raw mode cannot be active"
    );
    assert!(!raw::is_raw());
}

/// A sane fallback matters: a client that reported 0x0 would make the remote
/// shell wrap every line at column zero.
#[test]
#[cfg_attr(miri, ignore = "queries a real terminal; MIRI has no syscalls")]
fn the_window_size_falls_back_to_something_usable() {
    let (cols, rows) = RawMode::window_size();
    assert!(cols > 0 && rows > 0, "got {cols}x{rows}");
    assert!(
        cols >= 20 && rows >= 5,
        "the fallback should be a usable terminal, got {cols}x{rows}"
    );
}

/// Installing the hook twice must keep one chain rather than nesting, or a
/// panic message gets printed once per install.
#[test]
#[cfg_attr(miri, ignore = "queries a real terminal; MIRI has no syscalls")]
fn installing_the_panic_hook_twice_is_idempotent() {
    RawMode::install_panic_hook();
    RawMode::install_panic_hook();
    RawMode::install_panic_hook();
    // Reaching here without a stack overflow or a duplicated hook is the
    // assertion; a nested chain would recurse on the next panic.
}

/// Restoring twice must be safe. `restore` is called explicitly and again from
/// `Drop`, and the second call must be a no-op rather than clobbering settings
/// something else has since applied.
#[test]
#[cfg_attr(miri, ignore = "queries a real terminal; MIRI has no syscalls")]
fn restoring_twice_is_safe() {
    let mut guard = RawMode::enable().expect("enable");
    guard.restore();
    guard.restore();
    drop(guard);
    assert!(!raw::is_raw());
}

/// The real property, in a real terminal, in a child process.
///
/// Verified to bite: with both restoration paths removed — the `Drop` impl and
/// the panic hook — this fails with the exact message a user would experience.
/// With only the hook removed it still passes, because `Drop` runs during
/// unwinding, which is how we learned the hook covers `panic = "abort"` and
/// message legibility rather than being the load-bearing path.
///
/// The child allocates a pty, puts it into raw mode, and panics. The parent
/// then reads the pty's settings and requires echo to be back on — which is
/// the observable difference between a usable terminal and one the user has to
/// type `reset` into blind.
#[cfg(target_os = "linux")]
#[test]
#[cfg_attr(miri, ignore = "queries a real terminal; MIRI has no syscalls")]
fn a_panic_restores_the_terminal() {
    use std::io::Read;
    use std::process::Command;

    // `script` gives the child a real pty as stdin/stdout, which is the only
    // way to exercise a code path that does nothing without one.
    let probe = std::env::var("ESPER_RAW_PROBE").ok();
    if probe.is_some() {
        // Child role: enable raw mode, install the hook, then panic.
        RawMode::install_panic_hook();
        let _guard = RawMode::enable().expect("enable in child");
        assert!(raw::is_raw(), "the child should be in raw mode");
        panic!("deliberate panic to test terminal restoration");
    }

    let exe = std::env::current_exe().expect("current exe");
    // Run this same test binary, with the marker set, under a pty. Afterwards
    // `stty -a` on that pty reports whether echo came back.
    let script = format!(
        "{} a_panic_restores_the_terminal --exact --nocapture >/dev/null 2>&1; stty -a",
        exe.display()
    );
    let out = Command::new("script")
        .args(["-qec", &script, "/dev/null"])
        .env("ESPER_RAW_PROBE", "1")
        .output();

    let Ok(out) = out else {
        eprintln!("`script` is unavailable; skipping the real-tty panic check");
        return;
    };
    let mut text = String::new();
    let _ = out.stdout.as_slice().read_to_string(&mut text);

    if text.trim().is_empty() {
        eprintln!("`script` produced no output; skipping the real-tty panic check");
        return;
    }

    // `stty -a` lists flags as whitespace-separated tokens: `echo` when on,
    // `-echo` when off. **Tokenise rather than substring-match** — `-echo` is a
    // substring of `-echonl` and `-echoprt`, which are different flags
    // entirely, and matching the substring reported a correctly-restored
    // terminal as broken. That was this test's first version.
    let tokens: Vec<&str> = text.split_whitespace().collect();
    let echo_on = tokens.contains(&"echo");
    let echo_off = tokens.contains(&"-echo");

    assert!(
        !echo_off,
        "the terminal was left with echo disabled after a panic — the user would \
         be typing blind. stty reported:\n{text}"
    );
    assert!(
        echo_on,
        "stty did not report echo either way, so this test proved nothing. \
         Reported:\n{text}"
    );

    // And canonical mode back too: raw mode turns `icanon` off, so its return
    // is the other half of a restored terminal.
    assert!(
        tokens.contains(&"icanon"),
        "canonical mode was not restored, so Enter would not end a line. \
         Reported:\n{text}"
    );
}
