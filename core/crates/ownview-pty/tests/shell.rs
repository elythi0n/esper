#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! A real shell on a real pseudo-terminal.
//!
//! Nothing here is mocked. The point of terminal mode is that it works on a
//! machine with no display, and the only way to know a PTY is wired up
//! correctly is to run a shell on it and read what comes back.

use std::time::Duration;

use ownview_proto::{TerminalOpen, TerminalResize};
use ownview_pty::{PtyError, ReadHalf, Terminal, exit_status};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn request(cols: u16, rows: u16) -> TerminalOpen {
    TerminalOpen {
        cols,
        rows,
        term: "xterm-256color".to_owned(),
    }
}

/// Read until `needle` appears, or fail reporting what was actually seen —
/// far more useful than "timed out" when a shell misbehaves.
async fn read_until(read: &mut ReadHalf, needle: &str) -> String {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    let mut seen = String::new();
    let mut buf = [0u8; 4096];
    loop {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        assert!(
            !left.is_zero(),
            "never saw {needle:?}; the shell produced: {seen:?}"
        );
        match tokio::time::timeout(left, read.read(&mut buf)).await {
            Ok(Ok(0)) => panic!("the pty closed before {needle:?}; saw: {seen:?}"),
            Ok(Ok(n)) => {
                seen.push_str(&String::from_utf8_lossy(buf.get(..n).unwrap_or(&[])));
                if seen.contains(needle) {
                    return seen;
                }
            }
            Ok(Err(e)) => panic!("pty read failed: {e}; saw: {seen:?}"),
            Err(_) => panic!("never saw {needle:?}; the shell produced: {seen:?}"),
        }
    }
}

#[tokio::test]
#[cfg_attr(
    miri,
    ignore = "allocates a real pseudo-terminal; MIRI has no syscalls"
)]
async fn a_shell_runs_and_answers() {
    let t = Terminal::open(&request(80, 24)).expect("open a shell");
    assert!(t.pid().is_some(), "the shell should have a pid");
    let (mut read, mut write, mut child) = t.into_parts();

    write
        .write_all(b"echo ownview-marker-42\n")
        .await
        .expect("write to the shell");
    let seen = read_until(&mut read, "ownview-marker-42").await;
    assert!(seen.contains("ownview-marker-42"));

    write.write_all(b"exit\n").await.expect("exit");
    let status = tokio::time::timeout(Duration::from_secs(15), child.wait())
        .await
        .expect("the shell should exit")
        .expect("wait");
    assert_eq!(
        exit_status(&status).shell_code(),
        0,
        "a clean exit should be 0"
    );
}

/// The window size must reach the shell, or every line wraps in the wrong place.
#[tokio::test]
#[cfg_attr(
    miri,
    ignore = "allocates a real pseudo-terminal; MIRI has no syscalls"
)]
async fn the_window_size_reaches_the_shell_and_a_resize_takes_effect() {
    let t = Terminal::open(&request(132, 43)).expect("open");
    let (mut read, mut write, mut child) = t.into_parts();

    // `stty size` reports what the kernel believes, which is what we set.
    write.write_all(b"stty size\n").await.expect("write");
    let seen = read_until(&mut read, "43 132").await;
    assert!(
        seen.contains("43 132"),
        "shell should see 43x132, saw: {seen:?}"
    );

    Terminal::resize(
        &write,
        &TerminalResize {
            cols: 100,
            rows: 30,
        },
    )
    .expect("resize");
    write.write_all(b"stty size\n").await.expect("write");
    let seen = read_until(&mut read, "30 100").await;
    assert!(
        seen.contains("30 100"),
        "resize did not reach the shell: {seen:?}"
    );

    write.write_all(b"exit\n").await.expect("exit");
    let _ = tokio::time::timeout(Duration::from_secs(15), child.wait()).await;
}

/// A terminal that mangles UTF-8 is unusable outside English.
#[tokio::test]
#[cfg_attr(
    miri,
    ignore = "allocates a real pseudo-terminal; MIRI has no syscalls"
)]
async fn utf8_survives_including_wide_and_combining_characters() {
    let t = Terminal::open(&request(120, 40)).expect("open");
    let (mut read, mut write, mut child) = t.into_parts();

    // A CJK wide character, an emoji, a combining accent, and a right-to-left
    // script — four different ways a byte-oriented pipe can go wrong.
    write
        .write_all("echo 'MARK:\u{65e5}\u{672c}\u{8a9e}-\u{1f512}-e\u{301}-\u{639}\u{631}\u{628}\u{649}:END'\n".as_bytes())
        .await
        .expect("write");

    let seen = read_until(&mut read, ":END").await;
    for needle in [
        "\u{65e5}\u{672c}\u{8a9e}",
        "\u{1f512}",
        "e\u{301}",
        "\u{639}\u{631}\u{628}\u{649}",
    ] {
        assert!(
            seen.contains(needle),
            "{needle:?} did not survive: {seen:?}"
        );
    }

    write.write_all(b"exit\n").await.expect("exit");
    let _ = tokio::time::timeout(Duration::from_secs(15), child.wait()).await;
}

/// A controller that always reported success would be useless in a script, and
/// scripting a server is most of what terminal mode is for.
#[tokio::test]
#[cfg_attr(
    miri,
    ignore = "allocates a real pseudo-terminal; MIRI has no syscalls"
)]
async fn a_nonzero_exit_code_is_reported() {
    let t = Terminal::open(&request(80, 24)).expect("open");
    let (read, mut write, mut child) = t.into_parts();
    write.write_all(b"exit 42\n").await.expect("write");

    let status = tokio::time::timeout(Duration::from_secs(15), child.wait())
        .await
        .expect("exit")
        .expect("wait");
    let e = exit_status(&status);
    assert_eq!(e.code, Some(42));
    assert_eq!(e.shell_code(), 42);
    drop(read);
}

/// Flow control: a `cat` of something large is exactly what fills a pipe
/// nobody is draining fast enough.
#[tokio::test]
#[cfg_attr(
    miri,
    ignore = "allocates a real pseudo-terminal; MIRI has no syscalls"
)]
async fn a_large_burst_of_output_does_not_deadlock() {
    let t = Terminal::open(&request(80, 24)).expect("open");
    let (mut read, mut write, mut child) = t.into_parts();

    // The marker is *assembled* by the shell rather than written literally.
    //
    // A PTY echoes what you type. The first version of this test looked for
    // "BURST-DONE" and found it immediately — in the echo of its own command
    // line — so the read loop exited after 143 bytes having measured nothing.
    // `BURST"-"DONE` appears in the echo, `BURST-DONE` only in the output.
    write
        .write_all(b"i=0; while [ $i -lt 4000 ]; do echo 0123456789012345678901234567890123456789012345678901234567890123456789; i=$((i+1)); done; echo BURST\"-\"DONE\n")
        .await
        .expect("write");

    let mut total = 0usize;
    let mut buf = vec![0u8; 8192];
    let mut tail = String::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        assert!(!left.is_zero(), "the burst deadlocked after {total} bytes");
        match tokio::time::timeout(left, read.read(&mut buf)).await {
            Ok(Ok(0)) => break,
            Ok(Ok(n)) => {
                total = total.saturating_add(n);
                tail.push_str(&String::from_utf8_lossy(buf.get(..n).unwrap_or(&[])));
                if tail.len() > 4096 {
                    tail = tail.split_off(tail.len().saturating_sub(2048));
                }
                if tail.contains("BURST-DONE") {
                    break;
                }
            }
            Ok(Err(e)) => panic!("read failed after {total} bytes: {e}"),
            Err(_) => panic!("the burst deadlocked after {total} bytes"),
        }
    }
    assert!(total > 100_000, "expected a large burst, got {total} bytes");

    write.write_all(b"exit\n").await.expect("exit");
    let _ = tokio::time::timeout(Duration::from_secs(15), child.wait()).await;
}

// ── bounds, checked before anything reaches an ioctl or a child environment ──

#[tokio::test]
#[cfg_attr(
    miri,
    ignore = "allocates a real pseudo-terminal; MIRI has no syscalls"
)]
async fn a_malformed_request_is_refused_before_a_shell_starts() {
    for bad in [
        TerminalOpen {
            cols: 0,
            rows: 24,
            term: "xterm".to_owned(),
        },
        TerminalOpen {
            cols: 80,
            rows: 0,
            term: "xterm".to_owned(),
        },
        TerminalOpen {
            cols: 80,
            rows: 24,
            term: String::new(),
        },
        TerminalOpen {
            cols: u16::MAX,
            rows: 24,
            term: "xterm".to_owned(),
        },
        TerminalOpen {
            cols: 80,
            rows: 24,
            term: "x".repeat(200),
        },
    ] {
        assert!(
            matches!(Terminal::open(&bad).err(), Some(PtyError::Malformed)),
            "{bad:?} should be refused"
        );
    }
}

/// `TERM` becomes an environment variable in a child process. A value carrying
/// a separator or a control character is not a terminal name, it is an attempt.
#[tokio::test]
#[cfg_attr(
    miri,
    ignore = "allocates a real pseudo-terminal; MIRI has no syscalls"
)]
async fn a_hostile_term_value_is_refused() {
    for term in [
        "xterm; rm -rf /",
        "xterm\nLD_PRELOAD=/tmp/evil",
        "xterm\u{0}truncated",
        "xterm=$(whoami)",
        "../../../etc/passwd",
        "xterm\u{7}",
        "xterm 256color",
    ] {
        let r = TerminalOpen {
            cols: 80,
            rows: 24,
            term: term.to_owned(),
        };
        assert!(!r.is_well_formed(), "TERM {term:?} should be rejected");
        assert!(
            matches!(Terminal::open(&r).err(), Some(PtyError::Malformed)),
            "TERM {term:?} must not start a shell"
        );
    }
}

/// A resize is something the controller asserts about its own window. Silently
/// substituting a different size leaves the two ends disagreeing about where
/// lines wrap, which looks like a rendering bug for the rest of the session.
#[tokio::test]
#[cfg_attr(
    miri,
    ignore = "allocates a real pseudo-terminal; MIRI has no syscalls"
)]
async fn an_out_of_bounds_resize_is_refused_rather_than_clamped() {
    let t = Terminal::open(&request(80, 24)).expect("open");
    let (_read, write, mut child) = t.into_parts();
    for bad in [
        TerminalResize { cols: 0, rows: 24 },
        TerminalResize { cols: 80, rows: 0 },
        TerminalResize {
            cols: u16::MAX,
            rows: 24,
        },
    ] {
        assert!(
            matches!(
                Terminal::resize(&write, &bad).err(),
                Some(PtyError::Malformed)
            ),
            "{bad:?} should be refused, not clamped"
        );
    }
    child.kill().await.ok();
}
