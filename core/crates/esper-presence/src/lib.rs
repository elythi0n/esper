//! Presence: what the person at the host can see, and what they can do.
//!
//! # These are not settings
//!
//! Software that can watch a screen must make being watched obvious, and
//! software that can drive a keyboard must leave the person in front of it a
//! way to stop that. Both of those are properties of the product, not
//! preferences — so there is no configuration here to turn them off, and the
//! protocol has no message that could ask for either.
//!
//! **That absence is the mechanism.** A controller cannot suppress the
//! indicator because nothing it can send is about the indicator; it cannot
//! swallow the panic key because the key is grabbed by the host from the
//! window server, above every application, and the host's own engine acts on
//! it. The way to check both is to try, from the controller, and find there is
//! nothing to try with.
//!
//! # What the indicator has to be, and what it does not
//!
//! It has to be **unmissable and unsuppressible**. It does not have to be
//! pretty, and it is not the product's interface — the real one arrives with
//! the rest of the viewer's chrome.
//!
//! So the minimal version is a frame around the edge of the screen, in a
//! colour that says whether the far end can only watch or can also type. No
//! text: a modern X server ships no fonts at all — measured, on both the
//! development desktop and `Xvfb` — so text would mean a font dependency and a
//! bundled typeface for a badge that is about to be replaced. A coloured
//! border cannot be misread, and *who* is connected is already named in the
//! host's own output.

pub mod lock;
pub mod null;

pub use lock::{Locks, Reach, available_locker, lock_reaching, lock_screen};
pub use null::{NullPresence, Shown};

/// Anything that went wrong showing presence.
#[derive(Debug, thiserror::Error)]
pub enum PresenceError {
    /// Nothing on this machine can show an indicator.
    ///
    /// **A caller must treat this as serious.** A host that cannot show that
    /// it is being watched is a host that should say so very loudly, because
    /// the property it is failing to provide is one a person relies on.
    #[error("no way to show a session indicator: {tried}")]
    NoBackend { tried: String },

    /// The backend refused or failed.
    #[error("{backend}: {detail}")]
    Backend {
        backend: &'static str,
        detail: String,
    },
}

/// What the indicator is saying.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Watching {
    /// Who is connected, as the host knows them.
    pub peer: String,
    /// Whether they can type as well as watch.
    ///
    /// The colour changes with this, because "somebody is looking at my
    /// screen" and "somebody can use my machine" are different things to be
    /// told.
    ///
    /// **What they may do, not what they have done yet.** A peer holding
    /// `control` that has not sent a keystroke can send one at any moment, and
    /// an indicator that stayed amber until the first one would be a
    /// reassurance that expires without warning.
    pub controlling: bool,
    /// Whether the picture is being held right now.
    ///
    /// **A third thing to say, not the absence of the other two.** Taking the
    /// indicator down while paused would say "nobody is connected", which is
    /// false and is the one lie this crate exists to prevent: the session is
    /// still there, the permissions still stand, and the far end is one
    /// keystroke from seeing the screen again.
    pub paused: bool,
}

/// Something the person at the host did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostEvent {
    /// The panic key.
    ///
    /// Ends every session at once. It revokes nothing — a person reaching for
    /// this is not making a policy decision, they are making it stop — and the
    /// screen is locked, so whatever was on it is not left visible.
    Panic,
    /// The pause key.
    ///
    /// **The thing to reach for instead of the panic key**, and the reason it
    /// exists: the panic key is for "stop", and most of the time somebody
    /// wants "wait". Holding the picture keeps the connection, the permissions
    /// and any transfer in flight, so nothing has to be set up again — where
    /// ending the session means pairing state, a prompt, and a keyframe.
    ///
    /// A press rather than a state, because the *session* owns whether it is
    /// paused. A key that reported "now paused" would be a second place the
    /// answer lived, and the two would disagree the first time a press was
    /// missed.
    Pause,
}

/// Somewhere to show that a session is happening.
pub trait Presence: std::fmt::Debug {
    /// Which backend this is, for the log.
    fn backend(&self) -> &'static str;

    /// Show the indicator.
    ///
    /// Called again when what it says changes — a device escalating from
    /// watching to controlling has to change the colour, or the indicator is
    /// telling the person something that is no longer true.
    fn show(&mut self, watching: &Watching) -> Result<(), PresenceError>;

    /// Take the indicator away, because there is no longer a session.
    fn hide(&mut self) -> Result<(), PresenceError>;

    /// Whatever the person at the host did since the last call.
    ///
    /// Non-blocking, and called often: the panic key is the one thing in this
    /// product where a delay is measured against somebody's alarm.
    fn poll(&mut self) -> Result<Vec<HostEvent>, PresenceError>;

    /// Whether the indicator is up.
    fn is_showing(&self) -> bool;
}
