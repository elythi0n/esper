//! Locking the screen, which nothing in this product can do by itself.
//!
//! # Why this shells out
//!
//! There is no way to lock a screen from inside an application. The lock is
//! owned by the session — `systemd-logind`, or a screen locker the person
//! chose — and asking it is the only mechanism there is. Every remote desktop
//! and every hotkey daemon does the same thing, because there is nothing else
//! to do.
//!
//! So: try the ones that exist, in order, and **say which one worked**. A
//! panic key that silently failed to lock would leave somebody believing their
//! screen was locked when it was not, which is worse than telling them it
//! could not be done.
//!
//! # Ending the sessions comes first
//!
//! Locking is the second half of the panic key and the less important one. A
//! caller must end its sessions whatever this returns: a machine that could
//! not find a screen locker is still a machine somebody wants to stop being
//! watched.

use std::path::PathBuf;
use std::process::Command;

/// What a locker actually locks.
///
/// **The distinction is not academic, and it was found the hard way.** A host
/// serving a scratch display — a second X server, another seat, a test — is
/// not serving the login session, and a locker that asks the session manager
/// locks the *person's* screen rather than the one the host was showing. The
/// suite caught it: a host on an `Xvfb` reported locking a screen, and the
/// screen it would have locked on a desktop is the developer's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Locks {
    /// The display this process is pointed at, and nothing else.
    ThisDisplay,
    /// Whatever the session manager considers the current session.
    ThisLoginSession,
}

/// The commands that lock a screen, in the order they are tried.
///
/// `loginctl` first because it asks the session manager rather than a
/// particular locker, so it works whatever the person is running — and on a
/// machine with no seat it fails quickly and the next one is tried. It is also
/// the one that locks something other than the display it was asked about,
/// which is why each entry says what it locks.
pub const LOCKERS: &[(&str, &[&str], Locks)] = &[
    ("loginctl", &["lock-session"], Locks::ThisLoginSession),
    ("xdg-screensaver", &["lock"], Locks::ThisDisplay),
    ("swaylock", &["-f"], Locks::ThisDisplay),
    ("i3lock", &[], Locks::ThisDisplay),
];

/// Find an executable on `PATH`.
///
/// Split out and taking the path as an argument so it can be tested: the
/// interesting cases are "several are present, the first wins" and "none is",
/// and neither should need a particular machine.
#[must_use]
pub fn find_on(path: &str, program: &str) -> Option<PathBuf> {
    for dir in path.split(':').filter(|d| !d.is_empty()) {
        let candidate = PathBuf::from(dir).join(program);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// The first locker present on this machine.
#[must_use]
pub fn available_locker_on(path: &str) -> Option<(&'static str, &'static [&'static str])> {
    locker_on(path, Reach::Anything).map(|(p, a, _)| (p, a))
}

/// How far a caller is willing to let a lock reach.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reach {
    /// Any locker. What the panic key wants: somebody standing at the machine
    /// has asked for it all to stop, and locking their login session as well
    /// as the display in front of them is what they meant.
    Anything,
    /// Only a locker that acts on the display this process is pointed at.
    ///
    /// What an *automatic* lock wants. Nobody asked for this one, so it must
    /// not reach past the screen the session was on — a host serving a second
    /// X server, another seat or a test would otherwise lock the person's real
    /// desktop because something disconnected from a display they were not
    /// looking at.
    OnlyThisDisplay,
}

/// The first locker on `path` that reaches no further than asked.
#[must_use]
pub fn locker_on(
    path: &str,
    reach: Reach,
) -> Option<(&'static str, &'static [&'static str], Locks)> {
    LOCKERS
        .iter()
        .filter(|(_, _, locks)| reach == Reach::Anything || *locks == Locks::ThisDisplay)
        .find(|(program, _, _)| find_on(path, program).is_some())
        .copied()
}

/// The first locker present, looking at this process's `PATH`.
#[must_use]
pub fn available_locker() -> Option<(&'static str, &'static [&'static str])> {
    let path = std::env::var("PATH").unwrap_or_default();
    available_locker_on(&path)
}

/// Lock the screen, saying which mechanism did it.
///
/// **Not waited for.** A locker takes over the screen and keeps running; a
/// caller that waited for it would wait until the person unlocked, which is
/// the last thing a panic key should do to the process it is trying to stop
/// sessions from.
pub fn lock_screen() -> Result<&'static str, String> {
    lock_reaching(Reach::Anything)
}

/// Lock, going no further than `reach` allows.
///
/// # Errors
///
/// If nothing within reach is installed, or it will not start.
pub fn lock_reaching(reach: Reach) -> Result<&'static str, String> {
    let path = std::env::var("PATH").unwrap_or_default();
    let Some((program, args, _)) = locker_on(&path, reach) else {
        return Err(format!(
            "no screen locker found: tried {}",
            LOCKERS
                .iter()
                .filter(|(_, _, locks)| {
                    reach == Reach::Anything || *locks == Locks::ThisDisplay
                })
                .map(|(p, _, _)| *p)
                .collect::<Vec<_>>()
                .join(", ")
        ));
    };
    let child = Command::new(program)
        .args(args)
        .spawn()
        .map_err(|e| format!("{program}: {e}"))?;
    believe(child, program)
}

/// Whether a just-started locker is one to believe.
///
/// # Started is not locked, and the difference is worth a moment
///
/// Waiting for a locker to *finish* would wait until the person unlocked,
/// which is the last thing a panic key should do — so this waits only long
/// enough to catch one that fell over immediately. A locker that worked is
/// either still running (`i3lock` holds the screen) or exited zero
/// (`xdg-screensaver` delegates and returns); one that could not is a non-zero
/// status within a few milliseconds.
///
/// Found by a host with no `DISPLAY` reporting that it had locked a screen
/// with `xdg-screensaver`: the spawn succeeded, the program failed, and the
/// audit trail recorded a lock that never happened.
///
/// Takes the child rather than starting it, so a test can hand it a program
/// that behaves each of the three ways.
fn believe(mut child: std::process::Child, program: &'static str) -> Result<&'static str, String> {
    for _ in 0..GAVE_UP_AT_ONCE {
        match child.try_wait() {
            Ok(Some(status)) if !status.success() => {
                return Err(format!("{program} could not lock this screen: {status}"));
            }
            // Exited cleanly, which is what a locker that delegates does.
            Ok(Some(_)) => return Ok(program),
            // Still running, which is what a locker that holds the screen
            // does. Nothing more to learn.
            Ok(None) => std::thread::sleep(BETWEEN_LOOKS),
            Err(e) => return Err(format!("{program}: {e}")),
        }
    }
    Ok(program)
}

/// How many times to look at a just-started locker before believing it.
///
/// Ten looks at twenty milliseconds is a fifth of a second — long enough for a
/// program that cannot reach a display to say so, and short enough that
/// nothing waits on it noticeably. A locker that is still running after that
/// is a locker that is working.
const GAVE_UP_AT_ONCE: usize = 10;

/// How long between those looks.
const BETWEEN_LOOKS: std::time::Duration = std::time::Duration::from_millis(20);

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    /// **An automatic lock must not reach past the display it was serving.**
    ///
    /// Found by the suite, which runs a host on a scratch `Xvfb` for the
    /// clipboard checks: that host reported locking a screen, and the screen
    /// it would have locked on a desktop is the developer's own — because
    /// `loginctl lock-session` asks the session manager, which has nothing to
    /// do with the display anybody was looking at.
    #[test]
    fn an_automatic_lock_uses_no_locker_that_reaches_the_login_session() {
        for (program, _, locks) in LOCKERS {
            if *locks == Locks::ThisLoginSession {
                let path = "/usr/bin";
                // Whatever is installed, the filtered list never offers it.
                let offered = locker_on(path, Reach::OnlyThisDisplay);
                assert!(
                    offered.is_none_or(|(p, _, _)| p != *program),
                    "{program} locks the login session and was offered to an \
                     automatic lock"
                );
            }
        }
    }

    /// And the panic key keeps the full reach, correctly: somebody standing at
    /// the machine has asked for all of it to stop.
    #[test]
    fn the_panic_key_may_use_any_locker() {
        let with_everything: Vec<&str> = LOCKERS.iter().map(|(p, _, _)| *p).collect();
        let display_only: Vec<&str> = LOCKERS
            .iter()
            .filter(|(_, _, l)| *l == Locks::ThisDisplay)
            .map(|(p, _, _)| *p)
            .collect();
        assert!(
            with_everything.len() > display_only.len(),
            "the two reaches must actually differ, or the distinction is decoration"
        );
        assert!(with_everything.contains(&"loginctl"));
        assert!(!display_only.contains(&"loginctl"));
    }

    #[test]
    fn nothing_on_an_empty_path() {
        assert!(find_on("", "loginctl").is_none());
        assert!(available_locker_on("").is_none());
        assert!(available_locker_on(":::").is_none());
    }

    /// **The order is the point.** `loginctl` asks the session manager and
    /// works whatever locker the person runs, so it must be preferred over a
    /// particular one that happens to be installed.
    #[test]
    fn the_first_locker_in_the_list_wins() {
        let dir = std::env::temp_dir().join(format!("esper-lock-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("a directory");
        for program in ["i3lock", "loginctl", "swaylock"] {
            std::fs::write(dir.join(program), b"#!/bin/sh\n").expect("a file");
        }
        let path = dir.to_string_lossy().into_owned();
        assert_eq!(
            available_locker_on(&path).map(|(p, _)| p),
            Some("loginctl"),
            "loginctl is first in the list, so it wins over the others present"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_locker_further_down_the_list_is_found_when_it_is_the_only_one() {
        let dir = std::env::temp_dir().join(format!("esper-lock-only-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("a directory");
        std::fs::write(dir.join("i3lock"), b"#!/bin/sh\n").expect("a file");
        let path = dir.to_string_lossy().into_owned();
        assert_eq!(available_locker_on(&path).map(|(p, _)| p), Some("i3lock"));
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A directory named on `PATH` that is not there must not stop the search.
    #[test]
    fn a_missing_directory_on_the_path_is_skipped() {
        let dir = std::env::temp_dir().join(format!("esper-lock-skip-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("a directory");
        std::fs::write(dir.join("swaylock"), b"#!/bin/sh\n").expect("a file");
        let path = format!("/definitely/not/here:{}", dir.to_string_lossy());
        assert_eq!(available_locker_on(&path).map(|(p, _)| p), Some("swaylock"));
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A *directory* with the right name is not a program.
    #[test]
    fn a_directory_is_not_a_locker() {
        let dir = std::env::temp_dir().join(format!("esper-lock-dir-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("loginctl")).expect("a directory");
        let path = dir.to_string_lossy().into_owned();
        assert_eq!(available_locker_on(&path), None);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// **A locker that fell over is not a locked screen.**
    ///
    /// The three ways a just-started program can behave, and only one of them
    /// is a failure: still running is `i3lock` holding the screen, exited
    /// cleanly is `xdg-screensaver` delegating, and exited badly is a program
    /// that could not reach a display.
    #[test]
    fn only_a_locker_that_fell_over_is_a_failure() {
        let still_running = Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("sleep is on every machine this runs on");
        let id = still_running.id();
        assert_eq!(believe(still_running, "sleep").ok(), Some("sleep"));
        // Not left behind: a `sleep 30` per test run would outlive the suite.
        let _ = Command::new("kill").arg(id.to_string()).status();

        let finished = Command::new("true").spawn().expect("true exists");
        assert_eq!(believe(finished, "true").ok(), Some("true"));

        let failed = Command::new("false").spawn().expect("false exists");
        let err = believe(failed, "false").expect_err("a non-zero exit is a failure");
        assert!(err.contains("could not lock"), "{err}");
        assert!(err.contains("false"), "{err}");
    }
}
