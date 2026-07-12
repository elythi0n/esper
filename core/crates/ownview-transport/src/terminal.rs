//! Running a terminal session over a connection.
//!
//! `ownview-pty` owns the pseudo-terminal; this pumps bytes between it and a
//! control stream, in both directions at once.
//!
//! # Why both directions must be concurrent
//!
//! A user typing while `cat` floods the screen is the normal case, not an edge
//! one. A loop that read from the network, then wrote to the pty, then read
//! from the pty, then wrote to the network would deadlock the first time the
//! shell produced more output than a buffer holds while the user was mid-word.
//!
//! # The detach escape
//!
//! `~.` at the start of a line detaches without killing the shell — the same
//! sequence `ssh` uses, deliberately, because a user who knows one knows the
//! other. It is recognised **only** at a line start, so a `~` inside text
//! passes through untouched, and `~~` sends a literal `~`.
//!
//! The alternative to having an escape is that a session with a wedged remote
//! can only be left by killing the client, which loses the shell too.

use std::time::Instant;

use ownview_audit::{Entry, Event, Log, terminal_closed};
use ownview_crypto::identity::KEY_LEN;
use ownview_proto::{
    Permission, TerminalData, TerminalExit, TerminalMessage, TerminalOpen, TerminalResize,
};
use ownview_pty::{ReadHalf, Terminal, WriteHalf, hang_up};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

use crate::{Control, ControlError};

/// How much to read from the pseudo-terminal at once.
///
/// Matches the wire chunk cap, so a full read becomes exactly one message
/// rather than being split — a split write costs a second frame header and,
/// worse, can put a partial UTF-8 sequence on the wire, which a controller
/// rendering incrementally shows as a replacement character that then never
/// resolves.
const READ_CHUNK: usize = ownview_proto::MAX_TERMINAL_CHUNK;

/// What can go wrong running a terminal session.
#[derive(Debug, thiserror::Error)]
pub enum TerminalError {
    /// The peer does not hold [`Permission::Terminal`].
    ///
    /// A separate permission from view and control, and refused separately: a
    /// shell is unrestricted execution as the host's user, not a lesser form of
    /// looking at a screen.
    #[error("this device is not permitted to open a terminal on this machine")]
    NotPermitted,

    /// The request was outside its bounds.
    #[error("the terminal request was malformed")]
    Malformed,

    /// The pseudo-terminal or shell failed.
    #[error(transparent)]
    Pty(#[from] ownview_pty::PtyError),

    /// The control stream failed.
    #[error("control stream: {0}")]
    Control(String),
}

impl From<ControlError> for TerminalError {
    fn from(e: ControlError) -> Self {
        Self::Control(e.to_string())
    }
}

/// How a terminal session ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ended {
    /// The shell exited.
    ShellExited(TerminalExit),
    /// The controller detached, leaving the shell running.
    ///
    /// Distinct from the shell exiting, because the two mean different things
    /// to whoever reads the log: one is a user finishing, the other is a user
    /// stepping away from something still running.
    Detached,
    /// The connection went away.
    Disconnected,
}

/// Run a terminal session as the **host**.
///
/// Checks the permission, starts a shell, and pumps until one end stops.
/// Records the session's start and end in the audit log — never its content.
pub async fn host(
    control: &mut Control,
    request: &TerminalOpen,
    peer_key: &[u8; KEY_LEN],
    granted: &[Permission],
    log: &Log,
) -> Result<Ended, TerminalError> {
    if !granted.contains(&Permission::Terminal) {
        // Recorded: a refusal is as interesting as an acceptance, and a run of
        // them is what an attack looks like from this side.
        let _ = log.append(&Entry::now(
            peer_key,
            Event::Refused {
                reason: "terminal not permitted".to_owned(),
            },
        ));
        return Err(TerminalError::NotPermitted);
    }
    if !request.is_well_formed() {
        return Err(TerminalError::Malformed);
    }

    let terminal = Terminal::open(request)?;
    let started = Instant::now();
    // Best-effort: an audit failure must not kill a session. A full disk should
    // be a warning, not an outage.
    if let Err(e) = log.append(&Entry::now(peer_key, Event::TerminalOpened)) {
        tracing::warn!(error = %e, "could not record the start of a terminal session");
    }

    let (pty_read, pty_write, child) = terminal.into_parts();
    let outcome = pump_host(control, pty_read, pty_write, child).await;

    let (ended, exit) = match &outcome {
        Ok((e, x)) => (e.clone(), *x),
        Err(_) => (Ended::Disconnected, None),
    };
    let secs = started.elapsed().as_secs();
    if let Err(e) = log.append(&Entry::now(peer_key, terminal_closed(secs, exit.as_ref()))) {
        tracing::warn!(error = %e, "could not record the end of a terminal session");
    }

    outcome.map(|_| ended)
}

/// Pump both directions until one end stops.
async fn pump_host(
    control: &mut Control,
    mut pty_read: ReadHalf,
    mut pty_write: WriteHalf,
    child: tokio::process::Child,
) -> Result<(Ended, Option<TerminalExit>), TerminalError> {
    let mut buf = vec![0u8; READ_CHUNK];

    loop {
        tokio::select! {
            // Shell output -> controller.
            read = pty_read.read(&mut buf) => match read {
                // The session ended. **On Linux that is `EIO`, not end of
                // file.**
                //
                // Reading a pseudo-terminal master whose slave has been closed
                // by its last process returns `EIO` on Linux, where BSD and
                // macOS return 0. Treating only `Ok(0)` as the end means the
                // `Err` arm fires instead and every session on Linux reports
                // `Disconnected` — losing the shell's exit status, which is
                // most of what terminal mode is for on a server.
                //
                // So `EIO` here is not an error. It is the normal way a shell
                // exit is delivered, and handling both keeps the two platform
                // behaviours in one branch rather than one working and one
                // silently wrong.
                ref r if is_session_end(r) => {
                    let exit = hang_up(child, pty_read, pty_write).await;
                    match exit {
                        Some(x) => {
                            let _ = control.send(&TerminalMessage::Exit(x)).await;
                            let _ = control.finish();
                            return Ok((Ended::ShellExited(x), Some(x)));
                        }
                        None => return Ok((Ended::Disconnected, None)),
                    }
                }
                Ok(n) => {
                    let bytes = buf.get(..n).unwrap_or(&[]).to_vec();
                    control
                        .send(&TerminalMessage::Data(TerminalData { bytes }))
                        .await?;
                }
                Err(e) => {
                    tracing::debug!(error = %e, "pty read failed");
                    let exit = hang_up(child, pty_read, pty_write).await;
                    return Ok((Ended::Disconnected, exit));
                }
            },

            // Controller -> shell, or a resize.
            msg = control.recv::<TerminalMessage>() => match msg {
                Ok(TerminalMessage::Data(d)) => {
                    if !d.is_well_formed() {
                        let exit = hang_up(child, pty_read, pty_write).await;
                        return Ok((Ended::Disconnected, exit));
                    }
                    if let Err(e) = pty_write.write_all(&d.bytes).await {
                        tracing::debug!(error = %e, "pty write failed");
                        let exit = hang_up(child, pty_read, pty_write).await;
                        return Ok((Ended::Disconnected, exit));
                    }
                }
                Ok(TerminalMessage::Resize(r)) => {
                    // A malformed resize is ignored rather than fatal: the
                    // session is otherwise fine and killing it over a bad
                    // window size would be a worse outcome than the wrong wrap.
                    if let Err(e) = Terminal::resize(&pty_write, &r) {
                        tracing::debug!(error = %e, cols = r.cols, rows = r.rows, "ignoring a bad resize");
                    }
                }
                Ok(TerminalMessage::Exit(_)) => {
                    // Host-to-controller only. A controller sending it is
                    // confused or probing; either way the session ends.
                    let exit = hang_up(child, pty_read, pty_write).await;
                    return Ok((Ended::Disconnected, exit));
                }
                Err(ControlError::Finished) => {
                    // The controller detached cleanly.
                    let exit = hang_up(child, pty_read, pty_write).await;
                    return Ok((Ended::Detached, exit));
                }
                Err(e) => {
                    tracing::debug!(error = %e, "terminal control stream failed");
                    let exit = hang_up(child, pty_read, pty_write).await;
                    return Ok((Ended::Disconnected, exit));
                }
            },
        }
    }
}

/// Whether a pseudo-terminal read result means the session ended.
///
/// `EIO` on a master whose slave has closed is Linux's way of saying the last
/// process on the terminal exited; BSD and macOS return 0 instead. Both mean
/// the same thing, and a reader that only understands one of them works on one
/// platform and quietly loses the exit status on the other.
fn is_session_end(read: &std::io::Result<usize>) -> bool {
    match read {
        Ok(0) => true,
        Err(e) => {
            // Uncategorized on Linux; the raw code is what identifies it.
            e.raw_os_error() == Some(5) // EIO
        }
        Ok(_) => false,
    }
}

/// What the controller's input loop decided to do with a chunk of keystrokes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Typed {
    /// Send these bytes to the remote.
    Send(Vec<u8>),
    /// The user asked to detach.
    Detach,
}

/// Recognises the `~.` detach escape in a stream of keystrokes.
///
/// Stateful because the sequence spans reads: a user types `~`, the client
/// reads it, and `.` arrives in the next read. A stateless check would only
/// ever see one of the two.
///
/// Follows `ssh`'s rules, which exist for good reasons rather than tradition:
///
/// - Recognised **only at the start of a line**, so `~` in text is untouched.
/// - `~~` sends one literal `~`, which is how you type a tilde at a line start.
/// - `~` followed by anything else sends both, so a mistyped escape is not
///   silently eaten.
#[derive(Debug)]
pub struct EscapeWatcher {
    /// Whether the next byte begins a line. True initially: a session that
    /// opens with `~.` should detach.
    at_line_start: bool,
    /// Whether a `~` at a line start is pending.
    tilde_pending: bool,
}

impl Default for EscapeWatcher {
    /// Written rather than derived.
    ///
    /// `#[derive(Default)]` gives `at_line_start: false`, which disagrees with
    /// [`EscapeWatcher::new`] — so a caller using `default()` would get a
    /// watcher that misses an escape typed on the very first line, and would
    /// have no way of knowing. Two constructors for one type that differ in
    /// behaviour is a footgun; found by a test that asserted they agree.
    fn default() -> Self {
        Self::new()
    }
}

impl EscapeWatcher {
    /// A watcher positioned at the start of a line.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            at_line_start: true,
            tilde_pending: false,
        }
    }

    /// Feed keystrokes, and get back what to send or a detach request.
    ///
    /// Anything typed *before* a detach in the same chunk is returned to be
    /// sent first, so a user who types `ls\n~.` does not lose the `ls`.
    pub fn feed(&mut self, input: &[u8]) -> Typed {
        let mut out = Vec::with_capacity(input.len());

        for &b in input {
            if self.tilde_pending {
                self.tilde_pending = false;
                match b {
                    b'.' => return Typed::Detach,
                    // `~~` is a literal tilde.
                    b'~' => {
                        out.push(b'~');
                        self.at_line_start = false;
                        continue;
                    }
                    // Anything else: send both, so a mistake is visible rather
                    // than swallowed.
                    other => {
                        out.push(b'~');
                        out.push(other);
                        self.at_line_start = matches!(other, b'\r' | b'\n');
                        continue;
                    }
                }
            }

            if b == b'~' && self.at_line_start {
                // Hold it: the next byte decides.
                self.tilde_pending = true;
                continue;
            }

            out.push(b);
            self.at_line_start = matches!(b, b'\r' | b'\n');
        }

        Typed::Send(out)
    }

    /// Whether a `~` is being held pending the next byte.
    ///
    /// A client flushing before it exits must not lose it.
    #[must_use]
    pub const fn has_pending_tilde(&self) -> bool {
        self.tilde_pending
    }

    /// Take any held `~` back, for flushing at shutdown.
    pub fn take_pending(&mut self) -> Option<u8> {
        if self.tilde_pending {
            self.tilde_pending = false;
            Some(b'~')
        } else {
            None
        }
    }
}

/// Send a resize to the host.
pub async fn send_resize(control: &mut Control, cols: u16, rows: u16) -> Result<(), TerminalError> {
    let r = TerminalResize { cols, rows };
    if !r.is_well_formed() {
        return Err(TerminalError::Malformed);
    }
    control
        .send(&TerminalMessage::Resize(r))
        .await
        .map_err(TerminalError::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Feed a whole string and expect exactly these bytes to be sent.
    fn sends(w: &mut EscapeWatcher, input: &str) -> String {
        match w.feed(input.as_bytes()) {
            Typed::Send(b) => String::from_utf8_lossy(&b).into_owned(),
            Typed::Detach => "<DETACH>".to_owned(),
        }
    }

    #[test]
    fn ordinary_typing_passes_through_untouched() {
        let mut w = EscapeWatcher::new();
        assert_eq!(sends(&mut w, "ls -la\n"), "ls -la\n");
        assert_eq!(sends(&mut w, "echo hello\n"), "echo hello\n");
    }

    #[test]
    fn tilde_dot_at_a_line_start_detaches() {
        let mut w = EscapeWatcher::new();
        assert_eq!(w.feed(b"~."), Typed::Detach);
    }

    /// The sequence spans reads: a user types `~`, the client reads it, and `.`
    /// arrives in the next read. A stateless check would only ever see one.
    #[test]
    fn the_escape_is_recognised_across_separate_reads() {
        let mut w = EscapeWatcher::new();
        assert_eq!(w.feed(b"~"), Typed::Send(Vec::new()), "the tilde is held");
        assert!(w.has_pending_tilde());
        assert_eq!(w.feed(b"."), Typed::Detach);
    }

    /// The most important negative case. A tilde inside text — a path like
    /// `~/notes`, or prose — must reach the remote unchanged.
    #[test]
    fn a_tilde_inside_a_line_is_not_an_escape() {
        let mut w = EscapeWatcher::new();
        assert_eq!(sends(&mut w, "cat "), "cat ");
        assert_eq!(
            sends(&mut w, "~/notes.txt\n"),
            "~/notes.txt\n",
            "a home-directory path must not be eaten"
        );
    }

    #[test]
    fn a_tilde_mid_line_followed_by_a_dot_is_not_an_escape() {
        let mut w = EscapeWatcher::new();
        assert_eq!(
            sends(&mut w, "echo ~.\n"),
            "echo ~.\n",
            "only a line-start tilde is an escape"
        );
    }

    /// How you type a literal tilde at a line start.
    #[test]
    fn a_doubled_tilde_sends_one_literal_tilde() {
        let mut w = EscapeWatcher::new();
        assert_eq!(sends(&mut w, "~~"), "~");
        assert_eq!(sends(&mut w, "/notes\n"), "/notes\n");
    }

    /// A mistyped escape must be visible rather than swallowed.
    #[test]
    fn a_tilde_followed_by_anything_else_sends_both() {
        for follow in ["x", "?", " ", "1"] {
            let mut w = EscapeWatcher::new();
            assert_eq!(
                sends(&mut w, &format!("~{follow}")),
                format!("~{follow}"),
                "~{follow} should send both bytes"
            );
        }
    }

    /// After a newline the next byte is at a line start again.
    #[test]
    fn a_newline_re_arms_the_escape() {
        let mut w = EscapeWatcher::new();
        assert_eq!(sends(&mut w, "ls\n"), "ls\n");
        assert_eq!(w.feed(b"~."), Typed::Detach);
    }

    /// A terminal in raw mode sends carriage return for Enter, not newline.
    /// An escape recogniser that only knew `\n` would never re-arm.
    #[test]
    fn a_carriage_return_re_arms_the_escape_too() {
        let mut w = EscapeWatcher::new();
        assert_eq!(sends(&mut w, "ls\r"), "ls\r");
        assert_eq!(w.feed(b"~."), Typed::Detach);
    }

    /// Anything typed before the escape in the same chunk must still be sent,
    /// or a fast typist loses a command.
    #[test]
    fn text_before_an_escape_in_the_same_chunk_is_not_lost() {
        let mut w = EscapeWatcher::new();
        // The `ls\n` must go, and only then does the escape apply.
        let first = w.feed(b"ls\n");
        assert_eq!(first, Typed::Send(b"ls\n".to_vec()));
        assert_eq!(w.feed(b"~."), Typed::Detach);
    }

    /// A client shutting down must not silently eat a held tilde.
    #[test]
    fn a_held_tilde_can_be_reclaimed_at_shutdown() {
        let mut w = EscapeWatcher::new();
        let _ = w.feed(b"~");
        assert!(w.has_pending_tilde());
        assert_eq!(w.take_pending(), Some(b'~'));
        assert!(!w.has_pending_tilde());
        assert_eq!(
            w.take_pending(),
            None,
            "taking twice must not invent a tilde"
        );
    }

    /// Control characters — Ctrl-C, Ctrl-Z, arrow-key sequences — must pass
    /// through byte for byte. Raw mode exists so the remote sees them.
    #[test]
    fn control_characters_pass_through_unchanged() {
        let mut w = EscapeWatcher::new();
        // Ctrl-C, Ctrl-Z, Ctrl-D, and an escape sequence for the up arrow.
        let input: &[u8] = &[0x03, 0x1a, 0x04, 0x1b, b'[', b'A'];
        assert_eq!(w.feed(input), Typed::Send(input.to_vec()));
    }

    /// A session that opens with the escape should detach: the watcher starts
    /// at a line start, which is what a fresh shell prompt is.
    /// A fresh shell prompt *is* a line start, so a session that opens with
    /// the escape should detach.
    ///
    /// This also pins that `new()` and `default()` agree. They did not: the
    /// derived Default started mid-line, so a caller using it would miss an
    /// escape on the first line and have no way to know.
    #[test]
    fn a_fresh_watcher_starts_at_a_line_start_however_it_was_made() {
        assert_eq!(EscapeWatcher::new().feed(b"~."), Typed::Detach);
        assert_eq!(EscapeWatcher::default().feed(b"~."), Typed::Detach);
    }

    /// Arbitrary input must never panic — every byte a keyboard can produce
    /// reaches this.
    #[test]
    fn arbitrary_input_never_panics() {
        let mut w = EscapeWatcher::new();
        let mut x = 0x2545_F491_4F6C_DD1Du64;
        for _ in 0..5000 {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            let n = (x % 17) as usize;
            let chunk: Vec<u8> = (0..n).map(|i| ((x >> (i % 8)) & 0xFF) as u8).collect();
            let _ = w.feed(&chunk);
        }
    }
}
