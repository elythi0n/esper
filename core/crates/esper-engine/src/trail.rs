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
    let now = crate::paths::now_secs();
    match Log::at(path).retain(keep_secs, max_bytes, now) {
        // Said only when something happened: a host that prunes daily would
        // otherwise print a line a day about nothing.
        Ok(pruned) => {
            if let Some(line) = pruned.describe() {
                println!("      trail: {line}");
            }
        }
        Err(e) => eprintln!("      · could not apply the trail's retention: {e}"),
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
}
