//! Pseudo-terminals on Unix.
//!
//! Everything here is a fact about Unix rather than about terminal mode: the
//! `fork`/`exec` pair, the login shell, the environment an interactive shell
//! expects, and signalling a process group. The platform-independent half of
//! the crate is in `lib.rs` and never mentions any of it.

use std::ffi::OsString;

use esper_proto::TerminalOpen;
use pty_process::{Command, Size};

use crate::{PtyError, Signal};

/// The read and write halves of the pseudo-terminal master.
pub use pty_process::{OwnedReadPty as ReadHalf, OwnedWritePty as WriteHalf};

/// Choose the shell to run.
///
/// `$SHELL` if set, then `/bin/sh`, which POSIX requires to exist. Deliberately
/// **not** a configured list: a remote-execution surface that lets the caller
/// name an arbitrary binary is a different feature from a terminal, and it is
/// one Esper's design documents refuse.
fn shell_to_run() -> OsString {
    std::env::var_os("SHELL").unwrap_or_else(|| OsString::from("/bin/sh"))
}

/// Allocate a pseudo-terminal and start a login shell on it.
pub fn open(
    request: &TerminalOpen,
) -> Result<(ReadHalf, WriteHalf, tokio::process::Child, OsString), PtyError> {
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
    Ok((read, write, child, shell))
}

/// Tell the pseudo-terminal how big the window is now.
pub fn resize(write: &WriteHalf, rows: u16, cols: u16) -> Result<(), PtyError> {
    write
        .resize(Size::new(rows, cols))
        .map_err(|e| PtyError::Allocate(e.to_string()))
}

/// The signal that killed a process, where one did.
pub fn exit_signal(status: &std::process::ExitStatus) -> Option<i32> {
    use std::os::unix::process::ExitStatusExt as _;
    status.signal()
}

/// Close the master, and say which process group the terminal was giving its
/// input to.
///
/// **One operation, because the order is the whole point.** Closing the master
/// is what makes the kernel hang up the terminal; the foreground group can only
/// be read while the terminal still exists. Two functions would be two chances
/// to do them the wrong way round, and the wrong way round reads nothing.
///
/// That group is the only record of who the hangup was aimed at. With job
/// control the shell puts each job in a group of its own and makes it the
/// foreground one, so it is usually **not** the shell's group — and a caller
/// that has to chase a survivor has nothing else to go on.
///
/// `None` when there is no foreground group, which is what a session that has
/// already ended looks like, and when the two halves did not come from one
/// terminal — impossible from inside this crate, and not worth a panic.
pub fn close_master(read: ReadHalf, write: WriteHalf) -> Option<u32> {
    let Ok(pty) = read.unsplit(write) else {
        return None;
    };
    let group = rustix::termios::tcgetpgrp(&pty)
        .ok()
        .and_then(|pid| u32::try_from(pid.as_raw_nonzero().get()).ok());
    // The hangup itself.
    drop(pty);
    group
}

/// Whether `pid` is still part of the session `leader` leads.
///
/// The check that keeps a last-resort `SIGKILL` aimed at our own session: a
/// process group id is reused eventually, and a signal sent to a stranger's
/// group on the strength of a number read minutes ago would be this product
/// killing processes it has nothing to do with.
///
/// `false` when the process is gone, which is the ordinary case by the time
/// anything asks.
pub fn in_session(pid: u32, leader: u32) -> bool {
    let Ok(raw) = i32::try_from(pid) else {
        return false;
    };
    let Some(pid) = rustix::process::Pid::from_raw(raw) else {
        return false;
    };
    let Ok(sid) = rustix::process::getsid(Some(pid)) else {
        return false;
    };
    // The shell was made a session leader by `pty-process`, so the session id
    // is its pid.
    u32::try_from(sid.as_raw_nonzero().get()).is_ok_and(|sid| sid == leader)
}

/// Signal an entire process group.
///
/// Best-effort by design: the group may already be gone, which is the common
/// case and not an error. A failure here must never prevent the rest of
/// teardown, because the alternative to "signal failed, continue" is "signal
/// failed, leak the session".
pub fn signal_group(leader_pid: u32, signal: Signal) {
    use rustix::process::{Pid, Signal as RSignal};

    let Ok(raw) = i32::try_from(leader_pid) else {
        return;
    };
    let Some(pid) = Pid::from_raw(raw) else {
        return;
    };
    let sig = match signal {
        Signal::Hangup => RSignal::HUP,
        Signal::Kill => RSignal::KILL,
    };

    // The shell was made a session leader by `pty-process`, so its pid is also
    // its process group id — which is what makes signalling the group possible
    // from here at all.
    if let Err(e) = rustix::process::kill_process_group(pid, sig) {
        // ESRCH means the group is already gone: the normal outcome.
        if e != rustix::io::Errno::SRCH {
            tracing::debug!(pid = leader_pid, ?signal, error = %e, "could not signal process group");
        }
    }
}
