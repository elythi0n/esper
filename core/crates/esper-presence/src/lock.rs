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
    Command::new(program)
        .args(args)
        .spawn()
        .map(|_| program)
        .map_err(|e| format!("{program}: {e}"))
}

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
}
