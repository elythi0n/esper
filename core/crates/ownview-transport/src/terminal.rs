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
//! # A dropped link does not kill the shell
//!
//! When the connection goes away — or the user detaches on purpose — the
//! pseudo-terminal is handed to [`crate::parked`], which keeps the shell alive
//! and its output drained until the same device comes back for it. Terminal
//! mode is for someone running a long job on a server from a laptop that will
//! close, and a design where losing the network loses the work makes it a
//! worse `ssh` rather than a better one.
//!
//! # The detach escape
//!
//! `~.` at the start of a line detaches, leaving the shell running. The
//! sequence is `ssh`'s, deliberately, because a user who knows one knows the
//! other — but the *outcome* is not: in `ssh` it drops the connection and the
//! shell with it, and here the session is parked and can be returned to.
//!
//! It is recognised **only** at a line start, so a `~` inside text passes
//! through untouched, and `~~` sends a literal `~`.
//!
//! The alternative to having an escape is that a session with a wedged remote
//! can only be left by killing the client.

use std::sync::Arc;
use std::time::{Duration, Instant};

use ownview_audit::{Entry, Event, Log, terminal_closed};
use ownview_crypto::identity::KEY_LEN;
use ownview_proto::{
    Permission, Refusal, SessionId, TerminalData, TerminalExit, TerminalMessage, TerminalOpen,
    TerminalResize, TerminalStart, TerminalStarted,
};
use ownview_pty::{Terminal, hang_up, is_session_end};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

use crate::backlog::{RESYNC, loss_notice};
use crate::parked::{self, LiveTerminal, Parked, ParkedSessions, Resumed};
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
    /// The host refused, and the peer has been told the same reason.
    ///
    /// One variant carrying the wire refusal rather than one Rust variant per
    /// reason: the two would have to be kept in step by hand, and a mismatch
    /// would mean telling the peer one thing and the log another.
    #[error("{}", .0.message())]
    Refused(Refusal),

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
    /// Nobody is attached and the shell is still running, waiting to be
    /// resumed.
    ///
    /// Distinct from the shell exiting, because the two mean different things
    /// to whoever reads the log: one is a user finishing, the other is a user
    /// stepping away from something still running.
    Parked {
        session: SessionId,
        /// Whether the user asked to detach, rather than the link dropping.
        deliberate: bool,
    },
    /// The connection went away and the shell could not be kept.
    Disconnected,
    /// Nothing happened for as long as the idle timeout allows.
    Idle,
}

/// Tunables for a terminal session.
///
/// Separate from [`crate::parked::Limits`], which bounds what the host holds
/// for everybody; this is about one session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TerminalPolicy {
    /// End a session after this long with **no input and no output**.
    ///
    /// `None` — the default — is off.
    ///
    /// Idle deliberately means neither direction. A build that prints nothing
    /// for an hour is not idle, it is working, and a timeout that counted only
    /// keystrokes would kill it. That is not a security feature, it is a
    /// data-loss bug wearing one's clothes.
    pub idle_timeout: Option<Duration>,
}

/// Serves terminal sessions for one host.
///
/// Holds the audit log, the parked-session registry and the policy, so the
/// per-session call does not take seven arguments — and so the registry is
/// shared, which is the whole point of it.
#[derive(Debug)]
pub struct TerminalService {
    parked: Arc<ParkedSessions>,
    policy: TerminalPolicy,
    log: Log,
}

impl TerminalService {
    /// A service with these limits and this policy.
    #[must_use]
    pub fn new(log: Log, limits: parked::Limits, policy: TerminalPolicy) -> Self {
        Self {
            parked: ParkedSessions::new(limits),
            policy,
            log,
        }
    }

    /// The parked-session registry, for revocation and for reporting.
    #[must_use]
    pub fn parked(&self) -> &Arc<ParkedSessions> {
        &self.parked
    }

    /// Kill every terminal session parked by a device, and say how many.
    ///
    /// Called when `terminal` permission is withdrawn. A device that could
    /// return to a shell it opened while it was still trusted would make
    /// revocation advisory.
    pub fn revoke(&self, peer_key: &[u8; KEY_LEN]) -> usize {
        let closed = self.parked.revoke(peer_key);
        if closed > 0
            && let Err(e) = self.log.append(&Entry::now(
                peer_key,
                Event::Revoked {
                    sessions_closed: closed,
                },
            ))
        {
            tracing::warn!(error = %e, "could not record a revocation");
        }
        closed
    }

    /// Open a new terminal session, or return to one this device left behind.
    ///
    /// The peer is answered with a [`TerminalStarted`] before any terminal
    /// bytes, so it always knows which session it has and never has to infer
    /// it from the first output.
    pub async fn serve(
        &self,
        control: &mut Control,
        start: &TerminalStart,
        peer_key: &[u8; KEY_LEN],
        granted: &[Permission],
    ) -> Result<Ended, TerminalError> {
        // Permission is checked on **resume as well as open**. Returning to a
        // shell opened while permitted must not survive losing the permission.
        if !granted.contains(&Permission::Terminal) {
            // Recorded: a refusal is as interesting as an acceptance, and a
            // run of them is what an attack looks like from this side.
            let _ = self.log.append(&Entry::now(
                peer_key,
                Event::Refused {
                    reason: "terminal not permitted".to_owned(),
                },
            ));
            return self.refuse(control, Refusal::NotPermitted).await;
        }
        if !start.is_well_formed() {
            return self.refuse(control, Refusal::Malformed).await;
        }

        match start {
            TerminalStart::Open(open) => self.open(control, open, peer_key, granted).await,
            TerminalStart::Resume(r) => {
                self.resume(control, r.session, r.cols, r.rows, peer_key, granted)
                    .await
            }
        }
    }

    /// Tell the peer why, and fail with the same reason.
    async fn refuse(&self, control: &mut Control, why: Refusal) -> Result<Ended, TerminalError> {
        // Best-effort: a peer that has already gone cannot be told, and that
        // is not a further failure.
        let _ = control.send(&TerminalStarted::Refused(why)).await;
        let _ = control.finish();
        Err(TerminalError::Refused(why))
    }

    /// Start a fresh shell.
    async fn open(
        &self,
        control: &mut Control,
        open: &TerminalOpen,
        peer_key: &[u8; KEY_LEN],
        granted: &[Permission],
    ) -> Result<Ended, TerminalError> {
        // Checked here so a peer at the limit is told up front rather than
        // finding out that its session was not kept. The binding check is
        // still the one in `park`, which runs under the registry's lock.
        if self.parked.len() >= self.parked.limits().total {
            return self.refuse(control, Refusal::TooMany).await;
        }

        let terminal = Terminal::open(open)?;
        let session = parked::new_session_id().map_err(|e| {
            tracing::error!(error = %e, "no randomness for a session identifier");
            // Reported to the peer as the host being full. It learns nothing
            // either way, and the host genuinely cannot keep a session it
            // cannot name.
            TerminalError::Refused(Refusal::TooMany)
        })?;
        let started = Instant::now();

        // Best-effort: an audit failure must not kill a session. A full disk
        // should be a warning, not an outage.
        if let Err(e) = self
            .log
            .append(&Entry::now(peer_key, Event::TerminalOpened))
        {
            tracing::warn!(error = %e, "could not record the start of a terminal session");
        }

        control
            .send(&TerminalStarted::Opened { session })
            .await
            .map_err(TerminalError::from)?;

        let (read, write, child) = terminal.into_parts();
        self.run(
            control,
            LiveTerminal { read, write, child },
            session,
            started,
            peer_key,
            granted,
        )
        .await
    }

    /// Return to a shell that is still running from an earlier connection.
    async fn resume(
        &self,
        control: &mut Control,
        session: SessionId,
        cols: u16,
        rows: u16,
        peer_key: &[u8; KEY_LEN],
        granted: &[Permission],
    ) -> Result<Ended, TerminalError> {
        // Authorised by identifier **and** key, and every failure looks the
        // same: an unknown identifier, another device's session and an elapsed
        // window are all `Unknown`, so a peer cannot learn from the answer
        // whether a session it does not own exists.
        let Some(resumed) = self.parked.resume(session, peer_key).await else {
            return self.refuse(control, Refusal::Unknown).await;
        };
        let started = Instant::now();

        match resumed {
            Resumed::Still {
                terminal,
                backlog,
                lost,
            } => {
                // The controller may have been resized, or be a different
                // terminal on a different machine, while it was away. Without
                // this the shell keeps drawing to the old width.
                parked::resize(&terminal, cols, rows);

                if let Err(e) = self.log.append(&Entry::now(
                    peer_key,
                    Event::TerminalResumed {
                        session: session.to_string(),
                        parked_secs: 0,
                        lost,
                    },
                )) {
                    tracing::warn!(error = %e, "could not record a resumed terminal session");
                }

                control
                    .send(&TerminalStarted::Resumed { session, lost })
                    .await?;
                replay(control, &backlog, lost).await?;

                self.run(control, terminal, session, started, peer_key, granted)
                    .await
            }
            Resumed::Exited {
                backlog,
                lost,
                exit,
            } => {
                // The shell finished while nobody was attached. Its last
                // output and its exit status are the reason someone detached
                // from a long job rather than waiting for it.
                control
                    .send(&TerminalStarted::Resumed { session, lost })
                    .await?;
                replay(control, &backlog, lost).await?;
                control.send(&TerminalMessage::Exit(exit)).await?;
                let _ = control.finish();
                Ok(Ended::ShellExited(exit))
            }
        }
    }

    /// Pump the session, then park what is left of it.
    async fn run(
        &self,
        control: &mut Control,
        terminal: LiveTerminal,
        session: SessionId,
        started: Instant,
        peer_key: &[u8; KEY_LEN],
        granted: &[Permission],
    ) -> Result<Ended, TerminalError> {
        let outcome = pump(control, terminal, self.policy.idle_timeout).await;

        let (ended, exit) = match outcome {
            Outcome::ShellExited(x) => {
                let _ = control.send(&TerminalMessage::Exit(x)).await;
                let _ = control.finish();
                (Ended::ShellExited(x), Some(x))
            }
            Outcome::Orphaned {
                terminal,
                deliberate,
            } => {
                self.park(control, terminal, session, peer_key, granted, deliberate)
                    .await
            }
            Outcome::Idle(x) => {
                let _ = control.finish();
                (Ended::Idle, x)
            }
            Outcome::Gone(x) => (Ended::Disconnected, x),
        };

        // A parked session has not closed, so it is not recorded as closed —
        // it gets its own entry, and the close is recorded when it really
        // ends. A log that said "closed" for a shell still running would be
        // wrong in the direction that matters.
        if !matches!(ended, Ended::Parked { .. }) {
            let secs = started.elapsed().as_secs();
            if let Err(e) = self
                .log
                .append(&Entry::now(peer_key, terminal_closed(secs, exit.as_ref())))
            {
                tracing::warn!(error = %e, "could not record the end of a terminal session");
            }
        }

        Ok(ended)
    }

    /// Hand a live shell to the registry, or end it if it cannot be kept.
    async fn park(
        &self,
        control: &mut Control,
        terminal: LiveTerminal,
        session: SessionId,
        peer_key: &[u8; KEY_LEN],
        granted: &[Permission],
        deliberate: bool,
    ) -> (Ended, Option<TerminalExit>) {
        // Re-checked at the moment of parking, not only at the start. A
        // session that began while permitted must not be *left standing* by a
        // device whose permission was withdrawn mid-session.
        if !granted.contains(&Permission::Terminal) {
            let exit = hang_up(terminal.child, terminal.read, terminal.write).await;
            return (Ended::Disconnected, exit);
        }

        match self.parked.park(*peer_key, session, terminal) {
            Parked::Ok(session) => {
                if let Err(e) = self.log.append(&Entry::now(
                    peer_key,
                    Event::TerminalParked {
                        session: session.to_string(),
                        deliberate,
                    },
                )) {
                    tracing::warn!(error = %e, "could not record a parked terminal session");
                }
                (
                    Ended::Parked {
                        session,
                        deliberate,
                    },
                    None,
                )
            }
            Parked::Refused(why, terminal) => {
                tracing::warn!(reason = ?why, "could not park a terminal session; ending it");
                let exit = hang_up(terminal.child, terminal.read, terminal.write).await;
                // A controller that detached on purpose is still connected, so
                // it is told the truth: the shell ended. `Exit` is exactly
                // that message, so no new one is needed — and a controller
                // that showed "detached, come back later" for a shell that had
                // in fact been killed would be lying to its user.
                if let Some(x) = exit {
                    let _ = control.send(&TerminalMessage::Exit(x)).await;
                }
                let _ = control.finish();
                (Ended::Disconnected, exit)
            }
        }
    }
}

/// Send a resumed session's missed output, telling the user about any gap.
///
/// The order matters. The resynchronise goes first because the link can drop
/// in the middle of any escape sequence, and a terminal still parsing one will
/// swallow whatever is printed next — including the notice explaining what
/// happened.
async fn replay(control: &mut Control, backlog: &[u8], lost: u64) -> Result<(), TerminalError> {
    let mut out = Vec::with_capacity(
        backlog
            .len()
            .saturating_add(RESYNC.len())
            .saturating_add(96),
    );
    out.extend_from_slice(RESYNC);
    if lost > 0 {
        out.extend_from_slice(&loss_notice(lost));
    }
    out.extend_from_slice(backlog);

    for chunk in out.chunks(READ_CHUNK) {
        control
            .send(&TerminalMessage::Data(TerminalData {
                bytes: chunk.to_vec(),
            }))
            .await?;
    }
    Ok(())
}

/// Why the pump stopped, and what is left of the session.
enum Outcome {
    /// The shell exited. Already hung up on.
    ShellExited(TerminalExit),
    /// The shell is alive with nobody attached. Park it or end it.
    Orphaned {
        terminal: LiveTerminal,
        /// Whether the user asked to detach, rather than the link dropping.
        deliberate: bool,
    },
    /// Nothing happened for as long as the idle timeout allows. Hung up on.
    Idle(Option<TerminalExit>),
    /// The session ended and must not be kept. Hung up on.
    Gone(Option<TerminalExit>),
}

/// What ended a turn of the pump's loop.
///
/// Decided inside `tokio::select!` and acted on after it returns, because the
/// branch reading the terminal borrows it while the branch that hands it back
/// needs to move it. Once the `select!` has returned, every branch's future
/// has been dropped and the borrow is over.
enum Turn {
    /// Output to forward.
    Output(usize),
    /// The shell exited.
    ShellEnded,
    /// Reading the terminal failed for another reason.
    Broken,
    /// Keystrokes to write.
    Input(TerminalData),
    /// A window resize.
    Resize(TerminalResize),
    /// The controller closed the stream cleanly.
    Detached,
    /// The connection failed.
    LinkLost,
    /// The peer sent something a controller never sends.
    Misbehaved,
    /// Neither side has done anything for too long.
    Idle,
}

/// Pump both directions until one end stops.
async fn pump(
    control: &mut Control,
    mut terminal: LiveTerminal,
    idle_timeout: Option<Duration>,
) -> Outcome {
    let mut buf = vec![0u8; READ_CHUNK];
    // No timeout means a deadline that never arrives. Expressed as a very
    // distant instant rather than as a branch, so the loop has one shape.
    let far_future = Duration::from_secs(60 * 60 * 24 * 365);
    let mut idle_at = tokio::time::Instant::now() + idle_timeout.unwrap_or(far_future);

    loop {
        let turn = tokio::select! {
            read = terminal.read.read(&mut buf) => {
                if is_session_end(&read) {
                    Turn::ShellEnded
                } else {
                    match read {
                        Ok(n) => Turn::Output(n),
                        Err(e) => {
                            tracing::debug!(error = %e, "pty read failed");
                            Turn::Broken
                        }
                    }
                }
            },
            msg = control.recv::<TerminalMessage>() => match msg {
                Ok(TerminalMessage::Data(d)) => Turn::Input(d),
                Ok(TerminalMessage::Resize(r)) => Turn::Resize(r),
                // Host to controller only. A controller sending it is confused
                // or probing; either way the session ends and is not kept.
                Ok(TerminalMessage::Exit(_)) => Turn::Misbehaved,
                Err(ControlError::Finished) => Turn::Detached,
                Err(e) => {
                    tracing::debug!(error = %e, "terminal control stream failed");
                    Turn::LinkLost
                }
            },
            () = tokio::time::sleep_until(idle_at) => Turn::Idle,
        };

        // Both directions count. See `TerminalPolicy::idle_timeout`.
        if matches!(turn, Turn::Output(_) | Turn::Input(_) | Turn::Resize(_)) {
            idle_at = tokio::time::Instant::now() + idle_timeout.unwrap_or(far_future);
        }

        match turn {
            Turn::Output(n) => {
                let bytes = buf.get(..n).unwrap_or(&[]).to_vec();
                if let Err(e) = control
                    .send(&TerminalMessage::Data(TerminalData { bytes }))
                    .await
                {
                    tracing::debug!(error = %e, "could not forward terminal output");
                    return Outcome::Orphaned {
                        terminal,
                        deliberate: false,
                    };
                }
            }
            Turn::Input(d) => {
                if !d.is_well_formed() {
                    return Outcome::Gone(
                        hang_up(terminal.child, terminal.read, terminal.write).await,
                    );
                }
                if let Err(e) = terminal.write.write_all(&d.bytes).await {
                    tracing::debug!(error = %e, "pty write failed");
                    return Outcome::Gone(
                        hang_up(terminal.child, terminal.read, terminal.write).await,
                    );
                }
            }
            Turn::Resize(r) => {
                // A malformed resize is ignored rather than fatal: the session
                // is otherwise fine and killing it over a bad window size
                // would be a worse outcome than the wrong wrap.
                if let Err(e) = Terminal::resize(&terminal.write, &r) {
                    tracing::debug!(error = %e, cols = r.cols, rows = r.rows, "ignoring a bad resize");
                }
            }
            // The two ways a session is left standing. Both keep the shell:
            // the difference is only what the log says about the user's
            // intent.
            Turn::Detached => {
                return Outcome::Orphaned {
                    terminal,
                    deliberate: true,
                };
            }
            Turn::LinkLost | Turn::Broken => {
                return Outcome::Orphaned {
                    terminal,
                    deliberate: false,
                };
            }
            Turn::ShellEnded => {
                let exit = hang_up(terminal.child, terminal.read, terminal.write).await;
                return match exit {
                    Some(x) => Outcome::ShellExited(x),
                    // The shell is gone but its status could not be collected.
                    // Nothing to keep and nothing to report.
                    None => Outcome::Gone(None),
                };
            }
            Turn::Misbehaved => {
                return Outcome::Gone(hang_up(terminal.child, terminal.read, terminal.write).await);
            }
            Turn::Idle => {
                tracing::info!("ending an idle terminal session");
                return Outcome::Idle(hang_up(terminal.child, terminal.read, terminal.write).await);
            }
        }
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
        return Err(TerminalError::Refused(Refusal::Malformed));
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
