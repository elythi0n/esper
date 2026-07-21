//! A shell attached to a pseudo-terminal.
//!
//! Terminal mode is not a reduced version of screen sharing. For a headless
//! server it is the **correct** mode: no capture, no codec, no display anywhere
//! in the path, no GPU, and a few kilobytes per second. A machine with no screen
//! does not need a virtual one to be reachable.
//!
//! # What is never recorded
//!
//! The bytes flowing through a terminal include every password anyone types
//! into it. **Session content is never logged, at any level, ever.** The audit
//! trail records that a terminal session happened — peer, start, end, exit
//! status — and nothing about what went through it. A remote desktop that
//! records its own terminal traffic is a keylogger with a licence, and the
//! boundary is stated here because it is the kind of thing that gets added
//! later "for debugging".
//!
//! # Unsafe
//!
//! This crate contains no `unsafe` of its own. The one genuinely delicate part
//! — the code between `fork` and `exec`, which must be async-signal-safe — is
//! owned by `pty-process`, which reasons about it explicitly. See the note in
//! `Cargo.toml` for why that is a better trade than writing the same unsafe
//! here with less review.

#![forbid(unsafe_code)]

/// Putting the controller's own terminal into raw mode.
///
/// One module per platform, so a controller needs no `cfg` of its own. Off
/// Unix it refuses rather than pretending — see that arm for why an inert
/// guard would be the worse answer.
#[cfg(unix)]
#[path = "raw/unix.rs"]
pub mod raw;
#[cfg(not(unix))]
#[path = "raw/unsupported.rs"]
pub mod raw;

pub use raw::RawMode;

/// Everything that is a fact about the platform rather than about terminals.
///
/// One module per platform, chosen here, so the rest of the crate has no
/// `cfg` in it at all — and so a host on a platform with no pseudo-terminals
/// still compiles and still answers.
#[cfg(unix)]
#[path = "sys/unix.rs"]
mod sys;
#[cfg(not(unix))]
#[path = "sys/unsupported.rs"]
mod sys;

use std::ffi::OsString;

use esper_proto::{TerminalExit, TerminalOpen, TerminalResize};

/// The read and write halves, re-exported.
///
/// Callers name these without depending on `pty-process` directly: which crate
/// owns the pseudo-terminal is this crate's decision, and leaking it into every
/// caller's `Cargo.toml` would make it everyone's.
pub use sys::{ReadHalf, WriteHalf};

/// What can go wrong running a shell.
#[derive(Debug, thiserror::Error)]
pub enum PtyError {
    /// The request was outside its bounds.
    ///
    /// Checked before anything reaches an ioctl or a child environment: the
    /// peer is authenticated but an authenticated peer is not a trusted one.
    #[error("the terminal request was malformed")]
    Malformed,

    /// No shell could be determined or launched.
    ///
    /// Names what it tried, because "could not start a shell" on a server the
    /// user cannot see the screen of is not a debuggable message.
    #[error("could not start a shell ({tried}): {source}")]
    Spawn {
        tried: String,
        #[source]
        source: std::io::Error,
    },

    /// The pseudo-terminal could not be allocated.
    #[error("could not allocate a pseudo-terminal: {0}")]
    Allocate(String),

    /// A read or write on the pseudo-terminal failed.
    #[error("pseudo-terminal io: {0}")]
    Io(#[from] std::io::Error),

    /// This platform has no pseudo-terminal support in Esper yet.
    ///
    /// A distinct variant rather than an `Allocate` with a sad string,
    /// because the two mean different things to whoever reads it: one is a
    /// machine that has run out of something, the other is a feature that has
    /// not been built for this platform. Only the second is answered with a
    /// refusal that says "not here" rather than "try again".
    #[error("this platform cannot allocate a pseudo-terminal yet")]
    Unsupported,
}

/// A shell running on a pseudo-terminal, split for concurrent read and write.
///
/// Split because a terminal is bidirectional and both directions are live at
/// once: a user typing while `cat` floods the screen is the normal case, not an
/// edge one.
pub struct Terminal {
    read: ReadHalf,
    write: WriteHalf,
    child: tokio::process::Child,
    shell: OsString,
}

impl std::fmt::Debug for Terminal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Terminal")
            .field("shell", &self.shell)
            .field("pid", &self.child.id())
            .finish()
    }
}

impl Terminal {
    /// Allocate a pseudo-terminal and start a login shell on it.
    ///
    /// The child gets a *screened* environment rather than the parent's:
    /// whatever daemon launched the host may hold anything in its environment,
    /// and passing that wholesale into an interactive shell leaks it to whoever
    /// is on the far side. What is passed, and the shell that is chosen, are
    /// platform policy and live in the platform module.
    pub fn open(request: &TerminalOpen) -> Result<Self, PtyError> {
        if !request.is_well_formed() {
            return Err(PtyError::Malformed);
        }

        let (read, write, child, shell) = sys::open(request)?;
        Ok(Self {
            read,
            write,
            child,
            shell,
        })
    }

    /// The shell that was started.
    #[must_use]
    pub fn shell(&self) -> &OsString {
        &self.shell
    }

    /// The child process id, for the audit log.
    #[must_use]
    pub fn pid(&self) -> Option<u32> {
        self.child.id()
    }

    /// Split into the parts needed to pump both directions concurrently.
    #[must_use]
    pub fn into_parts(self) -> (ReadHalf, WriteHalf, tokio::process::Child) {
        (self.read, self.write, self.child)
    }

    /// Apply a window size change.
    ///
    /// Out-of-bounds values are refused rather than clamped: a resize is
    /// something the controller asserts about its own window, and silently
    /// substituting a different size would leave the two ends disagreeing about
    /// where lines wrap — which looks like a rendering bug for the rest of the
    /// session.
    pub fn resize(write: &WriteHalf, r: &TerminalResize) -> Result<(), PtyError> {
        if !r.is_well_formed() {
            return Err(PtyError::Malformed);
        }
        sys::resize(write, r.rows, r.cols)
    }
}

/// Shut a terminal session down without leaving anything behind.
///
/// # Why signalling the shell is not enough
///
/// The obvious implementation kills the shell, or signals the shell's process
/// group. Both leave orphans, and the second one surprisingly so.
///
/// A shell on a pseudo-terminal enables **job control**, and job control means
/// each background job gets *its own process group*. So `sleep 3000 &` is not
/// in the shell's group, and signalling that group never reaches it. Measured:
/// the first version of this did exactly that, and the sleep survived.
///
/// # What actually works: close the master
///
/// A pseudo-terminal is the shell's **controlling terminal**. When the master
/// side is closed, the kernel does a real hangup: it sends `SIGHUP` to the
/// terminal's foreground process group and makes further reads return end of
/// file. The kernel knows which group is in the foreground; we do not.
///
/// This is the mechanism a terminal emulator uses when you close its window,
/// and the one `sshd` uses when a connection drops. It is not a trick — it is
/// what a hangup *is*.
///
/// So teardown is: **close the master, then signal, then kill.**
///
/// # What is deliberately *not* killed
///
/// A **backgrounded** job is left running, and that is a decision rather than a
/// limitation.
///
/// The kernel's hangup reaches the terminal's *foreground* process group. A job
/// the user put in the background with `&` is in its own group and does not
/// receive it — and neither mechanism here goes hunting for it.
///
/// That is deliberate because it is what every Unix user already expects. It is
/// precisely why `nohup` and `tmux` exist: you start a long build, disconnect,
/// and come back to it. `sleep 3000 &` surviving an ssh disconnect is not a bug
/// in ssh. A terminal that hunted down and killed detached jobs would be *more*
/// aggressive than ssh and would break a workflow people rely on daily.
///
/// So "no orphans" means: no orphaned **shell**, no leaked pseudo-terminal, and
/// nothing left in the terminal's foreground group. It does not mean reaching
/// into a process tree the user deliberately detached.
/// `EIO`. Linux reports the end of a terminal session with it rather than EOF.
const EIO: i32 = 5;

/// Whether a read of the pseudo-terminal master means the session ended.
///
/// Reading a master whose slave has been closed by its last process returns
/// `EIO` on Linux, where BSD and macOS return 0. Both mean the same thing, and
/// a reader that only understands one of them works on one platform and
/// quietly loses the exit status on the other — which is most of what terminal
/// mode is for on a server.
///
/// So `EIO` here is not a failure. It is how a shell exit is delivered, and it
/// lives in this crate because it is a fact about pseudo-terminals rather than
/// about any one caller. Two callers already need it: the session pump and the
/// keeper that drains a session nobody is attached to.
#[must_use]
pub fn is_session_end(read: &std::io::Result<usize>) -> bool {
    match read {
        Ok(0) => true,
        // Uncategorized on Linux; the raw code is what identifies it.
        Err(e) => e.raw_os_error() == Some(EIO),
        Ok(_) => false,
    }
}

/// How long a session gets to respond to a hangup before `SIGKILL`.
///
/// Short, because a user who closed their terminal is waiting, but not zero:
/// most shells exit within milliseconds and the grace only costs anything for
/// something that is ignoring it.
pub const HANGUP_GRACE: std::time::Duration = std::time::Duration::from_millis(500);

/// Hang up on the session, then kill anything that survived.
///
/// Takes the pseudo-terminal halves **by value** so it can close the master.
/// That is not an API inconvenience, it is the point: a caller still holding
/// the master has not hung up, whatever else it signalled.
///
/// Returns the shell's exit status where one is available. Safe to call on a
/// session that has already ended.
pub async fn hang_up(
    mut child: tokio::process::Child,
    read: ReadHalf,
    write: WriteHalf,
) -> Option<TerminalExit> {
    // Already gone? Report it and stop.
    if let Ok(Some(status)) = child.try_wait() {
        drop(read);
        drop(write);
        return Some(exit_status(&status));
    }

    // **Close the master.** The kernel hangs up the terminal, which reaches
    // the foreground process group — including jobs in their own groups,
    // which a process-group signal from here would miss.
    drop(read);
    drop(write);

    if let Ok(Ok(status)) = tokio::time::timeout(HANGUP_GRACE, child.wait()).await {
        return Some(exit_status(&status));
    }

    // Something survived the hangup — it detached from the terminal, or is
    // ignoring SIGHUP. Signal the group we do know about.
    let Some(pid) = child.id() else {
        return child.try_wait().ok().flatten().map(|s| exit_status(&s));
    };
    tracing::debug!(pid, "session survived the hangup; signalling its group");
    sys::signal_group(pid, Signal::Hangup);

    if let Ok(Ok(status)) = tokio::time::timeout(HANGUP_GRACE, child.wait()).await {
        return Some(exit_status(&status));
    }

    tracing::debug!(pid, "session ignored SIGHUP; sending SIGKILL");
    sys::signal_group(pid, Signal::Kill);

    // Bounded even after SIGKILL: an unreapable child must not hold a task
    // open forever.
    match tokio::time::timeout(HANGUP_GRACE, child.wait()).await {
        Ok(Ok(status)) => Some(exit_status(&status)),
        _ => None,
    }
}

/// Which signal to send.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Signal {
    /// The terminal hung up. Shells exit; programs save and clean up.
    Hangup,
    /// Unignorable.
    Kill,
}

/// Translate a process exit into the wire message.
///
/// A controller that always reported success would be useless in a script, and
/// scripting a server is most of what terminal mode is for.
#[must_use]
pub fn exit_status(status: &std::process::ExitStatus) -> TerminalExit {
    TerminalExit {
        code: status.code(),
        signal: sys::exit_signal(status),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // This crate deliberately has no libc dependency, so the errno values the
    // tests need are named here rather than imported.
    const EINTR: i32 = 4;
    const EBADF: i32 = 9;
    const EAGAIN: i32 = 11;

    /// The distinction that cost every Linux session its exit status.
    #[test]
    fn a_read_of_zero_and_an_eio_both_mean_the_session_ended() {
        assert!(is_session_end(&Ok(0)), "BSD and macOS report EOF");
        assert!(
            is_session_end(&Err(std::io::Error::from_raw_os_error(EIO))),
            "Linux reports EIO, and treating it as a failure loses the exit status"
        );
    }

    #[test]
    fn real_output_and_real_failures_are_not_the_session_ending() {
        assert!(!is_session_end(&Ok(1)));
        assert!(!is_session_end(&Ok(4096)));
        for code in [EINTR, EAGAIN, EBADF] {
            assert!(
                !is_session_end(&Err(std::io::Error::from_raw_os_error(code))),
                "errno {code} is not a session ending"
            );
        }
    }
}
