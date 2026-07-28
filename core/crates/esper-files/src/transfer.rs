//! A file moving, and the decisions on both ends of it.
//!
//! # Why this is not in the engine
//!
//! What a session does with a file stream is read a frame, write some bytes,
//! and read another frame. Everything that is actually a *decision* — whether
//! to accept, where to resume from, what the partial is called, whether what
//! arrived is what was sent — is here, where it can be driven directly and
//! made to fail on purpose. A transfer that can only be tested by running two
//! machines is a transfer whose edge cases are never tested.
//!
//! # Nothing lands under its final name until it is whole
//!
//! Bytes go to a partial file in the destination's own directory, and the
//! destination gets its name in one `rename` after the hash has matched. A
//! process watching that directory sees no file or the complete one, never a
//! growing one — which is exactly what the thing people transfer most, a build
//! artefact something else is waiting for, actually needs.

use std::io::{Read as _, Seek as _, SeekFrom, Write as _};

use esper_proto::{FileEnd, FileOffer, FileRefusal};

use crate::{FileError, Running, SafePath, Sum, Vault};

/// How much is moved at a time.
///
/// A datagram's worth would be a syscall per packet; a megabyte would be a
/// megabyte of latency before the first byte lands. This is what a disk read
/// costs nothing extra for and what a stream can absorb without stalling.
pub const AT_A_TIME: usize = 64 * 1024;

/// After this, a partial transfer is the remains of something nobody wants.
///
/// Resumed before it, deleted after. Without the deleting, a transfer somebody
/// abandoned is a file that lives forever in a directory nobody looks at.
const WORTH_RESUMING: std::time::Duration = std::time::Duration::from_secs(24 * 60 * 60);

/// Room left over and above the file itself.
///
/// A filesystem that is exactly full is a filesystem where everything else on
/// the machine starts failing, so a transfer that would fit exactly is refused.
const SPARE: u64 = 16 * 1024 * 1024;

/// What this machine will accept.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Limits {
    /// The largest file this machine takes. `None` for no limit of its own.
    pub most_bytes: Option<u64>,
}

/// A file offer that has been checked.
///
/// The wire type is a string and a number; this is what is left after the name
/// rules have run. **A function that takes one of these cannot be handed a
/// path that was never checked.**
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Offer {
    pub path: SafePath,
    pub bytes: u64,
    pub hash: Sum,
    pub replace: bool,
}

impl Offer {
    /// Check what a peer sent.
    ///
    /// # Errors
    ///
    /// With the refusal to send back. A bad name and a name that leaves the
    /// directory are the same refusal on the wire, and different sentences in
    /// this machine's own log.
    pub fn check(wire: &FileOffer) -> Result<Self, FileRefusal> {
        let path = SafePath::parse(&wire.path).map_err(|why| {
            tracing::debug!(path = %wire.path, reason = %why, "refused a file name");
            FileRefusal::BadName
        })?;
        Ok(Self {
            path,
            bytes: wire.bytes,
            hash: Sum::from_bytes(wire.hash),
            replace: wire.replace,
        })
    }

    /// As the wire carries it.
    #[must_use]
    pub fn to_wire(&self) -> FileOffer {
        FileOffer {
            path: self.path.as_str().to_owned(),
            bytes: self.bytes,
            hash: self.hash.bytes(),
            replace: self.replace,
        }
    }
}

/// Where the bytes go until the whole file has arrived.
///
/// Named from the **hash and the destination** rather than from the file's own
/// name. From the hash so a partial left over from a different version of the
/// same path cannot be mistaken for this one; from the destination so two
/// transfers of the same content to different places do not share a file. Both
/// are fixed-length, so the name always fits whatever the destination is
/// called.
fn partial_for(offer: &Offer) -> Result<SafePath, FileError> {
    let mut of_path = Running::new();
    of_path.add(offer.path.as_str().as_bytes());
    let where_to = of_path.finish();
    let name = format!(
        ".esper-{}-{}.part",
        offer.hash.short(),
        where_to.short().get(..8).unwrap_or("00000000")
    );
    Ok(offer.path.with_file_name(&name)?)
}

/// A file arriving.
#[derive(Debug)]
pub struct Receiving {
    at: SafePath,
    partial: SafePath,
    file: std::fs::File,
    running: Running,
    want: Sum,
    total: u64,
    written: u64,
    from: u64,
}

impl Receiving {
    /// Decide whether to take this, and get ready to.
    ///
    /// # Errors
    ///
    /// With the refusal to send back.
    pub fn begin(vault: &Vault, offer: &Offer, limits: Limits) -> Result<Self, FileRefusal> {
        if limits.most_bytes.is_some_and(|most| offer.bytes > most) {
            return Err(FileRefusal::TooLarge);
        }
        // **Refused, not prompted.** An unattended host has nobody to ask, and
        // a transfer that waits for an answer nobody is there to give is a
        // transfer that hangs. The prompt happens where the person is: at the
        // end that typed the command.
        if !offer.replace && vault.exists(&offer.path) {
            return Err(FileRefusal::Exists);
        }
        let partial = partial_for(offer).map_err(|_| FileRefusal::BadName)?;

        // What is already here of this exact content, if anything.
        let (from, running) = match resume(vault, &partial, offer) {
            Some(pair) => pair,
            None => {
                let _ = vault.remove(&partial);
                (0, Running::new())
            }
        };

        // Checked before the first byte, not after the last: a transfer that
        // fills a disk and then fails takes down a machine belonging to
        // somebody who is not the one sending.
        let still_needed = offer.bytes.saturating_sub(from);
        if let Some(free) = vault.free_space()
            && free < still_needed.saturating_add(SPARE)
        {
            return Err(FileRefusal::NoRoom);
        }

        vault.make_parents(&offer.path).map_err(failed)?;
        let file = if from == 0 {
            vault.create_new(&partial).map_err(failed)?
        } else {
            let mut open = vault.open_append(&partial).map_err(failed)?;
            open.seek(SeekFrom::Start(from)).map_err(|e| {
                tracing::debug!(error = %e, "could not carry on from where a transfer stopped");
                FileRefusal::Failed
            })?;
            open
        };

        Ok(Self {
            at: offer.path.clone(),
            partial,
            file,
            running,
            want: offer.hash,
            total: offer.bytes,
            written: from,
            from,
        })
    }

    /// The first byte still wanted, which is what the far end is told.
    #[must_use]
    pub const fn from(&self) -> u64 {
        self.from
    }

    /// How many bytes are still to come.
    #[must_use]
    pub const fn left(&self) -> u64 {
        self.total.saturating_sub(self.written)
    }

    /// How far along this is, out of the whole file.
    #[must_use]
    pub const fn written(&self) -> u64 {
        self.written
    }

    /// Where it is going.
    #[must_use]
    pub const fn at(&self) -> &SafePath {
        &self.at
    }

    /// Take some of the file.
    ///
    /// # Errors
    ///
    /// If the write fails, or if the far end sent more than it said it would —
    /// which is a peer filling a disk with a transfer that was accepted for a
    /// smaller one.
    pub fn take(&mut self, bytes: &[u8]) -> Result<(), FileError> {
        let would_be = self.written.saturating_add(bytes.len() as u64);
        if would_be > self.total {
            return Err(FileError::Failed(format!(
                "more bytes arrived than the {} that were offered",
                self.total
            )));
        }
        self.file
            .write_all(bytes)
            .map_err(|e| FileError::Failed(e.to_string()))?;
        self.running.add(bytes);
        self.written = would_be;
        Ok(())
    }

    /// Say how it went, and put the file where it goes if it should.
    ///
    /// Consumes the transfer either way: what is left afterwards is either the
    /// file under its real name or a partial to carry on from, and never a
    /// half-written thing under a name something else might open.
    #[must_use]
    pub fn finish(self, vault: &Vault) -> FileEnd {
        let Self {
            at,
            partial,
            mut file,
            running,
            want,
            total,
            written,
            ..
        } = self;

        if written < total {
            // Kept, deliberately. The bytes that did arrive are the expensive
            // part, and the next attempt starts from them.
            let _ = file.flush();
            return FileEnd::Incomplete;
        }
        if let Err(e) = file.sync_all() {
            tracing::warn!(error = %e, "a transfer could not be written to the disk");
            let _ = vault.remove(&partial);
            return FileEnd::Failed;
        }
        drop(file);

        let got = running.finish();
        if got != want {
            // **Not a file.** Deleted rather than kept, because a partial that
            // hashes wrong would be resumed next time and hash wrong again.
            tracing::warn!(wanted = %want, got = %got, "a transfer arrived corrupted");
            let _ = vault.remove(&partial);
            return FileEnd::Mismatch;
        }
        if let Err(e) = vault.rename(&partial, &at) {
            tracing::warn!(error = %e, "a finished transfer could not be put where it goes");
            return FileEnd::Failed;
        }
        // The contents are durable and the *name* is not until this: after a
        // power cut the difference is a directory with nothing in it.
        vault.sync_dir(&at);
        FileEnd::Stored
    }
}

/// What is already here of this exact file, and the hash of it.
///
/// `None` when there is nothing worth carrying on from — no partial, one from
/// a different version, one longer than the file it claims to be, or one old
/// enough that nobody is coming back for it.
fn resume(vault: &Vault, partial: &SafePath, offer: &Offer) -> Option<(u64, Running)> {
    let have = vault.size_of(partial)?;
    if have == 0 || have > offer.bytes {
        return None;
    }
    if vault
        .age_of(partial)
        .is_some_and(|age| age > WORTH_RESUMING)
    {
        tracing::debug!(path = %offer.path, "a partial transfer was too old to carry on from");
        return None;
    }
    // **Re-read to rebuild the hash.** SHA-256 has no state this can write
    // down and pick up, so what was kept is hashed again — at disk speed, to
    // avoid moving it again at network speed, which is the right trade every
    // time the link is slower than the disk.
    let mut file = vault.open_read(partial).ok()?;
    let mut running = Running::new();
    let mut buffer = vec![0u8; AT_A_TIME];
    let mut read = 0u64;
    while read < have {
        let want = usize::try_from(have - read)
            .unwrap_or(AT_A_TIME)
            .min(AT_A_TIME);
        let got = file.read(buffer.get_mut(..want)?).ok()?;
        if got == 0 {
            return None;
        }
        running.add(buffer.get(..got)?);
        read = read.saturating_add(got as u64);
    }
    Some((have, running))
}

/// A file going out.
#[derive(Debug)]
pub struct Sending {
    file: std::fs::File,
    offer: Offer,
    sent: u64,
}

impl Sending {
    /// Open a file and work out what to say about it.
    ///
    /// Reads the whole file once, to hash it. That is the cost of putting the
    /// hash in the offer, and it is paid at disk speed for something that
    /// makes every resume afterwards provably about the same content.
    ///
    /// # Errors
    ///
    /// With the refusal to send back.
    pub fn begin(vault: &Vault, path: &SafePath, limits: Limits) -> Result<Self, FileRefusal> {
        let mut file = vault.open_read(path).map_err(|e| {
            tracing::debug!(path = %path, error = %e, "refused to send a file");
            FileRefusal::Missing
        })?;
        let bytes = vault.size_of(path).ok_or(FileRefusal::Missing)?;
        if limits.most_bytes.is_some_and(|most| bytes > most) {
            return Err(FileRefusal::TooLarge);
        }
        let hash = Sum::of(&mut file).map_err(|e| {
            tracing::debug!(error = %e, "could not read a file to send it");
            FileRefusal::Failed
        })?;
        file.rewind().map_err(|e| {
            tracing::debug!(error = %e, "could not go back to the start of a file");
            FileRefusal::Failed
        })?;
        Ok(Self {
            file,
            offer: Offer {
                path: path.clone(),
                bytes,
                hash,
                replace: false,
            },
            sent: 0,
        })
    }

    /// What to tell the far end about it.
    #[must_use]
    pub const fn offer(&self) -> &Offer {
        &self.offer
    }

    /// Start from here instead of the beginning.
    ///
    /// # Errors
    ///
    /// If the offset is past the end — a receiver claiming to hold more of a
    /// file than the file has is one this end does not humour.
    pub fn resume_at(&mut self, from: u64) -> Result<(), FileError> {
        if from > self.offer.bytes {
            return Err(FileError::Failed(format!(
                "asked to carry on from {from}, past the end of a {} byte file",
                self.offer.bytes
            )));
        }
        self.file
            .seek(SeekFrom::Start(from))
            .map_err(|e| FileError::Failed(e.to_string()))?;
        self.sent = from;
        Ok(())
    }

    /// Read the next piece. Zero means there is no more.
    ///
    /// # Errors
    ///
    /// If the read fails, or the file got shorter while it was being sent —
    /// which would otherwise be a transfer that ends early and hashes wrong at
    /// the far end, for a reason nothing here could name.
    pub fn read(&mut self, into: &mut [u8]) -> Result<usize, FileError> {
        if self.sent >= self.offer.bytes {
            return Ok(0);
        }
        let want = usize::try_from(self.offer.bytes - self.sent).unwrap_or(into.len());
        let room = into
            .get_mut(..want.min(into.len()))
            .ok_or_else(|| FileError::Failed("no room to read into".to_owned()))?;
        let got = self
            .file
            .read(room)
            .map_err(|e| FileError::Failed(e.to_string()))?;
        if got == 0 {
            return Err(FileError::Failed(
                "the file got shorter while it was being sent".to_owned(),
            ));
        }
        self.sent = self.sent.saturating_add(got as u64);
        Ok(got)
    }

    /// How much is still to go.
    #[must_use]
    pub const fn left(&self) -> u64 {
        self.offer.bytes.saturating_sub(self.sent)
    }
}

fn failed(e: FileError) -> FileRefusal {
    match e {
        FileError::Exists => FileRefusal::Exists,
        FileError::Outside | FileError::BadName(_) => FileRefusal::Missing,
        FileError::NoRoom { .. } => FileRefusal::NoRoom,
        other => {
            tracing::debug!(error = %other, "a transfer could not be started");
            FileRefusal::Failed
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    struct Temp(std::path::PathBuf);

    impl Temp {
        fn new(what: &str) -> Self {
            let stamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let mut path = std::env::temp_dir();
            path.push(format!(
                "esper-transfer-{what}-{stamp}-{:?}",
                std::thread::current().id()
            ));
            std::fs::create_dir_all(&path).expect("mkdir");
            Self(path)
        }

        fn vault(&self, name: &str) -> Vault {
            Vault::open(&self.0.join(name)).expect("a root")
        }
    }

    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn p(text: &str) -> SafePath {
        SafePath::parse(text).expect("a path the rules accept")
    }

    /// Something big enough to take several pieces and not a round number of
    /// them, so an off-by-one in the chunking is a difference rather than a
    /// shorter but plausible result.
    fn contents(bytes: usize) -> Vec<u8> {
        (0..bytes)
            .map(|i| u8::try_from(i % 251).unwrap_or(0))
            .collect()
    }

    fn put(vault: &Vault, at: &SafePath, bytes: &[u8]) {
        vault.make_parents(at).expect("directories");
        let mut file = vault.create_new(at).expect("a new file");
        file.write_all(bytes).expect("write");
    }

    /// Move a whole file from one root to another, the way a session does.
    fn carry(from: &Vault, to: &Vault, what: &SafePath, limits: Limits) -> FileEnd {
        let mut sending = Sending::begin(from, what, limits).expect("a file to send");
        let offer = sending.offer().clone();
        let mut receiving = Receiving::begin(to, &offer, limits).expect("somewhere to put it");
        sending.resume_at(receiving.from()).expect("seek");

        let mut buffer = vec![0u8; AT_A_TIME];
        loop {
            let got = sending.read(&mut buffer).expect("read");
            if got == 0 {
                break;
            }
            receiving
                .take(buffer.get(..got).expect("what was read"))
                .expect("write");
        }
        receiving.finish(to)
    }

    /// **The thing that has to work**, end to end, with nothing else going on.
    #[test]
    fn a_file_arrives_whole_and_under_its_own_name() {
        let home = Temp::new("whole");
        let (from, to) = (home.vault("a"), home.vault("b"));
        let what = p("reports/2026/q3.bin");
        let bytes = contents(300_003);
        put(&from, &what, &bytes);

        assert_eq!(carry(&from, &to, &what, Limits::default()), FileEnd::Stored);

        let mut back = Vec::new();
        to.open_read(&what)
            .expect("it is there")
            .read_to_end(&mut back)
            .expect("read");
        assert_eq!(back, bytes, "what landed is not what was sent");
        assert_eq!(to.size_of(&what), Some(300_003));
    }

    /// **Nothing appears under the final name until it is whole.** A process
    /// watching that directory sees no file or the complete one, and a build
    /// artefact something else is waiting for depends on exactly that.
    #[test]
    fn a_transfer_in_progress_is_not_visible_under_the_name_it_is_going_to() {
        let home = Temp::new("partial");
        let (from, to) = (home.vault("a"), home.vault("b"));
        let what = p("build/artefact.tar");
        put(&from, &what, &contents(200_000));

        let mut sending = Sending::begin(&from, &what, Limits::default()).expect("a file");
        let offer = sending.offer().clone();
        let mut receiving = Receiving::begin(&to, &offer, Limits::default()).expect("a place");

        let mut buffer = vec![0u8; AT_A_TIME];
        let got = sending.read(&mut buffer).expect("read");
        receiving
            .take(buffer.get(..got).expect("read"))
            .expect("write");
        assert!(receiving.written() > 0, "nothing was written at all");
        assert!(
            !to.exists(&what),
            "a half-written file was visible under the name it was going to"
        );
        assert_eq!(receiving.at(), &what);

        // And stopping there leaves it that way, with the bytes kept.
        assert_eq!(receiving.finish(&to), FileEnd::Incomplete);
        assert!(!to.exists(&what));
    }

    /// The expensive part of a transfer is the bytes that already crossed.
    #[test]
    fn a_transfer_that_stopped_carries_on_from_where_it_was() {
        let home = Temp::new("resume");
        let (from, to) = (home.vault("a"), home.vault("b"));
        let what = p("big.bin");
        let bytes = contents(500_007);
        put(&from, &what, &bytes);

        // Half of it, then the connection goes.
        let mut sending = Sending::begin(&from, &what, Limits::default()).expect("a file");
        let offer = sending.offer().clone();
        let mut receiving = Receiving::begin(&to, &offer, Limits::default()).expect("a place");
        assert_eq!(receiving.from(), 0, "nothing was there to carry on from");
        let mut buffer = vec![0u8; AT_A_TIME];
        for _ in 0..3 {
            let got = sending.read(&mut buffer).expect("read");
            receiving
                .take(buffer.get(..got).expect("read"))
                .expect("write");
        }
        let stopped_at = receiving.written();
        assert_eq!(receiving.finish(&to), FileEnd::Incomplete);

        // A fresh attempt, which is what a reconnect is.
        let mut again = Sending::begin(&from, &what, Limits::default()).expect("a file");
        let mut taking = Receiving::begin(&to, again.offer(), Limits::default()).expect("a place");
        assert_eq!(
            taking.from(),
            stopped_at,
            "it started over instead of carrying on"
        );
        assert_eq!(taking.left(), 500_007 - stopped_at);
        again.resume_at(taking.from()).expect("seek");
        loop {
            let got = again.read(&mut buffer).expect("read");
            if got == 0 {
                break;
            }
            taking
                .take(buffer.get(..got).expect("read"))
                .expect("write");
        }
        assert_eq!(taking.finish(&to), FileEnd::Stored);

        let mut back = Vec::new();
        to.open_read(&what)
            .expect("there")
            .read_to_end(&mut back)
            .expect("read");
        assert_eq!(back, bytes, "the two halves were not joined correctly");
    }

    /// **A partial from a different version of the same path is not this
    /// file.** Resuming from it would produce something that is neither.
    #[test]
    fn a_partial_of_something_else_is_not_carried_on_from() {
        let home = Temp::new("stale");
        let (from, to) = (home.vault("a"), home.vault("b"));
        let what = p("thing.bin");

        // A first version, half sent.
        put(&from, &what, &contents(200_000));
        let mut sending = Sending::begin(&from, &what, Limits::default()).expect("a file");
        let mut receiving =
            Receiving::begin(&to, sending.offer(), Limits::default()).expect("a place");
        let mut buffer = vec![0u8; AT_A_TIME];
        let got = sending.read(&mut buffer).expect("read");
        receiving
            .take(buffer.get(..got).expect("read"))
            .expect("write");
        assert_eq!(receiving.finish(&to), FileEnd::Incomplete);

        // The file changes, and is offered again. Same path, different content.
        from.remove(&what).expect("remove");
        let second = contents(200_000)
            .iter()
            .map(|b| b.wrapping_add(1))
            .collect::<Vec<_>>();
        put(&from, &what, &second);

        let fresh = Sending::begin(&from, &what, Limits::default()).expect("a file");
        let taking = Receiving::begin(&to, fresh.offer(), Limits::default()).expect("a place");
        assert_eq!(
            taking.from(),
            0,
            "a partial of a different file was carried on from"
        );

        // And carried through, what lands is the second version.
        drop(taking);
        assert_eq!(carry(&from, &to, &what, Limits::default()), FileEnd::Stored);
        let mut back = Vec::new();
        to.open_read(&what)
            .expect("there")
            .read_to_end(&mut back)
            .expect("read");
        assert_eq!(back, second);
    }

    /// **What arrived is not what was sent, and it is not a file.** Provoked
    /// directly, because on a real link this is a corrupted disk or a resumed
    /// transfer that picked up in the wrong place — neither of which a test
    /// can arrange by running two machines.
    #[test]
    fn a_file_that_hashes_wrong_is_not_stored_and_leaves_nothing_behind() {
        let home = Temp::new("mismatch");
        let (from, to) = (home.vault("a"), home.vault("b"));
        let what = p("thing.bin");
        put(&from, &what, &contents(100_000));

        let sending = Sending::begin(&from, &what, Limits::default()).expect("a file");
        let mut lying = sending.offer().clone();
        lying.hash = Sum::from_bytes([0xAB; 32]);
        let mut receiving = Receiving::begin(&to, &lying, Limits::default()).expect("a place");

        let bytes = contents(100_000);
        receiving.take(&bytes).expect("write");
        assert_eq!(receiving.finish(&to), FileEnd::Mismatch);
        assert!(!to.exists(&what), "a corrupted transfer was stored");

        // **And the partial is gone too.** Kept, it would be resumed next
        // time, hash wrong again, and never succeed.
        let leftovers: Vec<_> = std::fs::read_dir(to.shown())
            .expect("read the directory")
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert!(
            leftovers.is_empty(),
            "a corrupted transfer left {leftovers:?} behind"
        );
    }

    /// A peer that says a file is one size and then sends more is a peer
    /// filling a disk with a transfer that was accepted for something smaller.
    #[test]
    fn more_bytes_than_were_offered_are_refused_rather_than_written() {
        let home = Temp::new("overrun");
        let to = home.vault("b");
        let offer = Offer {
            path: p("small.bin"),
            bytes: 10,
            hash: Sum::from_bytes([0; 32]),
            replace: false,
        };
        let mut receiving = Receiving::begin(&to, &offer, Limits::default()).expect("a place");
        receiving.take(b"0123456789").expect("the ten it said");
        assert_eq!(receiving.left(), 0);
        assert!(
            receiving.take(b"and more").is_err(),
            "a peer wrote past the size it declared"
        );
    }

    /// **Replacing is a decision somebody made, not a side effect.**
    #[test]
    fn something_already_there_is_not_replaced_unless_the_offer_says_so() {
        let home = Temp::new("replace");
        let (from, to) = (home.vault("a"), home.vault("b"));
        let what = p("notes.txt");
        put(&from, &what, b"the new one");
        put(&to, &what, b"the old one");

        let sending = Sending::begin(&from, &what, Limits::default()).expect("a file");
        assert_eq!(
            Receiving::begin(&to, sending.offer(), Limits::default()).err(),
            Some(FileRefusal::Exists)
        );
        let mut back = Vec::new();
        to.open_read(&what)
            .expect("there")
            .read_to_end(&mut back)
            .expect("read");
        assert_eq!(back, b"the old one", "it was replaced anyway");

        // Said explicitly, it replaces — and the old contents are gone.
        let mut asked = sending.offer().clone();
        asked.replace = true;
        let mut receiving = Receiving::begin(&to, &asked, Limits::default()).expect("a place");
        receiving.take(b"the new one").expect("write");
        assert_eq!(receiving.finish(&to), FileEnd::Stored);
        let mut now = Vec::new();
        to.open_read(&what)
            .expect("there")
            .read_to_end(&mut now)
            .expect("read");
        assert_eq!(now, b"the new one");
    }

    #[test]
    fn a_file_larger_than_this_machine_takes_is_refused_at_both_ends() {
        let home = Temp::new("large");
        let (from, to) = (home.vault("a"), home.vault("b"));
        let what = p("big.bin");
        put(&from, &what, &contents(50_000));
        let small = Limits {
            most_bytes: Some(1000),
        };

        // The sending end refuses to read it out.
        assert_eq!(
            Sending::begin(&from, &what, small).err(),
            Some(FileRefusal::TooLarge)
        );
        // And the receiving end refuses it even when the sender did not.
        let sending = Sending::begin(&from, &what, Limits::default()).expect("a file");
        assert_eq!(
            Receiving::begin(&to, sending.offer(), small).err(),
            Some(FileRefusal::TooLarge)
        );
    }

    /// A name that is not one this protocol carries never reaches a disk.
    #[test]
    fn an_offer_with_a_name_the_rules_refuse_never_gets_a_place_to_land() {
        for bad in ["../escape", "/etc/passwd", "con.txt", "with\nnewline", ""] {
            let wire = FileOffer {
                path: bad.to_owned(),
                bytes: 1,
                hash: [0; 32],
                replace: false,
            };
            assert_eq!(
                Offer::check(&wire).err(),
                Some(FileRefusal::BadName),
                "{bad:?} was accepted"
            );
        }
        let fine = FileOffer {
            path: "reports/q3.pdf".to_owned(),
            bytes: 7,
            hash: [3; 32],
            replace: true,
        };
        let checked = Offer::check(&fine).expect("a name the rules accept");
        assert_eq!(checked.path.as_str(), "reports/q3.pdf");
        assert_eq!(checked.bytes, 7);
        assert!(checked.replace);
        assert_eq!(
            checked.to_wire(),
            fine,
            "the offer did not survive the round trip"
        );
    }

    /// A file that is not there is not a file, and asking for one outside the
    /// directory gets the same answer as asking for one that does not exist.
    #[test]
    fn a_file_that_cannot_be_sent_is_refused_the_same_way_whatever_the_reason() {
        let home = Temp::new("missing");
        let from = home.vault("a");
        assert_eq!(
            Sending::begin(&from, &p("nothing-here"), Limits::default()).err(),
            Some(FileRefusal::Missing)
        );
        // Something outside, reachable only through a link.
        #[cfg(unix)]
        {
            std::fs::write(home.0.join("outside-secret"), b"secret").expect("write");
            std::os::unix::fs::symlink(
                home.0.join("outside-secret"),
                from.shown().join("looks-local"),
            )
            .expect("symlink");
            assert_eq!(
                Sending::begin(&from, &p("looks-local"), Limits::default()).err(),
                Some(FileRefusal::Missing),
                "a file outside the directory was read out through a link"
            );
        }
    }

    /// A receiver claiming to hold more of a file than the file has is not
    /// humoured: seeking there and sending nothing would end as a hash that
    /// does not match, for a reason nothing could name.
    #[test]
    fn a_resume_point_past_the_end_is_refused() {
        let home = Temp::new("past");
        let from = home.vault("a");
        let what = p("small.bin");
        put(&from, &what, &contents(100));
        let mut sending = Sending::begin(&from, &what, Limits::default()).expect("a file");
        assert!(sending.resume_at(101).is_err());
        assert!(
            sending.resume_at(100).is_ok(),
            "the very end is a real place"
        );
        assert_eq!(sending.left(), 0);
        assert_eq!(sending.read(&mut [0u8; 16]).expect("no more"), 0);
    }

    /// The two ends have to agree about what the file is, or a resume is a
    /// guess.
    #[test]
    fn the_offer_describes_the_file_that_was_opened() {
        let home = Temp::new("offer");
        let from = home.vault("a");
        let what = p("x.bin");
        let bytes = contents(1234);
        put(&from, &what, &bytes);
        let sending = Sending::begin(&from, &what, Limits::default()).expect("a file");
        assert_eq!(sending.offer().bytes, 1234);
        assert_eq!(sending.offer().path, what);
        assert_eq!(
            sending.offer().hash,
            Sum::of(&mut bytes.as_slice()).expect("hash")
        );
        assert!(!sending.offer().replace);
        assert_eq!(sending.left(), 1234);
    }
}
