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

use std::ffi::OsString;

use ownview_proto::{TerminalExit, TerminalOpen, TerminalResize};
use pty_process::{Command, OwnedReadPty, OwnedWritePty, Size};

/// The read and write halves, re-exported.
///
/// Callers name these without depending on `pty-process` directly: which crate
/// owns the pseudo-terminal is this crate's decision, and leaking it into every
/// caller's `Cargo.toml` would make it everyone's.
pub use pty_process::{OwnedReadPty as ReadHalf, OwnedWritePty as WriteHalf};

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
}

/// A shell running on a pseudo-terminal, split for concurrent read and write.
///
/// Split because a terminal is bidirectional and both directions are live at
/// once: a user typing while `cat` floods the screen is the normal case, not an
/// edge one.
pub struct Terminal {
    read: OwnedReadPty,
    write: OwnedWritePty,
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

/// Choose the shell to run.
///
/// `$SHELL` if set, then `/bin/sh`, which POSIX requires to exist. Deliberately
/// **not** a configured list: a remote-execution surface that lets the caller
/// name an arbitrary binary is a different feature from a terminal, and it is
/// one OwnView's design documents refuse.
fn shell_to_run() -> OsString {
    std::env::var_os("SHELL").unwrap_or_else(|| OsString::from("/bin/sh"))
}

impl Terminal {
    /// Allocate a pseudo-terminal and start a login shell on it.
    ///
    /// The child gets a *screened* environment rather than the parent's:
    /// whatever daemon launched the host may hold anything in its environment,
    /// and passing that wholesale into an interactive shell leaks it to whoever
    /// is on the far side.
    pub fn open(request: &TerminalOpen) -> Result<Self, PtyError> {
        if !request.is_well_formed() {
            return Err(PtyError::Malformed);
        }

        let (pty, pts) = pty_process::open().map_err(|e| PtyError::Allocate(e.to_string()))?;
        pty.resize(Size::new(request.rows, request.cols))
            .map_err(|e| PtyError::Allocate(e.to_string()))?;

        let shell = shell_to_run();

        // A consuming builder, so the calls chain.
        //
        // `-l` for a login shell: the user gets their own profile, which is
        // what they would get sitting at the machine.
        //
        // `env_clear` first, then only what an interactive shell needs. The
        // alternative — inheriting — hands the far side whatever the
        // supervising daemon happened to be holding in its environment, and a
        // daemon's environment is not something a user chose to share.
        let mut cmd = Command::new(&shell)
            .arg("-l")
            .env_clear()
            .env("TERM", &request.term);

        for pass in ["HOME", "USER", "LOGNAME", "PATH", "LANG", "SHELL", "TZ"] {
            if let Some(v) = std::env::var_os(pass) {
                cmd = cmd.env(pass, v);
            }
        }
        // A shell with no PATH is nearly unusable, and a daemon environment may
        // legitimately lack one.
        if std::env::var_os("PATH").is_none() {
            cmd = cmd.env("PATH", "/usr/local/bin:/usr/bin:/bin");
        }

        let child = cmd.spawn(pts).map_err(|e| PtyError::Spawn {
            tried: shell.to_string_lossy().into_owned(),
            source: std::io::Error::other(e.to_string()),
        })?;

        let (read, write) = pty.into_split();
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
    pub fn into_parts(self) -> (OwnedReadPty, OwnedWritePty, tokio::process::Child) {
        (self.read, self.write, self.child)
    }

    /// Apply a window size change.
    ///
    /// Out-of-bounds values are refused rather than clamped: a resize is
    /// something the controller asserts about its own window, and silently
    /// substituting a different size would leave the two ends disagreeing about
    /// where lines wrap — which looks like a rendering bug for the rest of the
    /// session.
    pub fn resize(write: &OwnedWritePty, r: &TerminalResize) -> Result<(), PtyError> {
        if !r.is_well_formed() {
            return Err(PtyError::Malformed);
        }
        write
            .resize(Size::new(r.rows, r.cols))
            .map_err(|e| PtyError::Allocate(e.to_string()))
    }
}

/// Translate a process exit into the wire message.
///
/// A controller that always reported success would be useless in a script, and
/// scripting a server is most of what terminal mode is for.
#[must_use]
pub fn exit_status(status: &std::process::ExitStatus) -> TerminalExit {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        TerminalExit {
            code: status.code(),
            signal: status.signal(),
        }
    }
    #[cfg(not(unix))]
    {
        TerminalExit {
            code: status.code(),
            signal: None,
        }
    }
}
