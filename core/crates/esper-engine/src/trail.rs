//! What this machine did, written down where this machine can find it.
//!
//! # Why the outbound half exists
//!
//! The host has recorded who connected to it since Phase 2. Nothing recorded
//! what *this* machine connected to — so the trail could answer "who was on
//! this computer" and not "what did this computer reach", and the second is the
//! question asked about a laptop somebody shares, a laptop somebody lost, and
//! an afternoon nobody can account for.
//!
//! Both halves go in one file, because "what happened on this machine" is one
//! question. Which way a session went is a field on the line: see
//! [`esper_audit::Direction`].
//!
//! # What is never in it
//!
//! The same rule the log has always had, restated here because this is the
//! module that could break it: **never the contents of a session.** Not a
//! keystroke, not a frame, not a clipboard, not a byte of a file. What goes in
//! is what happened, to whom, for how long, and what it was allowed to do.
//!
//! # Why a failure to record is not a failure
//!
//! A session that cannot be logged still happens, and the alternative — a
//! product that refuses to work because a disk is full — is worse than one that
//! says so on the terminal. So every function here is best-effort and says so
//! once, on stderr, where somebody running it will see it.

use esper_audit::{Entry, Event, Log};

/// Record something **this machine** did to a peer.
///
/// Best effort: see the module's own note. A caller that wants to know whether
/// it worked has the wrong idea about what this is for.
pub fn went_out(peer: &[u8; 32], event: Event) {
    let Ok(path) = crate::paths::audit_log() else {
        return;
    };
    if let Err(e) = Log::at(path).append(&Entry::going_out(peer, event)) {
        // Once, and not per event: a disk that is full will fail every write,
        // and a session that printed a warning per keystroke would be worse
        // than one that printed none.
        eprintln!("      · could not record this in the trail: {e}");
    }
}

/// The peer key a session is with, as the log wants it.
///
/// A helper rather than an inline cast, because "the peer's key" arrives in
/// three shapes across the commands and getting it wrong writes a correct
/// entry against the wrong device — which is worse than not writing one.
#[must_use]
pub const fn peer_of(session: &esper_transport::Session) -> [u8; 32] {
    *session.peer_key()
}

/// How long the trail is kept, and how large it may grow.
///
/// Read from the settings, so `none` for either really is no limit of that
/// kind — a person who decided to keep everything gets to.
#[must_use]
pub fn policy() -> (Option<u64>, Option<u64>) {
    let keep_secs = crate::settings::whole("audit.retain");
    let megabytes = crate::settings::whole("audit.max_size");
    (keep_secs, megabytes.map(|mb| mb.saturating_mul(1_048_576)))
}

/// Apply the retention policy once, saying what went.
///
/// **Called where it will actually run**: when a host starts, and once a day
/// while it does. A retention policy that only ran when somebody typed a
/// command would be a retention policy nobody applies — and this one existed
/// for weeks with no caller at all, which is worse than not having it, because
/// the setting would have been a promise the product did not keep.
pub fn keep_only_what_the_policy_allows() {
    let Ok(path) = crate::paths::audit_log() else {
        return;
    };
    let (keep_secs, max_bytes) = policy();
    if keep_secs.is_none() && max_bytes.is_none() {
        return;
    }
    apply_policy(&path, keep_secs, max_bytes, crate::paths::now_secs());
}

/// The policy itself, on a trail this caller names.
///
/// The path is a parameter for the same reason `settings::set_in_file`'s is: a
/// helper that reads `ESPER_HOME` is a helper every test has to arrange
/// around, and the one that forgets trims somebody's real record.
pub fn apply_policy(
    path: &std::path::Path,
    keep_secs: Option<u64>,
    max_bytes: Option<u64>,
    now: u64,
) {
    match Log::at(path.to_path_buf()).retain(keep_secs, max_bytes, now) {
        // Said only when something happened: a host that prunes daily would
        // otherwise print a line a day about nothing.
        Ok(pruned) => {
            if let Some(line) = pruned.describe() {
                println!("      trail: {line}");
                // **And in the record itself.** The line above is on a
                // terminal nobody keeps; a reader of the file a week later
                // cannot otherwise tell a quiet week from a deleted one.
                note_the_trim(path, &pruned);
            }
        }
        Err(e) => eprintln!("      · could not apply the trail's retention: {e}"),
    }
}

/// Append the line saying the record was trimmed.
///
/// After the rewrite and through the ordinary append, so the trimming path
/// stays the one thing in `esper-audit` that only ever writes a subset of what
/// it read.
fn note_the_trim(path: &std::path::Path, pruned: &esper_audit::Pruned) {
    let entry = Entry::here(Event::TrailTrimmed {
        too_old: pruned.too_old,
        over_size: pruned.over_size,
        kept: pruned.kept,
    });
    if let Err(e) = Log::at(path.to_path_buf()).append(&entry) {
        eprintln!("      · could not record that the trail was trimmed: {e}");
    }
}

/// How often a running host applies it.
///
/// Daily. The policy is written in days, so checking more often would find
/// nothing to do; checking less often would let a machine that has been up for
/// a month sit over its ceiling for weeks.
pub const RETENTION_INTERVAL: std::time::Duration = std::time::Duration::from_secs(24 * 60 * 60);

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    /// **The direction is what distinguishes the two halves**, and a line
    /// written by this module must be the outbound one — otherwise a
    /// controller's own record reads as though somebody had connected *to* it.
    #[test]
    fn what_this_module_writes_is_marked_as_going_out() {
        let entry = Entry::going_out(&[9u8; 32], Event::TerminalOpened);
        assert_eq!(entry.direction, esper_audit::Direction::Out);
        assert_eq!(entry.direction.arrow(), "→");

        // And the inbound constructor is unchanged, so a host's lines look
        // exactly as they always have.
        let inbound = Entry::now(&[9u8; 32], Event::TerminalOpened);
        assert_eq!(inbound.direction, esper_audit::Direction::In);
    }

    /// **An outbound line names the machine at the other end**, not this one.
    /// A trail that recorded this machine's own fingerprint against everything
    /// it did would be a list of one name.
    #[test]
    fn an_outbound_line_names_the_far_end() {
        let far = [0xABu8; 32];
        let entry = Entry::going_out(&far, Event::TerminalOpened);
        assert_eq!(
            entry.peer_key,
            "ab".repeat(32),
            "the key on the line should be the far end's"
        );
        assert!(!entry.peer.is_empty(), "and its fingerprint, for reading");
    }

    /// Every line of a trail, read off the disk.
    fn entries_in(path: &std::path::Path) -> Vec<Entry> {
        std::fs::read_to_string(path)
            .expect("the record")
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| serde_json::from_str(line).expect("one JSON line"))
            .collect()
    }

    /// A directory of this test's own, removed when the run ends.
    fn scratch() -> std::path::PathBuf {
        static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("esper-trail-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("a directory");
        dir
    }

    /// **A trimmed record says it was trimmed.**
    ///
    /// Without this line a reader cannot tell a quiet week from a deleted
    /// one, which is the difference between a record and a record that can be
    /// relied on.
    #[test]
    fn trimming_leaves_a_line_saying_what_went() {
        let dir = scratch();
        let path = dir.join("sessions.log");
        let log = Log::at(path.clone());
        // Ten days of one line each, ending today.
        let now = 10 * 86_400;
        for day in 0..10u64 {
            log.append(&Entry::new(&[7u8; 32], Event::TerminalOpened, day * 86_400))
                .expect("a line");
        }

        // Keep three days.
        apply_policy(&path, Some(3 * 86_400), None, now);

        // Read as the file, not through the crate's own reader: what is on
        // the disk afterwards is the thing being checked, and `check-trail`
        // keeps the list of modules that open the log to read it short on
        // purpose.
        let left = entries_in(&path);
        let trims: Vec<&Entry> = left
            .iter()
            .filter(|e| matches!(e.event, Event::TrailTrimmed { .. }))
            .collect();
        assert_eq!(trims.len(), 1, "one line about the trim: {left:?}");
        let trim = trims.first().expect("the line");
        assert_eq!(trim.peer_key, "", "nobody connected to trim a record");
        let Event::TrailTrimmed {
            too_old,
            over_size,
            kept,
        } = &trim.event
        else {
            unreachable!("filtered above")
        };
        assert_eq!(*too_old, 7, "seven days fell outside three: {left:?}");
        assert_eq!(*over_size, 0);
        assert_eq!(*kept, 3, "and three were kept: {left:?}");
        // The line counts what the record held, not itself.
        assert_eq!(left.len(), 4);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A pass that removes nothing writes nothing: a host prunes daily, and a
    /// record that grew a line a day about having removed no lines would be a
    /// record mostly about itself.
    #[test]
    fn a_trim_that_removes_nothing_is_not_recorded() {
        let dir = scratch();
        let path = dir.join("sessions.log");
        let log = Log::at(path.clone());
        log.append(&Entry::new(&[7u8; 32], Event::TerminalOpened, 500))
            .expect("a line");

        apply_policy(&path, Some(86_400), None, 600);

        let left = entries_in(&path);
        assert_eq!(left.len(), 1, "nothing went, so nothing was said: {left:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
