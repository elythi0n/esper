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
//! # Shown is not the same as on screen
//!
//! Asking for the indicator and having it is not the same thing, and until
//! [`Presence::verify`] existed this crate could not tell the difference: a
//! backend recorded that `show` had been *called*, which is a fact about this
//! process rather than about the screen. Everything that can go wrong between
//! the two — a window that never mapped, one destroyed by a compositor
//! restarting, one restacked underneath something, or a local program drawing
//! over it — looked identical to success.
//!
//! So a backend has to answer a second question: **is it on the screen now?**
//! The answer has to come back through a different path than the one that drew
//! it, or it is the same fact asked twice. On X11 the frame is drawn by
//! creating windows and read back by asking the server for the pixels, which
//! is what the capture path does and therefore what the person in front of the
//! machine is looking at.
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

/// A rectangle on the screen, in the screen's own coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

/// Whether the indicator is on the screen, asked of the screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// It is there, confirmed at this many places.
    ///
    /// The count is carried because "confirmed at zero places" is the shape a
    /// broken check takes, and a caller that logs the number cannot mistake
    /// one for the other.
    OnScreen { checked: usize },
    /// It is not, and this is what was found instead.
    Missing(Missing),
}

impl Verdict {
    /// Whether a session may go on.
    #[must_use]
    pub const fn is_on_screen(&self) -> bool {
        matches!(self, Self::OnScreen { checked } if *checked > 0)
    }
}

/// Why the indicator is not on the screen.
///
/// **Each of these is a different thing to do about it**, which is why they
/// are not one boolean: a frame that was never asked for is not a fault, a
/// frame whose windows are gone can be built again, and a frame with something
/// drawn over it cannot be fixed from here at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Missing {
    /// Nothing is meant to be showing, so nothing is wrong.
    NothingAsked,
    /// The windows are gone — destroyed by something else.
    Gone,
    /// The windows exist and the server is not showing them.
    Unmapped,
    /// The pixels where the indicator should be are not its colour.
    ///
    /// Which is the case this whole mechanism exists for: everything else can
    /// be inferred from this process's own state, and this cannot.
    Covered {
        /// How many sampled points were wrong, of how many looked at.
        wrong: usize,
        of: usize,
        /// What was there instead, and where, so a person can go and look.
        saw: [u8; 3],
        at: (i32, i32),
    },
    /// The screen could not be read, so this is not a verdict either way.
    ///
    /// **Deliberately not treated as "it is there".** A check that cannot run
    /// says so; a caller decides what a session is allowed to do while nobody
    /// can tell.
    Unreadable(String),
}

impl Missing {
    /// A sentence for the person running the host.
    #[must_use]
    pub fn describe(&self) -> String {
        match self {
            Self::NothingAsked => "no indicator was asked for".to_owned(),
            Self::Gone => "the indicator's windows are gone".to_owned(),
            Self::Unmapped => "the indicator's windows are not on screen".to_owned(),
            Self::Covered {
                wrong,
                of,
                saw,
                at: (x, y),
            } => format!(
                "something is drawn over the indicator: {wrong} of {of} places checked are not                  its colour, at {x},{y} the screen is #{:02x}{:02x}{:02x}",
                saw[0], saw[1], saw[2]
            ),
            Self::Unreadable(why) => format!("the screen could not be read: {why}"),
        }
    }

    /// Whether building the frame again could plausibly fix it.
    ///
    /// A compositor restarting destroys override-redirect windows, which is
    /// worth one attempt to put back. Something drawn *over* the frame is not
    /// this crate's to fix — raising it is tried anyway, and if that were
    /// enough the next verdict says so.
    #[must_use]
    pub const fn worth_rebuilding(&self) -> bool {
        matches!(self, Self::Gone | Self::Unmapped)
    }
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

    /// Whether `show` was called and not undone.
    ///
    /// **A fact about this process, not about the screen** — which is the
    /// distinction [`Presence::verify`] exists for. Nothing that relies on the
    /// indicator being visible may rely on this.
    fn is_showing(&self) -> bool;

    /// Ask the screen whether the indicator is on it.
    ///
    /// Answered by reading the screen back, not by consulting this process's
    /// own memory of what it asked for. A backend that cannot read its screen
    /// returns [`Missing::Unreadable`] rather than guessing, because a check
    /// that cannot run must not report success.
    ///
    /// # Errors
    ///
    /// Only if the display itself has gone away. Everything about the
    /// indicator's own state is a [`Verdict`], not an error: "it is not there"
    /// is an answer.
    fn verify(&mut self) -> Result<Verdict, PresenceError>;
}
