//! Putting the display server back when the host that changed it is gone.
//!
//! # The problem this exists for
//!
//! A display server keeps most of what a client asks of it only while that
//! client is connected: windows, pixmaps and grabs are destroyed when the
//! connection closes. **Its configuration is not.** Turning a monitor off,
//! moving a device between masters, changing a mode — those outlive the
//! process that asked, and they outlive it whether it exited cleanly, panicked,
//! was killed, or was shot by the kernel for using too much memory.
//!
//! So a host that blanks somebody's screen and then dies leaves them looking
//! at a dark monitor with nothing running to bring it back. That is not a
//! theoretical objection: it is the reason two features in this product were
//! measured, written up and **not built** ([the privacy screen][1] and
//! blocking the host's local input), because a remote-desktop tool that can
//! brick the desk it runs on is worse than one that is missing a feature.
//!
//! [1]: the findings of 2026-09-06 and 2026-09-07
//!
//! # How it works
//!
//! A second process — this binary, re-executed — is told what to undo before
//! anything is changed, and undoes it when the host goes away.
//!
//! **The death signal is the pipe.** The keeper reads from a pipe whose only
//! writer is the host; when the host dies for *any* reason the write end
//! closes and the read returns end-of-file. Nothing has to poll, nothing has
//! to guess a timeout, and no way of dying is special: a `SIGKILL` closes a
//! pipe exactly as a clean exit does.
//!
//! That leaves one hole, and it is covered separately: if the keeper itself is
//! killed, nothing is listening. So every record is also written to a
//! **journal** on disk, and a host starting up applies whatever it finds
//! there. Between the two, the state comes back unless the machine loses power
//! — and if it loses power the display server goes with it, which means the
//! state was never persistent in the first place.
//!
//! # What it deliberately does not do
//!
//! It does not know what any record *means*. The records are applied by a
//! function the caller supplies, because what puts a monitor back is X11's
//! business and this crate's business is only *when*. That keeps the timing —
//! the part that is hard to get right and impossible to see when it is wrong —
//! testable with no display anywhere near it.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

/// Something the host changed that will outlive it.
///
/// **Described rather than closed over.** The undo has to survive the death of
/// the process that decided on it, so it cannot be a closure — it has to be
/// data, in a file, that another process can read.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "undo", rename_all = "snake_case")]
pub enum Undo {
    /// Put a RandR CRTC back the way it was found.
    ///
    /// The fields are the arguments of the request that restores it, so
    /// nothing has to be worked out again later by a process that cannot ask
    /// what the state used to be — by the time it runs, it is not.
    Crtc {
        crtc: u32,
        x: i16,
        y: i16,
        mode: u32,
        rotation: u16,
        outputs: Vec<u32>,
        /// What a person would call the thing this puts back, for the line
        /// printed when it happens.
        what: String,
    },
    /// Attach an input device back to the master it was detached from.
    ///
    /// The other half of the same hazard as [`Undo::Crtc`], and a worse one: a
    /// dark monitor still leaves somebody able to type blind, and a floating
    /// keyboard leaves them with no way to say anything at all. Which is
    /// exactly why the feature that floats one may not exist without this.
    ///
    /// The fields are the arguments of the request that restores it, for the
    /// same reason: by the time this runs, nothing can ask what the state used
    /// to be.
    Slave {
        /// The physical device that was detached.
        device: u16,
        /// The master it belonged to.
        master: u16,
        /// What a person would call it.
        what: String,
    },
    /// Put the desktop's own background back on the root window.
    ///
    /// The third thing in this list that outlives the process, and the mildest
    /// of the three: somebody left looking at a plain colour where their
    /// wallpaper was has lost nothing but the wallpaper. It is here for the
    /// same reason as the others — a host that dies must not leave a change
    /// nobody asked for — and because the way out is one request that only
    /// something still running can make.
    Backdrop {
        /// The pixmap the desktop published as its background, as it was found.
        ///
        /// **The desktop's, not ours.** It belongs to whatever draws the
        /// wallpaper, so a desktop that has since gone leaves an id nothing
        /// owns and the server refuses it — which is reported rather than
        /// hidden, because the alternative is a plain root and no line saying
        /// why.
        pixmap: u32,
        /// What a person would call it.
        what: String,
    },
}

impl Undo {
    /// What to say when this is applied.
    #[must_use]
    pub fn describe(&self) -> String {
        match self {
            Self::Crtc { what, .. } | Self::Slave { what, .. } | Self::Backdrop { what, .. } => {
                what.clone()
            }
        }
    }
}

/// Anything that went wrong keeping or restoring state.
#[derive(Debug, thiserror::Error)]
pub enum KeeperError {
    /// The keeper process could not be started.
    #[error("starting the keeper: {0}")]
    Spawn(String),
    /// The keeper did not say it was ready.
    ///
    /// **A caller must treat this as fatal to whatever it was about to do.**
    /// The whole arrangement is that nothing is changed until somebody is
    /// watching, and a keeper that has not answered is not watching.
    #[error("the keeper did not report itself ready: {0}")]
    NotReady(String),
    /// The journal could not be written.
    ///
    /// Also fatal for the same reason: the journal is what covers the keeper
    /// itself dying, and a change made with neither cover is the situation
    /// this crate exists to prevent.
    #[error("writing the restore journal at {path}: {detail}")]
    Journal { path: PathBuf, detail: String },
    /// The pipe to the keeper broke.
    #[error("talking to the keeper: {0}")]
    Pipe(String),
}

/// The word the keeper answers with when it is watching.
const READY: &str = "keeping";

/// The word that tells the keeper the host undid its own work.
const CANCEL: &str = "cancelled";

/// The environment variable that tells a re-executed engine to be the keeper.
///
/// One binary rather than two, for the reason the decoder workers give: the
/// two halves cannot be at different versions, which matters more than the
/// tidiness of a separate file.
pub const KEEPER_ENV: &str = "ESPER_KEEPER";

/// The argument the keeper is invoked with.
pub const KEEPER_ARG: &str = "keeper";

/// Where the journal lives, given the home directory.
#[must_use]
pub fn journal_path(home: &Path) -> PathBuf {
    home.join("restore.json")
}

/// Write the journal, replacing whatever was there.
///
/// Through a temporary and a rename, so a host that dies mid-write leaves
/// either the old journal or the new one and never half of either.
fn write_journal(path: &Path, undos: &[Undo]) -> Result<(), KeeperError> {
    let fail = |detail: String| KeeperError::Journal {
        path: path.to_path_buf(),
        detail,
    };
    if undos.is_empty() {
        // Nothing to restore is the ordinary state, and an empty file left
        // behind would make every later start look like a recovery.
        match std::fs::remove_file(path) {
            Ok(()) => return Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(fail(e.to_string())),
        }
    }
    let text = serde_json::to_string_pretty(undos).map_err(|e| fail(e.to_string()))?;
    let temp = path.with_extension("json.new");
    std::fs::write(&temp, text.as_bytes()).map_err(|e| fail(e.to_string()))?;
    std::fs::rename(&temp, path).map_err(|e| fail(e.to_string()))
}

/// Read the journal, or an empty list if there is nothing to do.
///
/// A journal that cannot be parsed is **reported and kept**, not deleted: it
/// is the record of something that needs putting back, and a file this build
/// cannot read may be readable by the next one.
pub fn read_journal(path: &Path) -> Result<Vec<Undo>, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text).map_err(|e| e.to_string()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(e.to_string()),
    }
}

/// Forget the journal, because everything in it has been applied.
pub fn clear_journal(path: &Path) -> Result<(), KeeperError> {
    write_journal(path, &[])
}

/// The host's handle on a keeper that is watching it.
///
/// Dropping it **cancels**: a host that finishes cleanly has already put
/// everything back itself, and a keeper that then restored again would fight
/// whatever the machine's owner did in the meantime.
pub struct Keeper {
    child: std::process::Child,
    to_keeper: Option<std::process::ChildStdin>,
    journal: PathBuf,
    /// What has been registered, in the order it was.
    undos: Vec<Undo>,
    cancelled: bool,
}

impl std::fmt::Debug for Keeper {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Keeper")
            .field("pid", &self.child.id())
            .field("watching", &self.undos.len())
            .finish_non_exhaustive()
    }
}

impl Keeper {
    /// Start a keeper, and wait until it says it is watching.
    ///
    /// # Errors
    ///
    /// If the process cannot be started or does not answer. Both are fatal to
    /// whatever the caller was about to change: the point is that nothing is
    /// changed until somebody is watching.
    /// `program` is the binary to re-execute — this one, as the caller sees
    /// it. A parameter rather than `current_exe()` read in here, so the thing
    /// that spawns a second copy of the product can be exercised by a test
    /// without the test binary re-running itself.
    pub fn start(program: &Path, home: &Path) -> Result<Self, KeeperError> {
        let journal = journal_path(home);
        let mut command = std::process::Command::new(program);
        command
            .arg(KEEPER_ARG)
            .env(KEEPER_ENV, "1")
            .env("ESPER_HOME", home)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::inherit());
        // **Its own process group.** Otherwise a `kill` of the host's group —
        // which is what closing a terminal does — takes the keeper with it,
        // and the one case the keeper exists for is the one where it is gone.
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt as _;
            command.process_group(0);
        }
        let mut child = command
            .spawn()
            .map_err(|e| KeeperError::Spawn(e.to_string()))?;

        let to_keeper = child
            .stdin
            .take()
            .ok_or_else(|| KeeperError::Spawn("the keeper has no stdin".to_owned()))?;
        let from_keeper = child
            .stdout
            .take()
            .ok_or_else(|| KeeperError::Spawn("the keeper has no stdout".to_owned()))?;
        let mut lines = BufReader::new(from_keeper);
        let mut hello = String::new();
        lines
            .read_line(&mut hello)
            .map_err(|e| KeeperError::NotReady(e.to_string()))?;
        if hello.trim() != READY {
            let _ = child.kill();
            return Err(KeeperError::NotReady(format!(
                "it said {:?} rather than {READY:?}",
                hello.trim()
            )));
        }

        Ok(Self {
            child,
            to_keeper: Some(to_keeper),
            journal,
            undos: Vec::new(),
            cancelled: false,
        })
    }

    /// Register something to undo, **before** doing it.
    ///
    /// The order matters and is the whole discipline: a change made before its
    /// undo is registered is a change nobody can put back if the next
    /// instruction never runs.
    ///
    /// # Errors
    ///
    /// If the journal cannot be written or the keeper cannot be told. Either
    /// way the caller must not make the change.
    pub fn keep(&mut self, undo: Undo) -> Result<(), KeeperError> {
        let mut undos = self.undos.clone();
        undos.push(undo.clone());
        // **The journal first.** It is what covers the keeper dying, and the
        // ordering is chosen so the worst case is a journal entry for a change
        // that never happened — which restores something to the state it is
        // already in, and is harmless — rather than a change with no record.
        write_journal(&self.journal, &undos)?;
        let line = serde_json::to_string(&undo).map_err(|e| KeeperError::Pipe(e.to_string()))?;
        let pipe = self
            .to_keeper
            .as_mut()
            .ok_or_else(|| KeeperError::Pipe("the keeper was already let go".to_owned()))?;
        writeln!(pipe, "{line}").map_err(|e| KeeperError::Pipe(e.to_string()))?;
        pipe.flush().map_err(|e| KeeperError::Pipe(e.to_string()))?;
        self.undos = undos;
        Ok(())
    }

    /// Say that the host has put everything back itself.
    ///
    /// After this the keeper does nothing when the host goes away, which is
    /// what a clean shutdown wants: the alternative is a second restore, later,
    /// undoing whatever the person at the machine has done since.
    pub fn cancel(&mut self) {
        if self.cancelled {
            return;
        }
        self.cancelled = true;
        if let Some(pipe) = self.to_keeper.as_mut() {
            let _ = writeln!(pipe, "{CANCEL}");
            let _ = pipe.flush();
        }
        // Dropping the pipe is what ends the keeper: it reads end-of-file
        // after the cancellation and exits without restoring.
        self.to_keeper = None;
        let _ = self.child.wait();
        let _ = clear_journal(&self.journal);
    }

    /// What this keeper is holding, for the host to report.
    #[must_use]
    pub fn watching(&self) -> usize {
        self.undos.len()
    }
}

impl Drop for Keeper {
    fn drop(&mut self) {
        self.cancel();
    }
}

/// How many times a restore is attempted before it is given up on.
///
/// **Because one attempt is not enough for the one action that must not fail
/// quietly.** Measured: applying the undo immediately after the host's process
/// died failed two runs in three with "connection reset by peer" — the display
/// server refusing a *new* connection while it is still tearing down the dead
/// client's. The same is true of anything else that could be happening to a
/// machine at the moment a host dies on it, which is not a moment to be
/// fastidious about trying once.
const ATTEMPTS: usize = 6;

/// How long between attempts. Six of these is about a second.
const BETWEEN: std::time::Duration = std::time::Duration::from_millis(180);

/// Apply one undo, trying again while it fails.
///
/// Returns how many attempts it took, so a caller can say when it was not the
/// first — a restore that needed four goes is worth knowing about even though
/// it worked.
fn try_hard<A>(apply: &mut A, undo: &Undo) -> Result<usize, String>
where
    A: FnMut(&Undo) -> Result<(), String>,
{
    let mut last = String::new();
    for attempt in 1..=ATTEMPTS {
        match apply(undo) {
            Ok(()) => return Ok(attempt),
            Err(e) => {
                last = e;
                if attempt < ATTEMPTS {
                    std::thread::sleep(BETWEEN);
                }
            }
        }
    }
    Err(format!("{last}, after {ATTEMPTS} attempts"))
}

/// The keeper's own loop: watch the pipe, and restore when it closes.
///
/// `apply` is what knows how to put a thing back. It is a parameter because
/// what restores a monitor is X11's business and this crate's business is the
/// timing — which is also why this function can be tested with no display in
/// sight.
///
/// Returns how many records it applied, which is zero on the ordinary path.
pub fn run_keeper<R, W, A>(input: R, mut output: W, home: &Path, mut apply: A) -> usize
where
    R: std::io::Read,
    W: Write,
    A: FnMut(&Undo) -> Result<(), String>,
{
    // **Ready before anything else.** The host waits for this word and does
    // not touch the display server until it arrives.
    if writeln!(output, "{READY}").is_err() || output.flush().is_err() {
        return 0;
    }

    let mut undos: Vec<Undo> = Vec::new();
    let mut cancelled = false;
    for line in BufReader::new(input).lines() {
        let Ok(line) = line else { break };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if line == CANCEL {
            cancelled = true;
            continue;
        }
        match serde_json::from_str::<Undo>(line) {
            Ok(undo) => undos.push(undo),
            // A record this build cannot read is still evidence that something
            // was changed, and the journal has it: saying so is better than
            // exiting, which would leave nobody watching at all.
            Err(e) => tracing::warn!("the keeper could not read a record: {e}"),
        }
    }

    if cancelled {
        return 0;
    }

    // End of the pipe with no cancellation: the host is gone and did not put
    // its own work back.
    //
    // **Said before anything is applied**, and said even when there is nothing
    // to do. This is the keeper's only voice, and "the host went away with
    // nothing registered" and "the keeper never woke up at all" are otherwise
    // the same silence — which cost an afternoon of guessing once.
    eprintln!(
        "esper: the host went away with {} thing(s) to put back",
        undos.len()
    );
    let mut applied = 0usize;
    for undo in &undos {
        match try_hard(&mut apply, undo) {
            Ok(attempts) => {
                let effort = if attempts > 1 {
                    format!(" (after {attempts} attempts)")
                } else {
                    String::new()
                };
                eprintln!(
                    "esper: put back {} after the host went away{effort}",
                    undo.describe()
                );
                applied = applied.saturating_add(1);
            }
            Err(e) => eprintln!("esper: could not put back {}: {e}", undo.describe()),
        }
    }
    if applied == undos.len() {
        let _ = clear_journal(&journal_path(home));
    }
    applied
}
