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
    /// Every name ever seen, with the number it was given and when it was last
    /// asked about.
    given: Vec<Remembered>,
    /// The next number never used.
    next: u32,
    /// Ticks up on every lookup, so the least recently seen can be found.
    now: u64,
}

/// One name and the number it was given.
#[derive(Debug)]
struct Remembered {
    name: String,
    id: MonitorId,
    /// The tick it was last asked about.
    at: u64,
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
        self.now = self.now.saturating_add(1);
        let at = self.now;
        if let Some(seen) = self.given.iter_mut().find(|seen| seen.name == name) {
            seen.at = at;
            return seen.id;
        }
        let id = MonitorId(self.next);
        self.next = self.next.saturating_add(1);
        // Bounded: a machine docked and undocked for a month should not grow
        // this without limit.
        //
        // **The least recently asked about goes, not the oldest.** A docking
        // station's screens come and go; the built-in panel is present at every
        // enumeration for the life of the process, and is also the very first
        // name ever seen — so evicting the oldest would evict the primary
        // display, which is the one thing here that must never be renumbered.
        if self.given.len() >= MOST_REMEMBERED
            && let Some(stalest) = self
                .given
                .iter()
                .enumerate()
                .min_by_key(|(_, seen)| seen.at)
                .map(|(i, _)| i)
        {
            self.given.remove(stalest);
        }
        self.given.push(Remembered {
            name: name.to_owned(),
            id,
            at,
        });
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

#[cfg(test)]
mod numbering_tests {
    use super::*;

    fn named(names: &[&str]) -> Vec<MonitorInfo> {
        names
            .iter()
            .enumerate()
            .map(|(i, name)| MonitorInfo {
                id: MonitorId(u32::MAX),
                name: (*name).to_owned(),
                geometry: crate::geometry::Rect {
                    x: i32::try_from(i).unwrap_or(0) * 1920,
                    y: 0,
                    width: 1920,
                    height: 1080,
                },
                scale: 1.0,
                primary: i == 0,
            })
            .collect()
    }

    fn numbers(list: &[MonitorInfo]) -> Vec<u32> {
        list.iter().map(|m| m.id.0).collect()
    }

    /// **The bug this exists for.** Three monitors, unplug the middle one, and
    /// numbering by position moves the third onto the second's number — so a
    /// session watching it is watching a different screen without having asked.
    #[test]
    fn unplugging_the_middle_of_three_does_not_renumber_the_third() {
        let mut n = Numbering::new();
        let mut all = named(&["DP-1", "DP-2", "HDMI-1"]);
        n.number(&mut all);
        assert_eq!(numbers(&all), vec![0, 1, 2]);

        let mut without_the_middle = named(&["DP-1", "HDMI-1"]);
        n.number(&mut without_the_middle);
        assert_eq!(
            numbers(&without_the_middle),
            vec![0, 2],
            "HDMI-1 was renumbered onto the screen that was unplugged"
        );

        // And plugged back in, it is itself again.
        let mut back = named(&["DP-1", "DP-2", "HDMI-1"]);
        n.number(&mut back);
        assert_eq!(numbers(&back), vec![0, 1, 2]);
    }

    /// A screen that arrives after another left gets a number never used.
    #[test]
    fn a_new_screen_never_inherits_a_departed_ones_number() {
        let mut n = Numbering::new();
        let mut two = named(&["DP-1", "DP-2"]);
        n.number(&mut two);
        assert_eq!(numbers(&two), vec![0, 1]);

        let mut swapped = named(&["DP-1", "HDMI-1"]);
        n.number(&mut swapped);
        assert_eq!(
            numbers(&swapped),
            vec![0, 2],
            "a different screen was given the departed one's number"
        );
        assert_eq!(n.handed_out(), 3);
    }

    /// The list's order is left exactly as it came: what order screens are
    /// presented in and what they are called are two different questions.
    #[test]
    fn the_order_of_the_list_is_not_touched() {
        let mut n = Numbering::new();
        let mut list = named(&["c", "a", "b"]);
        n.number(&mut list);
        assert_eq!(
            list.iter().map(|m| m.name.as_str()).collect::<Vec<_>>(),
            vec!["c", "a", "b"]
        );
        assert_eq!(numbers(&list), vec![0, 1, 2]);
    }

    /// **The primary is the one thing that must never be renumbered**, and it
    /// is also the first name ever seen — so an eviction policy of "the oldest
    /// goes" would evict exactly it. The stalest goes instead, and the primary
    /// is never stale because it is present at every enumeration.
    #[test]
    fn the_screen_that_is_always_there_is_never_the_one_forgotten() {
        let mut n = Numbering::new();
        let mut built_in = named(&["eDP-1"]);
        n.number(&mut built_in);
        let panel = built_in.first().map(|m| m.id).expect("one screen");

        // A docking station's worth of screens, coming and going, past the
        // bound — with the built-in panel present every time, as it is.
        for round in 0..MOST_REMEMBERED * 2 {
            let dock = format!("dock-{round}");
            let mut list = named(&["eDP-1", &dock]);
            n.number(&mut list);
        }

        let mut alone = named(&["eDP-1"]);
        n.number(&mut alone);
        assert_eq!(
            alone.first().map(|m| m.id),
            Some(panel),
            "the built-in panel was forgotten and renumbered"
        );
    }

    /// And it is bounded, or a month of docking grows it for ever.
    #[test]
    fn it_does_not_remember_for_ever() {
        let mut n = Numbering::new();
        for i in 0..MOST_REMEMBERED * 3 {
            let mut list = named(&[&format!("screen-{i}")]);
            n.number(&mut list);
        }
        assert!(
            n.given.len() <= MOST_REMEMBERED,
            "it remembers {} names",
            n.given.len()
        );
        // Every number is still distinct, however many were forgotten.
        assert_eq!(n.handed_out() as usize, MOST_REMEMBERED * 3);
    }
}
