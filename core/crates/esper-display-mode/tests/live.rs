#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![cfg(target_os = "linux")]

//! Changing a real screen's resolution, and putting it back.
//!
//! # Why these need more than `Xvfb`
//!
//! `Xvfb` answers RandR with exactly one mode and a maximum equal to its
//! current size, so nothing here can be exercised on it — the request would be
//! refused for want of anywhere to go, and a test that passes because the
//! server has nothing to offer is a test of the server.
//!
//! `Xephyr` is a real X server with a real RandR 1.6 implementation and nine
//! modes, and it nests inside `Xvfb`, so the whole thing stays headless.
//! `scripts/with-display.sh` is what arranges that. Without it these skip,
//! loudly enough to notice.

use std::path::{Path, PathBuf};

use esper_display_mode::{Displays, Mode, restore_anything_left_behind};

/// The display, to this suite alone: this one *changes the resolution*, which
/// every other suite in the workspace would notice.
fn exclusive() -> esper_display_lock::DisplayLock {
    esper_display_lock::DisplayLock::take()
}

/// A state directory of this test's own.
///
/// **Named for the test, not for the process.** All four share a process, so a
/// directory keyed on the pid is one directory — and a record one test left
/// behind then silently suppressed the next one's, which reads as a
/// restoration that did not happen.
fn state_dir(test: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!("esper-display-mode-{}-{test}", std::process::id()));
    p
}

/// `Some(displays)` when this display can actually change resolution.
fn open(dir: &Path) -> Option<Displays> {
    if std::env::var_os("DISPLAY").is_none() {
        println!("skipped: no DISPLAY. Run under scripts/with-display.sh.");
        return None;
    }
    let displays = match Displays::open(dir) {
        Ok(d) => d,
        Err(why) => {
            println!("skipped: {why}");
            return None;
        }
    };
    let offers = displays
        .screens()
        .map(|s| s.iter().map(|s| s.modes.len()).max().unwrap_or(0))
        .unwrap_or(0);
    if offers < 2 {
        // **A skip that can always fire is a test that never runs.** These
        // reported success while skipping every one of them, because a wrong
        // CRTC check made every screen invisible — and nothing said so. The
        // rig states how many modes it arranged, so when it says there are
        // some, finding none is a failure rather than a reason to stand down.
        let promised = std::env::var("ESPER_DISPLAY_MODES")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(0);
        assert!(
            promised < 2,
            "the rig arranged {promised} mode(s) and this crate found \
             {offers} — the display is fine and the reading of it is not"
        );
        println!(
            "skipped: this display offers {offers} mode(s). Xvfb has one; \
             scripts/with-display.sh nests Xephyr, which has nine."
        );
        return None;
    }
    Some(displays)
}

/// A mode that is not the one in force, so a change is observable.
fn something_else(screen: &esper_display_mode::Screen) -> Mode {
    screen
        .modes
        .iter()
        .copied()
        .find(|m| (m.width, m.height) != (screen.current.width, screen.current.height))
        .expect("a display offering two modes offers one that is not the current one")
}

#[test]
fn a_resolution_changes_and_comes_back() {
    let _lock = exclusive();
    let dir = state_dir("round-trip");
    let Some(mut displays) = open(&dir) else {
        return;
    };
    let before = displays.screens().unwrap().into_iter().next().unwrap();
    let want = something_else(&before);

    displays.set(&before, want).expect("set the resolution");

    let during = displays.screens().unwrap().into_iter().next().unwrap();
    assert_eq!(
        (during.current.width, during.current.height),
        (want.width, want.height),
        "the server reports the old size, so nothing was changed"
    );

    displays.restore().expect("restore");

    let after = displays.screens().unwrap().into_iter().next().unwrap();
    assert_eq!(
        (after.current.width, after.current.height),
        (before.current.width, before.current.height),
        "the screen was left at a resolution the person at that machine did \
         not choose"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Growing a screen grows the desktop canvas to hold it.
///
/// A CRTC cannot legally fall outside the canvas, and a server that does not
/// enforce that — Xephyr does not — leaves a build that never resized the
/// canvas looking perfectly correct when the screens are read back. So the
/// canvas is read directly.
#[test]
fn growing_a_screen_grows_the_canvas_to_hold_it() {
    let _lock = exclusive();
    let dir = state_dir("canvas");
    let Some(mut displays) = open(&dir) else {
        return;
    };
    let before = displays.screens().unwrap().into_iter().next().unwrap();
    let Some(bigger) = before
        .modes
        .iter()
        .copied()
        .find(|m| m.width > before.current.width && m.height > before.current.height)
    else {
        panic!("the rig's display starts at 1280x1024 and offers 1600x1200; it offers neither");
    };

    displays.set(&before, bigger).expect("grow");
    let (cw, ch) = displays.canvas().expect("the canvas");
    assert!(
        cw >= bigger.width && ch >= bigger.height,
        "the screen is {}x{} and the desktop canvas is {cw}x{ch} — the CRTC \
         is hanging off the edge, which a real server refuses",
        bigger.width,
        bigger.height
    );

    displays.restore().expect("restore");
    let _ = std::fs::remove_dir_all(&dir);
}

/// **The case the whole design is for.**
///
/// A clean teardown is the easy half. This one changes the resolution and then
/// throws the handle away without restoring — which is what a `SIGKILL`, an
/// OOM kill or a power cut looks like from the display's point of view — and
/// then does what the host does at startup.
#[test]
fn a_host_that_died_mid_session_still_puts_the_screen_back() {
    let _lock = exclusive();
    let dir = state_dir("crash");
    let Some(mut displays) = open(&dir) else {
        return;
    };
    let before = displays.screens().unwrap().into_iter().next().unwrap();
    let want = something_else(&before);

    displays.set(&before, want).expect("set the resolution");
    drop(displays);

    // Nothing here knows what the old resolution was; only the record does.
    let did = restore_anything_left_behind(&dir).expect("restore at startup");
    assert_eq!(
        did,
        esper_display_mode::Restored::PutBack,
        "there was a record and it was not acted on"
    );

    let displays = Displays::open(&dir).unwrap();
    let after = displays.screens().unwrap().into_iter().next().unwrap();
    assert_eq!(
        (after.current.width, after.current.height),
        (before.current.width, before.current.height),
        "a host that died left somebody's screen at the wrong resolution"
    );
    assert!(
        !dir.join("display-restore.json").exists(),
        "the record survived the restoration, so the next start would do it again"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Stepping through three resolutions comes back to what the person had, not
/// to the one before last.
#[test]
fn several_changes_restore_to_the_first_one() {
    let _lock = exclusive();
    let dir = state_dir("several");
    let Some(mut displays) = open(&dir) else {
        return;
    };
    let before = displays.screens().unwrap().into_iter().next().unwrap();
    let original = before.current;

    let mut sizes: Vec<Mode> = before
        .modes
        .iter()
        .copied()
        .filter(|m| (m.width, m.height) != (original.width, original.height))
        .take(2)
        .collect();
    assert!(!sizes.is_empty(), "nothing to step through");

    for want in sizes.drain(..) {
        let now = displays.screens().unwrap().into_iter().next().unwrap();
        displays.set(&now, want).expect("set");
    }

    displays.restore().expect("restore");
    let after = displays.screens().unwrap().into_iter().next().unwrap();
    assert_eq!(
        (after.current.width, after.current.height),
        (original.width, original.height),
        "it went back to the resolution before last rather than the one the \
         person was using"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A resolution the screen does not offer is refused rather than attempted.
#[test]
fn a_mode_the_screen_does_not_have_is_refused() {
    let _lock = exclusive();
    let dir = state_dir("refused");
    let Some(mut displays) = open(&dir) else {
        return;
    };
    let screen = displays.screens().unwrap().into_iter().next().unwrap();
    let err = displays
        .set(
            &screen,
            Mode {
                width: 7777,
                height: 4321,
                refresh_mhz: 0,
            },
        )
        .expect_err("a resolution no screen has");
    assert!(
        matches!(err, esper_display_mode::ModeError::NoSuchMode { .. }),
        "{err:?}"
    );
    assert!(
        !dir.join("display-restore.json").exists(),
        "a refused change left a record behind, so the next start would \
         'restore' a screen nothing touched"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
