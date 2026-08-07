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
//!
//! # Blanking the host's own screen
//!
//! A privacy screen — the far end keeps its picture, somebody walking past the
//! host sees nothing — **cannot be a window**: X11 capture reads the root, and
//! anything drawn on the root is part of what is captured, so a cover blanks
//! the remote view too. That was measured and is why the obvious version was
//! never built.
//!
//! What works is turning the **output** off with RandR, which stops the
//! monitor scanning out and leaves the framebuffer allocated for capture to
//! keep reading. Measured on a scratch display: the pixels stay readable with
//! the CRTC off, and the CRTC comes back exactly as it was.
//!
//! Two things make it safe to ship. Every CRTC is registered with a **keeper**
//! before it is touched, so a host that dies does not leave somebody's monitor
//! dark; and after blanking, the screen is **read back** — the same trick the
//! indicator uses — so a display where this breaks capture puts itself back
//! and refuses rather than serving a session that shows nothing.
//!
//! # Reading it back
//!
//! Creating the windows is not the same as the screen showing them, so
//! [`Presence::verify`] asks the server for the **pixels** where the frame
//! should be and compares them with the colour it painted. Three things make
//! that worth doing rather than trusting the windows: a compositor restarting
//! destroys override-redirect windows, a window can be restacked underneath
//! something, and a local program can simply draw over the top — and this
//! process's own state looks identical in all three cases.
//!
//! It reads the **root**, which is what the capture path reads, so what this
//! confirms is what the far end is being sent and what the person in front of
//! the machine is looking at. Twelve single-pixel reads, three along each
//! edge, two pixels inside it: enough that something covering any one edge is
//! caught, and small enough to run every second without being noticed.
//!
//! The one case it inherits from the capture path: on a display with a
//! compositing manager the root's contents can lag or differ from what is
//! composited to the monitor. This crate cannot do better than the capturer
//! there, and it is the same picture either way — if the root says the frame
//! is present, that is what the remote end receives.

// The engine only uses the X11 backends on Linux. Elsewhere this crate
// compiles to nothing rather than being absent, so a workspace build for
// another target still covers every member.
#![cfg(target_os = "linux")]

use esper_keeper::Undo;
use esper_presence::locked::Locked;
use esper_presence::{Blanking, HostEvent, Missing, Presence, PresenceError, Verdict, Watching};
use x11rb::connection::Connection;
use x11rb::protocol::Event;
use x11rb::protocol::randr::ConnectionExt as _;
use x11rb::protocol::screensaver::{
    ConnectionExt as _, Kind as ScreenSaverKind, State as ScreenSaverState,
};
use x11rb::protocol::shape::{ConnectionExt as _, SK, SO};
use x11rb::protocol::xproto::{
    ConfigureWindowAux, ConnectionExt as _, CreateWindowAux, EventMask, GrabMode, GrabStatus,
    ImageFormat, MapState, ModMask, Rectangle, StackMode, Window, WindowClass,
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

/// The colour while the far end is recording: magenta.
///
/// **It takes precedence over the others**, because a border says one thing at
/// a time and this is the thing that outlives the session. Somebody who can
/// see their screen is being watched can decide what to do about it now;
/// somebody whose screen is being *kept* has already lost that choice by the
/// time they find out, so it is the fact that gets the colour.
///
/// Magenta because the other three are taken and because nothing else on a
/// desktop is this colour by accident.
const RECORDING: u32 = 0x00FF_00E0;

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

/// A pixel as three bytes, for a message a person reads.
///
/// **Only for the message.** The comparison happens under the channel mask,
/// in the server's own terms; this is the part that turns "not the colour it
/// should be" into something somebody can recognise on their screen. Each
/// channel is taken from its own mask, so a server with an unusual byte order
/// produces the colour a person would name rather than a shuffled one.
fn as_rgb(pixel: u32, channels: u32) -> [u8; 3] {
    // The masks are not carried separately, so they are recovered from the
    // combined one: three whole bytes, in whichever order this visual has.
    let mut out = [0u8; 3];
    let mut seen = 0usize;
    for shift in (0..32).step_by(8).rev() {
        if channels & (0xFF << shift) != 0 && seen < out.len() {
            if let Some(slot) = out.get_mut(seen) {
                *slot = ((pixel >> shift) & 0xFF) as u8;
            }
            seen = seen.saturating_add(1);
        }
    }
    out
}

/// Which colour the frame is, for what the indicator is saying.
///
/// One function rather than the choice being made where the windows are
/// built, because the check that reads the screen back has to expect exactly
/// what was painted: two copies of this would eventually disagree, and the
/// symptom would be a session that stops because the indicator is the wrong
/// shade of correct.
const fn colour_of(watching: &Watching) -> u32 {
    if watching.recording {
        RECORDING
    } else if watching.paused {
        PAUSED
    } else if watching.controlling {
        CONTROLLING
    } else {
        WATCHING
    }
}

/// Whether the screensaver extension says an external saver is running.
///
/// **The one probe that disturbs nothing.** It is a query: no grab, no window,
/// and no keystroke goes anywhere it would not have gone. A locker that
/// registers itself as a screensaver — `xscreensaver`, and anything driven
/// through `xdg-screensaver` — shows up as an *external* saver with the state
/// on.
///
/// `Blanked` and `Internal` are **not** locked: those are the server's own
/// blanking, which is a dark screen and nothing more. Reading a blanked screen
/// as a locked one would refuse a device because a monitor went to sleep.
fn saver_says<C: Connection>(conn: &C, root: Window) -> Locked {
    let Ok(cookie) = conn.screensaver_query_info(root) else {
        return Locked::CannotTell;
    };
    let Ok(info) = cookie.reply() else {
        return Locked::CannotTell;
    };
    if info.kind != ScreenSaverKind::EXTERNAL {
        // Nothing is claiming to be a screen locker. That is not evidence
        // either way — the locker may simply not use this extension — so the
        // next probe gets a turn.
        return Locked::CannotTell;
    }
    if ScreenSaverState::from(info.state) == ScreenSaverState::ON {
        Locked::Yes(esper_presence::locked::SCREENSAVER)
    } else {
        Locked::No(esper_presence::locked::SCREENSAVER)
    }
}

/// How much of the screen a window has to cover to be covering it.
///
/// Nine tenths, which is a locker and is not a maximised application window:
/// a window manager leaves a panel or a title bar, and a locker leaves
/// nothing. Not the whole screen, because a locker on a multi-monitor desktop
/// may cover the monitors and not the gap between them.
const COVERED: u64 = 90;

/// Whether a screen locker is holding this display.
///
/// # Two things, because either alone is wrong
///
/// X11 has no "is the screen locked" question and no "who holds the grab"
/// query. What a locker does is hold the keyboard **and** cover the screen,
/// and each half on its own has a common false positive:
///
/// - A **grab** is held by any open menu, for as long as it is open. A policy
///   that read that as a locked screen would refuse a device because somebody
///   left a dropdown open.
/// - A **full-screen window** is a video player, a slideshow or a game. None
///   of them means nobody is at the machine; most of them mean somebody is.
///
/// Together they are specific: `i3lock` and `swaylock` do both for as long as
/// the screen is locked, and nothing else on a desktop does both at once.
///
/// # Why this probe is last
///
/// The only way to learn that somebody holds the keyboard is to ask for it and
/// be told `AlreadyGrabbed` — so this takes the keyboard for one round trip
/// and gives it straight back. That is a real cost, which is why it runs only
/// when the free probes could not answer: on a machine with `logind`, or with
/// a locker that registers as a screensaver, it never runs at all.
fn a_grab_says<C: Connection>(conn: &C, root: Window) -> Locked {
    let Ok(cookie) = conn.grab_keyboard(
        false,
        root,
        x11rb::CURRENT_TIME,
        GrabMode::ASYNC,
        GrabMode::ASYNC,
    ) else {
        return Locked::CannotTell;
    };
    let Ok(reply) = cookie.reply() else {
        return Locked::CannotTell;
    };
    match reply.status {
        GrabStatus::SUCCESS => {
            // Given straight back. Holding it would make this probe the thing
            // that stopped somebody typing.
            let _ = conn.ungrab_keyboard(x11rb::CURRENT_TIME);
            let _ = conn.flush();
            Locked::No(esper_presence::locked::A_GRAB)
        }
        GrabStatus::ALREADY_GRABBED | GrabStatus::FROZEN => {
            if something_covers_the_screen(conn, root) {
                Locked::Yes(esper_presence::locked::A_GRAB)
            } else {
                // The keyboard is held by something that is not covering the
                // screen, which is a menu rather than a locker. Not an answer
                // either way: somebody may well be at this machine, and
                // saying "not locked" on this evidence would be as wrong as
                // saying "locked".
                Locked::CannotTell
            }
        }
        // `NotViewable` and `InvalidTime` say something about the request
        // rather than about the screen.
        _ => Locked::CannotTell,
    }
}

/// Whether some window is covering nearly all of the screen.
///
/// Mapped windows only, and the root's own children: a locker's window is a
/// child of the root, and an unmapped one covers nothing.
fn something_covers_the_screen<C: Connection>(conn: &C, root: Window) -> bool {
    let Some(screen) = conn
        .get_geometry(root)
        .ok()
        .and_then(|c| c.reply().ok())
        .map(|g| u64::from(g.width) * u64::from(g.height))
        .filter(|area| *area > 0)
    else {
        return false;
    };
    let Ok(children) = conn
        .query_tree(root)
        .ok()
        .map_or(Err(()), |c| c.reply().map_err(|_| ()))
    else {
        return false;
    };
    for window in children.children {
        let Some(attributes) = conn
            .get_window_attributes(window)
            .ok()
            .and_then(|c| c.reply().ok())
        else {
            continue;
        };
        if attributes.map_state != MapState::VIEWABLE {
            continue;
        }
        let Some(geometry) = conn.get_geometry(window).ok().and_then(|c| c.reply().ok()) else {
            continue;
        };
        let area = u64::from(geometry.width) * u64::from(geometry.height);
        if area.saturating_mul(100) >= screen.saturating_mul(COVERED) {
            return true;
        }
    }
    false
}

/// Whether the display this process is pointed at is locked.
///
/// **The display, not the login session** — the distinction `Locks` makes for
/// the same reason. A host serving a second X server, another seat or a
/// scratch display is not serving the person's login session, and a probe that
/// asked the session manager would answer about a screen this host is not
/// showing.
///
/// Opens a connection of its own and closes it again: this is asked once per
/// connection attempt by a device that would otherwise walk straight in, and
/// keeping a display connection for the life of the host to answer it would be
/// a resource held for something that happens rarely.
#[must_use]
pub fn on_this_display() -> Locked {
    let Ok((conn, num)) = x11rb::connect(None) else {
        return Locked::CannotTell;
    };
    let Some(root) = conn.setup().roots.get(num).map(|s| s.root) else {
        return Locked::CannotTell;
    };
    match saver_says(&conn, root) {
        Locked::CannotTell => a_grab_says(&conn, root),
        answered => answered,
    }
}

/// Whether the screen somebody would be sitting at is locked.
///
/// # The order, and why it is not "most authoritative first"
///
/// 1. **The screensaver extension**, on the display this host serves. Free, and
///    it is about the screen actually being shown.
/// 2. **`logind`**, which knows about the *login session*. Authoritative where
///    it answers, and about the right screen on every ordinary desktop — where
///    the display the host serves and the session's display are the same one.
/// 3. **A grab**, on the display this host serves. Specific and correct, and
///    the only one that costs anything: it takes the keyboard for a round trip
///    to find out whether it can. So it goes last, and on a machine where
///    either of the first two answers it never runs.
#[must_use]
pub fn screen_is_locked() -> Locked {
    // Which display this is, so `logind`'s answer can be checked against the
    // screen it is about rather than assumed to be about ours.
    let ours = std::env::var("DISPLAY").ok();
    let Ok((conn, num)) = x11rb::connect(None) else {
        return esper_presence::locked::by_logind(ours.as_deref());
    };
    let Some(root) = conn.setup().roots.get(num).map(|s| s.root) else {
        return esper_presence::locked::by_logind(ours.as_deref());
    };
    match saver_says(&conn, root) {
        Locked::CannotTell => {}
        answered => return answered,
    }
    match esper_presence::locked::by_logind(ours.as_deref()) {
        Locked::CannotTell => {}
        answered => return answered,
    }
    a_grab_says(&conn, root)
}

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
    /// The bits of a pixel that carry colour, from the root visual.
    ///
    /// Kept so a read-back compares in the server's own terms rather than
    /// converting to RGB and back: whatever the byte order and however the
    /// channels are arranged, the value painted and the value read are the
    /// same integer under this mask.
    channels: u32,
    /// Bytes per pixel in a `GetImage` reply for the root's depth.
    pixel_bytes: usize,
    /// The screens this process turned off, and how to put each one back.
    blanked: Vec<(u32, Undo)>,
    /// The RandR configuration timestamp the undo was taken against.
    config_at: u32,
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

        // What a pixel of this screen looks like, for reading the frame back.
        // Absent or unusable values mean the read-back cannot judge a colour,
        // which it reports rather than guesses at.
        let setup = conn.setup();
        let channels = setup
            .roots
            .iter()
            .flat_map(|s| s.allowed_depths.iter())
            .flat_map(|d| d.visuals.iter())
            .find(|v| v.visual_id == screen.root_visual)
            .map_or(0, |v| v.red_mask | v.green_mask | v.blue_mask);
        let pixel_bytes = setup
            .pixmap_formats
            .iter()
            .find(|f| f.depth == screen.root_depth)
            .map_or(0, |f| usize::from(f.bits_per_pixel) / 8);
        let root_depth = screen.root_depth;

        let mut me = Self {
            conn,
            root,
            edges: None,
            screen: size,
            shaped,
            grabbed: false,
            paused_key: false,
            showing: None,
            channels,
            pixel_bytes,
            blanked: Vec::new(),
            config_at: 0,
        };
        if channels == 0 || !(1..=4).contains(&pixel_bytes) {
            tracing::warn!(
                "the root visual at depth {root_depth} is not a pixel this can read back, \
                 so the indicator cannot be confirmed on screen"
            );
        }
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
                // **A passive grab on the root is what makes this work
                // whatever has the focus**, not this flag — measured, because
                // the comment here used to claim otherwise and flipping it
                // changes nothing about whether the key arrives.
                //
                // `false` is still the right value: with `owner_events` the
                // server reports an event normally if it would go to a window
                // of *this* client, and this client owns the indicator's frame
                // windows, which select no events at all. A key that happened
                // to be routed at one of them would be delivered nowhere.
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

    /// Turn one CRTC off, which stops its outputs scanning out.
    fn turn_off(&self, crtc: u32, config_at: u32) -> Result<(), String> {
        let reply = self
            .conn
            .randr_set_crtc_config(
                crtc,
                x11rb::CURRENT_TIME,
                config_at,
                0,
                0,
                0,
                x11rb::protocol::randr::Rotation::ROTATE0,
                &[],
            )
            .map_err(|e| e.to_string())?
            .reply()
            .map_err(|e| format!("{e:?}"))?;
        if reply.status == x11rb::protocol::randr::SetConfig::SUCCESS {
            Ok(())
        } else {
            Err(format!("the server said {:?}", reply.status))
        }
    }

    /// Put every screen in a list back the way it was.
    ///
    /// Best effort and **says so out loud on failure**, because somebody is
    /// sitting in front of the thing that is still dark.
    fn put_back(&self, blanked: &[(u32, Undo)], config_at: u32) {
        for (crtc, undo) in blanked {
            let Undo::Crtc {
                x,
                y,
                mode,
                rotation,
                outputs,
                what,
                ..
            } = undo;
            let asked = self
                .conn
                .randr_set_crtc_config(
                    *crtc,
                    x11rb::CURRENT_TIME,
                    config_at,
                    *x,
                    *y,
                    *mode,
                    x11rb::protocol::randr::Rotation::from(*rotation),
                    outputs,
                )
                .map_err(|e| e.to_string())
                .and_then(|c| c.reply().map_err(|e| format!("{e:?}")));
            match asked {
                Ok(reply) if reply.status == x11rb::protocol::randr::SetConfig::SUCCESS => {}
                Ok(reply) => {
                    tracing::error!(
                        "{what} would not come back: the server said {:?}",
                        reply.status
                    );
                }
                Err(e) => tracing::error!("{what} would not come back: {e}"),
            }
        }
        let _ = self.conn.flush();
    }

    /// A few pixels of the root, for telling "still capturable" from "gone".
    ///
    /// `None` when the screen cannot be read at all, which is the answer that
    /// matters: it is the difference between a privacy screen and a session
    /// that shows nothing.
    fn screen_sample(&self) -> Option<Vec<u32>> {
        let points = self.sample_points();
        if points.is_empty() {
            return None;
        }
        let mut seen = Vec::with_capacity(points.len());
        for (x, y) in points {
            seen.push(self.pixel_at(x, y)?);
        }
        Some(seen)
    }

    /// Give up on the frame's windows, so the next `show` builds new ones.
    ///
    /// **Verifying is what discovers this**, and a verdict that did not act on
    /// it would leave the backend holding window ids that no longer name
    /// anything: `show` rebuilds only when it has none, so recovery would
    /// quietly do nothing and every later verdict would say the same thing for
    /// ever. Found by the test that has another connection destroy them.
    fn forget(&mut self, missing: Missing) -> Verdict {
        // Destroyed windows are already gone and the request would only earn
        // an error; unmapped ones are real and worth removing properly, so a
        // rebuild is not stacking windows behind the new ones.
        if matches!(missing, Missing::Unmapped) {
            self.destroy_edges();
        } else {
            self.edges = None;
        }
        Verdict::Missing(missing)
    }

    /// Where to look to see whether the frame is on screen.
    ///
    /// Three places along each edge, two pixels inside it. Three rather than
    /// one because a program covering part of an edge — a notification, a
    /// panel, a window placed at a corner — would be missed by a single
    /// sample in the middle; two pixels in because the frame is four thick, so
    /// that is inside it at any rounding.
    fn sample_points(&self) -> Vec<(i16, i16)> {
        let inset = i32::from(THICKNESS / 2);
        let (w, h) = (i32::from(self.screen.0), i32::from(self.screen.1));
        // A screen too small to hold the frame twice over is not something to
        // sample: the edges would overlap and every point would be ambiguous.
        if w < i32::from(THICKNESS) * 4 || h < i32::from(THICKNESS) * 4 {
            return Vec::new();
        }
        let along = |len: i32| [len / 4, len / 2, len * 3 / 4];
        let mut points = Vec::with_capacity(12);
        for x in along(w) {
            points.push((x, inset));
            points.push((x, h - 1 - inset));
        }
        for y in along(h) {
            points.push((inset, y));
            points.push((w - 1 - inset, y));
        }
        points
            .into_iter()
            .filter_map(|(x, y)| Some((i16::try_from(x).ok()?, i16::try_from(y).ok()?)))
            .collect()
    }

    /// One pixel of the screen, as the server's own integer.
    ///
    /// Compared under the visual's channel mask rather than converted to RGB:
    /// the value painted and the value read are the same number whatever the
    /// byte order is, and a conversion is a second place for the two to
    /// disagree.
    fn pixel_at(&self, x: i16, y: i16) -> Option<u32> {
        let reply = self
            .conn
            .get_image(ImageFormat::Z_PIXMAP, self.root, x, y, 1, 1, !0)
            .ok()?
            .reply()
            .ok()?;
        let bytes = reply.data.get(..self.pixel_bytes)?;
        let mut value = 0u32;
        for (i, byte) in bytes.iter().enumerate() {
            value |= u32::from(*byte) << (8 * i);
        }
        Some(value)
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
        let colour = colour_of(watching);
        // What it says has changed, so the windows are rebuilt in the new
        // colour rather than left saying the old thing.
        if self
            .showing
            .as_ref()
            .is_some_and(|s| colour_of(s) != colour_of(watching))
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

    fn blanking_plan(&mut self) -> Result<Blanking, PresenceError> {
        let fail = |detail: String| PresenceError::Backend {
            backend: BACKEND,
            detail,
        };
        if !self.blanked.is_empty() {
            return Err(fail("this screen is already blanked".to_owned()));
        }
        let resources = self
            .conn
            .randr_get_screen_resources_current(self.root)
            .map_err(|e| fail(e.to_string()))?
            .reply()
            .map_err(|e| fail(format!("{e:?}")))?;

        let mut plan: Vec<Undo> = Vec::new();
        for crtc in &resources.crtcs {
            let Ok(info) = self
                .conn
                .randr_get_crtc_info(*crtc, resources.config_timestamp)
                .map_err(|e| fail(e.to_string()))?
                .reply()
            else {
                continue;
            };
            // A CRTC with no mode is already off, and one driving no outputs
            // is not a monitor anybody can read.
            if info.mode == 0 || info.outputs.is_empty() {
                continue;
            }
            plan.push(Undo::Crtc {
                crtc: *crtc,
                x: info.x,
                y: info.y,
                mode: info.mode,
                rotation: u16::from(info.rotation),
                outputs: info.outputs.clone(),
                what: format!("a {}x{} screen", info.width, info.height),
            });
        }
        if plan.is_empty() {
            return Err(fail(
                "this display has no screen that could be turned off".to_owned(),
            ));
        }
        let what = if plan.len() == 1 {
            plan.first()
                .map_or_else(|| "a screen".to_owned(), Undo::describe)
        } else {
            format!("{} screens", plan.len())
        };
        self.config_at = resources.config_timestamp;
        Ok(Blanking { what, undo: plan })
    }

    fn blank(&mut self, plan: &Blanking) -> Result<(), PresenceError> {
        let fail = |detail: String| PresenceError::Backend {
            backend: BACKEND,
            detail,
        };
        if !self.blanked.is_empty() {
            return Err(fail("this screen is already blanked".to_owned()));
        }

        // What the screen reads like before anything is touched, so the
        // read-back afterwards has something to compare against.
        let before = self.screen_sample();

        let mut done: Vec<(u32, Undo)> = Vec::new();
        for undo in &plan.undo {
            let Undo::Crtc { crtc, .. } = undo;
            if let Err(e) = self.turn_off(*crtc, self.config_at) {
                // **Undo our own work before reporting.** A half-blanked
                // screen whose caller got an error is exactly the situation
                // nobody would be holding the undo for.
                self.put_back(&done, self.config_at);
                return Err(fail(format!("turning a screen off: {e}")));
            }
            done.push((*crtc, undo.clone()));
        }

        // **And read the screen back**, the same trick the indicator uses: a
        // display where turning the outputs off also stops the framebuffer
        // being readable would give a session that shows nothing, and that is
        // worse than no privacy screen.
        let after = self.screen_sample();
        if after.is_none() || (before.is_some() && after != before) {
            self.put_back(&done, self.config_at);
            return Err(fail(
                "the framebuffer stopped reading the same way once the screens were off, so \
                 blanking them here would blank the session too"
                    .to_owned(),
            ));
        }

        self.blanked = done;
        Ok(())
    }

    fn unblank(&mut self) -> Result<(), PresenceError> {
        if self.blanked.is_empty() {
            return Ok(());
        }
        let blanked = std::mem::take(&mut self.blanked);
        self.put_back(&blanked, self.config_at);
        Ok(())
    }

    fn verify(&mut self) -> Result<Verdict, PresenceError> {
        let Some(watching) = self.showing.clone() else {
            return Ok(Verdict::Missing(Missing::NothingAsked));
        };
        let Some(edges) = self.edges else {
            return Ok(Verdict::Missing(Missing::Gone));
        };

        // **The windows first, because their answers are more specific.**
        // Pixels can only ever say "the screen is not that colour there";
        // asking the server about the windows separates "something destroyed
        // them" from "something is drawn over them", and those are different
        // things to do about it.
        for window in edges {
            let Ok(cookie) = self.conn.get_window_attributes(window) else {
                return Ok(self.forget(Missing::Gone));
            };
            match cookie.reply() {
                Ok(attrs) if attrs.map_state == MapState::VIEWABLE => {}
                Ok(_) => return Ok(self.forget(Missing::Unmapped)),
                // A window that is gone answers with an error, which is the
                // compositor-restart case rather than a fault of ours.
                Err(_) => return Ok(self.forget(Missing::Gone)),
            }
        }

        if self.channels == 0 || !(1..=4).contains(&self.pixel_bytes) {
            return Ok(Verdict::Missing(Missing::Unreadable(
                "this screen's pixels cannot be read back".to_owned(),
            )));
        }
        let points = self.sample_points();
        if points.is_empty() {
            return Ok(Verdict::Missing(Missing::Unreadable(
                "this screen is too small to sample".to_owned(),
            )));
        }

        let want = colour_of(&watching) & self.channels;
        let mut wrong = 0usize;
        let mut first: Option<((i32, i32), u32)> = None;
        for (x, y) in &points {
            let Some(pixel) = self.pixel_at(*x, *y) else {
                return Ok(Verdict::Missing(Missing::Unreadable(format!(
                    "the server would not return the pixel at {x},{y}"
                ))));
            };
            if pixel & self.channels != want {
                wrong = wrong.saturating_add(1);
                if first.is_none() {
                    first = Some(((i32::from(*x), i32::from(*y)), pixel));
                }
            }
        }

        if wrong == 0 {
            return Ok(Verdict::OnScreen {
                checked: points.len(),
            });
        }
        let ((x, y), pixel) = first.unwrap_or(((0, 0), 0));
        Ok(Verdict::Missing(Missing::Covered {
            wrong,
            of: points.len(),
            saw: as_rgb(pixel, self.channels),
            at: (x, y),
        }))
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

/// Put one thing back, from a process that did not change it.
///
/// **The keeper's half.** It runs after the host is gone, so it can ask the
/// display server nothing about what the state *used to* be — by then it is
/// not. Everything it needs is in the record, which is why [`Undo`] carries
/// the arguments of the request rather than a description of the intent.
///
/// # Errors
///
/// If the display cannot be reached or the server refuses. The caller says so
/// on stderr, because somebody is sitting in front of the screen that is still
/// dark.
pub fn put_back(undo: &Undo) -> Result<(), String> {
    let Undo::Crtc {
        crtc,
        x,
        y,
        mode,
        rotation,
        outputs,
        ..
    } = undo;
    let (conn, screen_num) = x11rb::connect(None).map_err(|e| e.to_string())?;
    let root = conn
        .setup()
        .roots
        .get(screen_num)
        .ok_or_else(|| "the display has no screen".to_owned())?
        .root;
    // The configuration timestamp has to be the server's current one: the
    // host's is stale by now, and RandR refuses a request made against an
    // older configuration.
    let resources = conn
        .randr_get_screen_resources_current(root)
        .map_err(|e| e.to_string())?
        .reply()
        .map_err(|e| format!("{e:?}"))?;
    let reply = conn
        .randr_set_crtc_config(
            *crtc,
            x11rb::CURRENT_TIME,
            resources.config_timestamp,
            *x,
            *y,
            *mode,
            x11rb::protocol::randr::Rotation::from(*rotation),
            outputs,
        )
        .map_err(|e| e.to_string())?
        .reply()
        .map_err(|e| format!("{e:?}"))?;
    if reply.status == x11rb::protocol::randr::SetConfig::SUCCESS {
        Ok(())
    } else {
        Err(format!("the server said {:?}", reply.status))
    }
}
