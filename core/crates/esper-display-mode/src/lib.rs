//! Changing a screen's resolution for a session, and putting it back.
//!
//! # Why this is not just "call `SetCrtcConfig`"
//!
//! A person controlling a 4K workstation from a laptop wants the remote screen
//! to *be* a laptop screen for the duration, not a 4K screen scaled into one.
//! Scaling loses text; changing the mode does not. So the host changes its own
//! resolution — and having changed somebody else's screen, it is now
//! responsible for changing it back.
//!
//! That responsibility is the whole crate. Putting it back on a clean teardown
//! is easy and is not the case that matters: **the case that matters is the
//! host dying with the screen still changed**, because the person sitting at
//! that machine is then left with a resolution they did not choose and no idea
//! what did it. A guard that runs on `Drop` does nothing for a `SIGKILL`, an
//! OOM kill, or a power cut.
//!
//! So the original is written to a file *before* the change is made, and
//! removed after it is put back. [`restore_anything_left_behind`] reads that
//! file at startup and undoes whatever a previous run did not. The file is the
//! promise; the guard is only the fast path.
//!
//! # `GetScreenResources`, not `GetScreenResourcesCurrent`
//!
//! The `Current` form is the one to reach for: it answers from the server's
//! cache instead of re-probing the hardware, which is what you want for a
//! question asked repeatedly. It also returns **nothing at all** on a server
//! that has not been asked the full question first — Xephyr answers it with
//! zero CRTCs, zero outputs and zero modes, which reads exactly like a display
//! that cannot be resized rather than like a cache that was never filled.
//!
//! This is not a hot path. A resolution is listed when a session starts and
//! changed when somebody asks, so the expensive form is the right one and the
//! cheap one is a trap.
//!
//! # The order of the two requests is not free
//!
//! X has a screen — the virtual canvas — and CRTCs that scan out parts of it.
//! A CRTC cannot be configured to fall outside the screen, so growing means
//! `SetScreenSize` first and then `SetCrtcConfig`, while shrinking means the
//! reverse. Getting it backwards fails with a `Match` error on a request that
//! looks perfectly correct in isolation.
//!
//! **This ordering is reasoned from the protocol, not measured, and that is
//! worth saying out loud.** Xephyr is what the test rig nests, and it neither
//! enforces the constraint nor needs the request: it resizes its own canvas
//! when a CRTC is set to a larger mode, so a build with `SetScreenSize`
//! removed entirely passes every test here, including one that reads the
//! canvas back. Verifying the order needs a server that enforces it — Xorg
//! with the `dummy` driver, which is not installed on the machine this was
//! written on.
//!
//! So what is proven here is [`canvas_size`], the arithmetic, and what is
//! reasoned is when to send it. Keeping the requests in the right order costs
//! nothing and removing them would break on the servers that do care, which is
//! every real one.

use std::path::{Path, PathBuf};

use x11rb::connection::Connection;
use x11rb::protocol::randr::{self, ConnectionExt as _};
use x11rb::protocol::xproto::{ConnectionExt as _, Window};
use x11rb::rust_connection::RustConnection;

/// Why a resolution could not be read or changed.
#[derive(Debug, thiserror::Error)]
pub enum ModeError {
    /// The X server refused or failed a request.
    #[error("X11: {0}")]
    X11(String),
    /// No RandR, or a version too old to set a CRTC.
    #[error("this display server does not support changing the resolution")]
    Unsupported,
    /// Nothing on this display covers the area asked about.
    #[error("no screen covers the area the session is capturing")]
    NoSuchScreen,
    /// The screen cannot be set to that.
    #[error("{width}x{height} is not one of the modes this screen offers")]
    NoSuchMode { width: u32, height: u32 },
    /// The server accepted the request and refused the configuration.
    #[error("the display server refused the change ({0})")]
    Refused(String),
    /// The record of what to put back could not be kept.
    #[error("could not record the resolution to put back: {0}")]
    Record(String),
}

fn x11<E: std::fmt::Debug>(e: E) -> ModeError {
    ModeError::X11(format!("{e:?}"))
}

/// One resolution a screen can be set to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Mode {
    pub width: u32,
    pub height: u32,
    /// Refresh rate in millihertz, so it stays an integer on the wire and in
    /// the record. 60,000 is 60 Hz. Zero means the server did not say, which
    /// a virtual display commonly does not.
    pub refresh_mhz: u32,
}

impl Mode {
    /// Whether this is a resolution worth offering.
    ///
    /// Anything under 640x480 is not a desktop, and the upper bound is the
    /// protocol's own picture limit.
    #[must_use]
    pub const fn is_usable(&self) -> bool {
        self.width >= 640 && self.height >= 480 && self.width <= 8192 && self.height <= 8192
    }
}

/// What a previous run changed and has not put back.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct Outstanding {
    /// The display it was changed on. A record from another display is left
    /// alone: it is not this run's to undo, and guessing would change a screen
    /// nobody asked about.
    display: String,
    /// The RandR output, by the name the server gives it.
    output: String,
    /// What it was before.
    was: Mode,
}

/// The file a pending restoration lives in, inside the state directory.
const RECORD: &str = "display-restore.json";

/// A screen whose resolution can be changed.
#[derive(Debug, Clone)]
pub struct Screen {
    /// What the server calls the output, for showing a person and for the
    /// record.
    pub name: String,
    /// Where it sits on the desktop, so a caller can match it to the monitor
    /// a session is capturing.
    pub x: i32,
    pub y: i32,
    /// What it is set to now.
    pub current: Mode,
    /// Everything it could be set to, largest first, duplicates removed.
    pub modes: Vec<Mode>,
    crtc: randr::Crtc,
}

/// The displays on one X server.
#[derive(Debug)]
pub struct Displays {
    conn: RustConnection,
    root: Window,
    display: String,
    state_dir: PathBuf,
}

impl Displays {
    /// Connect, and find out whether this server can do it at all.
    ///
    /// # Errors
    ///
    /// [`ModeError::Unsupported`] when there is no RandR 1.2, which is what
    /// introduced CRTCs; anything older can only set the whole screen and is
    /// not worth a second code path in 2026.
    pub fn open(state_dir: &Path) -> Result<Self, ModeError> {
        let display = std::env::var("DISPLAY").unwrap_or_default();
        let (conn, screen_num) = RustConnection::connect(None).map_err(x11)?;
        let root = conn
            .setup()
            .roots
            .get(screen_num)
            .ok_or(ModeError::Unsupported)?
            .root;
        let version = conn
            .randr_query_version(1, 2)
            .map_err(x11)?
            .reply()
            .map_err(|_| ModeError::Unsupported)?;
        if version.major_version < 1 || (version.major_version == 1 && version.minor_version < 2) {
            return Err(ModeError::Unsupported);
        }
        Ok(Self {
            conn,
            root,
            display,
            state_dir: state_dir.to_path_buf(),
        })
    }

    /// Every screen that can be resized, with what it can be set to.
    ///
    /// # Errors
    ///
    /// [`ModeError::X11`] if the server will not answer.
    pub fn screens(&self) -> Result<Vec<Screen>, ModeError> {
        let resources = self
            .conn
            .randr_get_screen_resources(self.root)
            .map_err(x11)?
            .reply()
            .map_err(x11)?;
        let stamp = resources.config_timestamp;

        // Mode ids are meaningless on their own; this is the dictionary.
        let by_id: std::collections::HashMap<u32, Mode> = resources
            .modes
            .iter()
            .map(|m| (m.id, from_mode_info(m)))
            .collect();

        let mut out = Vec::new();
        for &output in &resources.outputs {
            let Ok(info) = self
                .conn
                .randr_get_output_info(output, stamp)
                .map_err(x11)?
                .reply()
            else {
                continue;
            };
            // A disconnected output has no screen behind it to resize, and
            // an output with no CRTC is not being scanned out. `0` is
            // `XCB_NONE`, which is how RandR says "none".
            if info.connection != randr::Connection::CONNECTED || info.crtc == 0 {
                continue;
            }
            let Ok(crtc) = self
                .conn
                .randr_get_crtc_info(info.crtc, stamp)
                .map_err(x11)?
                .reply()
            else {
                continue;
            };

            let mut modes: Vec<Mode> = info
                .modes
                .iter()
                .filter_map(|id| by_id.get(id).copied())
                .filter(Mode::is_usable)
                .collect();
            // Largest first is the order a person reads a resolution list in,
            // and the duplicates are real: a mode offered at two refresh rates
            // is two entries with the same size, which is noise on a virtual
            // display where the rate is zero anyway.
            modes.sort_by_key(|m| {
                (
                    std::cmp::Reverse(m.width),
                    std::cmp::Reverse(m.height),
                    std::cmp::Reverse(m.refresh_mhz),
                )
            });
            modes.dedup_by_key(|m| (m.width, m.height, m.refresh_mhz));

            let current = by_id.get(&crtc.mode).copied().unwrap_or(Mode {
                width: u32::from(crtc.width),
                height: u32::from(crtc.height),
                refresh_mhz: 0,
            });

            out.push(Screen {
                name: String::from_utf8_lossy(&info.name).into_owned(),
                x: i32::from(crtc.x),
                y: i32::from(crtc.y),
                current,
                modes,
                crtc: info.crtc,
            });
        }
        Ok(out)
    }

    /// How big the desktop canvas is.
    ///
    /// Every CRTC has to fit inside this, which is why changing a resolution
    /// is two requests rather than one. Exposed because it is the only way to
    /// see that the second one happened: a server that does not enforce the
    /// constraint will happily scan out a CRTC hanging off the edge, and the
    /// screens still read back correctly while the desktop is the wrong size.
    ///
    /// # Errors
    ///
    /// [`ModeError::X11`] if the server will not answer.
    pub fn canvas(&self) -> Result<(u32, u32), ModeError> {
        let geometry = self
            .conn
            .get_geometry(self.root)
            .map_err(x11)?
            .reply()
            .map_err(x11)?;
        Ok((u32::from(geometry.width), u32::from(geometry.height)))
    }

    /// The screen whose current mode covers this point on the desktop.
    ///
    /// Matching by position rather than by name, because the name a monitor
    /// list gives and the name an output has are not the same string on every
    /// server, and the position is what both are derived from.
    ///
    /// # Errors
    ///
    /// [`ModeError::NoSuchScreen`] if nothing covers it.
    pub fn covering(&self, x: i32, y: i32) -> Result<Screen, ModeError> {
        self.screens()?
            .into_iter()
            .find(|s| {
                let (w, h) = (s.current.width as i32, s.current.height as i32);
                x >= s.x && y >= s.y && x < s.x.saturating_add(w) && y < s.y.saturating_add(h)
            })
            .ok_or(ModeError::NoSuchScreen)
    }

    /// Set a screen's resolution, remembering what it was.
    ///
    /// The record is written **before** the change, so a host that dies
    /// between the two leaves a record of a change that did not happen —
    /// which restores to what is already in force and is harmless. The other
    /// order would leave a change nobody recorded, which is the one that
    /// cannot be undone.
    ///
    /// # Errors
    ///
    /// [`ModeError::NoSuchMode`] if the screen does not offer it, and
    /// [`ModeError::Refused`] if the server declines the configuration.
    pub fn set(&mut self, screen: &Screen, want: Mode) -> Result<(), ModeError> {
        let wanted = screen
            .modes
            .iter()
            .find(|m| m.width == want.width && m.height == want.height)
            .copied()
            .ok_or(ModeError::NoSuchMode {
                width: want.width,
                height: want.height,
            })?;

        self.remember(&Outstanding {
            display: self.display.clone(),
            output: screen.name.clone(),
            was: screen.current,
        })?;

        self.apply(screen, wanted)
    }

    /// Put a screen back to a mode, without recording anything.
    ///
    /// # Errors
    ///
    /// As [`Self::set`].
    fn apply(&self, screen: &Screen, want: Mode) -> Result<(), ModeError> {
        let resources = self
            .conn
            .randr_get_screen_resources(self.root)
            .map_err(x11)?
            .reply()
            .map_err(x11)?;
        let stamp = resources.config_timestamp;
        let mode_id = resources
            .modes
            .iter()
            .find(|m| {
                u32::from(m.width) == want.width
                    && u32::from(m.height) == want.height
                    && (want.refresh_mhz == 0 || from_mode_info(m).refresh_mhz == want.refresh_mhz)
            })
            .map(|m| m.id)
            .ok_or(ModeError::NoSuchMode {
                width: want.width,
                height: want.height,
            })?;

        let crtc = self
            .conn
            .randr_get_crtc_info(screen.crtc, stamp)
            .map_err(x11)?
            .reply()
            .map_err(x11)?;

        // The canvas has to contain the CRTC at every instant, so a change
        // that grows it needs the canvas first and one that shrinks it needs
        // the canvas last. See the module documentation.
        let growing = want.width > u32::from(crtc.width) || want.height > u32::from(crtc.height);
        let canvas = self.canvas_for(&resources, screen, want)?;

        if growing {
            self.resize_canvas(canvas)?;
        }
        let reply = self
            .conn
            .randr_set_crtc_config(
                screen.crtc,
                x11rb::CURRENT_TIME,
                stamp,
                crtc.x,
                crtc.y,
                mode_id,
                crtc.rotation,
                &crtc.outputs,
            )
            .map_err(x11)?
            .reply()
            .map_err(x11)?;
        if reply.status != randr::SetConfig::SUCCESS {
            return Err(ModeError::Refused(format!("{:?}", reply.status)));
        }
        if !growing {
            self.resize_canvas(canvas)?;
        }
        Ok(())
    }

    /// How big the canvas has to be for every CRTC, with this one changed.
    fn canvas_for(
        &self,
        resources: &randr::GetScreenResourcesReply,
        screen: &Screen,
        want: Mode,
    ) -> Result<(u16, u16), ModeError> {
        let stamp = resources.config_timestamp;
        let mut others = Vec::new();
        for &crtc_id in &resources.crtcs {
            if crtc_id == screen.crtc {
                continue;
            }
            let Ok(info) = self
                .conn
                .randr_get_crtc_info(crtc_id, stamp)
                .map_err(x11)?
                .reply()
            else {
                continue;
            };
            if info.mode == 0 {
                continue;
            }
            others.push(Placed {
                x: i32::from(info.x),
                y: i32::from(info.y),
                width: u32::from(info.width),
                height: u32::from(info.height),
            });
        }
        Ok(canvas_size(
            &others,
            Placed {
                x: screen.x,
                y: screen.y,
                width: want.width,
                height: want.height,
            },
        ))
    }

    /// Resize the virtual canvas, keeping the physical size proportional so
    /// the reported DPI does not jump.
    fn resize_canvas(&self, (w, h): (u16, u16)) -> Result<(), ModeError> {
        // 96 DPI in millimetres, which is what every desktop assumes when the
        // server reports nothing useful — and a virtual display reports 0x0,
        // which makes some toolkits compute an absurd scale.
        let mm = |px: u16| u32::from(px) * 254 / 960;
        self.conn
            .randr_set_screen_size(self.root, w, h, mm(w), mm(h))
            .map_err(x11)?
            .check()
            .map_err(|e| ModeError::Refused(format!("{e:?}")))?;
        Ok(())
    }

    /// Put back whatever this run changed, and forget it.
    ///
    /// # Errors
    ///
    /// [`ModeError::Record`] if the record cannot be read or removed.
    pub fn restore(&mut self) -> Result<(), ModeError> {
        let Some(outstanding) = self.recorded()? else {
            return Ok(());
        };
        if outstanding.display != self.display {
            // Another display's business. Left exactly as found.
            return Ok(());
        }
        if let Some(screen) = self
            .screens()?
            .into_iter()
            .find(|s| s.name == outstanding.output)
        {
            self.apply(&screen, outstanding.was)?;
        }
        self.forget()
    }

    fn record_path(&self) -> PathBuf {
        self.state_dir.join(RECORD)
    }

    fn remember(&self, what: &Outstanding) -> Result<(), ModeError> {
        // Only the first change is recorded. A session that steps through
        // three resolutions must come back to what the person had, not to
        // whatever it was set to a moment ago.
        if self.recorded()?.is_some() {
            return Ok(());
        }
        std::fs::create_dir_all(&self.state_dir).map_err(|e| ModeError::Record(e.to_string()))?;
        let body = serde_json::to_vec_pretty(what).map_err(|e| ModeError::Record(e.to_string()))?;
        std::fs::write(self.record_path(), body).map_err(|e| ModeError::Record(e.to_string()))
    }

    fn recorded(&self) -> Result<Option<Outstanding>, ModeError> {
        match std::fs::read(self.record_path()) {
            Ok(body) => match serde_json::from_slice(&body) {
                Ok(o) => Ok(Some(o)),
                // A record that cannot be read is worse than none: it would
                // block every future restoration. Said out loud and dropped.
                Err(why) => {
                    tracing::warn!(%why, "the resolution record is unreadable; discarding it");
                    let _ = std::fs::remove_file(self.record_path());
                    Ok(None)
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(ModeError::Record(e.to_string())),
        }
    }

    fn forget(&self) -> Result<(), ModeError> {
        match std::fs::remove_file(self.record_path()) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(ModeError::Record(e.to_string())),
        }
    }
}

/// What a startup restoration found to do, and whether it managed it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Restored {
    /// Nothing was outstanding, which is almost always.
    Nothing,
    /// A previous run had changed the resolution; it is back.
    PutBack,
    /// Something was outstanding and the display could not be reached.
    ///
    /// **Carried rather than logged away.** This is a screen somebody is
    /// sitting in front of, left at a resolution they did not choose, and the
    /// host is the only thing that knows. Swallowing the reason cost an
    /// afternoon of debugging on the day it was written, which is a fair
    /// preview of what it would cost somebody with a wrong screen.
    CouldNotReach(String),
}

/// Undo a resolution change a previous run did not put back.
///
/// Called at startup, before anything else touches the display. A host that
/// cannot reach the display says so and starts anyway — a headless server has
/// no display and is the normal case, and refusing to start would be worse
/// than a resolution nobody can fix remotely.
///
/// # Errors
///
/// [`ModeError::Record`] if the record itself cannot be read or removed, which
/// is a broken state directory rather than a display problem.
pub fn restore_anything_left_behind(state_dir: &Path) -> Result<Restored, ModeError> {
    if !state_dir.join(RECORD).exists() {
        return Ok(Restored::Nothing);
    }
    match Displays::open(state_dir) {
        Ok(mut displays) => {
            displays.restore()?;
            Ok(Restored::PutBack)
        }
        Err(why) => Ok(Restored::CouldNotReach(why.to_string())),
    }
}

/// Something occupying a rectangle of the desktop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Placed {
    x: i32,
    y: i32,
    width: u32,
    height: u32,
}

/// The smallest canvas that contains every screen, with one of them changed.
///
/// Pure arithmetic, and separated from the requests around it for a reason:
/// the *ordering* of `SetScreenSize` and `SetCrtcConfig` cannot be tested on
/// the rig this workspace has — Xephyr does not enforce the constraint and
/// resizes its canvas on its own, so a build that never resized it passed
/// every test written against a live server. This half can be tested, so it
/// is.
fn canvas_size(others: &[Placed], changed: Placed) -> (u16, u16) {
    let mut w = edge_at(changed.x, changed.width);
    let mut h = edge_at(changed.y, changed.height);
    for other in others {
        w = w.max(edge_at(other.x, other.width));
        h = h.max(edge_at(other.y, other.height));
    }
    (clamp_to_u16(w), clamp_to_u16(h))
}

/// The far edge of something placed at `at` with this size.
///
/// A monitor left of the primary one sits at a negative x; its far edge is
/// what the canvas has to contain, and a negative position contributes
/// nothing to the size.
fn edge_at(at: i32, size: u32) -> u32 {
    let far = i64::from(at).saturating_add(i64::from(size));
    u32::try_from(far.max(0)).unwrap_or(u32::MAX)
}

/// A canvas dimension, as X can express it.
fn clamp_to_u16(v: u32) -> u16 {
    u16::try_from(v).unwrap_or(u16::MAX)
}

/// A RandR mode's refresh rate, in millihertz.
///
/// The dot clock divided by the total pixels in a frame. Both totals include
/// the blanking intervals, which is why they are larger than the visible size
/// — using the visible size here is the classic way to get 59.94 wrong.
fn from_mode_info(m: &randr::ModeInfo) -> Mode {
    let total = u64::from(m.htotal) * u64::from(m.vtotal);
    let refresh_mhz = (u64::from(m.dot_clock) * 1000)
        .checked_div(total)
        .and_then(|hz| u32::try_from(hz).ok())
        .unwrap_or(0);
    Mode {
        width: u32::from(m.width),
        height: u32::from(m.height),
        refresh_mhz,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_refresh_rate_comes_from_the_totals_not_the_visible_size() {
        let m = randr::ModeInfo {
            id: 1,
            width: 1920,
            height: 1080,
            dot_clock: 148_500_000,
            hsync_start: 2008,
            hsync_end: 2052,
            htotal: 2200,
            hskew: 0,
            vsync_start: 1084,
            vsync_end: 1089,
            vtotal: 1125,
            name_len: 0,
            mode_flags: randr::ModeFlag::default(),
        };
        // 148.5 MHz / (2200 * 1125) = 60.000 Hz exactly.
        assert_eq!(from_mode_info(&m).refresh_mhz, 60_000);
    }

    #[test]
    fn a_mode_with_no_timings_reports_no_rate_rather_than_dividing_by_zero() {
        let m = randr::ModeInfo {
            id: 1,
            width: 800,
            height: 600,
            dot_clock: 0,
            hsync_start: 0,
            hsync_end: 0,
            htotal: 0,
            hskew: 0,
            vsync_start: 0,
            vsync_end: 0,
            vtotal: 0,
            name_len: 0,
            mode_flags: randr::ModeFlag::default(),
        };
        assert_eq!(from_mode_info(&m).refresh_mhz, 0);
    }

    fn at(x: i32, y: i32, width: u32, height: u32) -> Placed {
        Placed {
            x,
            y,
            width,
            height,
        }
    }

    #[test]
    fn one_screen_needs_a_canvas_its_own_size() {
        assert_eq!(canvas_size(&[], at(0, 0, 1600, 1200)), (1600, 1200));
    }

    #[test]
    fn a_second_screen_still_has_to_fit() {
        // Shrinking the left-hand screen must not cut off the right-hand one,
        // which starts where the old width ended.
        let others = [at(1920, 0, 1920, 1080)];
        assert_eq!(canvas_size(&others, at(0, 0, 1280, 1024)), (3840, 1080));
    }

    #[test]
    fn a_screen_left_of_the_primary_one_starts_at_a_negative_x() {
        // Its far edge is what the canvas has to hold; the negative position
        // contributes nothing, and arithmetic that wrapped would ask for four
        // billion pixels.
        assert_eq!(canvas_size(&[], at(-1920, 0, 1920, 1080)), (0, 1080));
        // A screen whose far edge is *still* left of the origin contributes
        // nothing at all. Chosen because the obvious wrong arithmetic —
        // casting the position to unsigned and adding — happens to give the
        // right answer when the edge lands exactly on zero, and gives 65535
        // here.
        assert_eq!(canvas_size(&[], at(-100, 0, 50, 10)), (0, 10));
        let others = [at(-1920, -1080, 1920, 1080)];
        assert_eq!(canvas_size(&others, at(0, 0, 2560, 1440)), (2560, 1440));
    }

    #[test]
    fn a_canvas_larger_than_x_can_express_is_clamped_rather_than_wrapped() {
        assert_eq!(canvas_size(&[], at(0, 0, 100_000, 8)), (u16::MAX, 8));
    }

    #[test]
    fn a_mode_too_small_to_be_a_desktop_is_not_offered() {
        assert!(
            !Mode {
                width: 320,
                height: 240,
                refresh_mhz: 60_000
            }
            .is_usable()
        );
        assert!(
            Mode {
                width: 1280,
                height: 1024,
                refresh_mhz: 0
            }
            .is_usable()
        );
    }
}
