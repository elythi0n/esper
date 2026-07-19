#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
// The crate under test is Linux-only; so is its test.
#![cfg(target_os = "linux")]

//! Tests that need a real X server, and that try to defeat what they test.
//!
//! # The point of these
//!
//! The indicator and the panic key are guarantees rather than features, so the
//! interesting question is not "does it work" but **"can it be prevented from
//! working"**. So these do what an attacker at the controller would: put a
//! window over the frame, take the input focus, hold every modifier, grab the
//! keyboard — and check the guarantee survives.

use std::time::Duration;

use esper_presence::{HostEvent, Presence, Watching};
use esper_presence_x11::X11Presence;
use x11rb::connection::Connection as _;
use x11rb::protocol::xproto::{
    ConfigureWindowAux, ConnectionExt as _, CreateWindowAux, GrabMode, InputFocus, StackMode,
    Window, WindowClass,
};
use x11rb::protocol::xtest::ConnectionExt as _;

/// Whether this run may do visible things to the display it can see.
///
/// **Off unless asked for.** These tests draw a frame around the whole screen,
/// take the input focus and press keys — on a real desktop that is somebody's
/// session, and a red border left on it until something repaints is not a
/// thing to do to somebody who ran the test suite. CI sets this, where the
/// display is an `Xvfb` nobody is looking at.
fn may_disturb_the_display() -> bool {
    if std::env::var_os("ESPER_DISPLAY_TESTS").is_none() {
        println!(
            "skipped: set ESPER_DISPLAY_TESTS=1 to allow drawing over this display and \
             pressing keys on it. It is off unless asked for."
        );
        return false;
    }
    true
}

/// One display, one keyboard, one stacking order — and one lock, shared with
/// every other suite in the workspace that touches a display.
fn exclusive() -> esper_display_lock::DisplayLock {
    esper_display_lock::DisplayLock::take()
}

fn open() -> Option<X11Presence> {
    if std::env::var_os("DISPLAY").is_none() {
        println!("skipped: no DISPLAY. CI runs these under Xvfb.");
        return None;
    }
    match X11Presence::open() {
        Ok(p) => Some(p),
        Err(e) => {
            println!("skipped: DISPLAY is set but presence is unavailable ({e})");
            None
        }
    }
}

fn watching(controlling: bool) -> Watching {
    Watching {
        peer: "a test".to_owned(),
        controlling,
    }
}

/// A window that tries to cover the frame.
struct Intruder {
    conn: x11rb::rust_connection::RustConnection,
    window: Window,
}

impl Intruder {
    /// A window covering the whole screen, override-redirect and raised — the
    /// most a program on the host can do to hide something.
    fn cover_the_screen() -> Option<Self> {
        let (conn, screen_num) = x11rb::connect(None).ok()?;
        let screen = conn.setup().roots.get(screen_num)?;
        let (root, depth, visual) = (screen.root, screen.root_depth, screen.root_visual);
        let (w, h) = (screen.width_in_pixels, screen.height_in_pixels);
        let window = conn.generate_id().ok()?;
        conn.create_window(
            depth,
            window,
            root,
            0,
            0,
            w,
            h,
            0,
            WindowClass::INPUT_OUTPUT,
            visual,
            &CreateWindowAux::new()
                .background_pixel(screen.white_pixel)
                .override_redirect(1),
        )
        .ok()?
        .check()
        .ok()?;
        conn.map_window(window).ok()?.check().ok()?;
        conn.configure_window(
            window,
            &ConfigureWindowAux::new().stack_mode(StackMode::ABOVE),
        )
        .ok()?
        .check()
        .ok()?;
        conn.flush().ok()?;
        Some(Self { conn, window })
    }
}

impl Drop for Intruder {
    fn drop(&mut self) {
        let _ = self.conn.destroy_window(self.window);
        let _ = self.conn.flush();
    }
}

/// The colour at a point on the screen, read from the root.
fn pixel_at(x: i16, y: i16) -> Option<(u8, u8, u8)> {
    use x11rb::protocol::xproto::ImageFormat;
    let (conn, screen_num) = x11rb::connect(None).ok()?;
    let root = conn.setup().roots.get(screen_num)?.root;
    let image = conn
        .get_image(ImageFormat::Z_PIXMAP, root, x, y, 1, 1, !0)
        .ok()?
        .reply()
        .ok()?;
    let px = image.data.first_chunk::<4>()?;
    // The development display and Xvfb are both BGRX.
    Some((px[2], px[1], px[0]))
}

#[test]
fn the_indicator_goes_up_and_comes_down() {
    if !may_disturb_the_display() {
        return;
    }
    let _display = exclusive();
    let Some(mut p) = open() else { return };
    if !p.can_show() {
        println!("skipped: this server has no SHAPE extension");
        return;
    }
    assert!(!p.is_showing());
    p.show(&watching(false)).expect("shown");
    assert!(p.is_showing());
    p.hide().expect("hidden");
    assert!(!p.is_showing());
}

/// **The frame is actually on the screen**, in the colour that says what the
/// far end can do — read back from the display rather than taken on trust.
#[test]
fn the_frame_is_visible_and_says_what_the_session_is() {
    if !may_disturb_the_display() {
        return;
    }
    let _display = exclusive();
    let Some(mut p) = open() else { return };
    if !p.can_show() {
        println!("skipped: this server has no SHAPE extension");
        return;
    }
    let before = pixel_at(0, 0);

    p.show(&watching(false)).expect("shown");
    std::thread::sleep(Duration::from_millis(120));
    let amber = pixel_at(1, 1).expect("a pixel");
    assert!(
        amber.0 > 200 && amber.1 > 120 && amber.2 < 100,
        "the edge should be amber while the far end can only watch, got {amber:?}"
    );

    // Escalating to control has to change what it says, or the indicator is
    // telling the person something that is no longer true.
    p.show(&watching(true)).expect("shown again");
    std::thread::sleep(Duration::from_millis(120));
    let red = pixel_at(1, 1).expect("a pixel");
    assert!(
        red.0 > 200 && red.1 < 100 && red.2 < 100,
        "the edge should be red once the far end can type, got {red:?}"
    );
    assert_ne!(amber, red, "the two states must look different");

    // **The windows are gone; what the screen shows next is not ours.**
    //
    // The first version read the pixel back and expected it to change. On a
    // real desktop it did not: X11 keeps no backing store, so the area a
    // destroyed window occupied stays as it was until whoever owns it
    // repaints — which left this failing while the code was correct, and left
    // a red line on the developer's screen.
    //
    // What this layer promises is that the windows are destroyed. That is
    // what gets asserted.
    let frame = p.windows();
    p.hide().expect("hidden");
    let (conn, _) = x11rb::connect(None).expect("a connection");
    for window in frame {
        assert!(
            conn.get_geometry(window).expect("request").reply().is_err(),
            "window {window} is still there after the session ended"
        );
    }
    let _ = (before, red);
}

/// **The frame does not swallow clicks.**
///
/// Without an empty input region, four windows around the edge of the screen
/// would eat every click on the edge of the screen — the scrollbars, the
/// taskbar, the window borders. Checked by asking the server what is under the
/// pointer at the very edge: with an empty input shape the answer is the root,
/// not the frame.
#[test]
fn the_frame_is_not_clickable() {
    if !may_disturb_the_display() {
        return;
    }
    let _display = exclusive();
    let Some(mut p) = open() else { return };
    if !p.can_show() {
        println!("skipped: this server has no SHAPE extension");
        return;
    }
    p.show(&watching(true)).expect("shown");
    std::thread::sleep(Duration::from_millis(120));

    let (conn, screen_num) = x11rb::connect(None).expect("a connection");
    let root = conn.setup().roots.get(screen_num).expect("a screen").root;
    // The pointer on the very first pixel, which is inside the frame.
    conn.xtest_fake_input(6, 0, 0, root, 0, 0, 0)
        .expect("move")
        .check()
        .expect("moved");
    conn.flush().expect("flush");
    std::thread::sleep(Duration::from_millis(60));

    let under = conn
        .query_pointer(root)
        .expect("query")
        .reply()
        .expect("reply");
    // **Not "nothing is there" but "the frame is not there".** The first
    // version asserted the root, which held only on an empty display: any
    // other window at the corner — on the machine this was written on, a
    // nested X server — made it fail for a reason that has nothing to do with
    // the frame.
    let frame = p.windows();
    assert!(
        !frame.contains(&under.child),
        "the pointer at the edge of the screen is over the indicator's own window \
         {:?}, so the frame is swallowing clicks",
        under.child
    );
    p.hide().expect("hidden");
}

/// **A window over the frame does not hide it.**
///
/// The nastiest thing a program on the host can do: a full-screen
/// override-redirect window, raised. The indicator has to come back on top.
#[test]
fn a_window_over_the_frame_does_not_hide_it() {
    if !may_disturb_the_display() {
        return;
    }
    let _display = exclusive();
    let Some(mut p) = open() else { return };
    if !p.can_show() {
        println!("skipped: this server has no SHAPE extension");
        return;
    }
    p.show(&watching(true)).expect("shown");
    std::thread::sleep(Duration::from_millis(100));

    let Some(_intruder) = Intruder::cover_the_screen() else {
        println!("skipped: could not create a covering window");
        return;
    };
    std::thread::sleep(Duration::from_millis(100));
    // It really is covered until the indicator is asked about anything.
    let covered = pixel_at(1, 1).expect("a pixel");

    // Any poll puts it back, and a host polls constantly.
    let _ = p.poll().expect("poll");
    std::thread::sleep(Duration::from_millis(120));
    let after = pixel_at(1, 1).expect("a pixel");
    assert!(
        after.0 > 200 && after.1 < 100 && after.2 < 100,
        "the frame should be back on top, got {after:?} (it was {covered:?} while covered)"
    );
    p.hide().expect("hidden");
}

/// **The panic key reaches the host with the focus somewhere else.**
///
/// A passive grab on the root means the server gives the key to this
/// connection before any application, whatever has the focus. Checked by
/// giving the focus to another window first and then pressing it.
#[test]
fn the_panic_key_arrives_even_when_another_window_has_the_focus() {
    if !may_disturb_the_display() {
        return;
    }
    let _display = exclusive();
    let Some(mut p) = open() else { return };
    if !p.panic_key_grabbed() {
        println!("skipped: something else on this display already holds the panic key");
        return;
    }
    let _ = p.poll();

    let (conn, screen_num) = x11rb::connect(None).expect("a connection");
    let screen = conn.setup().roots.get(screen_num).expect("a screen");
    let root = screen.root;
    // A window that takes the focus and asks for every key it can.
    let thief = conn.generate_id().expect("an id");
    conn.create_window(
        screen.root_depth,
        thief,
        root,
        0,
        0,
        200,
        200,
        0,
        WindowClass::INPUT_OUTPUT,
        screen.root_visual,
        &CreateWindowAux::new()
            .background_pixel(screen.black_pixel)
            .override_redirect(1)
            .event_mask(x11rb::protocol::xproto::EventMask::KEY_PRESS),
    )
    .expect("create")
    .check()
    .expect("created");
    conn.map_window(thief)
        .expect("map")
        .check()
        .expect("mapped");
    let _ = conn.set_input_focus(InputFocus::PARENT, thief, x11rb::CURRENT_TIME);
    conn.flush().expect("flush");
    std::thread::sleep(Duration::from_millis(80));

    // Ctrl+Alt+Shift+Escape, from the keycodes the table gives.
    let key = |usage: u16| -> u8 {
        u8::try_from(
            esper_input::hid::evdev_for_usage(usage)
                .expect("in the table")
                .0
                + 8,
        )
        .expect("a keycode")
    };
    for usage in [0xE0u16, 0xE2, 0xE1] {
        conn.xtest_fake_input(2, key(usage), 0, root, 0, 0, 0)
            .expect("down");
    }
    conn.xtest_fake_input(2, key(0x29), 0, root, 0, 0, 0)
        .expect("escape down");
    conn.xtest_fake_input(3, key(0x29), 0, root, 0, 0, 0)
        .expect("escape up");
    for usage in [0xE1u16, 0xE2, 0xE0] {
        conn.xtest_fake_input(3, key(usage), 0, root, 0, 0, 0)
            .expect("up");
    }
    conn.flush().expect("flush");

    let mut seen = Vec::new();
    for _ in 0..40 {
        seen.extend(p.poll().expect("poll"));
        if !seen.is_empty() {
            break;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    let _ = conn.destroy_window(thief);
    let _ = conn.flush();

    assert!(
        seen.contains(&HostEvent::Panic),
        "the panic key did not reach the host while another window had the focus"
    );
}

/// **What an active keyboard grab does, recorded rather than wished away.**
///
/// A client holding the whole keyboard — a screen locker, an open menu, a
/// full-screen game — receives every key, and an *active* grab takes
/// precedence over a *passive* one like ours. So during such a grab the panic
/// key does not reach the host. That is X11 working exactly as specified, and
/// there is nothing a passive grab can do about it.
///
/// **It is not a hole a controller can use.** A controller can inject keys and
/// nothing else; injecting cannot grab a keyboard, and there is no message in
/// the protocol that asks the host to grab its own. The threat the panic key
/// answers is "the person at the far end is doing something I want stopped",
/// and that person cannot cause this.
///
/// A program already running on the host *can* — and a program already running
/// on the host has far better options than hiding a panic key.
///
/// Asserted rather than left unwritten so that a change is noticed: if this
/// key starts arriving during a grab, either X11 or this code has changed, and
/// the note above should be rewritten rather than quietly left wrong.
#[test]
fn an_active_keyboard_grab_takes_the_panic_key_and_that_is_x11() {
    if !may_disturb_the_display() {
        return;
    }
    let _display = exclusive();
    let Some(mut p) = open() else { return };
    if !p.panic_key_grabbed() {
        println!("skipped: something else on this display already holds the panic key");
        return;
    }
    let _ = p.poll();

    let (conn, screen_num) = x11rb::connect(None).expect("a connection");
    let root = conn.setup().roots.get(screen_num).expect("a screen").root;
    let grabbed = conn
        .grab_keyboard(
            false,
            root,
            x11rb::CURRENT_TIME,
            GrabMode::ASYNC,
            GrabMode::ASYNC,
        )
        .expect("grab")
        .reply()
        .expect("reply");
    println!("keyboard grab status: {:?}", grabbed.status);

    let key = |usage: u16| -> u8 {
        u8::try_from(
            esper_input::hid::evdev_for_usage(usage)
                .expect("in the table")
                .0
                + 8,
        )
        .expect("a keycode")
    };
    for usage in [0xE0u16, 0xE2, 0xE1] {
        conn.xtest_fake_input(2, key(usage), 0, root, 0, 0, 0)
            .expect("down");
    }
    conn.xtest_fake_input(2, key(0x29), 0, root, 0, 0, 0)
        .expect("escape");
    conn.xtest_fake_input(3, key(0x29), 0, root, 0, 0, 0)
        .expect("escape up");
    for usage in [0xE1u16, 0xE2, 0xE0] {
        conn.xtest_fake_input(3, key(usage), 0, root, 0, 0, 0)
            .expect("up");
    }
    conn.flush().expect("flush");

    let mut seen = Vec::new();
    for _ in 0..40 {
        seen.extend(p.poll().expect("poll"));
        if !seen.is_empty() {
            break;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    let _ = conn.ungrab_keyboard(x11rb::CURRENT_TIME);
    let _ = conn.flush();

    assert!(
        !seen.contains(&HostEvent::Panic),
        "the panic key arrived during an active keyboard grab — X11 or this backend has \
         changed, which is good news: the note on this test should be rewritten"
    );
}

/// The panic key is grabbed from the moment the host is running, not when a
/// session starts — one of the moments a person might reach for it is while a
/// session is being offered.
#[test]
fn the_panic_key_is_grabbed_before_any_session() {
    let _display = exclusive();
    let Some(p) = open() else { return };
    // **Asserted outright.** This read an environment variable that nothing
    // sets, so the `is_none()` was always true and the assertion could never
    // fail — a test that cannot fail, left behind by renaming the gate around
    // it. If something else on the display already holds the key, that is
    // worth failing over here: the whole point of this test is that the key is
    // ours before any session exists.
    assert!(
        p.panic_key_grabbed(),
        "the panic key should be grabbed as soon as presence is open"
    );
    assert!(!p.is_showing(), "and no indicator until there is a session");
}
