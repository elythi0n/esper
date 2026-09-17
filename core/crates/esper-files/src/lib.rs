//! Moving a file onto somebody else's disk.
//!
//! # The one feature that writes and the writing stays
//!
//! Everything else a session carries is transient: a frame is drawn and
//! forgotten, a keystroke reaches an application, a clipboard holds one thing
//! at a time. A transfer writes to a disk, under a name the far end chose, and
//! what it writes is still there tomorrow.
//!
//! That makes it the one place where a missing check is not a bug but a
//! compromise. A path that escapes its directory writes `authorized_keys`; a
//! symlink followed on the way in writes into `/etc`; a filename with a
//! newline in it forges the log line that would have shown you. None of these
//! are exotic — they are the standard repertoire.
//!
//! # Two defences, and neither stands in for the other
//!
//! [`SafePath`] says what a name may *be*: relative, no traversal, nothing a
//! platform refuses, nothing that changes how the name looks. It is pure, it
//! refuses with a reason, and it runs before anything touches a disk.
//!
//! [`Vault`] says where a name may *reach*, and on Linux it asks the kernel to
//! enforce that in the same syscall that opens the file. There is no window
//! between a check and an open because there is no separate check.
//!
//! # What lands, lands whole
//!
//! Nothing appears under its final name until every byte has arrived and the
//! hash matches. A watcher in that directory sees no file or the complete one,
//! never a growing one — which is what a build artefact somebody is waiting
//! for actually needs.

#![forbid(unsafe_code)]

mod hash;
mod name;
mod transfer;
mod vault;

pub use hash::{Running, Sum};
pub use name::{BadName, MOST_BYTES, MOST_COMPONENTS, MOST_PER_COMPONENT, SafePath};
pub use transfer::{AT_A_TIME, Limits, Offer, Receiving, Sending};
pub use vault::{HowConfined, Listed, Vault};

/// What can go wrong touching a disk on somebody's behalf.
#[derive(Debug, thiserror::Error)]
pub enum FileError {
    /// The name is not one a path may contain.
    #[error("{0}")]
    BadName(BadName),
    /// The path resolved outside the root, or through a link.
    ///
    /// **Not distinguished from "no such file".** Which of the two it was is
    /// information about a filesystem the peer cannot see, and the answer to
    /// both is the same.
    #[error("no such path in this machine's file directory")]
    Outside,
    /// There is already something there.
    #[error("there is already a file there")]
    Exists,
    /// Not enough room.
    #[error("{wanted} bytes will not fit: {free} free")]
    NoRoom { wanted: u64, free: u64 },
    /// The file directory itself could not be opened.
    #[error("this machine's file directory could not be opened: {0}")]
    NoRoot(String),
    /// Anything else the platform said.
    #[error("{0}")]
    Failed(String),
}

impl From<BadName> for FileError {
    fn from(bad: BadName) -> Self {
        Self::BadName(bad)
    }
}
