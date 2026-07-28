//! Moving what somebody copied, and nothing else.
//!
//! # A clipboard is not a channel
//!
//! Every other thing a session carries is something a person asked for: they
//! clicked a monitor, they typed a key, they opened a forward. A clipboard is
//! different. It fills up as a side effect of ordinary work, it holds whatever
//! was last copied, and what was last copied is very often a password — a
//! password manager's whole interface is a copy button.
//!
//! So the rule this crate exists to enforce is that **nothing leaves a machine
//! because a session happened to be open**. A change announces its *shape*
//! (see [`esper_proto::ClipboardOffer`]) and stops there. The far end has to
//! ask, this end's policy has to allow it, and only then does content move.
//! The asking is what makes the taking visible; the policy is what makes it
//! refusable.
//!
//! # Two directions, two permissions
//!
//! Reading this machine's clipboard and writing to it are separate risks and
//! separate grants — see [`esper_proto::ClipboardWay`]. A support session
//! usually wants exactly one of them, and which one depends on who is helping
//! whom.
//!
//! # What lives here
//!
//! [`Clipboard`] is the platform seam: read, write, clear, and a cheap token
//! that changes when the clipboard does. [`Courier`] is everything above it —
//! the policy, the caps, the type checks — written once so that no backend
//! implements a decision. [`conformance`] is the suite every backend has to
//! pass, because a backend nothing exercises is a backend that does not work.
//!
//! # Nothing is remembered
//!
//! [`Held`] wipes its bytes when it is dropped, a newer offer replaces an
//! older one rather than joining it, and [`Courier::forget`] empties
//! everything at the end of a session. There is no history to leak because
//! there is no history.

#![forbid(unsafe_code)]

pub mod conformance;
mod courier;
mod held;

pub use courier::{Courier, Grants, Heard};
pub use held::Held;

// Re-exported so a backend crate names one vocabulary rather than two.
pub use esper_proto::ClipboardKind;

/// What can go wrong down at the platform.
///
/// Deliberately few. Everything a caller can do about a clipboard that will
/// not answer is the same thing, so the distinctions that survive are the ones
/// an operator reading a log needs — not the ones the platform happens to
/// offer.
#[derive(Debug, thiserror::Error)]
pub enum ClipboardError {
    /// There is no clipboard to talk to: no display, no session, no server.
    #[error("no clipboard is available on this machine: {0}")]
    Unavailable(String),
    /// The clipboard is there and would not answer in time.
    ///
    /// Ordinary on X11, where reading means asking whichever program owns the
    /// selection — and that program can be busy, or stopped, or gone.
    #[error("the program holding the clipboard did not answer")]
    NoAnswer,
    /// The content is larger than this kind may be.
    #[error("that is {size} bytes of {} — more than the {cap} this carries", kind.name())]
    TooLarge {
        kind: ClipboardKind,
        size: usize,
        cap: usize,
    },
    /// The platform failed in a way worth naming once.
    #[error("the clipboard could not be used: {0}")]
    Failed(String),
}

/// A machine's clipboard.
///
/// # What an implementation must promise
///
/// - [`Clipboard::read`] returns the content of that kind, or `None` if there
///   is none. It must not convert between kinds: text asked for is text found,
///   never an image rendered to a string.
/// - [`Clipboard::write`] replaces what is there.
/// - [`Clipboard::generation`] changes whenever the clipboard's content might
///   have changed, and does not change otherwise. It exists so that a session
///   can notice a copy without reading the clipboard on every tick — reading
///   is what costs, both in time and in what ends up in this process's memory.
/// - [`Clipboard::clear`] gives up anything this process put on the clipboard.
///   A remote desktop that leaves a password on a host's clipboard after the
///   session ends has done the exact thing this crate exists to prevent.
///
/// Nothing here checks a permission or a cap. Those are [`Courier`]'s, in one
/// place, because a check every backend has to remember is a check one of them
/// eventually forgets.
pub trait Clipboard {
    /// What is on the clipboard, of this kind.
    ///
    /// # Errors
    ///
    /// If the clipboard cannot be reached or would not answer.
    fn read(&mut self, kind: ClipboardKind) -> Result<Option<Vec<u8>>, ClipboardError>;

    /// Put this on the clipboard, replacing what was there.
    ///
    /// # Errors
    ///
    /// If the clipboard cannot be reached, or the content is too large for the
    /// platform to carry.
    fn write(&mut self, kind: ClipboardKind, content: &[u8]) -> Result<(), ClipboardError>;

    /// A token that changes when the clipboard might have.
    ///
    /// Cheap by contract: this is called often, and reading is not.
    fn generation(&mut self) -> u64;

    /// Give up anything this process put on the clipboard.
    ///
    /// # Errors
    ///
    /// If the clipboard cannot be reached. A caller ending a session should
    /// log the failure and carry on — there is nothing else to do, and the
    /// session is over either way.
    fn clear(&mut self) -> Result<(), ClipboardError>;
}

/// So that a caller can hold whichever backend this platform has without
/// naming it.
impl Clipboard for Box<dyn Clipboard + Send> {
    fn read(&mut self, kind: ClipboardKind) -> Result<Option<Vec<u8>>, ClipboardError> {
        (**self).read(kind)
    }
    fn write(&mut self, kind: ClipboardKind, content: &[u8]) -> Result<(), ClipboardError> {
        (**self).write(kind, content)
    }
    fn generation(&mut self) -> u64 {
        (**self).generation()
    }
    fn clear(&mut self) -> Result<(), ClipboardError> {
        (**self).clear()
    }
}

/// A clipboard that remembers what it was told, for tests and for a build with
/// no display.
///
/// Not only a test double: an engine on a machine with no clipboard needs
/// *something* behind the trait, and a null that answers honestly is better
/// than an `Option` at every call site.
#[derive(Debug, Default)]
pub struct NullClipboard {
    text: Option<Held>,
    image: Option<Held>,
    generation: u64,
    /// How many times [`Clipboard::clear`] has been called, so a test can show
    /// that ending a session empties the clipboard.
    pub clears: u32,
}

impl NullClipboard {
    /// An empty one.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Put something on it, as though a person had copied it.
    pub fn set(&mut self, kind: ClipboardKind, content: &[u8]) {
        let slot = match kind {
            ClipboardKind::Text => &mut self.text,
            ClipboardKind::Image => &mut self.image,
        };
        *slot = Some(Held::new(kind, content.to_vec()));
        self.generation = self.generation.saturating_add(1);
    }
}

impl Clipboard for NullClipboard {
    fn read(&mut self, kind: ClipboardKind) -> Result<Option<Vec<u8>>, ClipboardError> {
        let slot = match kind {
            ClipboardKind::Text => &self.text,
            ClipboardKind::Image => &self.image,
        };
        Ok(slot.as_ref().map(|h| h.bytes().to_vec()))
    }

    fn write(&mut self, kind: ClipboardKind, content: &[u8]) -> Result<(), ClipboardError> {
        self.set(kind, content);
        Ok(())
    }

    fn generation(&mut self) -> u64 {
        self.generation
    }

    fn clear(&mut self) -> Result<(), ClipboardError> {
        self.text = None;
        self.image = None;
        self.clears = self.clears.saturating_add(1);
        self.generation = self.generation.saturating_add(1);
        Ok(())
    }
}

/// A minimal PNG, for tests in this crate that need content of that kind.
#[cfg(test)]
pub(crate) fn conformance_png() -> Vec<u8> {
    let mut v = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    v.extend([0u8; 16]);
    v
}
