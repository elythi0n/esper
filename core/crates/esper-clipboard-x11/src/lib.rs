//! The clipboard on X11.
//!
//! # X11 has no clipboard
//!
//! It has *selections*: a name, and a client that currently answers for it.
//! Nothing is stored anywhere. When a program copies, it tells the server "I
//! am the owner of `CLIPBOARD` now", and when another program pastes, the
//! server forwards the request to that owner, which sends the bytes over.
//!
//! Three things follow from that, and they are the whole shape of this crate:
//!
//! - **Owning a selection is a promise to keep answering.** A client that
//!   takes ownership and then stops listening leaves every paste on the
//!   machine hanging. So a thread of our own holds a connection and a window
//!   and answers, for as long as this process holds anything.
//! - **Reading is a conversation, not a read.** Ask, wait, and hope the owner
//!   answers — it might be busy, stopped under a debugger, or gone. Every read
//!   here has a deadline, and a clipboard that will not answer is reported as
//!   that rather than waited on forever.
//! - **Anything large arrives in pieces.** A property has a size limit, so
//!   both ends of a large transfer speak `INCR`: a header saying how much is
//!   coming, then a chunk each time the other side takes the last one. Both
//!   directions are implemented, because a screenshot is megabytes and the
//!   common case for images is the one that needs it.
//!
//! # What tells us the clipboard changed
//!
//! XFIXES, which sends an event when a selection changes hands. The
//! alternative is asking the server who owns it several times a second, which
//! is what programs did before XFIXES existed and is still what many do.
//!
//! # Correctness is not this crate's to define
//!
//! What may be read, what may be written, what is capped and what is wiped
//! all belong to `esper-clipboard`, in one place, so that this backend and the
//! two that will follow it cannot disagree. What is here is X11 and nothing
//! else — and it is finished when it passes
//! [`esper_clipboard::conformance`].

// The engine only uses the X11 backends on Linux. Elsewhere this crate
// compiles to nothing rather than being absent, so a workspace build for
// another target still covers every member.
#![cfg(target_os = "linux")]

mod worker;

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::time::Duration;

use esper_clipboard::{Clipboard, ClipboardError};
use esper_proto::ClipboardKind;

/// The backend name reported in logs.
pub const BACKEND: &str = "x11-selection";

/// How long a caller waits for the thread to answer.
///
/// Longer than any deadline inside the thread, so a timeout here means the
/// thread itself is wedged — not that some other program was slow to answer,
/// which the thread reports properly on its own.
const ANSWER: Duration = Duration::from_secs(20);

/// What the thread is asked to do.
enum Job {
    Read(
        ClipboardKind,
        mpsc::Sender<Result<Option<Vec<u8>>, ClipboardError>>,
    ),
    Write(
        ClipboardKind,
        Vec<u8>,
        mpsc::Sender<Result<(), ClipboardError>>,
    ),
    Clear(mpsc::Sender<Result<(), ClipboardError>>),
}

/// This machine's clipboard, on X11.
///
/// Holds a thread with a connection of its own. Dropping it gives up whatever
/// this process was answering for, so a session that ends stops being the
/// owner of anything.
pub struct X11Clipboard {
    jobs: mpsc::Sender<Job>,
    generation: Arc<AtomicU64>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl X11Clipboard {
    /// Connect to the display named by the environment.
    ///
    /// # Errors
    ///
    /// If there is no display, or the server refuses the connection, or it has
    /// no XFIXES — which every X server built this century has.
    pub fn open() -> Result<Self, ClipboardError> {
        let generation = Arc::new(AtomicU64::new(1));
        let (jobs, inbox) = mpsc::channel();
        let (ready, started) = mpsc::channel();
        let shared = Arc::clone(&generation);
        let thread = std::thread::Builder::new()
            .name("esper-clipboard-x11".to_owned())
            .spawn(move || worker::run(&inbox, &ready, &shared))
            .map_err(|e| {
                ClipboardError::Failed(format!("could not start a clipboard thread: {e}"))
            })?;

        // The thread says whether it got as far as a window before anything is
        // asked of it, so a machine with no display fails at `open` rather than
        // at the first paste.
        match started.recv_timeout(ANSWER) {
            Ok(Ok(())) => Ok(Self {
                jobs,
                generation,
                thread: Some(thread),
            }),
            Ok(Err(e)) => Err(e),
            Err(_) => Err(ClipboardError::Unavailable(
                "the clipboard thread did not start".to_owned(),
            )),
        }
    }

    /// Send one job and wait for its answer.
    fn ask<T>(
        &self,
        make: impl FnOnce(mpsc::Sender<Result<T, ClipboardError>>) -> Job,
    ) -> Result<T, ClipboardError> {
        let (reply, answer) = mpsc::channel();
        self.jobs.send(make(reply)).map_err(|_| {
            ClipboardError::Unavailable("the clipboard thread has stopped".to_owned())
        })?;
        answer
            .recv_timeout(ANSWER)
            .unwrap_or_else(|_| Err(ClipboardError::NoAnswer))
    }
}

impl Clipboard for X11Clipboard {
    fn read(&mut self, kind: ClipboardKind) -> Result<Option<Vec<u8>>, ClipboardError> {
        self.ask(|reply| Job::Read(kind, reply))
    }

    fn write(&mut self, kind: ClipboardKind, content: &[u8]) -> Result<(), ClipboardError> {
        let content = content.to_vec();
        self.ask(move |reply| Job::Write(kind, content, reply))
    }

    fn generation(&mut self) -> u64 {
        self.generation.load(Ordering::Relaxed)
    }

    fn clear(&mut self) -> Result<(), ClipboardError> {
        self.ask(Job::Clear)
    }
}

impl Drop for X11Clipboard {
    fn drop(&mut self) {
        // Dropping the sender is what ends the thread's loop: it needs no
        // separate message, and a thread that has already died needs no
        // message at all.
        let (jobs, _) = mpsc::channel();
        drop(std::mem::replace(&mut self.jobs, jobs));
        if let Some(thread) = self.thread.take() {
            // Joined rather than detached, because the thread gives up the
            // selection on its way out and a process that exits first would
            // leave a paste half-answered.
            if thread.join().is_err() {
                tracing::warn!("the clipboard thread ended badly");
            }
        }
    }
}

impl std::fmt::Debug for X11Clipboard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("X11Clipboard")
            .field("generation", &self.generation.load(Ordering::Relaxed))
            .finish_non_exhaustive()
    }
}
