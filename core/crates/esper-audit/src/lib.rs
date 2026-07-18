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
    /// A device was paired.
    Paired {
        /// What the device was called at pairing. Peer-supplied.
        name: String,
    },
    /// A device was revoked, and how many live sessions that closed.
    Revoked { sessions_closed: usize },
    /// A session opened, with what it was actually granted.
    ///
    /// The granted set, not the requested set: what a controller *asked* for is
    /// not a fact about this machine, and recording it would make the log read
    /// as though the request had been honoured.
    SessionOpened {
        granted: Vec<Permission>,
        /// How the peer was reached, so "relayed" is visible after the fact.
        tier: String,
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
    /// A session was refused, and why.
    ///
    /// Refusals matter as much as acceptances: a run of them is what an attack
    /// looks like from this side.
    Refused { reason: String },
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
    /// What happened.
    #[serde(flatten)]
    pub event: Event,
}

impl Entry {
    /// Build an entry for a peer.
    #[must_use]
    pub fn new(peer_key: &[u8; KEY_LEN], event: Event, at: u64) -> Self {
        Self {
            at,
            peer: Fingerprint::of(peer_key).inline(),
            peer_key: peer_key.iter().map(|b| format!("{b:02x}")).collect(),
            event,
        }
    }

    /// Build an entry stamped with the current time.
    #[must_use]
    pub fn now(peer_key: &[u8; KEY_LEN], event: Event) -> Self {
        Self::new(peer_key, event, unix_now())
    }
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

    /// Drop entries older than `keep_secs`, rewriting the file.
    ///
    /// Returns how many were removed. Retention is a real feature rather than
    /// housekeeping: a log that grows without bound on a server nobody looks at
    /// eventually fills the disk, and a retention policy the user set must
    /// actually delete things or it is a lie.
    pub fn prune(&self, keep_secs: u64, now: u64) -> Result<usize, AuditError> {
        let all = self.read()?;
        let cutoff = now.saturating_sub(keep_secs);
        let keep: Vec<&Entry> = all.iter().filter(|e| e.at >= cutoff).collect();
        let removed = all.len().saturating_sub(keep.len());
        if removed == 0 {
            return Ok(0);
        }

        let mut text = String::new();
        for e in keep {
            let line = serde_json::to_string(e).map_err(|e| AuditError::Encode(e.to_string()))?;
            text.push_str(&line);
            text.push('\n');
        }

        // Rewritten through a temporary file: a truncate-then-write would lose
        // the whole history if the process died between the two.
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
        Ok(removed)
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
