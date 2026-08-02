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
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{
    ConfigureWindowAux, ConnectionExt as _, CreateWindowAux, EventMask, GrabMode, InputFocus,
    StackMode, Window, WindowClass,
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

/// Hold Ctrl+Alt+Shift, tap `usage`, let go.
///
/// The keycodes come from the HID table the input crate already owns, so this
/// presses the same key the backend grabbed rather than a keysym that happens
/// to look right on this layout.
fn hotkey(conn: &impl Connection, root: Window, usage: u16) {
    let key = |usage: u16| -> u8 {
        u8::try_from(
            esper_input::hid::evdev_for_usage(usage)
                .expect("in the table")
                .0
                + 8,
        )
        .expect("a keycode")
    };
    const PRESS: u8 = 2;
    const RELEASE: u8 = 3;
    for held in [0xE0u16, 0xE2, 0xE1] {
        conn.xtest_fake_input(PRESS, key(held), 0, root, 0, 0, 0)
            .expect("a modifier down");
    }
    conn.xtest_fake_input(PRESS, key(usage), 0, root, 0, 0, 0)
        .expect("the key down");
    conn.xtest_fake_input(RELEASE, key(usage), 0, root, 0, 0, 0)
        .expect("the key up");
    for held in [0xE1u16, 0xE2, 0xE0] {
        conn.xtest_fake_input(RELEASE, key(held), 0, root, 0, 0, 0)
            .expect("a modifier up");
    }
    conn.flush().expect("flush");
}

/// Poll until something arrives, or give up after a second.
fn presses(p: &mut X11Presence) -> Vec<HostEvent> {
    let mut seen = Vec::new();
    for _ in 0..40 {
        seen.extend(p.poll().expect("poll"));
        if !seen.is_empty() {
            break;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    seen
}

fn open() -> Option<X11Presence> {
    if std::env::var_os("DISPLAY").is_none() {
        println!("skipped: no DISPLAY. CI runs these under Xvfb.");
        return None;
    }
    match X11Presence::open() {
        Ok(p) => Some(p),
        // **A display that will not open is a broken rig, not a skip.**
        //
        // Somebody who set `ESPER_DISPLAY_TESTS` asked for these to run, so
        // quietly not running them is the worst of the three outcomes: it
        // reads as a pass. Found the hard way — a whole file of these
        // reported "17 passed" against a display that had been shut down, and
        // the number said nothing at all.
        //
        // A *momentarily* busy server is a different thing and is not a
        // failure: `esper_display_lock::connect` waits it out, which is why
        // the tests below reach the server through that rather than through
        // `x11rb::connect` directly.
        Err(e) => panic!(
            "ESPER_DISPLAY_TESTS is set and DISPLAY={} cannot be opened ({e}). \
             Start an X server for it, or unset the variable to skip these.",
            std::env::var("DISPLAY").unwrap_or_default()
        ),
    }
}

fn watching(controlling: bool) -> Watching {
    Watching {
        peer: "a test".to_owned(),
        controlling,
        recording: false,
        paused: false,
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
        let (conn, screen_num) = esper_display_lock::connect().ok()?;
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
    let (conn, screen_num) = esper_display_lock::connect().ok()?;
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
    let (conn, _) = esper_display_lock::connect().expect("a connection");
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

    let (conn, screen_num) = esper_display_lock::connect().expect("a connection");
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

    let (conn, screen_num) = esper_display_lock::connect().expect("a connection");
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
    // Ctrl+Alt+Shift+Escape.
    hotkey(&conn, root, 0x29);
    let seen = presses(&mut p);
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

    let (conn, screen_num) = esper_display_lock::connect().expect("a connection");
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

    hotkey(&conn, root, 0x29);
    let seen = presses(&mut p);
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

/// **A held session still says somebody is connected.**
///
/// The frame changes colour rather than coming down: taking it away would say
/// "nobody is watching", which is false — the session is there, the
/// permissions stand, and the far end is one keystroke from seeing the screen
/// again. That is the one lie this crate exists to prevent, and pausing is the
/// state most likely to produce it.
#[test]
fn a_paused_session_changes_the_frame_rather_than_removing_it() {
    let _display = exclusive();
    if !may_disturb_the_display() {
        return;
    }
    let Some(mut p) = open() else { return };

    p.show(&watching(false)).expect("shown");
    let amber = pixel_at(1, 1).expect("a pixel");
    assert!(p.is_showing(), "a frame while watching");

    p.show(&Watching {
        paused: true,
        ..watching(false)
    })
    .expect("shown");
    assert!(p.is_showing(), "and a frame while held");
    assert_eq!(p.windows().len(), 4, "all four edges are still there");
    let held = pixel_at(1, 1).expect("a pixel");
    assert_ne!(
        amber, held,
        "a held session looks exactly like a watched one, so nobody can tell"
    );

    // And back, so the colour is a state rather than a one-way trip.
    p.show(&watching(false)).expect("shown");
    assert_eq!(
        pixel_at(1, 1).expect("a pixel"),
        amber,
        "letting go did not put the colour back"
    );
    p.hide().expect("hidden");
}

/// **Both hotkeys are grabbed, and the panic key is the one that must be.**
///
/// If only one grab can be had — something else already holds a combination —
/// it has to be the one that makes a session stop: a pause key nobody can
/// reach is an inconvenience, and a panic key nobody can reach is a promise
/// this crate is not keeping.
#[test]
fn both_hotkeys_are_grabbed_before_any_session() {
    let _display = exclusive();
    let Some(p) = open() else { return };
    assert!(
        p.panic_key_grabbed(),
        "the panic key should be grabbed as soon as presence is open"
    );
    assert!(
        p.pause_key_grabbed(),
        "and so should the pause key, which is the one people will actually use"
    );
}

/// **Pressing the pause key pauses, and does not end the session.**
///
/// Both hotkeys are the same three modifiers and differ only in the last key,
/// so telling them apart is a keycode comparison — the kind of thing that is
/// easy to write and easy to get away with getting wrong, because the panic
/// key would keep working and only the pause key would be catastrophic.
#[test]
fn the_pause_key_arrives_as_a_pause() {
    if !may_disturb_the_display() {
        return;
    }
    let _display = exclusive();
    let Some(mut p) = open() else { return };
    if !p.pause_key_grabbed() {
        println!("skipped: something else on this display already holds the pause key");
        return;
    }
    let _ = p.poll();

    let (conn, screen_num) = esper_display_lock::connect().expect("a connection");
    let root = conn.setup().roots.get(screen_num).expect("a screen").root;

    // Ctrl+Alt+Shift+P.
    hotkey(&conn, root, 0x13);
    let seen = presses(&mut p);

    assert!(
        seen.contains(&HostEvent::Pause),
        "the pause key did not arrive as a pause, it arrived as {seen:?}"
    );
    assert!(
        !seen.contains(&HostEvent::Panic),
        "the pause key ended the session — {seen:?}"
    );
}

/// **Asking the screen, not this process's own memory.**
///
/// `show` returning `Ok` means the requests were accepted, which is a fact
/// about this connection. What a person relies on is the frame being *on the
/// screen*, and the two came apart in every interesting case until `verify`
/// existed. So: nothing asked for, then asked for and confirmed.
#[test]
fn the_screen_confirms_the_frame_when_it_is_there() {
    if !may_disturb_the_display() {
        return;
    }
    let _display = exclusive();
    let Some(mut p) = open() else { return };
    if !p.can_show() {
        println!("skipped: this server has no SHAPE extension");
        return;
    }

    let before = p.verify().expect("a verdict");
    assert_eq!(
        before,
        esper_presence::Verdict::Missing(esper_presence::Missing::NothingAsked),
        "nothing has been asked for yet, which is not a fault"
    );

    p.show(&watching(true)).expect("shown");
    let verdict = p.verify().expect("a verdict");
    match verdict {
        esper_presence::Verdict::OnScreen { checked } => {
            assert!(checked >= 12, "only {checked} places were looked at");
        }
        other => panic!("the frame is up and the screen says {other:?}"),
    }

    // And it is the right colour: a paused session says something different,
    // and a check that accepted any colour would accept the wrong one.
    p.show(&esper_presence::Watching {
        peer: "somebody".to_owned(),
        controlling: true,
        recording: false,
        paused: true,
    })
    .expect("shown paused");
    assert!(
        p.verify().expect("a verdict").is_on_screen(),
        "the paused colour should confirm as itself"
    );
    p.hide().expect("hidden");
}

/// **Something drawn over the frame is the case nothing else can catch.**
///
/// The windows exist, they are mapped, this process asked for them and every
/// answer it can give itself is "yes" — and the screen shows white. Only
/// reading the pixels back finds it, which is why `verify` reads pixels.
///
/// Deliberately does not poll: a poll raises the frame again, which is the
/// recovery this product already had. What is being tested is whether the
/// state is *noticed*, because a session that carries on while the indicator
/// is hidden is the failure this is about.
#[test]
fn the_screen_says_when_something_is_drawn_over_the_frame() {
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
    assert!(p.verify().expect("a verdict").is_on_screen(), "up first");

    let Some(intruder) = Intruder::cover_the_screen() else {
        println!("skipped: could not create a covering window");
        return;
    };
    let verdict = p.verify().expect("a verdict");
    match verdict {
        esper_presence::Verdict::Missing(esper_presence::Missing::Covered {
            wrong,
            of,
            saw,
            at,
        }) => {
            assert_eq!(wrong, of, "the whole screen is covered, at {at:?}");
            assert!(
                saw.iter().all(|c| *c > 200),
                "the cover is white, and the message says {saw:?}"
            );
        }
        other => panic!("the frame is covered and the screen says {other:?}"),
    }
    // The windows themselves are untouched, which is exactly why this needed
    // pixels: everything this process can ask about itself still says fine.
    assert!(p.is_showing(), "and this process still believes it is up");

    // Uncovered and raised again, it confirms — so the verdict follows the
    // screen rather than latching.
    drop(intruder);
    let _ = p.poll().expect("poll");
    let mut back = false;
    for _ in 0..30 {
        if p.verify().expect("a verdict").is_on_screen() {
            back = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        back,
        "the frame should confirm again once nothing covers it"
    );
    p.hide().expect("hidden");
}

/// **A compositor restarting destroys override-redirect windows.**
///
/// Simulated the only way it can be from a test: another connection destroys
/// them, which is what X11 allows anybody who knows the window id to do. The
/// verdict has to be `Gone` rather than `Covered`, because the two mean
/// different things to whoever acts on it — one can be rebuilt from here and
/// the other cannot.
#[test]
fn windows_destroyed_by_something_else_are_reported_as_gone() {
    if !may_disturb_the_display() {
        return;
    }
    let _display = exclusive();
    let Some(mut p) = open() else { return };
    if !p.can_show() {
        println!("skipped: this server has no SHAPE extension");
        return;
    }
    p.show(&watching(false)).expect("shown");
    assert!(p.verify().expect("a verdict").is_on_screen(), "up first");

    let windows = p.windows();
    assert_eq!(windows.len(), 4, "four edges");
    let (conn, _) = esper_display_lock::connect().expect("a second connection");
    for window in windows {
        let _ = conn.destroy_window(window);
    }
    conn.flush().expect("flushed");

    let verdict = p.verify().expect("a verdict");
    assert_eq!(
        verdict,
        esper_presence::Verdict::Missing(esper_presence::Missing::Gone),
        "destroyed windows are gone, not covered"
    );
    assert!(
        matches!(&verdict, esper_presence::Verdict::Missing(m) if m.worth_rebuilding()),
        "and rebuilding them is worth trying"
    );

    // Which is what showing again does, and the screen confirms it.
    p.show(&watching(false)).expect("shown again");
    assert!(
        p.verify().expect("a verdict").is_on_screen(),
        "the frame should be back after being rebuilt"
    );
    p.hide().expect("hidden");
}

/// **The panic key reaches the host under a fullscreen, modal application.**
///
/// Phase 15 asks for it in those words, and it is the case somebody actually
/// hits: a video player, a game or a presentation covering the screen, or a
/// dialog that has taken the pointer so nothing else can be clicked. All of
/// those change what has the *focus* and what is on top; none of them changes
/// who the server hands a passively grabbed key to.
///
/// The one arrangement that does take the key is an active **keyboard** grab —
/// what a menu holds while it is open — and that is recorded by the test above
/// rather than fixed, because on X11 it cannot be.
///
/// **What this guards is an assumption about the platform, not a branch in
/// this crate.** Nothing here looks at the focus, so no sabotage of this code
/// makes it fail; what would make it fail is an X server that stops handing
/// passively grabbed keys to the grabbing client when something owns the
/// screen. That is worth a test precisely because it is the sort of thing
/// nobody would notice had changed.
#[test]
fn the_panic_key_arrives_under_a_fullscreen_modal_window() {
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

    let (conn, screen_num) = esper_display_lock::connect().expect("a connection");
    let screen = conn.setup().roots.get(screen_num).expect("a screen");
    let (root, depth, visual) = (screen.root, screen.root_depth, screen.root_visual);
    let (w, h) = (screen.width_in_pixels, screen.height_in_pixels);
    let black = screen.black_pixel;

    // Fullscreen, override-redirect, raised and focused: the most a program can
    // do to own the screen without asking a window manager for permission.
    let window = conn.generate_id().expect("an id");
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
            .background_pixel(black)
            .override_redirect(1)
            .event_mask(EventMask::KEY_PRESS | EventMask::KEY_RELEASE),
    )
    .expect("create")
    .check()
    .expect("created");
    conn.map_window(window)
        .expect("map")
        .check()
        .expect("mapped");
    conn.configure_window(
        window,
        &ConfigureWindowAux::new().stack_mode(StackMode::ABOVE),
    )
    .expect("raise")
    .check()
    .expect("raised");
    conn.set_input_focus(InputFocus::PARENT, window, x11rb::CURRENT_TIME)
        .expect("focus")
        .check()
        .expect("focused");
    // And modal: it holds the pointer, which is what a dialog does. Pointer,
    // not keyboard — a program that grabs the keyboard takes the key, and that
    // is the recorded limitation rather than this case.
    let pointer = conn
        .grab_pointer(
            false,
            window,
            EventMask::BUTTON_PRESS,
            GrabMode::ASYNC,
            GrabMode::ASYNC,
            x11rb::NONE,
            x11rb::NONE,
            x11rb::CURRENT_TIME,
        )
        .expect("ask for the pointer")
        .reply()
        .expect("a reply");
    println!("pointer grab status: {:?}", pointer.status);
    conn.flush().expect("flush");
    std::thread::sleep(Duration::from_millis(100));

    hotkey(&conn, root, 0x29);
    let seen = presses(&mut p);

    let _ = conn.ungrab_pointer(x11rb::CURRENT_TIME);
    let _ = conn.destroy_window(window);
    let _ = conn.flush();

    assert!(
        seen.contains(&HostEvent::Panic),
        "the panic key did not reach the host under a fullscreen modal window: {seen:?}"
    );
}

/// **A display whose outputs are all virtual**, or a skip that says why.
///
/// `may_disturb_the_display` means "you may draw on this and press keys at
/// it". Turning somebody's monitor off is not that: if the restore fails they
/// are looking at a dark screen with no way to see what they type. So this
/// asks the outputs whether they are real — a monitor reports its millimetres
/// from its EDID, and a virtual one reports `0mm x 0mm`.
fn only_a_virtual_display() -> bool {
    use x11rb::protocol::randr::ConnectionExt as _;
    if !may_disturb_the_display() {
        return false;
    }
    let Ok((conn, screen_num)) = esper_display_lock::connect() else {
        return false;
    };
    let Some(root) = conn.setup().roots.get(screen_num).map(|s| s.root) else {
        return false;
    };
    let Ok(resources) = conn
        .randr_get_screen_resources_current(root)
        .map_err(|e| e.to_string())
        .and_then(|c| c.reply().map_err(|e| format!("{e:?}")))
    else {
        println!("skipped: this display has no RandR");
        return false;
    };
    for output in &resources.outputs {
        let Ok(info) = conn
            .randr_get_output_info(*output, resources.config_timestamp)
            .map_err(|e| e.to_string())
            .and_then(|c| c.reply().map_err(|e| format!("{e:?}")))
        else {
            continue;
        };
        if info.mm_width != 0 || info.mm_height != 0 {
            println!(
                "skipped: {} reports {}mm x {}mm, so it is a real monitor and this test will \
                 not turn it off",
                String::from_utf8_lossy(&info.name),
                info.mm_width,
                info.mm_height
            );
            return false;
        }
    }
    true
}

/// **The host's screen goes dark and the session does not.**
///
/// Which is the whole feature: a privacy screen that blanked the remote view
/// as well is not a privacy screen, and the obvious implementation — a window
/// over the screen — does exactly that, because X11 capture reads the root.
/// Turning the output off instead leaves the framebuffer allocated, and this
/// is that claim, measured through the same read-back the indicator uses.
#[test]
fn blanking_the_screen_leaves_the_framebuffer_readable() {
    if !only_a_virtual_display() {
        return;
    }
    let _display = exclusive();
    let Some(mut p) = open() else { return };

    // Something recognisable on the screen, so "still readable" is not
    // satisfied by a screen that was black to begin with.
    p.show(&watching(true)).expect("shown");
    assert!(
        p.verify().expect("a verdict").is_on_screen(),
        "the frame has to be up first, so there is something to read back"
    );

    // **The plan comes first, and changes nothing.** That ordering is the
    // whole safety story: the undo is registered with something that outlives
    // this process before a single screen goes dark.
    let plan = match p.blanking_plan() {
        Ok(plan) => plan,
        Err(e) => {
            println!("skipped: this display would not blank ({e})");
            return;
        }
    };
    println!("plan: {} with {} undo(s)", plan.what, plan.undo.len());
    assert!(
        !plan.undo.is_empty(),
        "a blanking with no undo is the thing that must never exist"
    );
    assert!(
        p.verify().expect("a verdict").is_on_screen(),
        "planning must change nothing at all"
    );
    p.blank(&plan).expect("blanked");

    // **The session survives it.** The indicator is drawn into the same
    // framebuffer the capture path reads, so confirming it is confirming that
    // the picture is still there.
    let while_blank = p.verify().expect("a verdict");
    assert!(
        while_blank.is_on_screen(),
        "the framebuffer stopped being readable while the screen was blanked, which would \
         make this a broken session rather than a privacy screen: {while_blank:?}"
    );

    p.unblank().expect("unblanked");
    assert!(
        p.verify().expect("a verdict").is_on_screen(),
        "and it is still readable afterwards"
    );
    p.hide().expect("hidden");
}

/// Blanking twice is refused rather than losing the first undo.
///
/// The second call would otherwise record CRTCs that are *already off* as the
/// state to return to — so putting it back would put back the blank, and the
/// screen would never come on again.
#[test]
fn a_screen_that_is_already_blanked_is_not_blanked_again() {
    if !only_a_virtual_display() {
        return;
    }
    let _display = exclusive();
    let Some(mut p) = open() else { return };
    let Ok(plan) = p.blanking_plan() else {
        println!("skipped: this display would not blank");
        return;
    };
    p.blank(&plan).expect("blanked");
    let again = p.blank(&plan);
    p.unblank().expect("unblanked");
    assert!(
        again.is_err(),
        "blanking an already blank screen would record the blank as the state to restore"
    );
}

/// **A recorded session looks different from a watched one, on the screen.**
///
/// The colour is the whole mechanism: a person glancing at their own machine
/// has to be able to tell "somebody is watching" from "somebody is keeping a
/// copy", without reading anything. And it takes precedence over the other
/// three, because by the time somebody learns their screen was recorded the
/// choice about it has already been made for them.
#[test]
fn a_recorded_session_shows_a_colour_of_its_own() {
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
    let controlled = pixel_at(1, 1).expect("a pixel");
    p.show(&esper_presence::Watching {
        peer: "somebody".to_owned(),
        controlling: true,
        recording: true,
        paused: false,
    })
    .expect("shown recording");
    let recorded = pixel_at(1, 1).expect("a pixel");

    assert_ne!(
        controlled, recorded,
        "a recorded session has to look different from a controlled one"
    );
    // And the screen agrees it is up in the new colour, which is the check
    // that the frame was actually rebuilt rather than left saying the old
    // thing.
    assert!(
        p.verify().expect("a verdict").is_on_screen(),
        "the recording colour has to confirm as itself"
    );

    // Recording outranks a pause: a held session that is being recorded is
    // still being recorded.
    p.show(&esper_presence::Watching {
        peer: "somebody".to_owned(),
        controlling: true,
        recording: true,
        paused: true,
    })
    .expect("shown paused and recording");
    assert_eq!(
        pixel_at(1, 1).expect("a pixel"),
        recorded,
        "a pause must not hide that a recording is being made"
    );
    p.hide().expect("hidden");
}
