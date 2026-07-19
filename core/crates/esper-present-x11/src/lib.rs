//! A plain X11 window that shows decoded pictures.
//!
//! # Why a plain window rather than a GPU one
//!
//! The decided architecture has two presenters: a webview rendering a `<video>`
//! element, which is the default, and a native low-latency one. The native one
//! was specified as `wgpu` plus `winit`.
//!
//! This is not that. It is a window drawn by the X server from a shared memory
//! buffer, and it exists because the *stated purpose* of having a second
//! presenter this early is "a plain window, easier to debug than a webview" —
//! and for that purpose `wgpu` and `winit` cost about a hundred crates to
//! audit, link GPU libraries into a binary that deliberately links no display
//! library, and cannot run on a CI runner at all.
//!
//! This costs **no new dependencies**: `x11rb` is already here for capture,
//! and drawing is the same shared-memory mechanism in reverse. It runs under
//! `Xvfb`, so CI exercises it.
//!
//! The GPU presenter is still the right thing for the low-latency mode, where
//! its real justifications live: colour conversion and scaling for free, and
//! one implementation across three platforms. That is also where the *other*
//! implementation lands, which matters — a trait with one implementation is
//! shaped by that implementation whichever one it is.
//!
//! # What this deliberately does not do
//!
//! **Scaling.** The window is the picture's size. Fit, fill and one-to-one are
//! product decisions about a viewer's chrome, and a debug window that quietly
//! resampled would hide exactly the artefacts it exists to show.

use esper_capture::cursor::CursorImage;
use esper_capture::damage::Damage;
use esper_capture::frame::PixelFormat;
use esper_capture::geometry::Rect;
use esper_capture_x11::shm::Segment;
use esper_color::{ColorSpace, I420};
use esper_present::{PresentError, PresentEvent, Presenter};
use x11rb::connection::Connection;
use x11rb::protocol::Event;
use x11rb::protocol::render::{ConnectionExt as _, CreatePictureAux};
use x11rb::protocol::xproto::{
    AtomEnum, ChangeWindowAttributesAux, ConfigureWindowAux, ConnectionExt as _, CreateGCAux,
    CreateWindowAux, EventMask, ImageFormat, PropMode, Window, WindowClass,
};
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

/// The backend name reported in logs and the session HUD.
pub const BACKEND: &str = "x11-window";

const BPP: usize = 4;

/// A window showing decoded pictures.
pub struct X11Presenter {
    conn: RustConnection,
    window: Window,
    gc: x11rb::protocol::xproto::Gcontext,
    depth: u8,
    format: PixelFormat,
    space: ColorSpace,
    width: u32,
    height: u32,
    stride: usize,
    /// The picture as packed RGB. Kept because damage means only part of it is
    /// rewritten each frame, and the rest has to still be the last picture.
    surface: Vec<u8>,
    segment: Option<Segment>,
    open: bool,
    /// Whether the person's input is reported. See
    /// [`esper_present::Presenter::forward_input`].
    forwarding: bool,
    /// The 32-bit ARGB picture format, if this server has one.
    argb_format: Option<x11rb::protocol::render::Pictformat>,
    /// The cursor the window is currently wearing, kept so it can be freed
    /// when it is replaced.
    cursor: Option<x11rb::protocol::xproto::Cursor>,
    /// `WM_DELETE_WINDOW`, so closing the window is a request rather than a
    /// severed connection.
    delete_window: u32,
}

impl std::fmt::Debug for X11Presenter {
    /// The shape, never the pixels.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("X11Presenter")
            .field("size", &(self.width, self.height))
            .field("format", &self.format.name())
            .field("shared_memory", &self.segment.is_some())
            .field("open", &self.open)
            .finish_non_exhaustive()
    }
}

impl X11Presenter {
    /// Open a window for pictures of this size.
    pub fn open(width: u32, height: u32, space: ColorSpace) -> Result<Self, PresentError> {
        if width == 0 || height == 0 {
            return Err(PresentError::WrongSize {
                want_w: 1,
                want_h: 1,
                got_w: width,
                got_h: height,
            });
        }
        let (conn, screen_num) = x11rb::connect(None).map_err(|e| PresentError::Unavailable {
            tried: format!("X11 via DISPLAY: {e}"),
        })?;
        let setup = conn.setup().clone();
        let screen = setup
            .roots
            .get(screen_num)
            .ok_or_else(|| PresentError::Backend {
                backend: BACKEND,
                detail: format!("the server has no screen {screen_num}"),
            })?
            .clone();

        // The window is drawn with the root's visual, so the picture's byte
        // order is the one the server expects — derived, not assumed, by the
        // same code the capture backend uses.
        let format = esper_capture_x11::format::of(&setup, screen.root_depth, screen.root_visual)
            .map_err(|e| PresentError::Backend {
            backend: BACKEND,
            detail: e.to_string(),
        })?;

        let fail = |detail: String| PresentError::Backend {
            backend: BACKEND,
            detail,
        };
        let window = conn.generate_id().map_err(|e| fail(e.to_string()))?;
        let (w, h) = (
            u16::try_from(width).map_err(|_| fail(format!("{width} is too wide for a window")))?,
            u16::try_from(height)
                .map_err(|_| fail(format!("{height} is too tall for a window")))?,
        );
        conn.create_window(
            screen.root_depth,
            window,
            screen.root,
            0,
            0,
            w,
            h,
            0,
            WindowClass::INPUT_OUTPUT,
            screen.root_visual,
            &CreateWindowAux::new().event_mask(
                EventMask::EXPOSURE
                    | EventMask::STRUCTURE_NOTIFY
                    // Everything a person does in the window. Selected
                    // whether or not it is being forwarded, because a mask is
                    // set once at creation and the session may turn
                    // forwarding on later — an unselected event is one the
                    // server never sends, and there is no second chance.
                    | EventMask::KEY_PRESS
                    | EventMask::KEY_RELEASE
                    | EventMask::BUTTON_PRESS
                    | EventMask::BUTTON_RELEASE
                    | EventMask::POINTER_MOTION
                    // The moment everything held has to be let go.
                    | EventMask::FOCUS_CHANGE,
            ),
        )
        .map_err(|e| fail(e.to_string()))?
        .check()
        .map_err(|e| fail(e.to_string()))?;

        let title = format!("esper — {width}x{height}");
        let _ = conn.change_property8(
            PropMode::REPLACE,
            window,
            AtomEnum::WM_NAME,
            AtomEnum::STRING,
            title.as_bytes(),
        );

        // Ask to be *told* when the person closes the window instead of having
        // the connection cut from under the session.
        let protocols = conn
            .intern_atom(false, b"WM_PROTOCOLS")
            .map_err(|e| fail(e.to_string()))?
            .reply()
            .map_err(|e| fail(e.to_string()))?
            .atom;
        let delete_window = conn
            .intern_atom(false, b"WM_DELETE_WINDOW")
            .map_err(|e| fail(e.to_string()))?
            .reply()
            .map_err(|e| fail(e.to_string()))?
            .atom;
        let _ = conn.change_property32(
            PropMode::REPLACE,
            window,
            protocols,
            AtomEnum::ATOM,
            &[delete_window],
        );

        let gc = conn.generate_id().map_err(|e| fail(e.to_string()))?;
        conn.create_gc(gc, window, &CreateGCAux::new())
            .map_err(|e| fail(e.to_string()))?
            .check()
            .map_err(|e| fail(e.to_string()))?;

        conn.map_window(window)
            .map_err(|e| fail(e.to_string()))?
            .check()
            .map_err(|e| fail(e.to_string()))?;
        conn.flush().map_err(|e| fail(e.to_string()))?;

        // **The ARGB picture format, found rather than assumed.** A cursor
        // with an alpha channel is built through RENDER, and RENDER names its
        // formats by id: the one wanted is 32 bits deep with eight bits of
        // alpha. A server without it can still show pictures, so this is
        // `None` rather than a refusal to open.
        let argb_format = conn
            .render_query_pict_formats()
            .ok()
            .and_then(|c| c.reply().ok())
            .and_then(|reply| {
                reply
                    .formats
                    .iter()
                    .find(|f| {
                        f.depth == 32
                            && f.type_ == x11rb::protocol::render::PictType::DIRECT
                            && f.direct.alpha_mask == 0xFF
                    })
                    .map(|f| f.id)
            });
        if argb_format.is_none() {
            tracing::warn!(
                "no 32-bit ARGB picture format: the remote pointer cannot be worn by the \
                 window on this server"
            );
        }

        let stride = (width as usize).saturating_mul(BPP);
        let bytes = stride.saturating_mul(height as usize);
        let segment = match esper_capture_x11::shm::probe(&conn) {
            Ok(_) => match Segment::create(&conn, bytes) {
                Ok(s) => Some(s),
                Err(e) => {
                    tracing::warn!("MIT-SHM segment failed ({e}); drawing over the socket");
                    None
                }
            },
            Err(e) => {
                tracing::warn!("no MIT-SHM ({e}); drawing over the socket");
                None
            }
        };

        Ok(Self {
            conn,
            window,
            gc,
            depth: screen.root_depth,
            format,
            space,
            width,
            height,
            stride,
            surface: vec![0; bytes],
            segment,
            open: true,
            forwarding: false,
            argb_format,
            cursor: None,
            delete_window,
        })
    }

    /// Whether pictures are being moved through shared memory.
    #[must_use]
    pub const fn uses_shared_memory(&self) -> bool {
        self.segment.is_some()
    }

    /// The X11 window this draws into.
    ///
    /// Exposed because a caller may need to reparent it — a viewer embedding
    /// this inside its own chrome — and because a test that wants to know
    /// whether a picture arrived has to read the window rather than the screen.
    /// Reading the *screen* at the origin is what a first version of that test
    /// did, and it passed under `Xvfb`, where a window with no window manager
    /// lands at 0,0 and nothing else is on the display.
    #[must_use]
    pub const fn window(&self) -> Window {
        self.window
    }

    /// The pixel layout the window is drawn in.
    #[must_use]
    pub const fn format(&self) -> PixelFormat {
        self.format
    }

    /// Blit one rectangle of the surface into the window.
    fn blit(&self, area: &Rect) -> Result<(), PresentError> {
        let fail = |detail: String| PresentError::Backend {
            backend: BACKEND,
            detail,
        };
        let (Ok(sx), Ok(sy), Ok(w), Ok(h)) = (
            u16::try_from(area.x.max(0)),
            u16::try_from(area.y.max(0)),
            u16::try_from(area.width),
            u16::try_from(area.height),
        ) else {
            return Err(fail(format!("{area:?} does not fit an X11 request")));
        };
        let (Ok(total_w), Ok(total_h)) = (u16::try_from(self.width), u16::try_from(self.height))
        else {
            return Err(fail("the picture does not fit an X11 request".to_owned()));
        };

        match self.segment.as_ref() {
            Some(segment) => {
                // Only the rows this rectangle touches are pushed into the
                // segment; the rest of it is still the last picture, which is
                // what makes a caret repaint cost a caret.
                let from = (area.y.max(0) as usize).saturating_mul(self.stride);
                let len = (area.height as usize).saturating_mul(self.stride);
                let Some(rows) = self.surface.get(from..from.saturating_add(len)) else {
                    return Err(fail(format!("{area:?} is outside the surface")));
                };
                segment
                    .write_from(from, rows)
                    .map_err(|e| fail(e.to_string()))?;
                x11rb::protocol::shm::put_image(
                    &self.conn,
                    self.window,
                    self.gc,
                    total_w,
                    total_h,
                    sx,
                    sy,
                    w,
                    h,
                    area.x.max(0) as i16,
                    area.y.max(0) as i16,
                    self.depth,
                    ImageFormat::Z_PIXMAP.into(),
                    false,
                    segment.id(),
                    0,
                )
                .map_err(|e| fail(e.to_string()))?
                .check()
                .map_err(|e| fail(e.to_string()))?;
            }
            None => {
                // Over the socket, row by row, because a whole picture can
                // exceed the server's maximum request size.
                for row in 0..area.height as usize {
                    let y = (area.y.max(0) as usize).saturating_add(row);
                    let from = y
                        .saturating_mul(self.stride)
                        .saturating_add((area.x.max(0) as usize).saturating_mul(BPP));
                    let len = (area.width as usize).saturating_mul(BPP);
                    let Some(line) = self.surface.get(from..from.saturating_add(len)) else {
                        return Err(fail(format!("{area:?} is outside the surface")));
                    };
                    self.conn
                        .put_image(
                            ImageFormat::Z_PIXMAP,
                            self.window,
                            self.gc,
                            w,
                            1,
                            area.x.max(0) as i16,
                            i16::try_from(y).unwrap_or(i16::MAX),
                            0,
                            self.depth,
                            line,
                        )
                        .map_err(|e| fail(e.to_string()))?
                        .check()
                        .map_err(|e| fail(e.to_string()))?;
                }
            }
        }
        Ok(())
    }
}

impl Drop for X11Presenter {
    fn drop(&mut self) {
        if let Some(s) = self.segment.take() {
            s.detach(&self.conn);
        }
        let _ = self.conn.free_gc(self.gc);
        let _ = self.conn.destroy_window(self.window);
        let _ = self.conn.flush();
    }
}

impl Presenter for X11Presenter {
    fn backend(&self) -> &'static str {
        BACKEND
    }

    fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// The window is resized to match, because a viewer showing a 2560x1440
    /// screen in a 1920x1080 window would either scale it or crop it, and this
    /// layer does neither: it shows pixels at their own size.
    fn resize(&mut self, width: u32, height: u32) -> Result<(), PresentError> {
        if (width, height) == (self.width, self.height) {
            return Ok(());
        }
        let fail = |detail: String| PresentError::Backend {
            backend: BACKEND,
            detail,
        };
        if width == 0 || height == 0 {
            return Err(PresentError::WrongSize {
                want_w: self.width,
                want_h: self.height,
                got_w: width,
                got_h: height,
            });
        }
        let stride = (width as usize).saturating_mul(BPP);
        let bytes = stride.saturating_mul(height as usize);

        // The new segment before the old one goes, so a failure leaves a
        // presenter that still works at the size it already had.
        let segment = match self.segment.as_ref() {
            Some(_) => match Segment::create(&self.conn, bytes) {
                Ok(s) => Some(s),
                Err(e) => {
                    tracing::warn!("MIT-SHM segment failed on resize ({e}); over the socket");
                    None
                }
            },
            None => None,
        };
        self.conn
            .configure_window(
                self.window,
                &ConfigureWindowAux::new().width(width).height(height),
            )
            .map_err(|e| fail(e.to_string()))?
            .check()
            .map_err(|e| fail(e.to_string()))?;
        self.conn.flush().map_err(|e| fail(e.to_string()))?;

        if let Some(old) = self.segment.take() {
            old.detach(&self.conn);
        }
        self.segment = segment;
        self.width = width;
        self.height = height;
        self.stride = stride;
        // Black rather than the old picture stretched: nothing here has seen
        // the new screen yet, and a keyframe is already on its way.
        self.surface.clear();
        self.surface.resize(bytes, 0);
        Ok(())
    }

    fn forward_input(&mut self, on: bool) {
        self.forwarding = on;
    }

    /// **The window wears the remote pointer.**
    ///
    /// Built through RENDER because a cursor with an alpha channel needs it —
    /// the core protocol's cursors are two colours and a mask — and handed to
    /// the window as its own cursor, so the server draws it wherever the local
    /// pointer is. Nothing is composited and there is no position to track:
    /// the pointer moves at the speed of the hand.
    ///
    /// A hidden pointer is a fully transparent cursor, which is exactly how
    /// applications hide one and so needs no separate mechanism.
    fn set_cursor(&mut self, image: Option<&CursorImage>) -> Result<(), PresentError> {
        let fail = |detail: String| PresentError::Backend {
            backend: BACKEND,
            detail,
        };
        let Some(format) = self.argb_format else {
            return Err(fail(
                "this server has no 32-bit ARGB picture format, so a cursor with an                  alpha channel cannot be built"
                    .to_owned(),
            ));
        };

        // A 1x1 transparent cursor for the hidden state: cheaper than a full
        // transparent bitmap and impossible to get wrong.
        let (width, height, hotspot, pixels) = match image {
            Some(i) if i.is_consistent() => (
                i.width,
                i.height,
                (i.hotspot_x, i.hotspot_y),
                i.rgba.clone(),
            ),
            Some(_) => {
                return Err(fail("the cursor's pixels do not match its size".to_owned()));
            }
            None => (1, 1, (0, 0), vec![0, 0, 0, 0]),
        };

        let pixmap = self.conn.generate_id().map_err(|e| fail(e.to_string()))?;
        self.conn
            .create_pixmap(32, pixmap, self.window, width, height)
            .map_err(|e| fail(e.to_string()))?
            .check()
            .map_err(|e| fail(e.to_string()))?;

        // The image, as the server wants it: BGRA bytes on a little-endian
        // server, which is what `image_byte_order` says. Built from the
        // protocol's named RGBA rather than assumed, so a big-endian server
        // gets the right bytes too.
        let mut data = Vec::with_capacity(pixels.len());
        let big_endian =
            self.conn.setup().image_byte_order == x11rb::protocol::xproto::ImageOrder::MSB_FIRST;
        for px in pixels.as_chunks::<4>().0 {
            let (r, g, b, a) = (px[0], px[1], px[2], px[3]);
            if big_endian {
                data.extend_from_slice(&[a, r, g, b]);
            } else {
                data.extend_from_slice(&[b, g, r, a]);
            }
        }

        let gc = self.conn.generate_id().map_err(|e| fail(e.to_string()))?;
        self.conn
            .create_gc(gc, pixmap, &CreateGCAux::new())
            .map_err(|e| fail(e.to_string()))?
            .check()
            .map_err(|e| fail(e.to_string()))?;
        self.conn
            .put_image(
                ImageFormat::Z_PIXMAP,
                pixmap,
                gc,
                width,
                height,
                0,
                0,
                0,
                32,
                &data,
            )
            .map_err(|e| fail(e.to_string()))?
            .check()
            .map_err(|e| fail(e.to_string()))?;
        let _ = self.conn.free_gc(gc);

        let picture = self.conn.generate_id().map_err(|e| fail(e.to_string()))?;
        self.conn
            .render_create_picture(picture, pixmap, format, &CreatePictureAux::new())
            .map_err(|e| fail(e.to_string()))?
            .check()
            .map_err(|e| fail(e.to_string()))?;

        let cursor = self.conn.generate_id().map_err(|e| fail(e.to_string()))?;
        self.conn
            .render_create_cursor(cursor, picture, hotspot.0, hotspot.1)
            .map_err(|e| fail(e.to_string()))?
            .check()
            .map_err(|e| fail(e.to_string()))?;
        self.conn
            .change_window_attributes(
                self.window,
                &ChangeWindowAttributesAux::new().cursor(cursor),
            )
            .map_err(|e| fail(e.to_string()))?
            .check()
            .map_err(|e| fail(e.to_string()))?;
        self.conn.flush().map_err(|e| fail(e.to_string()))?;

        // The pixmap and the picture have done their work; the cursor holds
        // its own copy. The *previous* cursor goes now rather than before, so
        // the window is never briefly without one.
        let _ = self.conn.render_free_picture(picture);
        let _ = self.conn.free_pixmap(pixmap);
        if let Some(old) = self.cursor.replace(cursor) {
            let _ = self.conn.free_cursor(old);
        }
        Ok(())
    }

    fn present(&mut self, picture: &I420, damage: &Damage) -> Result<(), PresentError> {
        if (picture.width(), picture.height()) != (self.width, self.height) {
            return Err(PresentError::WrongSize {
                want_w: self.width,
                want_h: self.height,
                got_w: picture.width(),
                got_h: picture.height(),
            });
        }
        if damage.is_empty() {
            return Ok(());
        }
        let bounds = Rect::sized(self.width, self.height);
        for area in damage.rects(&bounds) {
            esper_color::rgb::region(
                picture,
                self.space,
                &mut self.surface,
                self.stride,
                self.format,
                &area,
            )?;
            self.blit(&area)?;
        }
        self.conn.flush().map_err(|e| PresentError::Backend {
            backend: BACKEND,
            detail: e.to_string(),
        })?;
        Ok(())
    }

    fn poll(&mut self) -> Result<Vec<PresentEvent>, PresentError> {
        let mut out = Vec::new();
        loop {
            match self.conn.poll_for_event() {
                Ok(Some(ev)) => match ev {
                    Event::Expose(_) => {
                        if !out.contains(&PresentEvent::Exposed) {
                            out.push(PresentEvent::Exposed);
                        }
                    }
                    Event::ConfigureNotify(c) => out.push(PresentEvent::Resized {
                        width: u32::from(c.width),
                        height: u32::from(c.height),
                    }),
                    Event::ClientMessage(m) => {
                        // The window manager relaying a close request.
                        if m.data.as_data32().first() == Some(&self.delete_window) {
                            self.open = false;
                            out.push(PresentEvent::CloseRequested);
                        }
                    }
                    // Escape closes it, because a debug window with no
                    // decorations is otherwise hard to get rid of — but only
                    // while nothing is being forwarded. A session that
                    // swallowed Escape would be a remote desktop that cannot
                    // send it.
                    Event::KeyPress(k) if k.detail == ESCAPE && !self.forwarding => {
                        self.open = false;
                        out.push(PresentEvent::CloseRequested);
                    }
                    Event::KeyPress(k) if self.forwarding => out.push(PresentEvent::Key {
                        code: u32::from(k.detail),
                        pressed: true,
                    }),
                    Event::KeyRelease(k) if self.forwarding => out.push(PresentEvent::Key {
                        code: u32::from(k.detail),
                        pressed: false,
                    }),
                    // **The wheel arrives as a button on X11**, and a
                    // wheel-as-button forwarded as a button would be a middle
                    // click on the host. Buttons 4 to 7 are the wheel and
                    // become a scroll; the rest are buttons.
                    Event::ButtonPress(b) if self.forwarding => match b.detail {
                        4 => out.push(PresentEvent::Scroll { dx: 0, dy: -1 }),
                        5 => out.push(PresentEvent::Scroll { dx: 0, dy: 1 }),
                        6 => out.push(PresentEvent::Scroll { dx: -1, dy: 0 }),
                        7 => out.push(PresentEvent::Scroll { dx: 1, dy: 0 }),
                        other => out.push(PresentEvent::Button {
                            code: u32::from(other),
                            pressed: true,
                        }),
                    },
                    Event::ButtonRelease(b) if self.forwarding => {
                        // The wheel's release is the other half of a click
                        // already reported, so it is dropped rather than sent
                        // as a scroll of its own.
                        if !(4..=7).contains(&b.detail) {
                            out.push(PresentEvent::Button {
                                code: u32::from(b.detail),
                                pressed: false,
                            });
                        }
                    }
                    Event::MotionNotify(m) if self.forwarding => out.push(PresentEvent::Pointer {
                        x: i32::from(m.event_x),
                        y: i32::from(m.event_y),
                    }),
                    Event::FocusOut(_) if self.forwarding => {
                        out.push(PresentEvent::FocusLost);
                    }
                    _ => {}
                },
                Ok(None) => break,
                Err(e) => {
                    // A broken connection is the window being gone, which is
                    // not a failure to report as one.
                    self.open = false;
                    out.push(PresentEvent::CloseRequested);
                    tracing::debug!("X11 presenter connection ended: {e}");
                    break;
                }
            }
        }
        // An exposed window has to be repainted from the surface, not from the
        // next picture: a static screen may not produce another one.
        if out.contains(&PresentEvent::Exposed) && self.open {
            let all = Rect::sized(self.width, self.height);
            self.blit(&all)?;
            let _ = self.conn.flush();
        }
        Ok(out)
    }

    fn is_open(&self) -> bool {
        self.open
    }
}

/// X11 keycode for Escape on essentially every keyboard map.
///
/// A keycode rather than a keysym on purpose: resolving keysyms needs the
/// keyboard mapping, and this is a debug window's escape hatch rather than a
/// text input.
const ESCAPE: u8 = 9;
