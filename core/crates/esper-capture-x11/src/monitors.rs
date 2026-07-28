//! What monitors this X server has.
//!
//! # RandR 1.5, with two fallbacks
//!
//! `RRGetMonitors` is the only X11 request that answers the question directly:
//! it reports the *logical* monitors a desktop is arranged into, already
//! reflecting rotation, and it names a primary. It arrived in RandR 1.5, which
//! is 2015 and universal on anything current.
//!
//! Below that there are CRTCs, which are the hardware scan-out engines. They
//! are close enough to monitors to be useful and different enough to be wrong
//! in specific cases — two mirrored outputs on one CRTC are one entry, which
//! is right, while a monitor driven by two CRTCs is two, which is not. It is a
//! better answer than refusing to run.
//!
//! Below *that* is the root window, reported as one monitor covering
//! everything. On the four-monitor desktop this was written against that is a
//! 5920x2977 rectangle containing three gaps, which is a poor description of
//! reality — but a session that captures a strange-looking screen is worth
//! more than one that will not start.

use esper_capture::capturer::{MonitorId, MonitorInfo};
use esper_capture::geometry::Rect;
use x11rb::connection::Connection;
use x11rb::protocol::randr::ConnectionExt as _;
use x11rb::protocol::xproto::{ConnectionExt as _, Window};

/// Why monitors could not be listed.
#[derive(Debug, thiserror::Error)]
pub enum MonitorError {
    /// The X server refused or failed a request.
    #[error("X11: {0}")]
    X11(String),
    /// The server reported no usable monitor at all.
    #[error("the X server reports no monitor with a non-zero size")]
    None,
}

/// How the monitor list was obtained, for the log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// `RRGetMonitors`, the direct answer.
    RandrMonitors,
    /// CRTCs, close enough but not the same question.
    RandrCrtcs,
    /// The root window as a single monitor.
    RootWindow,
}

impl Source {
    /// A phrase for the startup log.
    #[must_use]
    pub const fn describe(self) -> &'static str {
        match self {
            Self::RandrMonitors => "RandR 1.5 monitors",
            Self::RandrCrtcs => "RandR CRTCs (no 1.5: mirrored outputs may merge)",
            Self::RootWindow => "the root window only (no RandR: gaps included)",
        }
    }
}

/// The monitor list, and where it came from.
#[derive(Debug, Clone)]
pub struct Monitors {
    pub list: Vec<MonitorInfo>,
    pub source: Source,
}

/// Every monitor on `root`, best source first.
pub fn enumerate<C: Connection>(conn: &C, root: Window) -> Result<Monitors, MonitorError> {
    if let Some(list) = from_randr_monitors(conn, root).filter(|l| !l.is_empty()) {
        return Ok(Monitors {
            list: order(list),
            source: Source::RandrMonitors,
        });
    }
    if let Some(list) = from_randr_crtcs(conn, root).filter(|l| !l.is_empty()) {
        return Ok(Monitors {
            list: order(list),
            source: Source::RandrCrtcs,
        });
    }
    let list = from_root(conn, root)?;
    if list.is_empty() {
        return Err(MonitorError::None);
    }
    Ok(Monitors {
        list: order(list),
        source: Source::RootWindow,
    })
}

/// Sort for presentation. **Numbering is somebody else's job.**
///
/// The primary first, then left to right and top to bottom. Ordering is a
/// product decision rather than a technical one: `--monitor 0` has to mean the
/// screen the person is most likely to want, and the X server's own order is
/// whatever RandR happens to return.
///
/// This used to hand out ids by position here, which is stable until the
/// desktop changes and then silently is not — unplugging the middle of three
/// monitors renumbers the third onto the second's number, and a session
/// watching it is watching a different screen without having asked. Numbers
/// come from [`esper_capture::Numbering`], which the capturer owns because that
/// is the thing with a lifetime for them to be stable across.
fn order(mut list: Vec<MonitorInfo>) -> Vec<MonitorInfo> {
    list.sort_by_key(|m| (!m.primary, m.geometry.x, m.geometry.y, m.name.clone()));
    list
}

/// X11 has no backing-store scale, and saying 1.0 is the truth rather than a
/// default.
///
/// On Wayland and macOS a window is drawn at a logical size and scaled up, so
/// there is a real factor to report. X11's framebuffer is the pixels: a HiDPI
/// screen is simply a big one, and toolkits scale themselves by reading
/// `Xft.dpi`. Deriving a factor from physical millimetres would be inventing a
/// number the compositor does not have, and every consumer of it would then be
/// scaling twice.
const X11_SCALE: f32 = 1.0;

fn from_randr_monitors<C: Connection>(conn: &C, root: Window) -> Option<Vec<MonitorInfo>> {
    // `get_active = true`: monitors with no CRTC are configured but dark, and
    // capturing one produces a rectangle of nothing.
    let reply = conn.randr_get_monitors(root, true).ok()?.reply().ok()?;
    let mut out = Vec::with_capacity(reply.monitors.len());
    for m in reply.monitors {
        if m.width == 0 || m.height == 0 {
            continue;
        }
        let name = conn
            .get_atom_name(m.name)
            .ok()
            .and_then(|c| c.reply().ok())
            .map(|r| String::from_utf8_lossy(&r.name).into_owned())
            .unwrap_or_else(|| format!("monitor at {},{}", m.x, m.y));
        out.push(MonitorInfo {
            id: MonitorId(0),
            name,
            geometry: Rect {
                x: i32::from(m.x),
                y: i32::from(m.y),
                width: u32::from(m.width),
                height: u32::from(m.height),
            },
            scale: X11_SCALE,
            primary: m.primary,
        });
    }
    Some(out)
}

fn from_randr_crtcs<C: Connection>(conn: &C, root: Window) -> Option<Vec<MonitorInfo>> {
    let res = conn
        .randr_get_screen_resources_current(root)
        .ok()?
        .reply()
        .ok()?;
    let primary = conn
        .randr_get_output_primary(root)
        .ok()
        .and_then(|c| c.reply().ok())
        .map(|r| r.output);

    let mut out = Vec::new();
    for crtc in res.crtcs {
        let Ok(info) = conn.randr_get_crtc_info(crtc, res.config_timestamp) else {
            continue;
        };
        let Ok(info) = info.reply() else { continue };
        if info.width == 0 || info.height == 0 {
            continue;
        }
        // A CRTC's width and height are already the rotated size, which is
        // what a captured frame will be.
        let is_primary = primary.is_some_and(|p| info.outputs.contains(&p));
        let name = info
            .outputs
            .first()
            .and_then(|o| conn.randr_get_output_info(*o, res.config_timestamp).ok())
            .and_then(|c| c.reply().ok())
            .map(|r| String::from_utf8_lossy(&r.name).into_owned())
            .unwrap_or_else(|| format!("CRTC {crtc}"));
        out.push(MonitorInfo {
            id: MonitorId(0),
            name,
            geometry: Rect {
                x: i32::from(info.x),
                y: i32::from(info.y),
                width: u32::from(info.width),
                height: u32::from(info.height),
            },
            scale: X11_SCALE,
            primary: is_primary,
        });
    }
    Some(out)
}

fn from_root<C: Connection>(conn: &C, root: Window) -> Result<Vec<MonitorInfo>, MonitorError> {
    let geom = conn
        .get_geometry(root)
        .map_err(|e| MonitorError::X11(e.to_string()))?
        .reply()
        .map_err(|e| MonitorError::X11(e.to_string()))?;
    if geom.width == 0 || geom.height == 0 {
        return Ok(Vec::new());
    }
    Ok(vec![MonitorInfo {
        id: MonitorId(0),
        name: "the whole X screen".to_owned(),
        geometry: Rect {
            x: 0,
            y: 0,
            width: u32::from(geom.width),
            height: u32::from(geom.height),
        },
        scale: X11_SCALE,
        primary: true,
    }])
}

/// Whether two monitor lists describe the same desktop.
///
/// Compared on everything a consumer depends on and **not** on the id, which
/// is assigned by position: a monitor unplugged from the middle renumbers the
/// ones after it, and that is a reconfiguration whichever way the ids fall.
#[must_use]
pub fn same_layout(a: &[MonitorInfo], b: &[MonitorInfo]) -> bool {
    a.len() == b.len()
        && a.iter().zip(b.iter()).all(|(x, y)| {
            x.name == y.name
                && x.geometry == y.geometry
                && x.primary == y.primary
                && x.scale.to_bits() == y.scale.to_bits()
        })
}
