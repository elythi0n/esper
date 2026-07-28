//! The capture backend interface.
//!
//! # Why this is synchronous
//!
//! Every real backend is a blocking call with a timeout: `XShmGetImage`,
//! `AcquireNextFrame`, a PipeWire buffer wait. A capture loop also wants a
//! thread of its own — it runs to a deadline, and sharing an executor with
//! request handling means a slow handler shows up as dropped frames.
//!
//! So the trait blocks, the loop gets a dedicated thread, and the frames go to
//! the rest of the pipeline over a channel. Making it `async` would also rule
//! out `dyn Capturer`, and the backend has to be chosen at runtime.

use std::time::Duration;

use crate::frame::{Frame, FrameError, PixelFormat};
use crate::geometry::Rect;

/// Identifies a monitor within one capturer.
///
/// Assigned by the capturer and stable for its lifetime. Not a platform
/// identifier — those are strings on X11, indices on DXGI and node numbers on
/// PipeWire, and none of them survive being unplugged. The platform's own name
/// is in [`MonitorInfo::name`], for showing a person.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MonitorId(pub u32);

impl std::fmt::Display for MonitorId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "monitor {}", self.0)
    }
}

/// One monitor, as the host sees it.
#[derive(Debug, Clone, PartialEq)]
pub struct MonitorInfo {
    pub id: MonitorId,
    /// What the platform calls it — `DP-1`, `\\.\DISPLAY1`. Shown to a person,
    /// never matched on.
    pub name: String,
    /// Position and size in desktop coordinates.
    ///
    /// The origin can be negative: a monitor to the left of the primary one
    /// starts at a negative x, and pretending otherwise is how multi-monitor
    /// coordinate bugs start.
    pub geometry: Rect,
    /// Backing-store scale, as reported. 1.0 on a normal display, 2.0 on a
    /// typical HiDPI one, and fractional on Wayland with fractional scaling.
    pub scale: f32,
    /// Whether the platform calls this the primary display.
    pub primary: bool,
}

impl MonitorInfo {
    /// Size in pixels, which is what a frame from this monitor will be.
    #[must_use]
    pub const fn size(&self) -> (u32, u32) {
        (self.geometry.width, self.geometry.height)
    }
}

/// Numbers that stay with a monitor for as long as a capturer runs.
///
/// # The bug this exists to stop
///
/// The obvious way to number a monitor list is by position in it: sort for
/// presentation, then hand out 0, 1, 2. That is stable until the desktop
/// changes, and then it silently is not.
///
/// Three monitors, A B C, numbered 0 1 2. Unplug **B**. Re-enumerate, sort,
/// renumber: A is 0 and **C is now 1**. A session watching 1 was watching B
/// and is now watching C — a different screen, with no switch asked for and
/// possibly the same size, so nothing downstream has any reason to notice. The
/// controller's monitor picker is wrong, the pointer is expressed as a fraction
/// of the wrong rectangle, and the person sees somebody else's desktop.
///
/// So a number is handed out once and kept. A monitor that comes back gets the
/// one it had; a new one gets a number never used before. The *order* of the
/// list stays a presentation decision — primary first, then left to right —
/// and has nothing to do with the numbers in it.
///
/// # What it is keyed on
///
/// The platform's own name, which is the only thing available and is good
/// enough for one capturer's lifetime: `DP-1` is a port, two monitors never
/// share one, and a screen unplugged and plugged back into the same port is
/// the same screen by any definition a person would use. It is deliberately
/// **not** a claim that the name means anything across restarts — see
/// [`MonitorId`].
#[derive(Debug, Default)]
pub struct Numbering {
    /// Every name ever seen, with the number it was given.
    given: Vec<(String, MonitorId)>,
    /// The next number never used.
    next: u32,
}

impl Numbering {
    /// A fresh set of numbers.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Give each of these the number it already has, or a new one.
    ///
    /// The list's order is left exactly as it came: what order the monitors
    /// are presented in and what they are called are two different questions.
    pub fn number(&mut self, list: &mut [MonitorInfo]) {
        for m in list {
            m.id = self.number_of(&m.name);
        }
    }

    /// The number for this name, assigning one if it has never been seen.
    #[must_use]
    pub fn number_of(&mut self, name: &str) -> MonitorId {
        if let Some((_, id)) = self.given.iter().find(|(seen, _)| seen == name) {
            return *id;
        }
        let id = MonitorId(self.next);
        self.next = self.next.saturating_add(1);
        // Bounded: a machine that plugs monitors in and out for a month should
        // not grow this without limit. The oldest name goes, which at worst
        // means a very long-departed monitor gets a fresh number on its
        // return — and nothing is watching it.
        if self.given.len() >= MOST_REMEMBERED {
            self.given.remove(0);
        }
        self.given.push((name.to_owned(), id));
        id
    }

    /// How many numbers have been handed out.
    #[must_use]
    pub const fn handed_out(&self) -> u32 {
        self.next
    }
}

/// How many monitor names a capturer remembers numbers for.
///
/// Far more than any desktop has, and a bound so that plugging screens in and
/// out all day cannot grow it for ever.
const MOST_REMEMBERED: usize = 64;

/// What a capture attempt produced.
#[derive(Debug)]
pub enum Capture {
    /// A frame, with what changed in it.
    Frame(Frame),
    /// Nothing changed before the deadline.
    ///
    /// A first-class answer, not a failure. On a damage-driven pipeline it is
    /// the *common* answer — a screen showing a document changes a few times a
    /// minute — and a backend that had to invent a frame to say so would
    /// defeat the whole design.
    Unchanged,
    /// The monitor's geometry changed, so the stream must be renegotiated.
    ///
    /// Separate from a frame because every buffer size, the encoder's
    /// configuration and the viewer's window all depend on it. A backend that
    /// silently returned a differently-sized frame would leave each of those
    /// to notice on its own.
    Reconfigured(Vec<MonitorInfo>),
}

/// What went wrong capturing.
#[derive(Debug, thiserror::Error)]
pub enum CaptureError {
    /// No backend works on this machine.
    ///
    /// Names what was tried, because "capture failed" on a headless server
    /// tells the operator nothing they can act on.
    #[error("no screen capture backend is available ({tried})")]
    NoBackend { tried: String },

    /// The requested monitor is not there.
    #[error("{id} does not exist; this host has {count}")]
    NoSuchMonitor { id: MonitorId, count: usize },

    /// This backend cannot change monitor once it is capturing.
    ///
    /// Separate from [`Self::NoSuchMonitor`] because the two mean opposite
    /// things to a controller: one is a monitor that does not exist, the other
    /// is a monitor that does and a backend that will not move to it.
    #[error("the {backend} backend cannot change monitor once it is capturing")]
    CannotSwitchMonitor { backend: &'static str },

    /// The user has not granted, or has withdrawn, screen capture permission.
    ///
    /// Its own variant because it is the one failure a person can fix, and it
    /// is common: a Wayland portal grant expires, macOS requires an explicit
    /// grant per application, and both look like a broken product if reported
    /// as a generic error.
    #[error("screen capture was not permitted: {detail}")]
    NotPermitted { detail: String },

    /// The frame the backend produced does not describe itself consistently.
    #[error(transparent)]
    BadFrame(#[from] FrameError),

    /// The backend failed.
    #[error("{backend}: {detail}")]
    Backend {
        backend: &'static str,
        detail: String,
    },
}

/// A source of frames.
pub trait Capturer: std::fmt::Debug + Send {
    /// Which backend this is, for the log and the session HUD.
    fn backend(&self) -> &'static str;

    /// Every monitor this capturer can see.
    fn monitors(&self) -> &[MonitorInfo];

    /// The monitor being captured.
    fn selected(&self) -> MonitorId;

    /// The pixel layout frames arrive in.
    fn format(&self) -> PixelFormat;

    /// Whether the backend reports real damage.
    ///
    /// A backend that cannot always answers [`crate::damage::Damage::Everything`],
    /// and the pipeline needs to know: with no damage information the idle
    /// ladder is the only thing keeping a static screen cheap, and it has to be
    /// driven by comparing frames instead.
    fn reports_damage(&self) -> bool;

    /// Wait up to `timeout` for the next frame.
    fn next_frame(&mut self, timeout: Duration) -> Result<Capture, CaptureError>;

    /// Capture a different monitor from now on.
    ///
    /// Required rather than defaulted, so a new backend has to answer the
    /// question instead of inheriting a refusal nobody notices. A backend that
    /// genuinely cannot switch says [`CaptureError::CannotSwitchMonitor`], and
    /// the session keeps the monitor it has.
    ///
    /// Selecting the monitor already selected is a no-op, **not** a reset: a
    /// controller clicking the current monitor should not cost a keyframe.
    /// Anything else discards the previous picture, so the next frame is whole.
    fn select(&mut self, monitor: MonitorId) -> Result<(), CaptureError>;
}
