//! Terminal sessions whose controller is not attached right now.
//!
//! A dropped link must not kill the shell. So when a connection goes away —
//! or the user detaches on purpose — the pseudo-terminal and its shell are
//! handed to a *keeper*: one task per parked session that drains the terminal
//! into a bounded backlog and waits for one of three things.
//!
//! # Why a keeper task and not just a stored handle
//!
//! Because **something must keep reading the pseudo-terminal master.** With no
//! reader the kernel's terminal buffer fills and the shell blocks on its next
//! write, so the build the user detached in order to keep running stops
//! running. That failure looks like "reconnect works but the job made no
//! progress", which is worse than not supporting reconnect at all: the user
//! believes their work is safe.
//!
//! # Authorisation
//!
//! A parked session is a live shell running as the host's user, so returning
//! to one is exactly as privileged as opening a new one. A session is found by
//! **identifier and owning device key together**; the identifier alone opens
//! nothing, and a lookup that fails for any reason fails the same way.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use esper_crypto::identity::KEY_LEN;
use esper_proto::{MAX_TERMINAL_CHUNK, Refusal, SESSION_ID_LEN, SessionId, TerminalExit};
use esper_pty::{ReadHalf, Terminal, WriteHalf, hang_up, is_session_end};
use tokio::io::AsyncReadExt as _;
use tokio::sync::oneshot;

use crate::backlog::Backlog;

/// A pseudo-terminal and the shell on it, with nothing attached.
pub struct LiveTerminal {
    pub read: ReadHalf,
    pub write: WriteHalf,
    pub child: tokio::process::Child,
}

impl std::fmt::Debug for LiveTerminal {
    /// The process id, which identifies the session in `ps`. The terminal
    /// halves are file descriptors and printing them says nothing useful.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LiveTerminal")
            .field("pid", &self.child.id())
            .finish_non_exhaustive()
    }
}

/// What a controller gets back when it returns to a parked session.
#[derive(Debug)]
// Made once per session and consumed at once, so boxing the big variant
// would buy an allocation and nothing else. Over the threshold only where
// `tokio::process::Child` is large, which is Windows and not Unix.
#[allow(clippy::large_enum_variant)]
pub enum Resumed {
    /// The shell is still running.
    Still {
        terminal: LiveTerminal,
        /// Output produced while nobody was attached.
        backlog: Vec<u8>,
        /// Bytes dropped from that backlog because it was full.
        lost: u64,
        /// How long it spent with nobody attached.
        parked_for: Duration,
    },
    /// The shell exited while nobody was attached.
    ///
    /// Kept rather than discarded, for the length of the park window, because
    /// the exit status of a long job is often the whole reason someone
    /// detached from it. Throwing it away would repeat — at a different layer
    /// — the bug where every Linux session lost its exit status.
    Exited {
        backlog: Vec<u8>,
        lost: u64,
        exit: TerminalExit,
        parked_for: Duration,
    },
}

/// Whether a terminal was parked, or handed straight back.
///
/// An enum rather than a `Result`, because the unhappy answer carries a whole
/// pseudo-terminal: a caller whose session was refused still owns the shell
/// and must hang up on it. That is not an error value, it is a handover, and
/// naming it one keeps callers from reaching for `?` and leaking a shell.
#[derive(Debug)]
// Made once per session and consumed at once, so boxing the big variant
// would buy an allocation and nothing else. Over the threshold only where
// `tokio::process::Child` is large, which is Windows and not Unix.
#[allow(clippy::large_enum_variant)]
pub enum Parked {
    /// Parked, and here is the identifier to give the controller.
    Ok(SessionId),
    /// Not parked. The terminal comes back, and the caller must end it.
    Refused(Refusal, LiveTerminal),
}

/// How many sessions may be parked, for how long, holding how much.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Parked sessions one device may hold.
    pub per_device: usize,
    /// Parked sessions the host will hold in total.
    pub total: usize,
    /// How long a parked session waits before its shell is killed.
    pub park_for: Duration,
    /// Bytes of output kept per parked session.
    pub backlog: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            per_device: 4,
            total: 32,
            park_for: Duration::from_secs(600),
            backlog: crate::backlog::DEFAULT_BACKLOG,
        }
    }
}

/// Asks a keeper to hand its terminal over, and carries the answer back.
type Handover = oneshot::Sender<Resumed>;

/// One parked session, from the registry's side.
#[derive(Debug)]
struct Slot {
    owner: [u8; KEY_LEN],
    /// When it was parked, for the audit record.
    ///
    /// An `Instant`, not a wall clock: the only question asked of it is how
    /// long ago, and a wall clock that stepped backwards would answer that
    /// with a negative number.
    at: std::time::Instant,
    /// Ordering for eviction. A counter rather than a clock: two sessions
    /// parked in the same millisecond still have an order, and a clock that
    /// steps backwards cannot make an old session look new.
    seq: u64,
    /// Sending asks the keeper to hand over. **Dropping it kills the
    /// session** — which is what makes removing a slot sufficient to revoke.
    ask: oneshot::Sender<Handover>,
}

/// The guarded state: the sessions, and the counter that orders them.
///
/// One structure under one lock rather than two locks. A counter with its own
/// lock could hand two sessions the same number, and checking a limit under
/// one lock while inserting under another makes the limit a suggestion.
#[derive(Debug, Default)]
struct Registry {
    slots: HashMap<SessionId, Slot>,
    seq: u64,
}

/// Every terminal session parked on this host.
pub struct ParkedSessions {
    state: Mutex<Registry>,
    limits: Limits,
}

impl std::fmt::Debug for ParkedSessions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ParkedSessions")
            .field("parked", &self.len())
            .field("limits", &self.limits)
            .finish()
    }
}

impl ParkedSessions {
    /// An empty registry.
    #[must_use]
    pub fn new(limits: Limits) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(Registry::default()),
            limits,
        })
    }

    /// The guarded state.
    ///
    /// A poisoned lock is recovered rather than propagated. The alternative is
    /// that one panic in one session's bookkeeping makes every *other* live
    /// shell unreachable and unkillable — and the state here is a map and a
    /// counter, with no invariant that a panic could have left half-applied.
    fn state(&self) -> std::sync::MutexGuard<'_, Registry> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// The limits in force.
    #[must_use]
    pub const fn limits(&self) -> &Limits {
        &self.limits
    }

    /// How many sessions are parked.
    #[must_use]
    pub fn len(&self) -> usize {
        self.state().slots.len()
    }

    /// Whether nothing is parked.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// How many sessions a device has parked.
    #[must_use]
    pub fn held_by(&self, owner: &[u8; KEY_LEN]) -> usize {
        self.state()
            .slots
            .values()
            .filter(|v| &v.owner == owner)
            .count()
    }

    /// Park a live terminal so its owner can come back to it.
    ///
    /// Returns the identifier to hand the controller. On refusal the caller
    /// still owns the terminal and must hang up on it — parking is the only
    /// thing that transfers it.
    /// The identifier is minted when the session **opens**, not here. A
    /// controller whose link drops has to already know how to ask for its
    /// session back, and by then there is nobody to tell.
    pub fn park(
        self: &Arc<Self>,
        owner: [u8; KEY_LEN],
        id: SessionId,
        terminal: LiveTerminal,
    ) -> Parked {
        let (ask_tx, ask_rx) = oneshot::channel();

        // Both limits and the insertion under **one** acquisition of the lock.
        //
        // Checking a limit and then inserting are two operations, and two
        // connections parking at the same moment would each see room and each
        // take it. One lock makes the limit a limit rather than a suggestion,
        // which for a bound on how many live shells a host will hold is the
        // difference that matters.
        let mut state = self.state();

        // Over the whole-host limit, a new park is refused rather than
        // evicting anyone. The asymmetry with the per-device limit below is
        // the point: losing your own oldest session is a consequence of your
        // own accumulation, but losing it to someone else's is one device
        // denying another.
        if state.slots.len() >= self.limits.total {
            tracing::warn!(
                parked = state.slots.len(),
                limit = self.limits.total,
                "refusing to park a terminal session: the host is at its limit"
            );
            return Parked::Refused(Refusal::TooMany, terminal);
        }

        // Over this device's limit, its own oldest goes.
        while state.slots.values().filter(|v| v.owner == owner).count() >= self.limits.per_device {
            let oldest = state
                .slots
                .iter()
                .filter(|(_, v)| v.owner == owner)
                .min_by_key(|(_, v)| v.seq)
                .map(|(k, _)| *k);
            match oldest {
                Some(id) => {
                    tracing::info!(
                        session = %id,
                        limit = self.limits.per_device,
                        "killing a device's oldest parked terminal to make room for a new one"
                    );
                    // Dropping the slot drops its `ask`, which the keeper
                    // reads as a kill.
                    let _ = state.slots.remove(&id);
                }
                // Nothing of this device's to evict, so the count cannot come
                // down. Refuse rather than spin.
                None => return Parked::Refused(Refusal::TooMany, terminal),
            }
        }

        state.seq = state.seq.saturating_add(1);
        let seq = state.seq;
        let _ = state.slots.insert(
            id,
            Slot {
                owner,
                at: std::time::Instant::now(),
                seq,
                ask: ask_tx,
            },
        );
        drop(state);

        tokio::spawn(keep(
            Arc::downgrade(self),
            id,
            ask_rx,
            terminal,
            self.limits.backlog,
            self.limits.park_for,
        ));
        Parked::Ok(id)
    }

    /// Take a parked session back.
    ///
    /// Authorised by identifier **and** owning key. Every failure — no such
    /// identifier, someone else's session, one whose window elapsed — returns
    /// `None`, so a caller cannot learn from the answer whether a session it
    /// does not own exists.
    pub async fn resume(&self, id: SessionId, owner: &[u8; KEY_LEN]) -> Option<Resumed> {
        let (ask, parked_for) = {
            let mut state = self.state();
            // Checked under the lock and removed in the same step, so two
            // connections racing to resume cannot both be handed the shell.
            match state.slots.get(&id) {
                Some(slot) if &slot.owner == owner => {
                    let slot = state.slots.remove(&id)?;
                    (slot.ask, slot.at.elapsed())
                }
                _ => return None,
            }
        };

        let (reply_tx, reply_rx) = oneshot::channel();
        // A keeper that has already ended — its window elapsed between the
        // lookup and now — makes this fail, which is the same `None`.
        ask.send(reply_tx).ok()?;
        let mut resumed = reply_rx.await.ok()?;

        // Filled in here rather than by the keeper, because the registry is
        // what knows when the session was parked.
        match &mut resumed {
            Resumed::Still { parked_for: p, .. } | Resumed::Exited { parked_for: p, .. } => {
                *p = parked_for;
            }
        }
        Some(resumed)
    }

    /// Kill every session parked by a device.
    ///
    /// Called when `terminal` permission is withdrawn. A device that could
    /// return to a shell it opened while it was still trusted would make
    /// revocation advisory, and a standing shell is the most valuable thing
    /// there is to revoke.
    pub fn revoke(&self, owner: &[u8; KEY_LEN]) -> usize {
        let doomed: Vec<SessionId> = self
            .state()
            .slots
            .iter()
            .filter(|(_, v)| &v.owner == owner)
            .map(|(k, _)| *k)
            .collect();
        for id in &doomed {
            tracing::info!(session = %id, "killing a parked terminal session: permission revoked");
            self.forget(*id);
        }
        doomed.len()
    }

    /// Kill every parked session, whoever owns it. For host shutdown.
    pub fn revoke_all(&self) -> usize {
        let doomed: Vec<SessionId> = self.state().slots.keys().copied().collect();
        for id in &doomed {
            self.forget(*id);
        }
        doomed.len()
    }

    /// Drop a slot, which asks its keeper to shut the session down.
    fn forget(&self, id: SessionId) {
        let _ = self.state().slots.remove(&id);
    }
}

/// A fresh, unguessable session identifier.
///
/// Not a capability — resuming needs the owning device's key as well — but an
/// enumerable identifier would let one paired device learn that another has a
/// live session, and 128 random bits costs nothing.
pub fn new_session_id() -> Result<SessionId, getrandom::Error> {
    let mut b = [0u8; SESSION_ID_LEN];
    getrandom::getrandom(&mut b)?;
    Ok(SessionId::from_bytes(b))
}

/// What ended a turn of the keeper's loop.
///
/// The keeper cannot act inside `tokio::select!`, because the branch that
/// reads the terminal borrows it while the branch that hands it over needs to
/// move it. Deciding first and acting after the `select!` has returned — and
/// so after every branch's future has been dropped — is what makes both
/// possible.
enum Turn {
    /// A controller wants the session back.
    HandOver(Handover),
    /// The registry dropped our slot: shut down.
    Killed,
    /// The park window elapsed.
    Elapsed,
    /// Output to keep.
    Output(usize),
    /// The shell exited.
    ShellEnded,
    /// The terminal failed in a way that is not the shell exiting.
    Broken(std::io::Error),
}

/// Look after one parked session until somebody comes back for it.
async fn keep(
    registry: Weak<ParkedSessions>,
    id: SessionId,
    mut ask: oneshot::Receiver<Handover>,
    mut terminal: LiveTerminal,
    cap: usize,
    park_for: Duration,
) {
    let deadline = tokio::time::Instant::now() + park_for;
    let mut backlog = Backlog::new(cap);
    let mut buf = vec![0u8; MAX_TERMINAL_CHUNK];

    loop {
        let turn = tokio::select! {
            asked = &mut ask => match asked {
                Ok(reply) => Turn::HandOver(reply),
                Err(_) => Turn::Killed,
            },
            read = terminal.read.read(&mut buf) => {
                if is_session_end(&read) {
                    Turn::ShellEnded
                } else {
                    match read {
                        Ok(n) => Turn::Output(n),
                        Err(e) => Turn::Broken(e),
                    }
                }
            },
            () = tokio::time::sleep_until(deadline) => Turn::Elapsed,
        };

        match turn {
            Turn::Output(n) => {
                backlog.push(buf.get(..n).unwrap_or(&[]));
            }
            Turn::HandOver(reply) => {
                let (bytes, lost) = backlog.take();
                // A failed send means the controller gave up between asking
                // and now. The shell must not be left with no reader, so it is
                // hung up on rather than leaked.
                if let Err(Resumed::Still { terminal, .. }) = reply.send(Resumed::Still {
                    terminal,
                    backlog: bytes,
                    lost,
                    // Overwritten by `resume`, which holds the clock.
                    parked_for: Duration::ZERO,
                }) {
                    let _ = hang_up(terminal.child, terminal.read, terminal.write).await;
                }
                return;
            }
            Turn::Killed | Turn::Elapsed => {
                let why = if matches!(turn, Turn::Elapsed) {
                    "the park window elapsed"
                } else {
                    "it was revoked or evicted"
                };
                tracing::info!(session = %id, why, "ending a parked terminal session");
                let _ = hang_up(terminal.child, terminal.read, terminal.write).await;
                forget(&registry, id);
                return;
            }
            Turn::ShellEnded | Turn::Broken(_) => {
                if let Turn::Broken(ref e) = turn {
                    tracing::debug!(session = %id, error = %e, "a parked terminal failed");
                }
                let exit = hang_up(terminal.child, terminal.read, terminal.write).await;
                let (bytes, lost) = backlog.take();
                entomb(&registry, id, ask, bytes, lost, exit, deadline).await;
                return;
            }
        }
    }
}

/// Hold a finished session's last output and exit status until its window ends.
///
/// The shell is gone, so there is nothing left to drain — but the person who
/// detached from a long job wants its exit status and its last lines, and they
/// are the reason they detached instead of waiting.
async fn entomb(
    registry: &Weak<ParkedSessions>,
    id: SessionId,
    ask: oneshot::Receiver<Handover>,
    backlog: Vec<u8>,
    lost: u64,
    exit: Option<TerminalExit>,
    deadline: tokio::time::Instant,
) {
    // No exit status available means there is nothing worth keeping: the
    // caller would learn only that the session is gone, which an absent
    // session already says.
    let Some(exit) = exit else {
        forget(registry, id);
        return;
    };

    tokio::select! {
        asked = ask => {
            if let Ok(reply) = asked {
                let _ = reply.send(Resumed::Exited {
                    backlog,
                    lost,
                    exit,
                    parked_for: Duration::ZERO,
                });
            }
        }
        () = tokio::time::sleep_until(deadline) => {}
    }
    forget(registry, id);
}

/// Remove a slot if the registry is still there.
///
/// A registry that has been dropped has nothing to forget — and that is the
/// ordinary shutdown path, not a failure.
fn forget(registry: &Weak<ParkedSessions>, id: SessionId) {
    if let Some(r) = registry.upgrade() {
        r.forget(id);
    }
}

/// Set a parked session's window size.
///
/// Applied on resume, because the controller may have been resized — or be a
/// different terminal on a different machine — while it was away. Without this
/// the shell keeps drawing to the old width and every line wraps wrongly until
/// the user resizes by hand.
pub fn resize(terminal: &LiveTerminal, cols: u16, rows: u16) {
    let r = esper_proto::TerminalResize { cols, rows };
    if let Err(e) = Terminal::resize(&terminal.write, &r) {
        tracing::debug!(error = %e, cols, rows, "could not resize a resumed terminal");
    }
}
