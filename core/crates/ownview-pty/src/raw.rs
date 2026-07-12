//! Putting the controller's own terminal into raw mode, and getting it back.
//!
//! # What raw mode is for
//!
//! In its normal (canonical) mode a terminal does work on the user's behalf: it
//! echoes what they type, buffers a line until Enter, and turns Ctrl-C into a
//! signal for the local process. For a remote shell every one of those is
//! wrong. The **remote** shell owns the line discipline — it should decide what
//! echoes, when a line is complete, and what Ctrl-C means — so the local
//! terminal must stop interfering and pass bytes through untouched.
//!
//! Concretely, raw mode is what makes these work:
//!
//! | Without raw mode | With it |
//! |---|---|
//! | Ctrl-C kills the local client | Ctrl-C reaches the remote shell |
//! | Ctrl-Z suspends the local client | Ctrl-Z suspends the remote job |
//! | Typing is echoed twice | Echoed once, by the remote |
//! | Nothing is sent until Enter | Every keystroke goes immediately |
//! | Tab completion and arrow keys are eaten locally | They reach the remote |
//!
//! # Why restoration is the hard part
//!
//! A process that leaves a terminal in raw mode leaves the user with a shell
//! that does not echo, does not handle Enter, and does not respond to Ctrl-C.
//! They cannot see what they are typing to fix it. `reset` fixes it, but only
//! if they know that, and the state persists across their next commands.
//!
//! So [`RawMode`] restores on `Drop`, which covers a normal return, an early
//! `?`, and a panic — because `Drop` runs during unwinding. What `Drop` does
//! *not* cover is `abort`, a `SIGKILL`, or `panic = "abort"`, and those are
//! handled separately: [`RawMode::install_panic_hook`] restores before the
//! panic message is printed, so even an aborting build leaves a usable
//! terminal.

use std::io;
use std::sync::atomic::{AtomicBool, Ordering};

use rustix::termios::{self, OptionalActions, Termios, Winsize};

/// Whether a terminal is currently in raw mode, process-wide.
///
/// Needed by the panic hook, which has no access to the [`RawMode`] guard. A
/// static is the right shape here: "is this process's terminal in raw mode" is
/// a property of the process, not of a value.
static RAW_ACTIVE: AtomicBool = AtomicBool::new(false);

/// The saved terminal settings, restored on drop.
///
/// Storing the *original* `Termios` rather than reconstructing a sane one:
/// the user may have had their own settings, and handing back a generic
/// terminal is not the same as handing back theirs.
static ORIGINAL: std::sync::Mutex<Option<Termios>> = std::sync::Mutex::new(None);

/// A guard that keeps the terminal in raw mode and restores it on drop.
///
/// Not `Clone` and not `Copy`: two guards would mean two restores, and the
/// second would restore settings the first had already replaced.
#[derive(Debug)]
pub struct RawMode {
    /// `false` when this process's input is not a terminal — piped, or running
    /// under a test harness. Then the guard is inert rather than an error,
    /// because a script driving the client is a legitimate use.
    active: bool,
}

impl RawMode {
    /// Put the terminal into raw mode.
    ///
    /// If stdin is not a terminal — piped input, a CI job, a test — this is a
    /// no-op rather than a failure. A remote shell driven by a script has no
    /// terminal to put into raw mode and does not need one.
    pub fn enable() -> io::Result<Self> {
        let stdin = io::stdin();
        if !termios::isatty(&stdin) {
            return Ok(Self { active: false });
        }

        let original = termios::tcgetattr(&stdin).map_err(io::Error::from)?;

        let mut raw = original.clone();
        raw.make_raw();

        termios::tcsetattr(&stdin, OptionalActions::Flush, &raw).map_err(io::Error::from)?;

        if let Ok(mut slot) = ORIGINAL.lock() {
            // Only record the first save. If two guards somehow existed, the
            // second must not overwrite the genuine original with a raw one.
            if slot.is_none() {
                *slot = Some(original);
            }
        }
        RAW_ACTIVE.store(true, Ordering::SeqCst);

        Ok(Self { active: true })
    }

    /// Whether raw mode is actually in effect.
    #[must_use]
    pub const fn is_active(&self) -> bool {
        self.active
    }

    /// Install a panic hook that restores the terminal before printing.
    ///
    /// `Drop` already covers unwinding panics, but the panic *message* is
    /// printed by the hook, which runs first — and a multi-line message printed
    /// to a raw terminal is unreadable, every line starting where the last one
    /// ended. Restoring in the hook means the user can read what went wrong.
    ///
    /// It also covers `panic = "abort"`, where `Drop` never runs at all.
    ///
    /// **Measured**: with the hook removed but `Drop` intact, the real-tty
    /// panic test still passes — `Drop` runs during unwinding and restores. So
    /// the hook is not what saves an unwinding panic, and its value is
    /// precisely the two cases `Drop` cannot reach: an aborting build, and a
    /// readable panic message. Worth knowing, because it means the hook is not
    /// the load-bearing part and removing it would fail no test.
    ///
    /// Idempotent: installing twice keeps one chain rather than nesting.
    pub fn install_panic_hook() {
        static INSTALLED: AtomicBool = AtomicBool::new(false);
        if INSTALLED.swap(true, Ordering::SeqCst) {
            return;
        }
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            restore_now();
            previous(info);
        }));
    }

    /// The current terminal size, for the initial `TerminalOpen`.
    ///
    /// Falls back to 80x24 when there is no terminal to ask — the historical
    /// default, and a size every program copes with.
    #[must_use]
    pub fn window_size() -> (u16, u16) {
        let stdin = io::stdin();
        termios::tcgetwinsize(&stdin).map_or((80, 24), |w: Winsize| {
            let cols = if w.ws_col == 0 { 80 } else { w.ws_col };
            let rows = if w.ws_row == 0 { 24 } else { w.ws_row };
            (cols, rows)
        })
    }

    /// Restore the terminal immediately, before this guard is dropped.
    ///
    /// For the case where the client wants the terminal back while still
    /// running — printing a summary after a session ends, for instance.
    pub fn restore(&mut self) {
        if self.active {
            restore_now();
            self.active = false;
        }
    }
}

impl Drop for RawMode {
    fn drop(&mut self) {
        self.restore();
    }
}

/// Put the terminal back the way it was found.
///
/// Best-effort and never panicking. This runs from `Drop` and from a panic
/// hook, and a failure to restore must not become a second panic — that would
/// abort the process and leave the terminal in exactly the state this function
/// exists to prevent.
fn restore_now() {
    if !RAW_ACTIVE.swap(false, Ordering::SeqCst) {
        return;
    }
    let Ok(slot) = ORIGINAL.lock() else {
        return;
    };
    let Some(original) = slot.as_ref() else {
        return;
    };
    let stdin = io::stdin();
    // `Flush` discards anything typed since: a user whose session just ended
    // should not have their keystrokes land in whatever shell comes next.
    let _ = termios::tcsetattr(&stdin, OptionalActions::Flush, original);
}

/// Whether this process's terminal is currently in raw mode.
///
/// Exposed for `doctor` and for tests. A client that thinks it restored and
/// did not is a bug worth being able to observe.
#[must_use]
pub fn is_raw() -> bool {
    RAW_ACTIVE.load(Ordering::SeqCst)
}
