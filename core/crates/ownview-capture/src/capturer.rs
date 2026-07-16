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
