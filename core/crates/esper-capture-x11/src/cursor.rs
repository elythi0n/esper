//! Reading the pointer from an X server.
//!
//! # One request for everything
//!
//! XFIXES' `GetCursorImage` answers the whole question at once: the shape, the
//! hotspot, the position, and a **serial** the server bumps when the shape
//! changes. So "send the shape only when it changes" is a comparison rather
//! than a heuristic, and reading the position costs nothing extra.
//!
//! # Its own connection
//!
//! Separate from the capturer's, because this is polled on a different
//! schedule — a mouse moves faster than a screen changes — and sharing a
//! connection would mean one thread's reply sitting behind another's, on the
//! path whose whole purpose is to be quick.
//!
//! # Premultiplied, and byte order named
//!
//! XFIXES returns premultiplied ARGB packed into `u32`s in the server's own
//! order. `u32`s are unpacked here into RGBA bytes so that nothing downstream
//! has to know what a machine's word order is.

use esper_capture::capturer::CaptureError;
use esper_capture::cursor::{CursorImage, CursorSource, CursorState};
use x11rb::protocol::xfixes::ConnectionExt as _;
use x11rb::rust_connection::RustConnection;

/// The backend name reported in logs.
pub const BACKEND: &str = "x11-xfixes";

/// The largest cursor this will carry, per side.
///
/// The protocol's own bound, restated here so an absurd cursor is refused
/// before its pixels are unpacked rather than after.
const MAX_EDGE: u16 = 256;

/// Reads the pointer from an X server.
pub struct X11Cursor {
    conn: RustConnection,
}

impl std::fmt::Debug for X11Cursor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("X11Cursor").finish_non_exhaustive()
    }
}

impl X11Cursor {
    /// Connect, and check XFIXES is really there.
    pub fn open() -> Result<Self, CaptureError> {
        let (conn, _) = x11rb::connect(None).map_err(|e| CaptureError::NoBackend {
            tried: format!("X11 via DISPLAY: {e}"),
        })?;
        // Version 1 is enough for `GetCursorImage`; asked for rather than
        // assumed, because a server can list an extension it will not serve.
        conn.xfixes_query_version(5, 0)
            .map_err(|e| CaptureError::NoBackend {
                tried: format!("X11 XFIXES: {e}"),
            })?
            .reply()
            .map_err(|e| CaptureError::NoBackend {
                tried: format!("X11 XFIXES: {e}"),
            })?;
        Ok(Self { conn })
    }
}

impl CursorSource for X11Cursor {
    fn backend(&self) -> &'static str {
        BACKEND
    }

    fn read(&mut self) -> Result<Option<CursorState>, CaptureError> {
        let fail = |detail: String| CaptureError::Backend {
            backend: BACKEND,
            detail,
        };
        let reply = match self
            .conn
            .xfixes_get_cursor_image()
            .map_err(|e| fail(e.to_string()))?
            .reply()
        {
            Ok(reply) => reply,
            // **A display with no pointer has no pointer, and that is not a
            // fault.** A bare `Xvfb` has no pointer device and answers this
            // request with `Access`; treated as an error, a host there would
            // claim to be sending a cursor and then complain on every read.
            Err(x11rb::errors::ReplyError::X11Error(e))
                if e.error_kind == x11rb::protocol::ErrorKind::Access =>
            {
                return Ok(None);
            }
            Err(e) => return Err(fail(e.to_string())),
        };

        // A cursor bigger than any cursor is refused before its pixels are
        // touched: whatever produced it, unpacking a megapixel "pointer" is
        // work nobody asked for.
        if reply.width == 0 || reply.height == 0 {
            return Ok(None);
        }
        if reply.width > MAX_EDGE || reply.height > MAX_EDGE {
            return Err(fail(format!(
                "the server reports a {}x{} cursor, past the {MAX_EDGE} this carries",
                reply.width, reply.height
            )));
        }
        let pixels = usize::from(reply.width).saturating_mul(usize::from(reply.height));
        if reply.cursor_image.len() != pixels {
            return Err(fail(format!(
                "a {}x{} cursor came with {} pixels",
                reply.width,
                reply.height,
                reply.cursor_image.len()
            )));
        }

        let mut rgba = Vec::with_capacity(pixels.saturating_mul(4));
        for argb in &reply.cursor_image {
            // Unpacked from the number rather than transmuted from its bytes,
            // so the result is the same on any machine.
            let a = u8::try_from((argb >> 24) & 0xFF).unwrap_or(0);
            let r = u8::try_from((argb >> 16) & 0xFF).unwrap_or(0);
            let g = u8::try_from((argb >> 8) & 0xFF).unwrap_or(0);
            let b = u8::try_from(argb & 0xFF).unwrap_or(0);
            rgba.extend_from_slice(&[r, g, b, a]);
        }

        let image = CursorImage {
            width: reply.width,
            height: reply.height,
            hotspot_x: reply.xhot.min(reply.width.saturating_sub(1)),
            hotspot_y: reply.yhot.min(reply.height.saturating_sub(1)),
            rgba,
        };
        Ok(Some(CursorState {
            serial: reply.cursor_serial,
            // **An invisible cursor is a hidden pointer.** Applications hide
            // the pointer by setting a fully transparent cursor, and there is
            // no other way to ask.
            image: if image.is_invisible() {
                None
            } else {
                Some(image)
            },
            x: i32::from(reply.x),
            y: i32::from(reply.y),
        }))
    }
}
