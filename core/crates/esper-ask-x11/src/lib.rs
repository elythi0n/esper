//! The consent question on somebody's own screen.
//!
//! # Why this exists
//!
//! A host with a display and no terminal could not ask anybody anything, so it
//! refused every session that needed a decision. That is honest and it is not
//! good enough: somebody who starts Esper from a desktop launcher, or as a
//! user service, has nowhere to say yes.
//!
//! # What makes a prompt trustworthy, and what cannot
//!
//! The question is drawn by [`esper_dialog`], which has no idea there is an X
//! server. What is here is the part that decides whether the thing somebody
//! answers is the thing that asked:
//!
//! - **Override-redirect**, so no window manager can iconify, reparent or
//!   decorate it, and so it sits above ordinary windows.
//! - **It raises itself** when the server says it has been obscured. Another
//!   client can make its own override-redirect window and put it on top; it
//!   cannot keep it there.
//! - **The keyboard and pointer are grabbed**, so a click means this window
//!   and nothing behind it, and so an answer cannot be typed into something
//!   else.
//! - **Nothing counts for the first moment.** The queue is drained and input
//!   is ignored while [`SETTLING`] passes, because a keystroke or click from
//!   before a prompt existed is not an answer to it — the same rule the
//!   terminal prompt has, where anything already typed is thrown away.
//! - **A key or button already held when the prompt appeared does not count**
//!   until it is seen to come up, checked against the server's own key state
//!   rather than against the event stream, because auto-repeat produces a
//!   press-and-release cycle that looks exactly like somebody typing.
//! - **Silence is a refusal**, and it is the only thing a timeout can be.
//!
//! What this cannot defend against is X11 itself. Any process that can open
//! this display can synthesise input with XTEST, read the screen, and take a
//! grab — this product uses two of those three to do its job. A prompt cannot
//! be made safe against a program that is already running as the person it
//! would be asking, and saying otherwise would be a promise nobody can keep.
//! The isolation that would fix it is the display server's to provide, which
//! is one of the reasons Wayland is on the map.

use std::time::{Duration, Instant};

use esper_dialog::{Painted, Question};
use x11rb::connection::Connection;
use x11rb::protocol::Event;
use x11rb::protocol::randr::ConnectionExt as _;
use x11rb::protocol::xproto::{
    ButtonIndex, ConnectionExt as _, CreateGCAux, CreateWindowAux, EventMask, GrabMode,
    ImageFormat, StackMode, Window, WindowClass,
};
use x11rb::rust_connection::RustConnection;

/// What somebody said.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Answer {
    /// Somebody said yes.
    Yes,
    /// Somebody said no.
    No,
    /// Nobody said anything in time.
    Silence,
}

/// How long the prompt ignores everything after appearing.
///
/// Long enough that a keystroke or a click on its way when it appeared cannot
/// be the answer to it, short enough that somebody who was already looking at
/// the screen does not notice. The browsers settled on the same order of
/// magnitude for the same reason.
pub const SETTLING: Duration = Duration::from_millis(400);

/// How often the loop looks at the world.
///
/// A prompt waits forty-five seconds for a person; there is nothing here worth
/// spending a millisecond on, and a poll is simpler to reason about than a
/// timeout on a socket that also has to be woken for a repaint.
const TICK: Duration = Duration::from_millis(25);

/// The most bytes to put in one `PutImage` request.
///
/// A prompt is about three quarters of a megabyte and one request may be a
/// quarter of that without the big-requests extension, so it goes in bands. A
/// number well inside the limit rather than exactly at it: the limit is in
/// four-byte units and the request has a header.
const BAND_BYTES: usize = 128 * 1024;

/// Ask on this machine's screen, and wait up to `within`.
///
/// # Errors
///
/// When there is no display to draw on, or the display cannot be drawn on in a
/// format this understands. A caller that gets an error has nobody to ask and
/// should say so rather than treating it as a refusal — those are different
/// answers and they need different words.
pub fn ask(question: &Question, within: Duration) -> Result<Answer, String> {
    let mut screen = Screen::open()?;
    screen.ask(question, within)
}

/// Whether this machine has a screen a question could be drawn on.
///
/// Checked by opening the display and looking at what it can do, not by
/// looking at an environment variable: `DISPLAY` being set says somebody
/// intended there to be a display, which is not the same as there being one.
#[must_use]
pub fn can_ask() -> bool {
    Screen::open().is_ok()
}

/// What the backend is called, for a report that says how it would ask.
pub const BACKEND: &str = "X11";

/// The display, and what it takes to draw on it.
struct Screen {
    conn: RustConnection,
    root: Window,
    depth: u8,
    visual: u32,
    /// Bytes per pixel of the format the server wants for `depth`.
    stride: usize,
    /// Whether the server wants each pixel's bytes most significant first.
    big_endian: bool,
    /// Where to put a window so it is in the middle of one monitor.
    centre: (i16, i16, u16, u16),
}

impl Screen {
    /// Open the display and establish that a picture can be put on it.
    fn open() -> Result<Self, String> {
        let (conn, num) = x11rb::connect(None).map_err(|e| format!("opening the display: {e}"))?;
        let setup = conn.setup().clone();
        let screen = setup
            .roots
            .get(num)
            .ok_or_else(|| "the display has no screen".to_owned())?
            .clone();
        let depth = screen.root_depth;
        // **The format the server wants, asked for rather than assumed.** A
        // prompt drawn in the wrong pixel format is a prompt somebody cannot
        // read, which for a security question is worse than not drawing one.
        let format = setup
            .pixmap_formats
            .iter()
            .find(|f| f.depth == depth)
            .ok_or_else(|| format!("the display has no pixmap format for depth {depth}"))?;
        if format.bits_per_pixel != 32 || depth < 24 {
            return Err(format!(
                "this display is {depth}-bit at {} bits per pixel, and drawing a prompt here \
                 needs 24 or 32 bits at 32 — a question nobody could read is not one to ask",
                format.bits_per_pixel
            ));
        }
        let big_endian = setup.image_byte_order == x11rb::protocol::xproto::ImageOrder::MSB_FIRST;
        let centre = middle_of_a_monitor(
            &conn,
            screen.root,
            screen.width_in_pixels,
            screen.height_in_pixels,
        );
        Ok(Self {
            conn,
            root: screen.root,
            depth,
            visual: screen.root_visual,
            stride: 4,
            big_endian,
            centre,
        })
    }

    /// Draw the question, wait for an answer, and take the window away.
    fn ask(&mut self, question: &Question, within: Duration) -> Result<Answer, String> {
        let deadline = Instant::now() + within;
        let mut painted = esper_dialog::paint(&countdown(question, within));
        let window = self.show(&painted)?;
        // Whatever happens from here, the window and the grabs go.
        let answer = self.wait(window, &mut painted, question, deadline);
        let _ = self.conn.ungrab_keyboard(x11rb::CURRENT_TIME);
        let _ = self.conn.ungrab_pointer(x11rb::CURRENT_TIME);
        let _ = self.conn.destroy_window(window);
        let _ = self.conn.flush();
        answer
    }

    /// Create the window, put the picture in it, and take the input.
    fn show(&self, painted: &Painted) -> Result<Window, String> {
        let (cx, cy, cw, ch) = self.centre;
        let w = u16::try_from(painted.canvas.width).unwrap_or(u16::MAX);
        let h = u16::try_from(painted.canvas.height).unwrap_or(u16::MAX);
        let x = cx.saturating_add(i16::try_from(cw.saturating_sub(w) / 2).unwrap_or(0));
        let y = cy.saturating_add(i16::try_from(ch.saturating_sub(h) / 2).unwrap_or(0));

        let window = self
            .conn
            .generate_id()
            .map_err(|e| format!("asking for a window id: {e}"))?;
        self.conn
            .create_window(
                self.depth,
                window,
                self.root,
                x,
                y,
                w,
                h,
                0,
                WindowClass::INPUT_OUTPUT,
                self.visual,
                &CreateWindowAux::new()
                    // **No window manager.** A prompt that could be
                    // iconified, moved behind something or given a title bar
                    // saying whatever a theme likes is a prompt somebody else
                    // has a say in.
                    .override_redirect(1)
                    .background_pixel(0x0000_0000)
                    .event_mask(
                        EventMask::EXPOSURE
                            | EventMask::KEY_PRESS
                            | EventMask::KEY_RELEASE
                            | EventMask::BUTTON_PRESS
                            | EventMask::VISIBILITY_CHANGE,
                    ),
            )
            .map_err(|e| format!("making the window: {e}"))?
            .check()
            .map_err(|e| format!("making the window: {e}"))?;
        self.conn
            .map_window(window)
            .map_err(|e| format!("showing the window: {e}"))?
            .check()
            .map_err(|e| format!("showing the window: {e}"))?;
        self.raise(window);
        // **Drawn here as well as on `Expose`.** The server sends an expose
        // when the window is mapped, so the loop would draw it anyway — an
        // attempt to prove this call mattered by deleting it changed nothing,
        // which is how that was found out. It stays because the loop is not
        // where the guarantee belongs: input is grabbed a few lines below, and
        // there must be no moment where this window is taking answers to a
        // question it has not drawn yet.
        self.put(window, painted)?;

        // **Grabbed after the picture is up**, so there is never a moment
        // where input is being taken for a window that is not showing the
        // question yet.
        //
        // A grab that fails is not fatal: the window still gets clicks and
        // key presses of its own, and a prompt somebody can answer with the
        // mouse is better than no prompt. It is reported, because "the
        // keyboard could not be taken" is worth knowing on a machine where
        // something else holds it.
        if self
            .conn
            .grab_keyboard(
                false,
                window,
                x11rb::CURRENT_TIME,
                GrabMode::ASYNC,
                GrabMode::ASYNC,
            )
            .ok()
            .and_then(|c| c.reply().ok())
            .is_none()
        {
            eprintln!("  · the prompt could not take the keyboard; it can still be clicked");
        }
        let _ = self.conn.grab_pointer(
            false,
            window,
            EventMask::BUTTON_PRESS,
            GrabMode::ASYNC,
            GrabMode::ASYNC,
            x11rb::NONE,
            x11rb::NONE,
            x11rb::CURRENT_TIME,
        );
        self.conn
            .flush()
            .map_err(|e| format!("drawing the question: {e}"))?;
        Ok(window)
    }

    /// Put the picture in the window, in bands that fit one request.
    fn put(&self, window: Window, painted: &Painted) -> Result<(), String> {
        let gc = self
            .conn
            .generate_id()
            .map_err(|e| format!("asking for a graphics context: {e}"))?;
        self.conn
            .create_gc(gc, window, &CreateGCAux::new())
            .map_err(|e| format!("making a graphics context: {e}"))?;

        let width = painted.canvas.width;
        let row_bytes = width * self.stride;
        let rows_at_once = (BAND_BYTES / row_bytes.max(1)).max(1);
        let mut row = 0usize;
        while row < painted.canvas.height {
            let rows = rows_at_once.min(painted.canvas.height - row);
            let mut bytes = Vec::with_capacity(rows * row_bytes);
            for y in row..row + rows {
                for x in 0..width {
                    let pixel = painted.canvas.at(x, y).unwrap_or(0);
                    if self.big_endian {
                        bytes.extend_from_slice(&pixel.to_be_bytes());
                    } else {
                        bytes.extend_from_slice(&pixel.to_le_bytes());
                    }
                }
            }
            self.conn
                .put_image(
                    ImageFormat::Z_PIXMAP,
                    window,
                    gc,
                    u16::try_from(width).unwrap_or(u16::MAX),
                    u16::try_from(rows).unwrap_or(u16::MAX),
                    0,
                    i16::try_from(row).unwrap_or(i16::MAX),
                    0,
                    self.depth,
                    &bytes,
                )
                .map_err(|e| format!("drawing the question: {e}"))?
                .check()
                .map_err(|e| format!("drawing the question: {e}"))?;
            row += rows;
        }
        let _ = self.conn.free_gc(gc);
        Ok(())
    }

    /// Put the window above everything.
    fn raise(&self, window: Window) {
        let _ = self.conn.configure_window(
            window,
            &x11rb::protocol::xproto::ConfigureWindowAux::new().stack_mode(StackMode::ABOVE),
        );
    }

    /// Wait for an answer.
    fn wait(
        &self,
        window: Window,
        painted: &mut Painted,
        question: &Question,
        deadline: Instant,
    ) -> Result<Answer, String> {
        let keysyms = self.keyboard_mapping();
        // **What was already held down does not count.** Checked against the
        // server's key state rather than the event stream: auto-repeat sends a
        // press and a release over and over, so a key held from before the
        // prompt existed produces something that looks exactly like somebody
        // typing an answer.
        let mut held = self.keys_down();
        let mut buttons_held = self.buttons_down();
        let appeared = Instant::now();
        let mut shown_seconds = seconds_left(deadline);

        loop {
            let now = Instant::now();
            if now >= deadline {
                return Ok(Answer::Silence);
            }
            let settled = now.duration_since(appeared) >= SETTLING;

            // The countdown, once a second, so a prompt that is running out
            // says so rather than vanishing.
            let left = seconds_left(deadline);
            if left != shown_seconds {
                shown_seconds = left;
                *painted = esper_dialog::paint(&countdown(question, Duration::from_secs(left)));
                self.put(window, painted)?;
                let _ = self.conn.flush();
            }

            while let Some(event) = self
                .conn
                .poll_for_event()
                .map_err(|e| format!("reading the display: {e}"))?
            {
                match event {
                    Event::Expose(_) => {
                        self.put(window, painted)?;
                        let _ = self.conn.flush();
                    }
                    // **Obscured, so it puts itself back on top.** Another
                    // client can cover an override-redirect window; it cannot
                    // keep it covered.
                    Event::VisibilityNotify(v)
                        if v.state != x11rb::protocol::xproto::Visibility::UNOBSCURED =>
                    {
                        self.raise(window);
                        let _ = self.conn.flush();
                    }
                    Event::KeyRelease(_) | Event::VisibilityNotify(_) => {}
                    Event::KeyPress(key) => {
                        if !settled || held.contains(&key.detail) {
                            continue;
                        }
                        match keysym_of(&keysyms, key.detail) {
                            // Escape, Return, `n`, and `q` all refuse. A
                            // prompt somebody wants gone should go, and the
                            // direction it goes in is the safe one.
                            Some(0xff1b | 0xff0d | 0x006e | 0x0071) => return Ok(Answer::No),
                            Some(0x0079) => return Ok(Answer::Yes),
                            _ => {}
                        }
                    }
                    Event::ButtonPress(click) => {
                        if !settled || buttons_held.contains(&click.detail) {
                            continue;
                        }
                        if click.detail != u8::from(ButtonIndex::M1) {
                            continue;
                        }
                        let Ok(x) = usize::try_from(click.event_x) else {
                            continue;
                        };
                        let Ok(y) = usize::try_from(click.event_y) else {
                            continue;
                        };
                        if painted.refuse.holds(x, y) {
                            return Ok(Answer::No);
                        }
                        if painted.allow.holds(x, y) {
                            return Ok(Answer::Yes);
                        }
                    }
                    _ => {}
                }
            }

            // Anything held when the prompt appeared stops being held only
            // when the server says it is up.
            if !held.is_empty() {
                let down = self.keys_down();
                held.retain(|code| down.contains(code));
            }
            if !buttons_held.is_empty() {
                let down = self.buttons_down();
                buttons_held.retain(|b| down.contains(b));
            }
            std::thread::sleep(TICK);
        }
    }

    /// The keysyms for every keycode, so a `y` is a `y` on somebody's own
    /// layout rather than on the author's.
    fn keyboard_mapping(&self) -> (u8, u8, Vec<u32>) {
        let setup = self.conn.setup();
        let first = setup.min_keycode;
        let count = setup.max_keycode.saturating_sub(first).saturating_add(1);
        let reply = self
            .conn
            .get_keyboard_mapping(first, count)
            .ok()
            .and_then(|c| c.reply().ok());
        match reply {
            Some(r) => (first, r.keysyms_per_keycode, r.keysyms),
            None => (first, 0, Vec::new()),
        }
    }

    /// Which keycodes the server says are down right now.
    fn keys_down(&self) -> Vec<u8> {
        let Some(reply) = self.conn.query_keymap().ok().and_then(|c| c.reply().ok()) else {
            return Vec::new();
        };
        let mut down = Vec::new();
        for (byte, bits) in reply.keys.iter().enumerate() {
            for bit in 0..8u8 {
                if bits & (1 << bit) != 0
                    && let Ok(code) = u8::try_from(byte * 8 + usize::from(bit))
                {
                    down.push(code);
                }
            }
        }
        down
    }

    /// Which pointer buttons the server says are down right now.
    fn buttons_down(&self) -> Vec<u8> {
        let Some(reply) = self
            .conn
            .query_pointer(self.root)
            .ok()
            .and_then(|c| c.reply().ok())
        else {
            return Vec::new();
        };
        let mask = u16::from(reply.mask);
        let mut down = Vec::new();
        for (bit, button) in [(8u16, 1u8), (9, 2), (10, 3), (11, 4), (12, 5)] {
            if mask & (1 << bit) != 0 {
                down.push(button);
            }
        }
        down
    }
}

/// The keysym a keycode produces with no modifiers.
fn keysym_of(mapping: &(u8, u8, Vec<u32>), code: u8) -> Option<u32> {
    let (first, per, syms) = mapping;
    if *per == 0 || code < *first {
        return None;
    }
    let at = usize::from(code - *first) * usize::from(*per);
    syms.get(at).copied().filter(|s| *s != 0)
}

/// Whole seconds left, rounded up, so a prompt never shows a zero it is still
/// waiting on.
fn seconds_left(deadline: Instant) -> u64 {
    let left = deadline.saturating_duration_since(Instant::now());
    left.as_secs() + u64::from(left.subsec_nanos() > 0)
}

/// The question with its countdown set.
fn countdown(question: &Question, left: Duration) -> Question {
    Question {
        seconds: left.as_secs(),
        ..question.clone()
    }
}

/// Where to put a window so it lands in the middle of one monitor.
///
/// The primary one if RandR says which, else the first, else the whole screen —
/// because a prompt straddling the gap between two monitors is a prompt
/// somebody reads half of.
fn middle_of_a_monitor<C: Connection>(
    conn: &C,
    root: Window,
    width: u16,
    height: u16,
) -> (i16, i16, u16, u16) {
    let whole = (0i16, 0i16, width, height);
    let Some(reply) = conn
        .randr_get_monitors(root, true)
        .ok()
        .and_then(|c| c.reply().ok())
    else {
        return whole;
    };
    let mut best: Option<(i16, i16, u16, u16)> = None;
    for m in reply.monitors {
        if m.width == 0 || m.height == 0 {
            continue;
        }
        let this = (m.x, m.y, m.width, m.height);
        if m.primary {
            return this;
        }
        best = best.or(Some(this));
    }
    best.unwrap_or(whole)
}
