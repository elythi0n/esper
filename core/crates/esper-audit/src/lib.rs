//! The local record of what happened.
//!
//! # What this is for
//!
//! A user must be able to answer "who has connected to this machine, when, and
//! what were they allowed to do" — correctly, from one place, without trusting
//! anyone else's records. That is the whole purpose.
//!
//! # What is deliberately not recorded
//!
//! **Session content, at any level, ever.** Not the framebuffer, not
//! keystrokes, and emphatically not terminal traffic.
//!
//! What a person types into a shell includes every password they enter through
//! it. A remote desktop that logged its own terminal traffic would be a
//! keylogger, and the fact that the log is local does not change that — a local
//! file is exactly what an attacker who gets in reads first.
//!
//! So the record answers *that a session happened* and never *what happened in
//! it*. The boundary is stated here, and in the types: there is no field a
//! well-meaning change could put content into without adding one, which makes
//! that change visible in review.
//!
//! # Why it cannot leave the machine
//!
//! There is no transmit path. Not a disabled one, not a configurable one —
//! none. The log is a file, and nothing in Esper reads it to send it
//! somewhere. That is the strongest form the guarantee can take, and the test
//! suite asserts the type carries no serialisation target other than the local
//! file.

#![forbid(unsafe_code)]

use std::fs;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

use esper_crypto::Fingerprint;
use esper_crypto::identity::KEY_LEN;
use esper_proto::{Permission, TerminalExit};
use serde::{Deserialize, Serialize};

/// What can go wrong writing the record.
#[derive(Debug, thiserror::Error)]
pub enum AuditError {
    /// The entry could not be written.
    ///
    /// **A caller must not treat this as fatal to the session.** An audit
    /// failure that killed a live session would make a full disk into an
    /// outage; an audit failure that is reported and continues makes it a
    /// warning. The record is important, not more important than the thing it
    /// records.
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    /// The entry could not be encoded.
    #[error("could not encode an audit entry: {0}")]
    Encode(String),
}

/// What kind of thing happened.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "event")]
pub enum Event {
    /// The controller said it started or stopped recording the session.
    ///
    /// **Recorded because a recording outlives the session.** Everything else
    /// in this log is about access that ended when the session did; a file on
    /// somebody else's disk does not end, and the one moment it can be written
    /// down is when it was announced.
    ///
    /// It is the far end's own claim, which the log says nothing about the
    /// truth of — the same standing as the name a device gives itself.
    Recording { on: bool },
    /// A device was paired.
    Paired {
        /// What the device was called at pairing. Peer-supplied.
        name: String,
    },
    /// A device was unpaired: its key is no longer known here.
    ///
    /// Distinct from [`Event::Revoked`], which is about the *sessions* a
    /// revocation closed. Unpairing while nothing is connected closes
    /// nothing, and would otherwise leave no trace of the most consequential
    /// thing a person can do to a device record.
    Unpaired {
        /// What the device was called. Recorded because the key is unreadable
        /// and the name is what somebody will be looking for afterwards.
        name: String,
    },
    /// A device was revoked, and how many live sessions that closed.
    Revoked { sessions_closed: usize },
    /// A live session was narrowed rather than ended.
    ///
    /// Distinct from [`Event::Revoked`], which counts the sessions a
    /// revocation *closed*. A narrowing closes nothing — the picture keeps
    /// flowing and the cursor stops driving — so without this it would be the
    /// one enforcement action that left no trace. The operator's
    /// [`Event::PermissionsChanged`] records what they decided; this records
    /// that it reached a session which was already open, which is the half a
    /// reader cannot infer.
    SessionNarrowed {
        /// Everything the session may do now.
        granted: Vec<Permission>,
    },
    /// A session that was already open asked to be allowed one more thing,
    /// and what came of it.
    ///
    /// **Recorded whichever way it went**, because the refusals are the half
    /// somebody investigating wants: a device that asked for control four
    /// times in a session and was told no is a device whose behaviour is worth
    /// knowing about, and nothing else in this log would show it.
    ///
    /// It is *not* a [`Event::PermissionsChanged`]: nothing about the device
    /// record changed, and a reader who found this and then found the device
    /// still without the permission would be right — the answer lasted for one
    /// session and was never saved.
    SessionAsked {
        /// What was asked for.
        wanted: Permission,
        /// Whether it was allowed.
        allowed: bool,
        /// Why, in a few words: who or what decided.
        how: String,
    },
    /// Whether a session with one device leaves this machine locked was
    /// changed by somebody at this machine.
    ///
    /// **Recorded because it is a security setting, even though it changes no
    /// access at all.** "Somebody set this machine never to lock after that
    /// device's sessions" is exactly the line an investigation wants and
    /// nothing else in this log would carry it.
    LockAfterChanged {
        /// The word it was set to, or `None` for "whatever the host decides".
        after: Option<String>,
    },
    /// What a device may do was changed by somebody at this machine.
    ///
    /// The whole new set, not the difference: a log line saying "control was
    /// added" leaves a reader working out what that made the total, and a
    /// sequence of differences is exactly the thing that drifts from the
    /// record it describes.
    PermissionsChanged {
        /// Everything the device may do now.
        granted: Vec<Permission>,
    },
    /// A session opened, with what it was actually granted.
    ///
    /// The granted set, not the requested set: what a controller *asked* for is
    /// not a fact about this machine, and recording it would make the log read
    /// as though the request had been honoured.
    SessionOpened {
        granted: Vec<Permission>,
        /// How the peer was reached, so "relayed" is visible after the fact.
        tier: String,
        /// How the session came to be allowed, in the host's own words, or
        /// `None` where whoever wrote the line does not know.
        ///
        /// **The question an auditor asks first.** "A session opened, granted
        /// view and control" reads the same whether somebody at the machine
        /// agreed to it or whether nobody was asked — and those are the two
        /// answers the log exists to tell apart. A device that walked in on
        /// `unattended` is the one somebody goes looking for afterwards.
        ///
        /// `None` on the *controller's* own outbound line: that end asked for
        /// a session and was given one, and it was never told which of the
        /// host's reasons applied. Inventing one there would be worse than
        /// leaving it out.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        allowed: Option<String>,
    },
    /// A device asked for this machine's screen to be locked.
    ///
    /// **Recorded whether or not it worked.** A device that could lock
    /// somebody's screen is a device that could lock it repeatedly, and the
    /// only way to find that out afterwards is a line per attempt. `how` is
    /// which locker ran, or why none did.
    Locked {
        /// Which locker ran, or why none could.
        how: String,
        /// Whether the screen was actually locked.
        worked: bool,
    },
    /// A device asked for this machine to be logged out or restarted.
    ///
    /// **Recorded before it happens**, which for a restart is the only chance:
    /// the machine is gone a moment later, and a line written afterwards would
    /// be a line nobody wrote. `what` is the action and `how` is which
    /// program ran, or why none did.
    Powered {
        /// What was asked for: `log off` or `restart`.
        what: String,
        /// Which program ran, or why none could.
        how: String,
        /// Whether it was carried out.
        worked: bool,
    },
    /// A session ended.
    SessionClosed {
        /// Seconds it lasted.
        duration_secs: u64,
        /// Why, as a human sentence.
        reason: String,
    },
    /// A terminal session opened.
    TerminalOpened,
    /// A terminal session ended, with the shell's exit status.
    TerminalClosed {
        duration_secs: u64,
        exit_code: Option<i32>,
        exit_signal: Option<i32>,
    },
    /// A terminal session was parked, its shell still running with nobody
    /// attached.
    ///
    /// Recorded because a standing shell that no connection is holding is a
    /// thing an operator should be able to see, and because it is the other
    /// half of every [`Event::TerminalResumed`].
    TerminalParked {
        /// The session identifier, as hex. Not a secret — resuming needs the
        /// device key too — and having it here is what lets a reader tie a
        /// park to the resume that followed it.
        session: String,
        /// Whether the user asked to detach, rather than the link dropping.
        deliberate: bool,
    },
    /// A controller returned to a parked terminal session.
    TerminalResumed {
        session: String,
        /// Seconds it spent with nobody attached.
        parked_secs: u64,
        /// How much output was dropped while nobody was attached, in bytes.
        ///
        /// Named `lost` rather than `lost_bytes` to match the wire field, and
        /// because the content screen in the tests rejects any field whose
        /// name contains "bytes" — bluntly, and correctly: a count belongs
        /// here, the bytes themselves never do.
        lost: u64,
    },
    /// A forwarded connection was opened, and to what.
    ///
    /// The target, never the traffic. The same rule as a terminal session and
    /// the same reason: what goes through a tunnel is the user's, and a
    /// product that recorded it would be a wiretap that happened to be local.
    ForwardOpened {
        /// Where the host connected, as `host:port`.
        target: String,
    },
    /// A forwarded connection ended.
    ForwardClosed {
        target: String,
        duration_secs: u64,
        /// How much went each way. Counts, not content — the same distinction
        /// [`Event::TerminalResumed`]'s `lost` makes.
        sent: u64,
        received: u64,
    },
    /// A forwarded connection was refused on policy grounds.
    ///
    /// Only policy: a target that is not on the allowlist, or a device that no
    /// longer holds the permission. A target that simply was not listening is
    /// not here, because that is a service being down rather than anything
    /// about the peer — and it is the one that repeats.
    ///
    /// A device that collects several of these in one session is probing, and
    /// the host ends the session rather than going on refusing it. That bound
    /// is also what keeps this event from being a way to fill a disk.
    ForwardRefused {
        target: String,
        /// Why, as the sentence the peer was given.
        reason: String,
    },
    /// A file crossed, or tried to.
    ///
    /// The path relative to this machine's file directory, the size and the
    /// outcome. **Never the contents** — the same rule terminal output and
    /// forwarded traffic follow, and the same reason: what a person moves is
    /// theirs, and a product that recorded it would be a wiretap that happened
    /// to be local.
    ///
    /// Recorded whether it worked or not. A device repeatedly asking for paths
    /// it may not have is the signal that matters, and it is invisible in a
    /// record that only keeps what succeeded.
    FileTransferred {
        /// Which way the **file** went, from this machine's point of view:
        /// `in` or `out`.
        ///
        /// **Not called `direction`, and that is not a preference.** The entry
        /// around this one has a field of that name for which way the
        /// *session* went, and the event is flattened into the same object —
        /// so two fields called `direction` become one key. Every
        /// `file_transferred` line written before this rename was a line the
        /// engine could not read back: outbound wrote the key twice and read
        /// as a duplicate, inbound had its single key taken by the entry and
        /// read as missing. The screen skipped them all, and a file leaving
        /// this machine is exactly what somebody opens that screen to find.
        ///
        /// The two are independent: a session this machine opened outward can
        /// still receive a file.
        way: String,
        /// Where, relative to this machine's file directory. Already checked
        /// to be free of control characters, so it cannot forge a line here.
        path: String,
        /// How large it was said to be.
        size: u64,
        /// How it went, as the sentence a person would be told.
        outcome: String,
    },
    /// A session was refused, and why.
    ///
    /// Refusals matter as much as acceptances: a run of them is what an attack
    /// looks like from this side.
    Refused { reason: String },
}

/// Which way a session went.
///
/// **The log holds both halves of what this machine did**, and they are
/// different facts. "A device opened a session on me" is a statement about
/// this machine's exposure; "I opened a session on that device" is a statement
/// about what somebody sitting here did. A trail that recorded only the first
/// cannot answer "what did this laptop connect to, and when" — which is the
/// question asked about a shared machine, a stolen one, or an afternoon
/// somebody cannot remember.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Direction {
    /// Somebody connected **to** this machine.
    ///
    /// The default, and absent from the line when it is: every entry written
    /// before this existed is one of these, and a log whose old lines all grew
    /// a field would be a log somebody's tools stopped reading.
    #[default]
    In,
    /// This machine connected **out** to somebody.
    Out,
}

impl Direction {
    /// Whether this is the default, for leaving it out of the line.
    #[must_use]
    pub const fn is_inbound(&self) -> bool {
        matches!(self, Self::In)
    }

    /// The arrow a person reads.
    ///
    /// Two characters rather than a word, because it appears on every line of
    /// a list somebody scans down.
    #[must_use]
    pub const fn arrow(&self) -> &'static str {
        match self {
            Self::In => "←",
            Self::Out => "→",
        }
    }
}

/// One line in the record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    /// Unix seconds. Not a formatted string: a log that is grep-able by a
    /// person and parse-able by a program should not have to pick.
    pub at: u64,
    /// The peer's fingerprint, in the word form a person can compare against
    /// what the device list shows them.
    pub peer: String,
    /// The peer's key as hex, so an entry can be matched to a store record
    /// exactly rather than by a fingerprint that is only 64 bits.
    pub peer_key: String,
    /// Which way it went: somebody came in, or this machine went out.
    ///
    /// Left out of the line when it is inbound, which is both the default and
    /// what every entry written before this field existed was.
    #[serde(default, skip_serializing_if = "Direction::is_inbound")]
    pub direction: Direction,
    /// What happened.
    #[serde(flatten)]
    pub event: Event,
}

impl Entry {
    /// Build an entry for a peer that connected here.
    #[must_use]
    pub fn new(peer_key: &[u8; KEY_LEN], event: Event, at: u64) -> Self {
        Self {
            at,
            peer: Fingerprint::of(peer_key).inline(),
            peer_key: peer_key.iter().map(|b| format!("{b:02x}")).collect(),
            direction: Direction::In,
            event,
        }
    }

    /// Build an entry stamped with the current time.
    #[must_use]
    pub fn now(peer_key: &[u8; KEY_LEN], event: Event) -> Self {
        Self::new(peer_key, event, unix_now())
    }

    /// The same, for something **this machine** did to somebody else.
    ///
    /// A separate constructor rather than a flag on the two above, so that
    /// recording the wrong direction takes a different call rather than a
    /// forgotten argument.
    #[must_use]
    pub fn going_out(peer_key: &[u8; KEY_LEN], event: Event) -> Self {
        Self {
            direction: Direction::Out,
            ..Self::new(peer_key, event, unix_now())
        }
    }
}

impl Event {
    /// The tag this event goes by in the log.
    ///
    /// The same string serde writes, deliberately: a log line somebody greps
    /// for and a value a program branches on must not be two different words
    /// for one thing.
    #[must_use]
    pub const fn tag(&self) -> &'static str {
        match self {
            Self::Recording { .. } => "recording",
            Self::Paired { .. } => "paired",
            Self::Unpaired { .. } => "unpaired",
            Self::Revoked { .. } => "revoked",
            Self::SessionNarrowed { .. } => "session_narrowed",
            Self::SessionAsked { .. } => "session_asked",
            Self::LockAfterChanged { .. } => "lock_after_changed",
            Self::PermissionsChanged { .. } => "permissions_changed",
            Self::SessionOpened { .. } => "session_opened",
            Self::Locked { .. } => "locked",
            Self::Powered { .. } => "powered",
            Self::SessionClosed { .. } => "session_closed",
            Self::TerminalOpened => "terminal_opened",
            Self::TerminalClosed { .. } => "terminal_closed",
            Self::TerminalParked { .. } => "terminal_parked",
            Self::TerminalResumed { .. } => "terminal_resumed",
            Self::ForwardOpened { .. } => "forward_opened",
            Self::ForwardClosed { .. } => "forward_closed",
            Self::ForwardRefused { .. } => "forward_refused",
            Self::FileTransferred { .. } => "file_transferred",
            Self::Refused { .. } => "refused",
        }
    }

    /// One line of specifics, for somebody reading a list.
    ///
    /// **Never the contents of a session** — the same rule as the log itself.
    /// What a person needs from a line is what happened and to what: which
    /// permissions, which target, how long, why.
    #[must_use]
    pub fn detail(&self) -> String {
        match self {
            Self::Recording { on } => {
                if *on {
                    "the controller said it started recording this session".to_owned()
                } else {
                    "the controller said it stopped recording".to_owned()
                }
            }
            Self::Paired { name } => format!("paired with {name}"),
            Self::Unpaired { name } => format!("unpaired {name}"),
            Self::Revoked { sessions_closed } => match sessions_closed {
                0 => "revoked, with nothing connected".to_owned(),
                1 => "revoked, ending 1 session".to_owned(),
                n => format!("revoked, ending {n} sessions"),
            },
            Self::PermissionsChanged { granted } => format!("may now {}", names(granted)),
            Self::LockAfterChanged { after } => match after {
                Some(word) => format!("sessions with it now leave this screen: {word}"),
                None => "sessions with it now follow this host's own locking rule".to_owned(),
            },
            Self::SessionNarrowed { granted } => {
                format!("a live session was narrowed to {}", names(granted))
            }
            Self::SessionAsked {
                wanted,
                allowed,
                how,
            } => {
                let what = if *allowed { "allowed" } else { "refused" };
                format!(
                    "a live session asked for {} and was {what}: {how}",
                    wanted.name()
                )
            }
            Self::SessionOpened {
                granted,
                tier,
                allowed,
            } => {
                let how = allowed
                    .as_deref()
                    .map(|how| format!(", allowed by {how}"))
                    .unwrap_or_default();
                format!(
                    "session opened over {tier}, granted {}{how}",
                    names(granted)
                )
            }
            Self::Locked { how, worked } => {
                if *worked {
                    format!("locked this machine's screen with {how}")
                } else {
                    format!("could not lock this machine's screen: {how}")
                }
            }
            Self::Powered { what, how, worked } => {
                if *worked {
                    format!("asked this machine to {what}, with {how}")
                } else {
                    format!("could not make this machine {what}: {how}")
                }
            }
            Self::SessionClosed {
                duration_secs,
                reason,
            } => format!("session ended after {}, {reason}", how_long(*duration_secs)),
            Self::TerminalOpened => "a shell started".to_owned(),
            Self::TerminalClosed {
                duration_secs,
                exit_code,
                exit_signal,
            } => {
                let how = match (exit_code, exit_signal) {
                    (Some(c), _) => format!("code {c}"),
                    (None, Some(s)) => format!("signal {s}"),
                    (None, None) => "no status".to_owned(),
                };
                format!("the shell ended after {} ({how})", how_long(*duration_secs))
            }
            Self::TerminalParked {
                session,
                deliberate,
            } => format!(
                "the shell was left running ({}), session {}",
                if *deliberate {
                    "detached"
                } else {
                    "link dropped"
                },
                short(session)
            ),
            Self::TerminalResumed {
                session,
                parked_secs,
                lost,
            } => format!(
                "came back to session {} after {}, {lost} bytes of output lost",
                short(session),
                how_long(*parked_secs)
            ),
            Self::ForwardOpened { target } => format!("forwarding to {target}"),
            Self::ForwardClosed {
                target,
                duration_secs,
                sent,
                received,
            } => format!(
                "the forward to {target} ended after {}, {sent} up and {received} down",
                how_long(*duration_secs)
            ),
            Self::ForwardRefused { target, reason } => {
                format!("refused a forward to {target}: {reason}")
            }
            Self::FileTransferred {
                way,
                path,
                size,
                outcome,
            } => {
                let verb = if way == "in" { "took" } else { "sent" };
                format!("{verb} {path} ({size} bytes): {outcome}")
            }
            Self::Refused { reason } => format!("refused: {reason}"),
        }
    }
}

/// Permission names, for a sentence.
fn names(granted: &[Permission]) -> String {
    if granted.is_empty() {
        return "nothing".to_owned();
    }
    granted
        .iter()
        .map(|p| p.name())
        .collect::<Vec<_>>()
        .join(", ")
}

/// A duration a person reads rather than counts.
fn how_long(secs: u64) -> String {
    match secs {
        0 => "no time at all".to_owned(),
        1 => "1 second".to_owned(),
        s if s < 60 => format!("{s} seconds"),
        s if s < 3600 => format!("{} minutes", s / 60),
        s => format!("{} hours", s / 3600),
    }
}

/// Enough of a session identifier to recognise, since the whole thing is
/// thirty-two characters nobody reads.
fn short(session: &str) -> String {
    session.chars().take(8).collect()
}

/// Seconds since the epoch, or 0 if the clock is before it.
fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// An append-only record on the local filesystem.
///
/// One JSON object per line. Chosen so the file is readable with `tail`,
/// grep-able by a person looking for a fingerprint, and parseable by a program
/// — and so a partially-written final line can be discarded without losing
/// everything before it, which a single JSON array could not survive.
#[derive(Debug, Clone)]
pub struct Log {
    path: PathBuf,
}

impl Log {
    /// A log at this path. The file is created on first write.
    #[must_use]
    pub fn at(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// Where the log lives.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Append one entry.
    ///
    /// Opened in append mode for every write rather than holding a handle. That
    /// is slower and it is correct: an append-mode write of less than a pipe
    /// buffer is atomic, so two processes writing concurrently interleave whole
    /// lines rather than halves of two.
    ///
    /// `fsync` is deliberately **not** called. A record that survives a power
    /// cut is worth less than a session that does not stutter every time
    /// something is logged, and the entries that matter for an investigation
    /// are minutes old, not milliseconds.
    pub fn append(&self, entry: &Entry) -> Result<(), AuditError> {
        let mut line =
            serde_json::to_string(entry).map_err(|e| AuditError::Encode(e.to_string()))?;
        line.push('\n');

        if let Some(dir) = self.path.parent() {
            fs::create_dir_all(dir).map_err(|source| AuditError::Io {
                path: dir.to_owned(),
                source,
            })?;
        }

        let mut f = open_append_owner_only(&self.path).map_err(|source| AuditError::Io {
            path: self.path.clone(),
            source,
        })?;
        f.write_all(line.as_bytes())
            .map_err(|source| AuditError::Io {
                path: self.path.clone(),
                source,
            })
    }

    /// Read every entry, skipping any line that does not parse.
    ///
    /// A malformed line is skipped rather than failing the read. The last line
    /// of an append-only log can be a partial write after a crash, and refusing
    /// to show a user their history because of one truncated line would be the
    /// wrong trade.
    pub fn read(&self) -> Result<Vec<Entry>, AuditError> {
        let text = match fs::read_to_string(&self.path) {
            Ok(t) => t,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(source) => {
                return Err(AuditError::Io {
                    path: self.path.clone(),
                    source,
                });
            }
        };
        Ok(text
            .lines()
            .filter(|l| !l.trim().is_empty())
            .filter_map(|l| serde_json::from_str::<Entry>(l).ok())
            .collect())
    }

    /// Entries for one peer.
    pub fn for_peer(&self, peer_key: &[u8; KEY_LEN]) -> Result<Vec<Entry>, AuditError> {
        let want: String = peer_key.iter().map(|b| format!("{b:02x}")).collect();
        Ok(self
            .read()?
            .into_iter()
            .filter(|e| e.peer_key == want)
            .collect())
    }

    /// Apply the retention policy: an age, a size, or both.
    ///
    /// **One function for both rules**, because two would eventually disagree
    /// about what to keep. They fail differently and both are needed: a
    /// machine with a hundred sessions a day fills a disk long before an age
    /// limit expires, and a size limit alone would silently forget a quiet
    /// machine's whole history the week somebody needed it. Whichever rule
    /// bites, the oldest entries go first.
    ///
    /// `None` for either is no limit of that kind. Both `None` reads the file
    /// and writes nothing, which is what a person who set `none` asked for.
    ///
    /// # Errors
    ///
    /// If the log cannot be read, encoded or replaced.
    pub fn retain(
        &self,
        keep_secs: Option<u64>,
        max_bytes: Option<u64>,
        now: u64,
    ) -> Result<Pruned, AuditError> {
        let all = self.read()?;
        let before = all.len();

        // Age first. It is the rule somebody wrote down, and applying it
        // before the ceiling means the ceiling only ever removes entries the
        // age was willing to keep.
        let mut keep: Vec<&Entry> = match keep_secs {
            Some(secs) => {
                let cutoff = now.saturating_sub(secs);
                all.iter().filter(|e| e.at >= cutoff).collect()
            }
            None => all.iter().collect(),
        };
        let too_old = before.saturating_sub(keep.len());

        // Then the ceiling, oldest first. Measured on the encoded lines
        // rather than the entries, because bytes on a disk is what a ceiling
        // in megabytes is about.
        let mut over_size = 0usize;
        if let Some(most) = max_bytes {
            let mut lines: Vec<(usize, usize)> = Vec::with_capacity(keep.len());
            for (i, e) in keep.iter().enumerate() {
                let line =
                    serde_json::to_string(e).map_err(|e| AuditError::Encode(e.to_string()))?;
                lines.push((i, line.len().saturating_add(1)));
            }
            let mut total: u64 = lines.iter().map(|(_, n)| *n as u64).sum();
            let mut drop_before = 0usize;
            while total > most && drop_before < lines.len() {
                total = total.saturating_sub(lines.get(drop_before).map_or(0, |(_, n)| *n as u64));
                drop_before = drop_before.saturating_add(1);
            }
            if drop_before > 0 {
                over_size = drop_before;
                keep.drain(..drop_before);
            }
        }

        let removed = too_old.saturating_add(over_size);
        if removed == 0 {
            return Ok(Pruned {
                too_old: 0,
                over_size: 0,
                kept: keep.len(),
            });
        }

        let mut text = String::new();
        for e in &keep {
            let line = serde_json::to_string(e).map_err(|e| AuditError::Encode(e.to_string()))?;
            text.push_str(&line);
            text.push('\n');
        }

        // Rewritten through a temporary file: a truncate-then-write would lose
        // the whole history if the process died between the two.
        //
        // **This is the only path in this crate that does not append**, and it
        // is why the file's append-only-ness is a property of the writers
        // rather than of the filesystem. What it may do is bounded: it can
        // only ever write a prefix-free subset of what it read, and
        // `retaining_rewrites_nothing_it_keeps` holds it to that.
        let tmp = self.path.with_extension("tmp");
        write_owner_only(&tmp, text.as_bytes()).map_err(|source| AuditError::Io {
            path: tmp.clone(),
            source,
        })?;
        fs::rename(&tmp, &self.path).map_err(|source| {
            let _ = fs::remove_file(&tmp);
            AuditError::Io {
                path: self.path.clone(),
                source,
            }
        })?;
        Ok(Pruned {
            too_old,
            over_size,
            kept: keep.len(),
        })
    }
}

/// What a retention pass removed, and what is left.
///
/// The two causes are counted apart because they mean different things to
/// whoever reads the line: an age limit doing its job is routine, and a size
/// ceiling biting says the machine is busier than the policy expected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Pruned {
    /// Removed for being older than the age limit.
    pub too_old: usize,
    /// Removed to fit inside the size ceiling.
    pub over_size: usize,
    /// Still there afterwards.
    pub kept: usize,
}

impl Pruned {
    /// How many entries went.
    #[must_use]
    pub const fn removed(&self) -> usize {
        self.too_old.saturating_add(self.over_size)
    }

    /// A line for somebody who asked, or nothing when nothing happened.
    ///
    /// `None` rather than an empty string, so a caller cannot print a blank
    /// line about a pass that did nothing — which on a host that prunes daily
    /// would be a line a day saying nothing.
    #[must_use]
    pub fn describe(&self) -> Option<String> {
        let entries = |n: usize| {
            if n == 1 {
                "1 entry".to_owned()
            } else {
                format!("{n} entries")
            }
        };
        match (self.too_old, self.over_size) {
            (0, 0) => None,
            (old, 0) => Some(format!("{} past its retention", entries(old))),
            (0, big) => Some(format!(
                "{} removed to stay inside the size limit",
                entries(big)
            )),
            (old, big) => Some(format!(
                "{} past its retention, and {} more to stay inside the size limit",
                entries(old),
                entries(big)
            )),
        }
    }
}

/// Open for append with owner-only permissions, set at creation.
#[cfg(unix)]
fn open_append_owner_only(path: &Path) -> io::Result<fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    fs::OpenOptions::new()
        .append(true)
        .create(true)
        .mode(0o600)
        .open(path)
}

#[cfg(not(unix))]
fn open_append_owner_only(path: &Path) -> io::Result<fs::File> {
    fs::OpenOptions::new().append(true).create(true).open(path)
}

/// Write a whole file with owner-only permissions.
#[cfg(unix)]
fn write_owner_only(path: &Path, bytes: &[u8]) -> io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    f.write_all(bytes)?;
    f.sync_all()
}

#[cfg(not(unix))]
fn write_owner_only(path: &Path, bytes: &[u8]) -> io::Result<()> {
    fs::write(path, bytes)
}

/// Build the entry for a terminal session ending.
#[must_use]
pub fn terminal_closed(duration_secs: u64, exit: Option<&TerminalExit>) -> Event {
    Event::TerminalClosed {
        duration_secs,
        exit_code: exit.and_then(|e| e.code),
        exit_signal: exit.and_then(|e| e.signal),
    }
}
