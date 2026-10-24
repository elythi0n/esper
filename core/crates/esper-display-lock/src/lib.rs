//! One display, one test at a time.
//!
//! # Why a file and not a mutex
//!
//! A display has one keyboard focus, one pointer, one stacking order and one
//! set of pixels. Four test suites in this workspace take some of those:
//! capture reads pixels back, presenting draws and reads them, input takes the
//! focus and moves the pointer, and presence draws a frame over everything.
//!
//! Each of them held a `static Mutex`, which serialises the tests **inside one
//! binary** and does nothing at all between binaries — and `cargo test` runs
//! the binaries in parallel. So a presence test drawing a frame over the
//! screen would happen in the middle of a capture test reading pixels, and one
//! of them would fail for no reason a reader of either could see.
//!
//! A lock file is the smallest thing that spans processes. `create_new` is
//! atomic in every filesystem that matters, so creating it *is* the lock.
//!
//! # Why it forgets a stale lock
//!
//! A test that panics while holding the lock leaves the file behind, and every
//! later run on that machine would then wait for it and give up. So a lock
//! whose file is older than the longest a test could reasonably hold it is
//! taken anyway, with a note.
//!
//! Test-only. Nothing in the product depends on this, and nothing should.

use std::path::PathBuf;
use std::time::{Duration, SystemTime};

/// How long to wait for another suite to finish.
///
/// Generous: a capture test can take twenty seconds on a busy machine, and
/// waiting is much cheaper than a failure nobody can reproduce.
const WAIT: Duration = Duration::from_secs(60);

/// After this, a lock file is assumed to be the remains of something that
/// died.
const STALE: Duration = Duration::from_secs(120);

/// Held for as long as a test needs the display to itself.
#[derive(Debug)]
pub struct DisplayLock {
    path: PathBuf,
    /// False when the lock could not be taken and the test is proceeding
    /// anyway, which is better than refusing to test.
    held: bool,
}

impl DisplayLock {
    /// Take the display, waiting for whoever has it.
    ///
    /// Never fails: a test that cannot take the lock runs regardless, because
    /// a suite that skipped itself over a lock file would be a suite that
    /// stops testing for a reason nobody notices.
    #[must_use]
    pub fn take() -> Self {
        let path = Self::path();
        let deadline = std::time::Instant::now() + WAIT;
        loop {
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(_) => return Self { path, held: true },
                Err(_) => {
                    if Self::is_stale(&path) {
                        eprintln!(
                            "display lock at {} looks abandoned; taking it",
                            path.display()
                        );
                        let _ = std::fs::remove_file(&path);
                        continue;
                    }
                    if std::time::Instant::now() >= deadline {
                        eprintln!(
                            "waited {WAIT:?} for the display lock at {}; carrying on without it",
                            path.display()
                        );
                        return Self { path, held: false };
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
            }
        }
    }

    /// Whether this really holds the lock.
    #[must_use]
    pub const fn held(&self) -> bool {
        self.held
    }

    /// One lock per display, so two `Xvfb`s can be tested at once.
    fn path() -> PathBuf {
        let display = std::env::var("DISPLAY").unwrap_or_else(|_| "none".to_owned());
        let safe: String = display
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .collect();
        std::env::temp_dir().join(format!("esper-display-{safe}.lock"))
    }

    fn is_stale(path: &PathBuf) -> bool {
        let Ok(meta) = std::fs::metadata(path) else {
            return false;
        };
        let Ok(modified) = meta.modified() else {
            return false;
        };
        SystemTime::now()
            .duration_since(modified)
            .is_ok_and(|age| age > STALE)
    }
}

impl Drop for DisplayLock {
    fn drop(&mut self) {
        if self.held {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}
/// How many times a connection is attempted before giving up.
///
/// **Measured, not chosen.** A display server under a burst of test processes
/// resets new connections: `cargo test` runs the suites in parallel and each
/// test opens its own, so several dozen arrive at once. It surfaced three
/// separate times as three different "bugs" — a presence test that skipped
/// itself, an input test that failed on `expect("a connection")`, and a
/// keeper that could not put a screen back — before it was recognised as one
/// thing.
const TRIES: usize = 8;

/// How long between attempts. Eight of these is about a second.
const BETWEEN: Duration = Duration::from_millis(120);

/// Connect to the display, tolerating a server that is momentarily busy.
///
/// **This is the test rig's connect, and it should not be the product's.** A
/// program that a person is running wants to hear immediately that the display
/// is gone; a test suite competing with seven other suites for one `Xvfb`
/// wants to wait a moment and try again. The distinction matters: retrying in
/// the product would hide a real failure behind a second of silence.
///
/// # Errors
///
/// The last error, when every attempt failed. A caller should treat that as a
/// broken rig rather than a reason to skip: somebody asked for these tests to
/// run.
pub fn connect() -> Result<(x11rb::rust_connection::RustConnection, usize), String> {
    let mut last = String::new();
    for attempt in 1..=TRIES {
        match x11rb::connect(None) {
            Ok((conn, screen)) => return Ok((conn, screen)),
            Err(e) => {
                last = e.to_string();
                if attempt < TRIES {
                    std::thread::sleep(BETWEEN);
                }
            }
        }
    }
    Err(format!("{last}, after {TRIES} attempts"))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    #[test]
    fn taking_it_creates_the_file_and_dropping_it_removes_it() {
        let path = DisplayLock::path();
        let _ = std::fs::remove_file(&path);
        {
            let lock = DisplayLock::take();
            assert!(lock.held(), "nothing else should hold it in this test");
            assert!(path.exists(), "the lock file is the lock");
        }
        assert!(!path.exists(), "and it goes when the lock does");
    }

    /// **A lock left behind by something that died is taken anyway.**
    /// Otherwise one panicking test poisons the machine for every later run.
    #[test]
    fn an_abandoned_lock_is_taken() {
        let path = std::env::temp_dir().join("esper-display-stale-test.lock");
        std::fs::write(&path, b"").expect("a file");
        // Backdate it past the staleness bound.
        let old = SystemTime::now() - STALE - Duration::from_secs(60);
        // Opened for writing, and the result read: Windows changes a time
        // only through a handle that may write, and a backdating that failed
        // quietly made this test about nothing.
        std::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .and_then(|f| f.set_modified(old))
            .expect("backdating the lock");
        assert!(
            DisplayLock::is_stale(&path),
            "a lock this old must count as abandoned"
        );
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn a_fresh_lock_is_not_stale() {
        let path = std::env::temp_dir().join("esper-display-fresh-test.lock");
        std::fs::write(&path, b"").expect("a file");
        assert!(!DisplayLock::is_stale(&path));
        std::fs::remove_file(&path).ok();
    }

    /// Two displays are two locks, so an `Xvfb` and a real desktop can be
    /// tested at the same time.
    #[test]
    fn each_display_has_its_own_lock() {
        // The path depends on the environment, so this checks the shape rather
        // than mutating a variable other tests read.
        let name = DisplayLock::path();
        let text = name.to_string_lossy();
        assert!(text.contains("esper-display-"), "{text}");
        assert!(text.ends_with(".lock"), "{text}");
    }
}
