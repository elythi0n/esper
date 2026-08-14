//! Capturing a Windows desktop through DXGI Desktop Duplication.
//!
//! # Why Desktop Duplication and not Windows Graphics Capture
//!
//! WGC is newer, is the API Microsoft points at, and **does not report
//! damage**. Damage is what makes software encode viable here: an idle screen
//! has to cost nothing, and without a list of what changed the only way to
//! know is to compare whole frames, which costs more than the encode it saves.
//! So the older API, for the one thing it does that the newer one does not.
//!
//! # What is here and what is elsewhere
//!
//! This crate is the calls and the loop around them. Everything that could be
//! got wrong quietly lives in [`esper_capture`] and is compiled and tested on
//! every platform, because a `cfg(target_os = "windows")` crate's tests run
//! nowhere else:
//!
//! - what a frame's metadata means — [`esper_capture::duplication`]
//! - what a pointer shape's bytes mean — [`esper_capture::pointer_shape`]
//! - **when a frame may be acquired and must be released** —
//!   [`esper_capture::frames`], which is the rule that wedges Duplication when
//!   it is broken
//!
//! [`sys`] holds every `unsafe` call, in one file, with an inventory.
//!
//! # One output at a time, for now
//!
//! Duplication duplicates an output. Tiling several into one picture is the
//! X11 backend's arrangement and it needs a second staging texture and a
//! composite step; asked for more than one screen this refuses with
//! [`esper_capture::CaptureError::CannotTile`] and the session keeps what it
//! has, which is the contract the trait states for a backend that cannot.

#![cfg(target_os = "windows")]

mod sys;

use std::time::Duration;

use esper_capture::capturer::{Capture, CaptureError, Capturer, MonitorId, MonitorInfo, Numbering};
use esper_capture::damage::Damage;
use esper_capture::frames::{Frames, Next};
use esper_capture::pool::BufferPool;
use esper_capture::{Frame, PixelFormat, duplication};

/// The name this backend goes by in a log and in the session HUD.
pub const BACKEND: &str = "dxgi duplication";

/// A capturer over one duplicated output.
#[derive(Debug)]
pub struct DxgiCapturer {
    /// The duplication, absent while it is being rebuilt.
    ///
    /// `None` after `ACCESS_LOST` and before the next successful open — a
    /// session switch, a mode change, another process going exclusive full
    /// screen, or the secure desktop coming up. A capturer in that state is
    /// not broken: it answers `Unchanged` and tries again.
    live: Option<sys::Duplicated>,
    /// The frame lifecycle, which decides what may be called next.
    frames: Frames,
    /// Every output, numbered, with DXGI's own index for each.
    ///
    /// The index is kept rather than looked up: finding it means enumerating
    /// the adapter, which means creating a Direct3D device, and doing that on
    /// every `serve` would be a device per click.
    monitors: Vec<MonitorInfo>,
    indices: Indices,
    /// Which one is being captured.
    serving: Vec<MonitorId>,
    /// Its index for `sys`, which keys on DXGI's own numbering rather than
    /// ours.
    index: u32,
    numbering: Numbering,
    pool: BufferPool,
    /// The last cursor seen, so a caller can ask without a frame arriving.
    cursor: Option<esper_capture::cursor::CursorImage>,
}

impl DxgiCapturer {
    /// Duplicate an output, or the primary one.
    ///
    /// # Errors
    ///
    /// If there is no Direct3D device, no such output, or the output is
    /// rotated — see [`sys::SysError::Rotated`] for why that is a refusal
    /// rather than a sideways picture.
    pub fn open(monitor: Option<MonitorId>) -> Result<Self, CaptureError> {
        let mut numbering = Numbering::new();
        let (mut monitors, indices) = list(&mut numbering)?;
        if monitors.is_empty() {
            return Err(CaptureError::NoBackend {
                tried: format!("{BACKEND}: this machine reports no attached output"),
            });
        }
        let count = monitors.len();
        let wanted = match monitor {
            Some(id) => monitors
                .iter()
                .find(|m| m.id == id)
                .ok_or(CaptureError::NoSuchMonitor { id, count })?,
            // The primary if the platform names one, else the first — which is
            // the order `list` already put them in.
            None => monitors
                .iter()
                .find(|m| m.primary)
                .or_else(|| monitors.first())
                .ok_or(CaptureError::NoBackend {
                    tried: format!("{BACKEND}: no output to capture"),
                })?,
        };
        let id = wanted.id;
        let index =
            index_of(&monitors, &indices, id).ok_or(CaptureError::NoSuchMonitor { id, count })?;

        let live = sys::Duplicated::open(index).map_err(from_sys)?;
        // The duplication may describe the output differently from the
        // enumeration — a mode change between the two calls — so what it says
        // is what the caller is told.
        refresh(&mut monitors, live.screen(), id);

        Ok(Self {
            live: Some(live),
            frames: Frames::new(),
            monitors,
            indices,
            serving: vec![id],
            index,
            numbering,
            pool: BufferPool::default(),
            cursor: None,
        })
    }

    /// The pointer as the last frame described it, if one did.
    #[must_use]
    pub const fn cursor(&self) -> Option<&esper_capture::cursor::CursorImage> {
        self.cursor.as_ref()
    }

    /// Build the duplication again after it was lost.
    ///
    /// Returns whether the output's geometry moved, because that is a
    /// renegotiation rather than a frame: every buffer size, the encoder's
    /// configuration and the viewer's window depend on it.
    fn rebuild(&mut self) -> Result<bool, CaptureError> {
        let id = self.serving.first().copied();
        let before = id
            .and_then(|id| self.monitors.iter().find(|m| m.id == id))
            .map(|m| m.geometry);

        // **Re-enumerated through the same numbering.** A mode change or a
        // replug must not renumber an output: `Numbering` exists so that a
        // monitor keeps the number a session already knows it by, and the
        // primary display in particular is the one thing that must never be
        // renumbered.
        let (fresh, indices) = list(&mut self.numbering)?;
        self.monitors = fresh;
        self.indices = indices;
        if let Some(id) = id
            && let Some(index) = index_of(&self.monitors, &self.indices, id)
        {
            self.index = index;
        }

        let live = sys::Duplicated::open(self.index).map_err(from_sys)?;
        let moved = before.is_some_and(|was| was != live.screen().bounds);
        if let Some(id) = id {
            refresh(&mut self.monitors, live.screen(), id);
        }
        self.live = Some(live);
        self.frames = Frames::new();
        Ok(moved)
    }
}

/// An output's name and the index DXGI knows it by.
type Indices = Vec<(String, u32)>;

/// Every attached output, numbered, primary first — and DXGI's index for each.
fn list(numbering: &mut Numbering) -> Result<(Vec<MonitorInfo>, Indices), CaptureError> {
    let screens = sys::screens().map_err(from_sys)?;
    let indices = screens
        .iter()
        .filter(|s| s.attached)
        .map(|s| (s.name.clone(), s.index))
        .collect();
    let mut out: Vec<MonitorInfo> = screens
        .iter()
        .filter(|s| s.attached)
        .map(|s| MonitorInfo {
            id: MonitorId(0),
            name: s.name.clone(),
            geometry: s.bounds,
            // **Not asked for here.** DXGI does not report a scale, and
            // `GetDpiForMonitor` needs the process to be DPI-aware to answer
            // truthfully — which is a decision the host makes once at startup,
            // not something a capturer should set on the side. Reported as 1.0
            // until that is wired through, which is what every backend without
            // fractional scaling reports.
            scale: 1.0,
            // DXGI has no notion of a primary output. The primary monitor is
            // the one whose desktop coordinates start at the origin, which is
            // how Windows defines it.
            primary: s.bounds.x == 0 && s.bounds.y == 0,
        })
        .collect();
    // Primary first, so `open(None)` and `serving()` agree about "the
    // monitor" without either having to look it up.
    out.sort_by_key(|m| !m.primary);
    numbering.number(&mut out);
    Ok((out, indices))
}

/// DXGI's index for one of our numbers.
///
/// By name, because our numbering and DXGI's ordering are independent — the
/// list is sorted primary-first here and DXGI's is not — and the name is the
/// one thing both sides agree on.
fn index_of(monitors: &[MonitorInfo], indices: &Indices, id: MonitorId) -> Option<u32> {
    let name = &monitors.iter().find(|m| m.id == id)?.name;
    indices
        .iter()
        .find(|(known, _)| known == name)
        .map(|(_, index)| *index)
}

/// Put what the duplication says about an output into the list.
fn refresh(monitors: &mut [MonitorInfo], screen: &sys::Screen, id: MonitorId) {
    if let Some(m) = monitors.iter_mut().find(|m| m.id == id) {
        m.geometry = screen.bounds;
        m.name = screen.name.clone();
    }
}

fn from_sys(e: sys::SysError) -> CaptureError {
    CaptureError::Backend {
        backend: BACKEND,
        detail: e.to_string(),
    }
}

impl Capturer for DxgiCapturer {
    fn backend(&self) -> &'static str {
        BACKEND
    }

    fn monitors(&self) -> &[MonitorInfo] {
        &self.monitors
    }

    fn serving(&self) -> &[MonitorId] {
        &self.serving
    }

    fn format(&self) -> PixelFormat {
        // The desktop arrives as BGRA with a real alpha channel, which is what
        // the staging texture is created as.
        PixelFormat::Bgra8
    }

    fn reports_damage(&self) -> bool {
        true
    }

    fn next_frame(&mut self, timeout: Duration) -> Result<Capture, CaptureError> {
        // Lost last time round. Rebuilding here rather than at the moment of
        // loss, so a session switch costs one `Unchanged` instead of a failed
        // capture — and so the rebuild happens on the capture thread's own
        // schedule.
        if self.live.is_none() || self.frames.is_finished() {
            let moved = self.rebuild()?;
            return Ok(if moved {
                Capture::Reconfigured(self.monitors.clone())
            } else {
                Capture::Unchanged
            });
        }
        let Some(live) = self.live.as_mut() else {
            return Ok(Capture::Unchanged);
        };

        // Asked before the call, not after: an acquire while a frame is held
        // fails and keeps failing, and the session goes on looking connected
        // while producing nothing.
        self.frames
            .may_acquire()
            .map_err(|e| CaptureError::Backend {
                backend: BACKEND,
                detail: e.to_string(),
            })?;

        let outcome = live.acquire(timeout);
        let next = self
            .frames
            .acquired(outcome)
            .map_err(|e| CaptureError::Backend {
                backend: BACKEND,
                detail: e.to_string(),
            })?;
        match next {
            Next::AskAgain => return Ok(Capture::Unchanged),
            Next::Recreate => {
                self.live = None;
                return Ok(Capture::Unchanged);
            }
            Next::Read => {}
        }

        // From here a release is owed on every path, which is what the
        // closure below is for: the readback's errors must not skip it.
        let read = read_held(live, &self.pool, &self.monitors, &self.serving);
        match read {
            Ok((frame, cursor)) => {
                if cursor.is_some() {
                    self.cursor = cursor;
                }
                live.release().map_err(from_sys)?;
                self.frames.released().map_err(|e| CaptureError::Backend {
                    backend: BACKEND,
                    detail: e.to_string(),
                })?;
                Ok(Capture::Frame(frame))
            }
            Err(e) => {
                // The duplication may have gone during the readback —
                // `GetFrameDirtyRects` and the copy can both report it — so
                // the lock went with it and there is nothing to release into.
                self.frames.lost();
                self.live = None;
                Err(e)
            }
        }
    }

    fn serve(&mut self, monitors: &[MonitorId]) -> Result<(), CaptureError> {
        match monitors {
            [] => Err(CaptureError::NoSuchMonitor {
                id: MonitorId(u32::MAX),
                count: self.monitors.len(),
            }),
            [only] if self.serving.as_slice() == [*only] => Ok(()),
            [only] => {
                let count = self.monitors.len();
                let index = index_of(&self.monitors, &self.indices, *only)
                    .ok_or(CaptureError::NoSuchMonitor { id: *only, count })?;
                let live = sys::Duplicated::open(index).map_err(from_sys)?;
                refresh(&mut self.monitors, live.screen(), *only);
                self.live = Some(live);
                self.frames = Frames::new();
                self.index = index;
                self.serving = vec![*only];
                self.cursor = None;
                Ok(())
            }
            // Duplication duplicates one output. Tiling needs a second
            // texture and a composite step; refused so the session keeps what
            // it has rather than silently sending one screen of several.
            several => Err(CaptureError::CannotTile {
                backend: BACKEND,
                asked: several.len(),
            }),
        }
    }
}

/// Read the held frame: its damage, its pointer, and its pixels.
///
/// Split out so that the caller has one place to release from, whatever this
/// returns — the release is owed on every path once a frame is held.
fn read_held(
    live: &mut sys::Duplicated,
    pool: &BufferPool,
    monitors: &[MonitorInfo],
    serving: &[MonitorId],
) -> Result<(Frame, Option<esper_capture::cursor::CursorImage>), CaptureError> {
    let bounds = serving
        .first()
        .and_then(|id| monitors.iter().find(|m| m.id == *id))
        .map(|m| m.geometry)
        .unwrap_or(live.screen().bounds);

    let (moves, dirty, complete) = live.rects().map_err(from_sys)?;
    let mut report = live.report();
    report.metadata_complete = complete;
    // The frame of reference is the output's own, starting at zero: DXGI's
    // rectangles are in the desktop image's space, not the virtual desktop's.
    let frame_rect = esper_capture::Rect::sized(bounds.width, bounds.height);
    let damage = duplication::damage_of(&report, &moves, &dirty, &frame_rect);

    let cursor = live.pointer().map_err(from_sys)?;

    let (width, height) = (bounds.width, bounds.height);
    let stride = usize::try_from(width).unwrap_or(0).saturating_mul(4);
    let mut buffer = pool.take(stride.saturating_mul(usize::try_from(height).unwrap_or(0)));

    // **Nothing changed but the pointer.** Still a frame as far as Duplication
    // is concerned, so the release is owed — but there is no picture to send,
    // so the pixels are not read at all. That is the whole point of the
    // damage-driven design: an idle screen with a moving mouse costs a buffer
    // and no readback.
    if !matches!(damage, Damage::Nothing) {
        live.read_into(&mut buffer, stride).map_err(from_sys)?;
    }
    let frame = Frame::new(width, height, stride, PixelFormat::Bgra8, damage, buffer)?;
    Ok((frame, cursor))
}

const _: fn() = || {
    // The trait requires `Send`, and the COM interfaces this holds satisfy it
    // in `windows` 0.62 — checked rather than assumed, because an earlier
    // reading of the crate's source said otherwise and was wrong.
    fn assert_send<T: Send>() {}
    assert_send::<DxgiCapturer>();
};
