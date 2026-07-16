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

/// The commands that lock a screen, in the order they are tried.
///
/// `loginctl` first because it asks the session manager rather than a
/// particular locker, so it works whatever the person is running — and on a
/// machine with no seat it fails quickly and the next one is tried.
pub const LOCKERS: &[(&str, &[&str])] = &[
    ("loginctl", &["lock-session"]),
    ("xdg-screensaver", &["lock"]),
    ("swaylock", &["-f"]),
    ("i3lock", &[]),
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
    LOCKERS
        .iter()
        .find(|(program, _)| find_on(path, program).is_some())
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
    let Some((program, args)) = available_locker() else {
        return Err(format!(
            "no screen locker found: tried {}",
            LOCKERS
                .iter()
                .map(|(p, _)| *p)
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
        let dir = std::env::temp_dir().join(format!("ownview-lock-test-{}", std::process::id()));
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
        let dir = std::env::temp_dir().join(format!("ownview-lock-only-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("a directory");
        std::fs::write(dir.join("i3lock"), b"#!/bin/sh\n").expect("a file");
        let path = dir.to_string_lossy().into_owned();
        assert_eq!(available_locker_on(&path).map(|(p, _)| p), Some("i3lock"));
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A directory named on `PATH` that is not there must not stop the search.
    #[test]
    fn a_missing_directory_on_the_path_is_skipped() {
        let dir = std::env::temp_dir().join(format!("ownview-lock-skip-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("a directory");
        std::fs::write(dir.join("swaylock"), b"#!/bin/sh\n").expect("a file");
        let path = format!("/definitely/not/here:{}", dir.to_string_lossy());
        assert_eq!(available_locker_on(&path).map(|(p, _)| p), Some("swaylock"));
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A *directory* with the right name is not a program.
    #[test]
    fn a_directory_is_not_a_locker() {
        let dir = std::env::temp_dir().join(format!("ownview-lock-dir-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("loginctl")).expect("a directory");
        let path = dir.to_string_lossy().into_owned();
        assert_eq!(available_locker_on(&path), None);
        std::fs::remove_dir_all(&dir).ok();
    }
}
