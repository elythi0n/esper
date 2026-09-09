//! One file, moving, on a stream of its own.
//!
//! # What is here and what is not
//!
//! Everything that is a *decision* — whether to accept, where to resume from,
//! whether what arrived is what was sent — is in `esper-files`, where it can
//! be driven directly and made to fail on purpose. What is here is the stream:
//! read a frame, move some bytes, read another frame.
//!
//! # Which end does what
//!
//! The controller opens the stream whichever way the file is going, so a
//! controller never has to accept one. Pushing, it sends an offer and then the
//! body; pulling, it asks and then reads the body. **A half of the stream
//! carries frames until it starts carrying a body, and only the sending half
//! ever does**, which is what lets one vocabulary work in both directions.
//!
//! # Where the blocking goes
//!
//! Starting a transfer can hash a partial file, and finishing one waits for a
//! disk. Both are seconds on a large file, and seconds on the engine's runtime
//! are seconds of frozen video — so both happen on a blocking thread. The
//! per-chunk write does not: a buffered write is microseconds, and moving each
//! sixty-four kilobytes to another thread would cost more than it saves.

use std::sync::Arc;

use esper_audit::{Entry, Event, Log};
use esper_files::{AT_A_TIME, Limits, Offer, Receiving, SafePath, Sending, Vault};
use esper_proto::{FileAccept, FileEnd, FileMessage, FileOffer, FileRefusal, FileWant, Permission};
use esper_transport::Control;

/// How much a file stream's data matters against everything else on the
/// connection.
///
/// Below zero, which is where everything else sits. Media is already ahead of
/// all of it — quinn writes datagram frames into a packet before stream frames
/// — so this is about the session's *control* streams: a keyframe request must
/// not queue behind a gigabyte.
const BACKGROUND: i32 = -1;

/// What this machine will do with files for one session.
pub struct Files {
    vault: Arc<Vault>,
    limits: Limits,
    /// What the far end may do to this machine's file directory.
    granted: Vec<Permission>,
    /// **One transfer at a time in each direction.** Ten at once do not go
    /// faster than one — the link is the same link — and they turn one slow
    /// transfer into ten.
    taking: Arc<tokio::sync::Semaphore>,
    giving: Arc<tokio::sync::Semaphore>,
    log: Option<Log>,
    device: [u8; 32],
    /// What to put in front of a line this end says.
    indent: &'static str,
}

impl std::fmt::Debug for Files {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Files")
            .field("root", &self.vault.shown())
            .field("confined", &self.vault.how())
            .finish_non_exhaustive()
    }
}

impl Files {
    /// Open a file directory for a session, if this machine serves files and
    /// the grant asks for one.
    ///
    /// # Errors
    ///
    /// With the reason, so a session can say it out loud rather than being
    /// silently without file transfer — which from the far end looks exactly
    /// like a directory that is empty.
    pub fn open(
        root: &std::path::Path,
        limits: Limits,
        granted: &[Permission],
        log: Option<Log>,
        device: [u8; 32],
        indent: &'static str,
    ) -> Result<Self, String> {
        if !granted.contains(&Permission::FilesOut) && !granted.contains(&Permission::FilesIn) {
            return Err("not granted for this session".to_owned());
        }
        let vault = Vault::open(root).map_err(|e| e.to_string())?;
        Ok(Self {
            vault: Arc::new(vault),
            limits,
            granted: granted.to_vec(),
            taking: Arc::new(tokio::sync::Semaphore::new(1)),
            giving: Arc::new(tokio::sync::Semaphore::new(1)),
            log,
            device,
            indent,
        })
    }

    /// Where files go, and how a peer is kept there.
    #[must_use]
    pub fn described(&self) -> String {
        format!(
            "{} — {}",
            self.vault.shown().display(),
            self.vault.how().describe()
        )
    }

    fn may(&self, p: Permission) -> bool {
        self.granted.contains(&p)
    }

    fn note(&self, direction: &str, path: &str, size: u64, outcome: &str) {
        println!(
            "{}files      {direction} {path} ({size} bytes): {outcome}",
            self.indent
        );
        if let Some(log) = &self.log
            && let Err(e) = log.append(&Entry::now(
                &self.device,
                Event::FileTransferred {
                    way: direction.to_owned(),
                    path: path.to_owned(),
                    size,
                    outcome: outcome.to_owned(),
                },
            ))
        {
            eprintln!("{}· the transfer could not be recorded: {e}", self.indent);
        }
    }
}

/// Serve one file stream, whichever way it turns out to go.
pub async fn serve(mut control: Control, files: Arc<Files>) {
    // The far end opened it and speaks first, so this end waits — with a
    // deadline, because a stream that says nothing is a stream holding a
    // transfer slot.
    let opening = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        control.recv::<FileMessage>(),
    )
    .await;
    let first = match opening {
        Ok(Ok(message)) if message.is_well_formed() => message,
        Ok(Ok(message)) => {
            tracing::debug!(
                what = message.name(),
                "a file stream opened with a bad message"
            );
            let _ = control
                .send(&FileMessage::Refuse(FileRefusal::BadName))
                .await;
            return;
        }
        Ok(Err(e)) => {
            tracing::debug!(error = %e, "a file stream ended before it said anything");
            return;
        }
        Err(_) => {
            let _ = control
                .send(&FileMessage::Refuse(FileRefusal::Failed))
                .await;
            return;
        }
    };

    match first {
        FileMessage::Offer(offer) => take(&mut control, &files, offer).await,
        FileMessage::Want(want) => give(&mut control, &files, want).await,
        other => {
            tracing::debug!(
                what = other.name(),
                "a file stream opened with the wrong message"
            );
            let _ = control
                .send(&FileMessage::Refuse(FileRefusal::Failed))
                .await;
        }
    }
}

/// The far end is sending this machine a file.
async fn take(control: &mut Control, files: &Files, wire: FileOffer) {
    if !files.may(Permission::FilesIn) {
        let _ = control
            .send(&FileMessage::Refuse(FileRefusal::NotPermitted))
            .await;
        files.note("refused", &shown(&wire.path), wire.bytes, "not permitted");
        return;
    }
    let offer = match Offer::check(&wire) {
        Ok(offer) => offer,
        Err(why) => {
            let _ = control.send(&FileMessage::Refuse(why)).await;
            files.note("refused", &shown(&wire.path), wire.bytes, why.message());
            return;
        }
    };
    let Ok(_slot) = files.taking.clone().try_acquire_owned() else {
        let _ = control.send(&FileMessage::Refuse(FileRefusal::Busy)).await;
        return;
    };

    // Beginning can read and hash a partial, which is a disk's worth of work.
    let vault = Arc::clone(&files.vault);
    let limits = files.limits;
    let for_thread = offer.clone();
    let started =
        tokio::task::spawn_blocking(move || Receiving::begin(&vault, &for_thread, limits)).await;
    let mut receiving = match started {
        Ok(Ok(receiving)) => receiving,
        Ok(Err(why)) => {
            let _ = control.send(&FileMessage::Refuse(why)).await;
            files.note("refused", offer.path.as_str(), offer.bytes, why.message());
            return;
        }
        Err(e) => {
            tracing::warn!(error = %e, "a transfer could not be started");
            let _ = control
                .send(&FileMessage::Refuse(FileRefusal::Failed))
                .await;
            return;
        }
    };

    let from = receiving.from();
    if control
        .send(&FileMessage::Accept(FileAccept { from }))
        .await
        .is_err()
    {
        return;
    }
    if from > 0 {
        println!(
            "{}files      carrying on {} from {from} bytes in",
            files.indent,
            offer.path.as_str()
        );
    }

    let mut buffer = vec![0u8; AT_A_TIME];
    while receiving.left() > 0 {
        let got = match control.recv_raw(&mut buffer).await {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) => {
                tracing::debug!(error = %e, "a transfer stopped part way");
                break;
            }
        };
        let piece = buffer.get(..got).unwrap_or_default();
        if let Err(e) = receiving.take(piece) {
            tracing::warn!(error = %e, "a transfer could not be written");
            let _ = control.send(&FileMessage::Done(FileEnd::Failed)).await;
            files.note("in", offer.path.as_str(), offer.bytes, &e.to_string());
            return;
        }
    }

    // Finishing waits for the disk, and on a large file that is seconds.
    let vault = Arc::clone(&files.vault);
    let end = tokio::task::spawn_blocking(move || receiving.finish(&vault))
        .await
        .unwrap_or(FileEnd::Failed);
    let _ = control.send(&FileMessage::Done(end)).await;
    files.note("in", offer.path.as_str(), offer.bytes, end.message());
}

/// The far end is asking this machine for a file.
async fn give(control: &mut Control, files: &Files, want: FileWant) {
    if !files.may(Permission::FilesOut) {
        let _ = control
            .send(&FileMessage::Refuse(FileRefusal::NotPermitted))
            .await;
        files.note("refused", &shown(&want.path), 0, "not permitted");
        return;
    }
    let path = match SafePath::parse(&want.path) {
        Ok(path) => path,
        Err(why) => {
            let _ = control
                .send(&FileMessage::Refuse(FileRefusal::BadName))
                .await;
            files.note("refused", &shown(&want.path), 0, &why.message());
            return;
        }
    };
    let Ok(_slot) = files.giving.clone().try_acquire_owned() else {
        let _ = control.send(&FileMessage::Refuse(FileRefusal::Busy)).await;
        return;
    };

    // Opening reads the whole file once, to hash it.
    let vault = Arc::clone(&files.vault);
    let limits = files.limits;
    let for_thread = path.clone();
    let opened =
        tokio::task::spawn_blocking(move || Sending::begin(&vault, &for_thread, limits)).await;
    let mut sending = match opened {
        Ok(Ok(sending)) => sending,
        Ok(Err(why)) => {
            let _ = control.send(&FileMessage::Refuse(why)).await;
            files.note("refused", path.as_str(), 0, why.message());
            return;
        }
        Err(e) => {
            tracing::warn!(error = %e, "a file could not be opened to send");
            let _ = control
                .send(&FileMessage::Refuse(FileRefusal::Failed))
                .await;
            return;
        }
    };
    let offer = sending.offer().clone();
    if control
        .send(&FileMessage::Have(offer.to_wire()))
        .await
        .is_err()
    {
        return;
    }

    // The receiver says where to start, because the receiver is the one that
    // knows what it already has.
    let from = match control.recv::<FileMessage>().await {
        Ok(FileMessage::Accept(accept)) => accept.from,
        Ok(FileMessage::Refuse(why)) => {
            files.note("refused", path.as_str(), offer.bytes, why.message());
            return;
        }
        Ok(other) => {
            tracing::debug!(what = other.name(), "a puller said something unexpected");
            return;
        }
        Err(_) => return,
    };
    if let Err(e) = sending.resume_at(from) {
        tracing::debug!(error = %e, "a puller asked to carry on from somewhere impossible");
        let _ = control
            .send(&FileMessage::Refuse(FileRefusal::Failed))
            .await;
        return;
    }

    if let Err(e) = push(control, &mut sending).await {
        tracing::debug!(error = %e, "a transfer out stopped part way");
        files.note("out", path.as_str(), offer.bytes, "it stopped part way");
        return;
    }
    // The far end says how it went, on its own half.
    let end = match control.recv::<FileMessage>().await {
        Ok(FileMessage::Done(end)) => end.message().to_owned(),
        _ => "sent".to_owned(),
    };
    files.note("out", path.as_str(), offer.bytes, &end);
}

/// Read a file out onto the stream.
pub async fn push(control: &mut Control, sending: &mut Sending) -> Result<(), String> {
    push_reporting(control, sending, &mut |_, _| Going::On).await
}

/// Whether a transfer should keep going.
///
/// Returned by the progress callback, so **the same call that is told how far
/// it has got is the one that can stop it**. A separate cancellation channel
/// would be a second thing to poll in a loop whose whole job is to not stop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Going {
    /// Keep sending.
    On,
    /// Stop, and say so to the far end.
    Stop,
}

/// How often to report, in bytes.
///
/// **Not every chunk.** `AT_A_TIME` is small enough that a large file is tens
/// of thousands of chunks, and a progress line per chunk is a progress line
/// nobody can read and a lock nobody needs. Every quarter of a mebibyte is
/// about four reports a second on a slow link and forty on a fast one, which
/// is the range a person reads as "moving".
pub const REPORT_EVERY: u64 = 256 * 1024;

/// Push a file, reporting how far it has got and stopping when asked.
///
/// `progress` is called with the bytes sent so far and the total, and its
/// answer decides whether to carry on. A transfer stopped this way ends the
/// stream rather than the session: the far end sees a short file and says so,
/// which is what it already does for a link that broke.
///
/// # Errors
///
/// If the file cannot be read or the stream will not take the bytes.
pub async fn push_reporting(
    control: &mut Control,
    sending: &mut Sending,
    progress: &mut (dyn FnMut(u64, u64) -> Going + Send),
) -> Result<(), String> {
    // Asked for here rather than at the stream's opening, because this is the
    // point where a lot of bytes are about to be queued.
    let _ = control.set_priority(BACKGROUND);
    let mut buffer = vec![0u8; AT_A_TIME];
    let total = sending.bytes();
    let mut sent = sending.sent();
    let mut last_reported = sent;
    loop {
        let got = sending.read(&mut buffer).map_err(|e| e.to_string())?;
        if got == 0 {
            // **The last report is always made**, whatever the interval says.
            // A transfer that finished on 99% and never said so is a progress
            // display that looks stuck at the moment it is done.
            progress(total, total);
            return Ok(());
        }
        let piece = buffer.get(..got).unwrap_or_default();
        control.send_raw(piece).await.map_err(|e| e.to_string())?;
        sent = sent.saturating_add(got as u64);
        if sent.saturating_sub(last_reported) >= REPORT_EVERY {
            last_reported = sent;
            if progress(sent, total) == Going::Stop {
                return Err(format!(
                    "stopped after {sent} of {total} bytes, at this end's request"
                ));
            }
        }
    }
}

/// A path a peer sent, for this machine's own log.
///
/// **Never printed as it arrived.** A name is checked before it is used, and
/// a name that failed the check is exactly the one that might have a newline
/// in it — so what goes in the log is a description rather than the thing.
fn shown(raw: &str) -> String {
    if raw.chars().any(char::is_control) {
        return format!(
            "a name of {} bytes with control characters in it",
            raw.len()
        );
    }
    // Marked when it is cut, because a name that quietly loses its end is a
    // name the person reading the log takes for the whole one — and this one
    // came from somebody else's machine.
    if raw.chars().count() > MOST_SHOWN {
        return raw
            .chars()
            .take(MOST_SHOWN)
            .chain(std::iter::once('…'))
            .collect();
    }
    raw.chars().collect()
}

/// How much of a peer's name goes in the log.
///
/// A bound rather than a preference: without one, a peer can put a line of any
/// length into this machine's log by naming a file that way.
const MOST_SHOWN: usize = 120;

/// A flag that becomes true when somebody presses Ctrl-C.
///
/// **Installed once and shared**, because a second handler would replace the
/// first: a program that installed one per transfer would have only the last
/// one working, and a `cp` of several files would become uninterruptible
/// after the first.
///
/// The first press stops the transfer politely — the far end sees a short
/// file and says so, which is what it already does for a link that broke. A
/// second press is the runtime's own, and ends the process.
pub fn stop_on_interrupt() -> std::sync::Arc<std::sync::atomic::AtomicBool> {
    use std::sync::OnceLock;
    use std::sync::atomic::{AtomicBool, Ordering};
    static ASKED: OnceLock<std::sync::Arc<AtomicBool>> = OnceLock::new();
    std::sync::Arc::clone(ASKED.get_or_init(|| {
        let flag = std::sync::Arc::new(AtomicBool::new(false));
        let mine = std::sync::Arc::clone(&flag);
        tokio::spawn(async move {
            if tokio::signal::ctrl_c().await.is_ok() {
                eprintln!("\n  stopping   at your request, after the bytes already sent");
                mine.store(true, Ordering::Relaxed);
            }
        });
        flag
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **A name a peer sent never reaches the log as it arrived.**
    ///
    /// The log is this machine's own record of what happened to it, and the
    /// names in it come from somebody else. A newline in one splits a line and
    /// lets a peer write whatever it likes as though this machine had said it;
    /// a carriage return rewrites the line in a terminal; an escape sequence
    /// recolours or clears the screen of whoever is watching. All three are
    /// control characters, which is why the check is that broad rather than a
    /// list of the ones somebody thought of.
    ///
    /// What replaces it is a *description* — the length and the fact — because
    /// the name that failed the check is exactly the one not to repeat.
    #[test]
    fn a_name_with_control_characters_is_described_rather_than_printed() {
        for hostile in [
            "notes.txt\npaired with attacker",
            "notes.txt\r\npaired with attacker",
            "\u{1b}[2J\u{1b}[H all clear",
            "quiet\u{0}name",
            "tab\there",
        ] {
            let said = shown(hostile);
            assert!(
                !said.contains('\n') && !said.contains('\r') && !said.contains('\u{1b}'),
                "a control character reached the log: {said:?}"
            );
            assert!(
                said.starts_with("a name of ") && said.contains("control characters"),
                "a hostile name was not described: {said:?}"
            );
            assert!(
                !said.contains(hostile.trim()),
                "the description repeats the name it exists not to repeat: {said:?}"
            );
        }
    }

    #[test]
    fn an_ordinary_name_is_itself() {
        for fine in [
            "notes.txt",
            "a file with spaces.pdf",
            "réunion.md",
            "デスクトップ.txt",
        ] {
            assert_eq!(
                shown(fine),
                fine,
                "an ordinary name was not printed as it is"
            );
        }
    }

    /// A bound, because without one a peer can put a line of any length into
    /// this machine's log by naming a file that way — and the cut is marked,
    /// because a name that quietly loses its end is read as the whole one.
    #[test]
    fn a_very_long_name_is_cut_and_the_cut_is_marked() {
        let long = "a".repeat(500);
        let said = shown(&long);
        assert_eq!(
            said.chars().count(),
            MOST_SHOWN + 1,
            "the bound is {MOST_SHOWN} characters and a mark"
        );
        assert!(said.ends_with('…'), "the cut is not marked: {said:?}");

        // Exactly at the bound is not cut: an off-by-one here marks a name
        // that lost nothing.
        let exact = "b".repeat(MOST_SHOWN);
        assert_eq!(
            shown(&exact),
            exact,
            "a name exactly the bound was marked as cut"
        );
    }

    /// Characters, not bytes.
    ///
    /// The same trap the session list fell into: a name in a script that is
    /// not Latin has more bytes than characters, and cutting at a byte offset
    /// cuts one in half. Here it would put a broken sequence in the log.
    #[test]
    fn the_cut_falls_between_characters() {
        let long = "デ".repeat(200);
        let said = shown(&long);
        assert_eq!(said.chars().count(), MOST_SHOWN + 1);
        assert!(
            said.chars().take(MOST_SHOWN).all(|c| c == 'デ'),
            "the cut fell inside a character: {said:?}"
        );
    }
}
