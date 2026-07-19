//! Terminal mode on a platform with no pseudo-terminal support yet.
//!
//! The crate compiles here so that the rest of the host does: a session
//! dispatcher that exists only on Unix would mean a Windows host with no
//! terminal code at all, and the peer would learn that by being disconnected
//! instead of being told. [`open`] fails with a reason, and the layer above
//! turns that into a refusal the controller can print.
//!
//! Windows has ConPTY (`CreatePseudoConsole`), which is the implementation
//! this arm is shaped to be replaced by.

use std::ffi::OsString;
use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};

use esper_proto::TerminalOpen;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

use crate::{PtyError, Signal};

/// The read half of a pseudo-terminal master.
///
/// Never constructed on this platform: [`open`] is the only thing that makes
/// one and it always fails. The trait implementations exist because callers
/// name the type in signatures, not because anything reaches them.
#[derive(Debug)]
pub struct ReadHalf;

/// The write half of a pseudo-terminal master.
#[derive(Debug)]
pub struct WriteHalf;

// Dropping the halves is not bookkeeping, it is the hangup: closing the master
// is what makes the kernel signal the terminal's foreground process group.
// These arms have nothing to close, but the contract that dropping them means
// something has to hold on every platform — a caller reads `drop(read)` in
// `hang_up` as the hangup, and that must not become a no-op that reads like a
// mistake.
impl Drop for ReadHalf {
    fn drop(&mut self) {}
}

impl Drop for WriteHalf {
    fn drop(&mut self) {}
}

impl AsyncRead for ReadHalf {
    fn poll_read(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        _buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        // End of file: nothing will ever arrive.
        Poll::Ready(Ok(()))
    }
}

impl AsyncWrite for WriteHalf {
    fn poll_write(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        _buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Poll::Ready(Err(io::Error::from(io::ErrorKind::Unsupported)))
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

/// Refuse, naming the platform rather than reporting a generic failure.
pub fn open(
    _request: &TerminalOpen,
) -> Result<(ReadHalf, WriteHalf, tokio::process::Child, OsString), PtyError> {
    Err(PtyError::Unsupported)
}

/// Refuse a resize, for the same reason there is nothing to resize.
pub fn resize(_write: &WriteHalf, _rows: u16, _cols: u16) -> Result<(), PtyError> {
    Err(PtyError::Unsupported)
}

/// No signals: this platform reports an exit only as a code.
pub fn exit_signal(_status: &std::process::ExitStatus) -> Option<i32> {
    None
}

/// Nothing to signal.
///
/// Windows has job objects rather than process groups, so the ConPTY
/// implementation will not reach for a process-group signal here either.
pub fn signal_group(_leader_pid: u32, _signal: Signal) {}
