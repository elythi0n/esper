#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Tests that need a real X server, and one that needs its keyboard.
//!
//! # The table is checked, not reviewed
//!
//! A wrong entry in the HID table is a key that types the wrong character —
//! the exact bug that sending physical positions instead of characters exists
//! to prevent, reintroduced one row at a time. Reading it over proves nothing:
//! `KEY_A` is 30 and `KEY_S` is 31, and a reader who knows that is exactly as
//! likely to accept a typo as to catch it.
//!
//! So the server is asked. For every usage in the table, the keycode this
//! backend computes is looked up in the server's own keyboard mapping, and the
//! keysym it produces is compared against what that physical key produces on a
//! Latin layout. The letters and digits and function keys are layout-
//! independent; the punctuation is not, and is reported rather than asserted.
//!
//! # And it does not type into anybody's session
//!
//! The injection test focuses a window of its own and **verifies that it has
//! the focus** before sending a single event. If it does not, it skips. A test
//! that typed into the desktop of whoever ran it would be worse than no test.

use std::time::Duration;

use esper_input::{InputSink, Point, hid};
use esper_input_x11::X11Injector;
use esper_proto::{Button, KeyEvent, PointerButton, Scroll};
use x11rb::connection::Connection as _;
use x11rb::protocol::Event;
use x11rb::protocol::xproto::{
    ConnectionExt as _, CreateWindowAux, EventMask, InputFocus, WindowClass,
};

/// **A display has one keyboard focus and one pointer**, and every test that
/// injects moves both. Run in parallel — which is the default, and what CI
/// does — one test's `set_input_focus` sends another's keystrokes to the wrong
/// window, and one test's pointer warp moves another's pointer out from under
/// it. Both happened, in CI, after passing serially on the developer's
/// machine.
///
/// Across processes, because a `static Mutex` here would serialise these tests
/// and do nothing about the presence suite drawing a frame over the screen
/// from another binary at the same time.
fn exclusive() -> esper_display_lock::DisplayLock {
    esper_display_lock::DisplayLock::take()
}

/// `Some(injector)` when there is a display with XTEST.
fn open() -> Option<X11Injector> {
    if std::env::var_os("DISPLAY").is_none() {
        println!("skipped: no DISPLAY. CI runs these under Xvfb.");
        return None;
    }
    match X11Injector::open() {
        Ok(i) => Some(i),
        Err(e) => {
            println!("skipped: DISPLAY is set but injection is unavailable ({e})");
            None
        }
    }
}

/// What a US layout produces at each physical position, for the keys where
/// that is the same on every Latin layout.
///
/// Punctuation is deliberately absent: on a Finnish or German layout the key
/// beside `p` is not `[`, and asserting otherwise would fail on a correct
/// system. The letters, the digits, the function keys and the navigation
/// block are the same everywhere, and they are most of the table.
fn expected_keysyms() -> Vec<(u16, Vec<u32>, &'static str)> {
    let mut out = Vec::new();
    // Letters: usage 0x04 is 'a' and they run in alphabetical order.
    for i in 0..26u16 {
        let name = char::from(b'a' + u8::try_from(i).unwrap_or(0));
        out.push((
            0x04 + i,
            vec![0x0061 + u32::from(i)],
            Box::leak(name.to_string().into_boxed_str()) as &'static str,
        ));
    }
    // Digits: usage 0x1E is '1' and 0x27 is '0'.
    for i in 0..9u16 {
        out.push((
            0x1E + i,
            vec![0x0031 + u32::from(i)],
            Box::leak(format!("{}", i + 1).into_boxed_str()) as &'static str,
        ));
    }
    out.push((0x27, vec![0x0030], "0"));
    // The keys with names of their own.
    out.extend([
        (0x28, vec![0xFF0D], "Return"),
        (0x29, vec![0xFF1B], "Escape"),
        (0x2A, vec![0xFF08], "BackSpace"),
        (0x2B, vec![0xFF09], "Tab"),
        (0x2C, vec![0x0020], "space"),
        (0x39, vec![0xFFE5], "Caps_Lock"),
        (0x46, vec![0xFF61], "Print"),
        (0x47, vec![0xFF14], "Scroll_Lock"),
        (0x49, vec![0xFF63], "Insert"),
        (0x4A, vec![0xFF50], "Home"),
        (0x4B, vec![0xFF55], "Page_Up"),
        (0x4C, vec![0xFFFF], "Delete"),
        (0x4D, vec![0xFF57], "End"),
        (0x4E, vec![0xFF56], "Page_Down"),
        (0x4F, vec![0xFF53], "Right"),
        (0x50, vec![0xFF51], "Left"),
        (0x51, vec![0xFF54], "Down"),
        (0x52, vec![0xFF52], "Up"),
        (0x53, vec![0xFF7F], "Num_Lock"),
        (0x58, vec![0xFF8D], "KP_Enter"),
        (0x65, vec![0xFF67], "Menu"),
        // The modifiers, which are the ones that stick if they are wrong.
        (0xE0, vec![0xFFE3], "Control_L"),
        (0xE1, vec![0xFFE1], "Shift_L"),
        (0xE2, vec![0xFFE9], "Alt_L"),
        (0xE3, vec![0xFFEB], "Super_L"),
        (0xE4, vec![0xFFE4], "Control_R"),
        (0xE5, vec![0xFFE2], "Shift_R"),
        // **Right Alt is not always Alt.** On most layouts outside the United
        // States it is AltGr — `ISO_Level3_Shift` — which is how a Finnish
        // keyboard types `@` and `£`. Both are the same physical key and the
        // same evdev code; what it *means* is the host's layout, which is the
        // whole reason positions travel and characters do not. Found by this
        // test on a Finnish keyboard, where the table was right and this list
        // was wrong.
        (0xE6, vec![0xFFEA, 0xFE03], "Alt_R or AltGr"),
        (0xE7, vec![0xFFEC], "Super_R"),
    ]);
    // **The media keys**, which are on most keyboards made this century and
    // are the part of the table nothing else exercises.
    out.extend([
        (0x7F, vec![0x1008_FF12], "XF86AudioMute"),
        (0x80, vec![0x1008_FF13], "XF86AudioRaiseVolume"),
        (0x81, vec![0x1008_FF11], "XF86AudioLowerVolume"),
    ]);
    // Function keys F1 to F12.
    for i in 0..12u16 {
        out.push((
            0x3A + i,
            vec![0xFFBE + u32::from(i)],
            Box::leak(format!("F{}", i + 1).into_boxed_str()) as &'static str,
        ));
    }
    out
}

/// Every keysym the server has for a keycode.
fn keysyms_for(conn: &x11rb::rust_connection::RustConnection, code: u8) -> Vec<u32> {
    let setup = conn.setup();
    let mapping = conn
        .get_keyboard_mapping(setup.min_keycode, setup.max_keycode - setup.min_keycode + 1)
        .expect("mapping request")
        .reply()
        .expect("mapping reply");
    let per = usize::from(mapping.keysyms_per_keycode).max(1);
    let index = usize::from(code.saturating_sub(setup.min_keycode));
    let start = index.saturating_mul(per);
    mapping
        .keysyms
        .get(start..start.saturating_add(per))
        .unwrap_or(&[])
        .to_vec()
}

/// **The table, against the server's own idea of its keyboard.**
///
/// This is the test that makes the HID table trustworthy. Everything else here
/// checks that injection works; this checks that it injects the right key.
#[test]
fn every_known_key_maps_to_the_symbol_that_physical_key_produces() {
    if open().is_none() {
        return;
    }
    let (conn, _) = x11rb::connect(None).expect("a connection");
    let mut checked = 0usize;
    let mut wrong: Vec<String> = Vec::new();

    for (usage, acceptable, name) in expected_keysyms() {
        let Some(evdev) = hid::evdev_for_usage(usage) else {
            wrong.push(format!("{usage:#06x} ({name}) is missing from the table"));
            continue;
        };
        let code = u8::try_from(evdev.0 + 8).expect("a keycode");
        let syms = keysyms_for(&conn, code);
        if syms.iter().all(|s| *s == 0) {
            // A keyboard with no key at that position — a laptop without a
            // keypad, say. Not a fault in the table.
            println!("  {name}: no key at keycode {code} on this keyboard");
            continue;
        }
        if acceptable.iter().any(|want| syms.contains(want)) {
            checked += 1;
        } else {
            wrong.push(format!(
                "{usage:#06x} ({name}) -> evdev {} -> keycode {code}, which produces {:04x?} and \
                 none of {:04x?}",
                evdev.0, syms, acceptable
            ));
        }
    }

    println!("{checked} keys map to the symbol their physical position produces");

    // **F13 to F24 are not F13 to F24 here, and that is the platform.**
    //
    // Linux calls them `KEY_F13`..`KEY_F24` (evdev 183..194), and X's own
    // evdev keymap gives those keycodes the tool and launch symbols instead —
    // keycode 191 is `XF86Tools`, not `F13`. Nothing in this table is wrong:
    // the position is the position, and what a host makes of it is the host's
    // keymap's business, which is the whole point of sending positions.
    //
    // Reported rather than asserted, because which symbol a keymap puts there
    // is not this project's to promise. It is here so the next person to
    // wonder why F13 types something odd finds the answer.
    for (usage, name) in [(0x68u16, "F13"), (0x73, "F24")] {
        if let Some(evdev) = hid::evdev_for_usage(usage) {
            let code = u8::try_from(evdev.0 + 8).expect("a keycode");
            let syms = keysyms_for(&conn, code);
            println!("  {name} (usage {usage:#04x}) is keycode {code}, which is {syms:04x?}");
        }
    }
    assert!(
        wrong.is_empty(),
        "the key table disagrees with this keyboard:\n  {}",
        wrong.join("\n  ")
    );
    assert!(
        checked > 60,
        "only {checked} keys were checked, which is too few to trust the table"
    );
}

/// Every usage in the table produces a keycode a server can carry.
#[test]
fn every_usage_in_the_table_fits_a_keycode() {
    for usage in hid::usages() {
        let evdev = hid::evdev_for_usage(usage).expect("in the table");
        assert!(
            u8::try_from(evdev.0 + 8).is_ok(),
            "{usage:#06x} maps to evdev {}, which is past a keycode",
            evdev.0
        );
    }
}

/// Whether this run may do visible things to the display it can see.
///
/// **Off unless asked for.** Injecting takes the input focus and moves the
/// pointer, and on a real desktop that is somebody's session: a test that
/// stole the focus mid-sentence and moved the mouse whenever they ran the
/// suite would be a test they turn off. CI sets this alongside `DISPLAY`,
/// where the display is an `Xvfb` nobody is looking at.
///
/// One switch for every test in the product that disturbs a display, so
/// there is one thing to know rather than one per crate.
///
/// The test that matters most — the key table against the server's own
/// keyboard — needs none of this and always runs, because it only *reads*.
fn may_disturb_the_display() -> bool {
    if std::env::var_os("ESPER_DISPLAY_TESTS").is_none() {
        println!(
            "skipped: set ESPER_DISPLAY_TESTS=1 to allow injecting into this display. \
             It takes the input focus and moves the pointer, so it is off unless asked for."
        );
        return false;
    }
    true
}

/// A window of our own, focused, so injected keys go nowhere near the
/// desktop.
struct Focused {
    conn: x11rb::rust_connection::RustConnection,
    window: x11rb::protocol::xproto::Window,
}

impl Focused {
    /// `None` when the focus could not be taken, which is a skip rather than a
    /// failure: a window manager is entitled to refuse.
    fn take() -> Option<Self> {
        let (conn, screen_num) = x11rb::connect(None).ok()?;
        let screen = conn.setup().roots.get(screen_num)?;
        let (root, depth, visual) = (screen.root, screen.root_depth, screen.root_visual);
        let window = conn.generate_id().ok()?;
        conn.create_window(
            depth,
            window,
            root,
            0,
            0,
            64,
            64,
            0,
            WindowClass::INPUT_OUTPUT,
            visual,
            &CreateWindowAux::new()
                .background_pixel(screen.black_pixel)
                .override_redirect(1)
                .event_mask(
                    EventMask::KEY_PRESS
                        | EventMask::KEY_RELEASE
                        | EventMask::BUTTON_PRESS
                        | EventMask::BUTTON_RELEASE,
                ),
        )
        .ok()?
        .check()
        .ok()?;
        conn.map_window(window).ok()?.check().ok()?;
        conn.set_input_focus(InputFocus::PARENT, window, x11rb::CURRENT_TIME)
            .ok()?
            .check()
            .ok()?;
        // **The focus is not enough.** Keyboard events follow the focus and
        // **pointer events follow the pointer**, so a button pressed while the
        // cursor is somewhere else arrives at whatever is under the cursor and
        // not here. Found exactly that way: the key test passed and the button
        // tests saw nothing at all.
        conn.warp_pointer(x11rb::NONE, window, 0, 0, 0, 0, 32, 32)
            .ok()?
            .check()
            .ok()?;
        conn.flush().ok()?;

        // **Verified, not assumed.** If this window does not have the focus,
        // injected keys go to whatever does — which on a real desktop is
        // somebody's editor.
        for _ in 0..40 {
            let focus = conn.get_input_focus().ok()?.reply().ok()?;
            if focus.focus == window {
                return Some(Self { conn, window });
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        let _ = conn.destroy_window(window);
        let _ = conn.flush();
        println!("skipped: could not take the input focus, so nothing was injected");
        None
    }

    /// Everything that has arrived, as (keycode, pressed) and button events.
    fn drain(&self) -> Vec<Event> {
        let mut out = Vec::new();
        let _ = self.conn.flush();
        while let Ok(Some(event)) = self.conn.poll_for_event() {
            out.push(event);
        }
        out
    }
}

impl Drop for Focused {
    fn drop(&mut self) {
        let _ = self.conn.destroy_window(self.window);
        let _ = self.conn.flush();
    }
}

/// **Injection reaches a window, as the key that was asked for.**
#[test]
fn a_key_arrives_at_a_focused_window_as_the_key_it_was_sent_as() {
    let _display = exclusive();
    if !may_disturb_the_display() {
        return;
    }
    let Some(mut injector) = open() else { return };
    let Some(focused) = Focused::take() else {
        return;
    };
    let _ = focused.drain();

    // 'a', 'z' and Left Control: two letters far apart in the keycode space,
    // and the modifier whose sticking is the worst outcome here.
    for usage in [0x04u16, 0x1D, 0xE0] {
        let expected = u8::try_from(hid::evdev_for_usage(usage).expect("in the table").0 + 8)
            .expect("a keycode");
        injector
            .key(KeyEvent {
                usage,
                pressed: true,
            })
            .expect("press");
        injector
            .key(KeyEvent {
                usage,
                pressed: false,
            })
            .expect("release");
        std::thread::sleep(Duration::from_millis(60));

        let events = focused.drain();
        let presses: Vec<u8> = events
            .iter()
            .filter_map(|e| match e {
                Event::KeyPress(k) => Some(k.detail),
                _ => None,
            })
            .collect();
        let releases: Vec<u8> = events
            .iter()
            .filter_map(|e| match e {
                Event::KeyRelease(k) => Some(k.detail),
                _ => None,
            })
            .collect();
        assert!(
            presses.contains(&expected),
            "{usage:#06x} should have arrived as keycode {expected}, got presses {presses:?}"
        );
        assert!(
            releases.contains(&expected),
            "{usage:#06x} was pressed and not released: releases {releases:?}"
        );
    }
    assert!(
        injector.held().is_empty(),
        "everything pressed was released, so nothing should be held"
    );
}

/// **What is held is let go of.** The failure this prevents is a stuck Ctrl on
/// somebody else's machine, which they cannot diagnose and cannot fix.
#[test]
fn release_all_releases_what_was_left_down() {
    let _display = exclusive();
    if !may_disturb_the_display() {
        return;
    }
    let Some(mut injector) = open() else { return };
    let Some(focused) = Focused::take() else {
        return;
    };
    let _ = focused.drain();

    // Ctrl and Shift down, and left mouse button down, and nothing released.
    for usage in [0xE0u16, 0xE1] {
        injector
            .key(KeyEvent {
                usage,
                pressed: true,
            })
            .expect("press");
    }
    injector
        .button(PointerButton {
            button: Button::Left,
            pressed: true,
        })
        .expect("button down");
    assert_eq!(injector.held().count(), 3, "three things are held");

    std::thread::sleep(Duration::from_millis(60));
    let _ = focused.drain();

    injector.release_all().expect("release everything");
    assert!(injector.held().is_empty(), "and now nothing is");
    std::thread::sleep(Duration::from_millis(80));

    let events = focused.drain();
    let released: Vec<u8> = events
        .iter()
        .filter_map(|e| match e {
            Event::KeyRelease(k) => Some(k.detail),
            _ => None,
        })
        .collect();
    for usage in [0xE0u16, 0xE1] {
        let code = u8::try_from(hid::evdev_for_usage(usage).expect("in the table").0 + 8)
            .expect("a keycode");
        assert!(
            released.contains(&code),
            "{usage:#06x} (keycode {code}) was not released: {released:?}"
        );
    }
    let buttons: Vec<u8> = events
        .iter()
        .filter_map(|e| match e {
            Event::ButtonRelease(b) => Some(b.detail),
            _ => None,
        })
        .collect();
    assert!(
        buttons.contains(&1),
        "the left button was not released: {buttons:?}"
    );
}

/// A usage the page defines and this platform has no key for is refused by
/// name, and does not leave anything held.
#[test]
fn a_key_this_platform_does_not_have_is_refused() {
    let _display = exclusive();
    let Some(mut injector) = open() else { return };
    let e = injector
        .key(KeyEvent {
            usage: 0x82,
            pressed: true,
        })
        .expect_err("locking caps lock has no Linux code");
    assert!(
        matches!(e, esper_input::InputError::NoSuchKey { usage: 0x82 }),
        "{e:?}"
    );
    assert!(
        injector.held().is_empty(),
        "a refused press must not be recorded as held"
    );
}

/// Scrolling on X11 is a wheel button, and a scroll of several notches is
/// several clicks — which a window has to see as separate events, because
/// applications count them.
#[test]
fn a_scroll_arrives_as_one_wheel_click_per_notch() {
    let _display = exclusive();
    if !may_disturb_the_display() {
        return;
    }
    let Some(mut injector) = open() else { return };
    let Some(focused) = Focused::take() else {
        return;
    };
    let _ = focused.drain();

    injector
        .scroll(Scroll {
            dx_notches: 0,
            dy_notches: 3,
            dx_pixels: None,
            dy_pixels: None,
        })
        .expect("scroll");
    std::thread::sleep(Duration::from_millis(80));

    let events = focused.drain();
    let clicks = events
        .iter()
        .filter(|e| matches!(e, Event::ButtonPress(b) if b.detail == 5))
        .count();
    assert_eq!(clicks, 3, "three notches are three clicks of button 5");
    let ups = events
        .iter()
        .filter(|e| matches!(e, Event::ButtonRelease(b) if b.detail == 5))
        .count();
    assert_eq!(ups, 3, "and each one is released");
}

/// The pointer goes where it is told, in desktop coordinates.
#[test]
fn the_pointer_moves_to_an_absolute_position() {
    let _display = exclusive();
    if !may_disturb_the_display() {
        return;
    }
    let Some(mut injector) = open() else { return };
    let (conn, screen_num) = x11rb::connect(None).expect("a connection");
    let screen = conn.setup().roots.get(screen_num).expect("a screen");
    let root = screen.root;
    let (w, h) = (screen.width_in_pixels, screen.height_in_pixels);

    // Somewhere unambiguous and inside any screen.
    let targets = [(64i32, 48i32), (i32::from(w) / 2, i32::from(h) / 2)];
    for (x, y) in targets {
        injector.pointer(Point { x, y }).expect("move");
        let mut landed = (i32::MIN, i32::MIN);
        for _ in 0..20 {
            let p = conn
                .query_pointer(root)
                .expect("query")
                .reply()
                .expect("reply");
            landed = (i32::from(p.root_x), i32::from(p.root_y));
            if landed == (x, y) {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(landed, (x, y), "the pointer should be at {x},{y}");
    }
}
