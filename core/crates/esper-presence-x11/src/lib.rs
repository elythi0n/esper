//! Presence on X11: a frame around the screen, and a key nothing can swallow.
//!
//! # The frame
//!
//! Four override-redirect windows, one along each edge. Override-redirect so
//! no window manager can move, decorate, or refuse them, and raised again
//! whenever the indicator is touched so an application that came up on top
//! does not hide it.
//!
//! **They are visible and not clickable.** Without that they would swallow
//! every click on the edge of the screen — where the scrollbars, the taskbar
//! and the window borders are — so the SHAPE extension gives each one an
//! *empty input region*: the server routes pointer events straight through
//! them to whatever is underneath.
//!
//! # The panic key
//!
//! Grabbed from the server on the root window, which means the host's own
//! connection is given the key **before any application sees it**, including
//! before anything a controller is driving. A controller cannot swallow it
//! because a controller is not in the path; the most it can do is press it
//! itself, which ends its own session.
//!
//! Grabbed with the two lock modifiers in every combination, because a person
//! reaching for the panic key does not first check whether Num Lock is on —
//! and a hotkey that works only sometimes is worse than one that does not
//! exist, because the person will reach for it in an emergency.

// The engine only uses the X11 backends on Linux. Elsewhere this crate
// compiles to nothing rather than being absent, so a workspace build for
// another target still covers every member.
#![cfg(target_os = "linux")]

use esper_presence::{HostEvent, Presence, PresenceError, Watching};
use x11rb::connection::Connection as _;
use x11rb::protocol::Event;
use x11rb::protocol::shape::{ConnectionExt as _, SK, SO};
use x11rb::protocol::xproto::{
    ConfigureWindowAux, ConnectionExt as _, CreateWindowAux, EventMask, GrabMode, ModMask,
    Rectangle, StackMode, Window, WindowClass,
};
use x11rb::rust_connection::RustConnection;

/// The backend name reported in logs.
pub const BACKEND: &str = "x11-frame";

/// How thick the frame is, in pixels.
///
/// Four is enough to be unmissable at any scaling and small enough to cover
/// nothing. A thinner line disappears on a high-density display.
const THICKNESS: u16 = 4;

/// The colour when the far end can only watch: amber.
const WATCHING: u32 = 0x00FF_B000;

/// The colour when the far end can type: red.
///
/// Different because "somebody is looking at my screen" and "somebody can use
/// my machine" are different things to be told, and a single colour for both
/// would train a person to ignore the one that matters.
const CONTROLLING: u32 = 0x00FF_2020;

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
const PAUSED: u32 = 0x0020_60FF;

/// The panic key's keycode, from the same table the injector uses.
///
/// Escape is HID usage 0x29. Looked up rather than written as 9, so that this
/// and the injecting side cannot disagree about which key Escape is.
fn escape_keycode() -> Option<u8> {
    keycode_for_usage(0x29)
}

/// The pause key's keycode: `P`, HID usage 0x13.
///
/// The same modifiers as the panic key, because they are the same kind of
/// thing — something the person at the machine reaches for about their own
/// screen — and one modifier combination to learn is better than two.
fn pause_keycode() -> Option<u8> {
    keycode_for_usage(0x13)
}

/// An X keycode for a HID usage.
///
/// Looked up rather than written as a number, so this and the injecting side
/// cannot disagree about which key is which.
fn keycode_for_usage(usage: u16) -> Option<u8> {
    let evdev = esper_input::hid::evdev_for_usage(usage)?;
    u8::try_from(evdev.0.saturating_add(8)).ok()
}

/// The modifiers the panic key needs: Ctrl, Alt and Shift together.
/// Control, Alt (`Mod1`) and Shift, as the protocol numbers them. Written as
/// the bit positions the X protocol defines rather than derived from the
/// library's types, which are not const.
const PANIC_MODS: u16 = 0x0004 | 0x0008 | 0x0001;

/// Lock modifiers that must not stop it working.
///
/// Caps Lock is `Lock`, and Num Lock is conventionally `Mod2`. A grab matches
/// the modifier state *exactly*, so a hotkey grabbed without these does
/// nothing while either lock is on — and a person reaching for a panic key
/// will not think to check.
const LOCKS: [u16; 4] = [0, 0x0002, 0x0010, 0x0002 | 0x0010];

/// The session indicator and the panic key.
pub struct X11Presence {
    conn: RustConnection,
    root: Window,
    /// Top, bottom, left, right.
    edges: Option<[Window; 4]>,
    screen: (u16, u16),
    shaped: bool,
    grabbed: bool,
    /// Whether the pause key was had as well.
    paused_key: bool,
    showing: Option<Watching>,
}

impl std::fmt::Debug for X11Presence {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("X11Presence")
            .field("showing", &self.showing.is_some())
            .field("input_shaped", &self.shaped)
            .field("panic_key", &self.grabbed)
            .field("pause_key", &self.paused_key)
            .finish_non_exhaustive()
    }
}

impl X11Presence {
    /// Connect, and grab the panic key.
    ///
    /// The grab happens on open rather than when a session starts, so the key
    /// works from the moment the host is running — including while a session
    /// is being *offered*, which is one of the moments a person might want it.
    pub fn open() -> Result<Self, PresenceError> {
        let (conn, screen_num) = x11rb::connect(None).map_err(|e| PresenceError::NoBackend {
            tried: format!("X11 via DISPLAY: {e}"),
        })?;
        let screen =
            conn.setup()
                .roots
                .get(screen_num)
                .ok_or_else(|| PresenceError::NoBackend {
                    tried: "X11: the display has no screen".to_owned(),
                })?;
        let root = screen.root;
        let size = (screen.width_in_pixels, screen.height_in_pixels);

        // SHAPE, by asking for its version: without it the frame would eat
        // clicks, which is worse than no frame.
        let shaped = conn
            .shape_query_version()
            .ok()
            .and_then(|c| c.reply().ok())
            .is_some();
        if !shaped {
            tracing::warn!(
                "no SHAPE extension: the session indicator would swallow clicks on the \
                 edge of the screen, so it will not be shown"
            );
        }

        // **Told when the stacking order changes, rather than raising the
        // frame on a timer.**
        //
        // The first version re-raised on every poll — thirty times a second —
        // which put a `ConfigureNotify` on the root that often. Anything
        // watching the root saw a flood: four thousand events in nine seconds,
        // measured while trying to watch for something else entirely.
        //
        // `SubstructureNotify` says when a window is mapped, raised or
        // configured. The frame is put back only then, which is exactly when
        // it could have been covered.
        if let Ok(cookie) = conn.change_window_attributes(
            root,
            &x11rb::protocol::xproto::ChangeWindowAttributesAux::new()
                .event_mask(EventMask::SUBSTRUCTURE_NOTIFY),
        ) {
            let _ = cookie.check();
        }

        let mut me = Self {
            conn,
            root,
            edges: None,
            screen: size,
            shaped,
            grabbed: false,
            paused_key: false,
            showing: None,
        };
        me.grab_panic_key();
        Ok(me)
    }

    /// Whether the panic key is grabbed.
    ///
    /// Reported rather than assumed: another program may already hold this
    /// combination, and a host whose panic key is not working must say so
    /// rather than let somebody rely on it.
    #[must_use]
    pub const fn panic_key_grabbed(&self) -> bool {
        self.grabbed
    }

    /// Whether the pause key was had.
    ///
    /// Separate from the panic key because they fail differently: a pause key
    /// nobody can reach is an inconvenience, and a panic key nobody can reach
    /// is a promise this crate is not keeping.
    #[must_use]
    pub const fn pause_key_grabbed(&self) -> bool {
        self.paused_key
    }

    /// Whether the frame can be shown without swallowing clicks.
    #[must_use]
    pub const fn can_show(&self) -> bool {
        self.shaped
    }

    /// The frame's windows, for a caller checking they are really gone.
    ///
    /// Exposed for the test that hides the indicator and then asks the server
    /// whether the windows exist — which is what this layer promises, unlike
    /// what the screen shows afterwards, which belongs to whoever repaints it.
    #[must_use]
    pub fn windows(&self) -> Vec<Window> {
        self.edges.map(Vec::from).unwrap_or_default()
    }

    fn grab_panic_key(&mut self) {
        // **Both keys, and the panic key first.** If only one grab can be had
        // — something else already holds a combination — it must be the one
        // that makes a session stop, because a pause key nobody can reach is
        // an inconvenience and a panic key nobody can reach is the property
        // this crate promises.
        self.grabbed = self.grab_one(escape_keycode(), "Escape");
        self.paused_key = self.grab_one(pause_keycode(), "P");
    }

    /// Grab one key with the shared modifiers, saying whether it was had.
    fn grab_one(&self, code: Option<u8>, name: &str) -> bool {
        let Some(code) = code else {
            tracing::error!("no keycode for {name}, so that hotkey cannot be grabbed");
            return false;
        };
        let mut got = 0;
        for lock in LOCKS {
            let modifiers = ModMask::from(PANIC_MODS | lock);
            match self.conn.grab_key(
                // Not `owner_events`: the key must come to this connection
                // whatever has the focus.
                false,
                self.root,
                modifiers,
                code,
                GrabMode::ASYNC,
                GrabMode::ASYNC,
            ) {
                Ok(cookie) => match cookie.check() {
                    Ok(()) => got += 1,
                    Err(e) => tracing::warn!("could not grab {name} ({modifiers:?}): {e}"),
                },
                Err(e) => tracing::warn!("could not ask for {name}: {e}"),
            }
        }
        if got > 0 && got < LOCKS.len() {
            // Said, because a key that works with Caps Lock off and not on is
            // the kind of thing somebody discovers at the worst moment.
            tracing::warn!(
                "{name} was grabbed for {got} of {} modifier combinations",
                LOCKS.len()
            );
        }
        got > 0
    }

    /// Build the four edge windows.
    fn make_edges(&mut self, colour: u32) -> Result<[Window; 4], PresenceError> {
        let fail = |detail: String| PresenceError::Backend {
            backend: BACKEND,
            detail,
        };
        let (w, h) = self.screen;
        let t = THICKNESS;
        // Top, bottom, left, right — the corners belong to the horizontal
        // bars, so the verticals are inset.
        let places = [
            (0i16, 0i16, w, t),
            (0, i16::try_from(h.saturating_sub(t)).unwrap_or(0), w, t),
            (0, i16::try_from(t).unwrap_or(0), t, h.saturating_sub(t * 2)),
            (
                i16::try_from(w.saturating_sub(t)).unwrap_or(0),
                i16::try_from(t).unwrap_or(0),
                t,
                h.saturating_sub(t * 2),
            ),
        ];
        let setup = self.conn.setup().clone();
        let screen = setup
            .roots
            .first()
            .ok_or_else(|| fail("no screen".into()))?;
        let (depth, visual) = (screen.root_depth, screen.root_visual);

        let mut windows = [0u32; 4];
        for (slot, (x, y, width, height)) in windows.iter_mut().zip(places) {
            let window = self.conn.generate_id().map_err(|e| fail(e.to_string()))?;
            self.conn
                .create_window(
                    depth,
                    window,
                    self.root,
                    x,
                    y,
                    width.max(1),
                    height.max(1),
                    0,
                    WindowClass::INPUT_OUTPUT,
                    visual,
                    &CreateWindowAux::new()
                        .background_pixel(colour)
                        .override_redirect(1)
                        // Nothing is read from these windows; the mask is
                        // empty so the server does not queue events nobody
                        // will drain.
                        .event_mask(EventMask::NO_EVENT),
                )
                .map_err(|e| fail(e.to_string()))?
                .check()
                .map_err(|e| fail(e.to_string()))?;

            // **Empty input region.** Without this the frame swallows every
            // click on the edge of the screen.
            self.conn
                .shape_rectangles(
                    SO::SET,
                    SK::INPUT,
                    x11rb::protocol::xproto::ClipOrdering::UNSORTED,
                    window,
                    0,
                    0,
                    &[] as &[Rectangle],
                )
                .map_err(|e| fail(e.to_string()))?
                .check()
                .map_err(|e| fail(e.to_string()))?;

            self.conn
                .map_window(window)
                .map_err(|e| fail(e.to_string()))?
                .check()
                .map_err(|e| fail(e.to_string()))?;
            *slot = window;
        }
        self.conn.flush().map_err(|e| fail(e.to_string()))?;
        Ok(windows)
    }

    /// Put the frame back on top, in case something came up over it.
    ///
    /// This itself produces `ConfigureNotify` on the root — the frame is being
    /// configured — so a caller that raised on every such event would chase
    /// its own tail for ever. The events our own windows generate are ignored
    /// by [`Self::is_ours`].
    fn raise(&self) {
        let Some(edges) = self.edges else { return };
        for window in edges {
            let _ = self.conn.configure_window(
                window,
                &ConfigureWindowAux::new().stack_mode(StackMode::ABOVE),
            );
        }
        let _ = self.conn.flush();
    }

    /// Whether a window is part of the frame.
    fn is_ours(&self, window: Window) -> bool {
        self.edges.is_some_and(|edges| edges.contains(&window))
    }

    fn destroy_edges(&mut self) {
        if let Some(edges) = self.edges.take() {
            for window in edges {
                let _ = self.conn.destroy_window(window);
            }
            let _ = self.conn.flush();
        }
    }
}

impl Presence for X11Presence {
    fn backend(&self) -> &'static str {
        BACKEND
    }

    fn show(&mut self, watching: &Watching) -> Result<(), PresenceError> {
        if !self.shaped {
            return Err(PresenceError::Backend {
                backend: BACKEND,
                detail: "this server has no SHAPE extension, so a frame would swallow clicks"
                    .to_owned(),
            });
        }
        let colour = if watching.paused {
            PAUSED
        } else if watching.controlling {
            CONTROLLING
        } else {
            WATCHING
        };
        // What it says has changed, so the windows are rebuilt in the new
        // colour rather than left saying the old thing.
        if self
            .showing
            .as_ref()
            .is_some_and(|s| s.controlling != watching.controlling || s.paused != watching.paused)
        {
            self.destroy_edges();
        }
        if self.edges.is_none() {
            self.edges = Some(self.make_edges(colour)?);
        }
        self.raise();
        self.showing = Some(watching.clone());
        Ok(())
    }

    fn hide(&mut self) -> Result<(), PresenceError> {
        self.destroy_edges();
        self.showing = None;
        Ok(())
    }

    fn poll(&mut self) -> Result<Vec<HostEvent>, PresenceError> {
        let mut out = Vec::new();
        let mut somebody_moved = false;
        while let Ok(Some(event)) = self.conn.poll_for_event() {
            match event {
                // **Two keys now, so the keycode has to be read.** This
                // used to say the grab was the filter and there was nothing
                // to check, which was true of one key and would silently
                // make the pause key end the session.
                Event::KeyPress(key) => {
                    if Some(key.detail) == pause_keycode() {
                        out.push(HostEvent::Pause);
                    } else {
                        // Anything else this connection is given can only be
                        // the other grab, and a keycode that somehow was
                        // neither is safer read as the one that stops.
                        out.push(HostEvent::Panic);
                    }
                }
                // Something appeared, moved or was raised — which is the only
                // way the frame can end up underneath anything.
                Event::MapNotify(e) if !self.is_ours(e.window) => somebody_moved = true,
                Event::ConfigureNotify(e) if !self.is_ours(e.window) => somebody_moved = true,
                _ => {}
            }
        }
        if somebody_moved && self.edges.is_some() {
            self.raise();
        }
        Ok(out)
    }

    fn is_showing(&self) -> bool {
        self.showing.is_some()
    }
}

impl Drop for X11Presence {
    fn drop(&mut self) {
        self.destroy_edges();
        if self.grabbed
            && let Some(code) = escape_keycode()
        {
            for lock in LOCKS {
                let _ = self
                    .conn
                    .ungrab_key(code, self.root, ModMask::from(PANIC_MODS | lock));
            }
            let _ = self.conn.flush();
        }
    }
}
