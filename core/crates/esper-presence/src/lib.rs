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
pub mod locked;
pub mod null;
pub mod power;

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
                "something is drawn over the indicator: {wrong} of {of} places checked \
                 are not its colour, at {x},{y} the screen is #{:02x}{:02x}{:02x}",
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

/// What blanking the host's screen would turn off, and how to undo it.
///
/// **A plan, produced before anything changes.** The hazard of blanking a
/// screen is that the change outlives the process that made it, so the undo
/// has to be registered with something that will still be alive — and that
/// cannot happen after the change without leaving a window where a crash
/// leaves somebody's monitor dark and nobody holding the undo.
///
/// So the order is fixed: ask for a plan, register the plan, then apply it. A
/// registered plan that is never applied costs nothing, because putting a
/// screen back that was never turned off is a no-op.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Blanking {
    /// What a person would call what will go dark.
    pub what: String,
    /// How to put it back, for whoever has to.
    pub undo: Vec<esper_keeper::Undo>,
}

/// A desktop background about to be made plain, and how to put it back.
///
/// The same shape as [`Blanking`] and for the same reason: the plan is made by
/// the backend that can read the desktop, and the change is made by a caller
/// that has already handed the undo to something which will outlive it.
#[derive(Debug, Clone)]
pub struct Backdrop {
    /// What a person would call what is about to change.
    pub what: String,
    /// How to put it back, for whoever has to.
    pub undo: Vec<esper_keeper::Undo>,
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
    /// Whether the far end is writing this session to a file.
    ///
    /// **The fact that matters most, so it takes the colour.** A border can
    /// only say one thing at a time, and "somebody is keeping a copy of this"
    /// outranks "somebody can type" — a recording outlives the session, the
    /// room and the reason it was made. Which permissions are in force is
    /// still said in full on the host's own output and in its log; what the
    /// border is for is the thing a person has to notice without reading.
    ///
    /// It is the controller's own claim. Nothing can make that claim true — a
    /// modified client can record without saying so, exactly as somebody can
    /// point a camera at the screen — and this product always sends it.
    pub recording: bool,
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
/// How thick the indicator's frame is, in pixels.
///
/// Four is enough to be unmissable at any scaling and small enough to cover
/// nothing. A thinner line disappears on a high-density display.
///
/// Here rather than in a backend because there are two of them now — X11 and
/// wlroots — and an indicator that is four pixels on one display server and
/// something else on the other is a thing somebody would have to discover.
pub const THICKNESS: u16 = 4;

/// The colour when the far end can only watch: amber.
pub const WATCHING: u32 = 0x00FF_B000;

/// The colour when the far end can type: red.
///
/// Different because "somebody is looking at my screen" and "somebody can use
/// my machine" are different things to be told, and one colour for both would
/// train a person to ignore the one that matters.
pub const CONTROLLING: u32 = 0x00FF_2020;

/// The colour while the far end is recording: magenta.
///
/// **It takes precedence over the others**, because a border says one thing at
/// a time and this is the thing that outlives the session. Somebody who can
/// see their screen is being watched can decide what to do about it now;
/// somebody whose screen is being *kept* has already lost that choice by the
/// time they find out, so it is the fact that gets the colour.
///
/// Magenta because the other three are taken and because nothing else on a
/// desktop is this colour by accident.
pub const RECORDING: u32 = 0x00FF_00E0;

/// The colour while the picture is held: blue.
///
/// **Deliberately not amber or red.** Those two mean "somebody is watching"
/// and "somebody can type", and a paused session is neither — the point of the
/// third colour is that a person glancing at their own screen can tell "held"
/// from "being watched" without reading anything, which is the whole design of
/// this indicator.
///
/// And it is a colour rather than the frame coming down: taking it away would
/// say "nobody is connected", which is false and is the one lie this crate
/// exists to prevent.
pub const PAUSED: u32 = 0x0020_60FF;

/// What colour the frame should be for what is happening.
///
/// One function rather than the choice being made where the windows are built,
/// because a check that reads the screen back has to expect exactly what was
/// painted: two copies of this would eventually disagree, and the symptom
/// would be a session that stops because the indicator is the wrong shade of
/// correct.
///
/// **Recording takes precedence**, because a border says one thing at a time
/// and that is the thing which outlives the session: somebody who can see they
/// are being watched can decide what to do now, and a recording is decided
/// about later by somebody else.
#[must_use]
pub const fn colour_of(watching: &Watching) -> u32 {
    if watching.recording {
        RECORDING
    } else if watching.paused {
        PAUSED
    } else if watching.controlling {
        CONTROLLING
    } else {
        WATCHING
    }
}

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

    /// Work out what blanking this machine's screens would take.
    ///
    /// **Changes nothing.** It reads the display's current state and returns
    /// the undo for it, so a caller can register that undo with something
    /// that outlives this process *before* anything goes dark.
    ///
    /// # Errors
    ///
    /// If the display cannot be asked, or has no screen that could be turned
    /// off.
    fn blanking_plan(&mut self) -> Result<Blanking, PresenceError>;

    /// Blank this machine's own screens, leaving the session running.
    ///
    /// **For working on a machine in a shared room**: the person at the far
    /// end keeps their picture, and somebody walking past the host sees
    /// nothing.
    ///
    /// Takes the plan rather than making one, so the undo a caller registered
    /// is the undo that gets applied — and so this cannot be called without
    /// somebody having had the chance to register it.
    ///
    /// # Errors
    ///
    /// If the screens cannot be blanked, or if blanking them would break the
    /// session — which a backend checks by reading its own screen back. A
    /// backend **must undo its own work** before returning an error: a
    /// half-blanked screen whose caller got an error is the situation this
    /// whole arrangement is designed around.
    fn blank(&mut self, plan: &Blanking) -> Result<(), PresenceError>;

    /// Put the screen back.
    ///
    /// # Errors
    ///
    /// If the screen cannot be restored — which a caller should treat as
    /// serious and say out loud, because somebody is sitting in front of it.
    fn unblank(&mut self) -> Result<(), PresenceError>;

    /// Whether the panic key is held by this backend.
    ///
    /// **False is a real answer, not a failure.** Wayland gives an ordinary
    /// client no global hotkey by design, so a wlroots host has an indicator
    /// and no panic key — and a caller that assumed every indicator came with
    /// one would tell somebody a key exists that does nothing.
    fn panic_key_grabbed(&self) -> bool {
        false
    }

    /// Why there is no panic key here, when there is not one.
    ///
    /// `None` means there is one. The reason belongs to the backend because
    /// the reasons have nothing in common: on X11 something else has grabbed
    /// the combination, and on Wayland there is no such thing to grab. A
    /// caller that printed one message for both sent somebody on a Wayland
    /// desktop hunting for an application that was not there.
    fn why_no_panic_key(&self) -> Option<&'static str> {
        (!self.panic_key_grabbed()).then_some("this backend offers no panic key")
    }

    /// Why the frame cannot be shown, when it cannot.
    ///
    /// `None` means it can.
    fn why_cannot_show(&self) -> Option<&'static str> {
        (!self.can_show()).then_some("this backend cannot draw an indicator here")
    }

    /// Whether this backend can put the frame on the screen at all.
    ///
    /// Separate from [`Presence::is_showing`], which is about whether it has
    /// been asked to: a backend may open successfully and still have nothing
    /// to draw on.
    fn can_show(&self) -> bool {
        true
    }

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
