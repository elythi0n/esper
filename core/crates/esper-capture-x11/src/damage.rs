//! What changed, according to XDamage.
//!
//! # The report level, and why it is `NON_EMPTY`
//!
//! XDamage can report at four levels of detail. `RAW_RECTANGLES` sends an
//! event per rectangle, which for one text cursor blink and a scroll is
//! dozens of events a frame and a socket doing more work than the pixels
//! would have. `BOUNDING_BOX` sends one rectangle covering everything that
//! changed, which for a scrolling window and a moving cursor is the whole
//! screen — the exact case the band decomposition exists to avoid.
//!
//! `NON_EMPTY` sends **one** event when the damage region goes from empty to
//! not, and then says nothing until it is cleared. The region accumulates in
//! the server. So the loop is woken once however much changed, and then asks
//! for the whole accumulated region in one request. That is one event and one
//! round trip per frame regardless of how busy the screen is.
//!
//! # Subtract and fetch, together
//!
//! `DamageSubtract` moves the accumulated region out of the damage object and
//! into an XFixes region, atomically, and re-arms the event. Reading it in two
//! steps — fetch, then clear — would lose anything that changed in between,
//! which is a patch of screen that never repaints until something else
//! happens to touch it.

use esper_capture::geometry::Rect;
use x11rb::connection::Connection;
use x11rb::protocol::damage::{self, ConnectionExt as _};
use x11rb::protocol::xfixes::{self, ConnectionExt as _};
use x11rb::protocol::xproto::Drawable;

/// Why damage tracking is not available.
#[derive(Debug, thiserror::Error)]
pub enum DamageError {
    /// One of the two extensions is missing.
    #[error("{extension} is not available: {detail}")]
    Missing {
        extension: &'static str,
        detail: String,
    },
    /// The X server refused a request.
    #[error("X11: {0}")]
    X11(String),
}

/// A damage object on one drawable, and the region used to drain it.
#[derive(Debug)]
pub struct Tracker {
    damage: damage::Damage,
    region: xfixes::Region,
}

impl Tracker {
    /// Start watching `drawable`.
    ///
    /// Both extensions have to be handshaked before anything else is sent to
    /// them, and both are asked for at the versions whose requests this uses:
    /// XFixes 2 for regions, Damage 1.1 for subtract.
    pub fn create<C: Connection>(conn: &C, drawable: Drawable) -> Result<Self, DamageError> {
        conn.xfixes_query_version(5, 0)
            .map_err(|e| DamageError::Missing {
                extension: "XFIXES",
                detail: e.to_string(),
            })?
            .reply()
            .map_err(|e| DamageError::Missing {
                extension: "XFIXES",
                detail: e.to_string(),
            })?;
        conn.damage_query_version(1, 1)
            .map_err(|e| DamageError::Missing {
                extension: "DAMAGE",
                detail: e.to_string(),
            })?
            .reply()
            .map_err(|e| DamageError::Missing {
                extension: "DAMAGE",
                detail: e.to_string(),
            })?;

        let handle = conn
            .generate_id()
            .map_err(|e| DamageError::X11(e.to_string()))?;
        conn.damage_create(handle, drawable, damage::ReportLevel::NON_EMPTY)
            .map_err(|e| DamageError::X11(e.to_string()))?
            .check()
            .map_err(|e| DamageError::X11(e.to_string()))?;

        let region = conn
            .generate_id()
            .map_err(|e| DamageError::X11(e.to_string()))?;
        conn.xfixes_create_region(region, &[])
            .map_err(|e| DamageError::X11(e.to_string()))?
            .check()
            .map_err(|e| DamageError::X11(e.to_string()))?;

        Ok(Self {
            damage: handle,
            region,
        })
    }

    /// Take everything accumulated since the last call, in drawable
    /// coordinates.
    ///
    /// Empty when nothing changed, which on a damage-driven pipeline is the
    /// common answer rather than a failure.
    pub fn take<C: Connection>(&self, conn: &C) -> Result<Vec<Rect>, DamageError> {
        // Subtract into our region and re-arm, in one request.
        conn.damage_subtract(self.damage, x11rb::NONE, self.region)
            .map_err(|e| DamageError::X11(e.to_string()))?
            .check()
            .map_err(|e| DamageError::X11(e.to_string()))?;
        let reply = conn
            .xfixes_fetch_region(self.region)
            .map_err(|e| DamageError::X11(e.to_string()))?
            .reply()
            .map_err(|e| DamageError::X11(e.to_string()))?;
        Ok(reply
            .rectangles
            .into_iter()
            .map(|r| Rect {
                x: i32::from(r.x),
                y: i32::from(r.y),
                width: u32::from(r.width),
                height: u32::from(r.height),
            })
            .collect())
    }

    /// Release the server-side objects.
    ///
    /// Not in `Drop`, for the same reason the shared segment is not: both need
    /// the connection, and one that has gone away has nothing left to free.
    pub fn destroy<C: Connection>(&self, conn: &C) {
        let _ = conn.xfixes_destroy_region(self.region);
        let _ = conn.damage_destroy(self.damage);
    }
}
