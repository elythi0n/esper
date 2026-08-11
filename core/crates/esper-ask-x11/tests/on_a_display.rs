#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
// The crate under test is Linux-only; so is its test.
#![cfg(target_os = "linux")]

//! Tests that need a real X server, and that try to get a yes out of a prompt
//! nobody answered.
//!
//! # The point of these
//!
//! A consent prompt is a guarantee rather than a feature. So the interesting
//! question is not whether a click on Allow returns yes — it is whether
//! anything *else* can: a click that was on its way before the prompt existed,
//! a key held down from before it appeared, a click somewhere else on the
//! screen, a window put over the top of it. Each of those has a test here that
//! tries it.

use std::time::Duration;

use esper_ask_x11::Answer;
use esper_dialog::Question;
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{ConnectionExt as _, Window};
use x11rb::protocol::xtest::ConnectionExt as _;

/// Whether this run may do visible things to the display it can see.
///
/// **Off unless asked for.** These draw a prompt over the whole middle of the
/// screen, grab the keyboard and synthesise clicks. On a real desktop that is
/// somebody's session. CI sets this, where the display is an `Xvfb` nobody is
/// looking at.
fn may_disturb_the_display() -> bool {
    if std::env::var_os("ESPER_DISPLAY_TESTS").is_none() {
        println!(
            "skipped: set ESPER_DISPLAY_TESTS=1 to allow drawing a prompt over this display \
             and clicking on it. It is off unless asked for."
        );
        return false;
    }
    true
}

/// One display, one keyboard, one pointer — and one lock, shared with every
/// other suite in the workspace that touches a display.
fn exclusive() -> esper_display_lock::DisplayLock {
    esper_display_lock::DisplayLock::take()
}

fn question() -> Question {
    Question {
        asking: esper_dialog::Asking::ANewSession,
        kind: "screen".to_owned(),
        name: "elythion".to_owned(),
        fingerprint: "giraffe tapestry geyser rifle jersey domain razor oboe".to_owned(),
        granted: vec!["view".to_owned(), "control".to_owned()],
        seconds: 45,
    }
}

/// A second connection, for the side of the test that plays the attacker.
fn beside() -> (impl Connection, Window) {
    let (conn, num) = esper_display_lock::connect().expect("a display");
    let root = conn.setup().roots.get(num).expect("a screen").root;
    (conn, root)
}

/// Where the prompt is on the screen, found by looking for it.
///
/// Its own function because finding it is also a check: a window of exactly
/// the prompt's width, which nothing else on a test display has.
fn find_the_prompt(conn: &impl Connection, root: Window) -> Option<(i16, i16, u16, u16)> {
    let children = conn.query_tree(root).ok()?.reply().ok()?.children;
    for window in children.into_iter().rev() {
        let Ok(geometry) = conn.get_geometry(window) else {
            continue;
        };
        let Ok(g) = geometry.reply() else { continue };
        if usize::from(g.width) == esper_dialog::the_width() {
            return Some((g.x, g.y, g.width, g.height));
        }
    }
    None
}

/// Wait for the prompt to be on the screen, or give up.
fn wait_for_the_prompt(conn: &impl Connection, root: Window) -> (i16, i16, u16, u16) {
    for _ in 0..200 {
        if let Some(found) = find_the_prompt(conn, root) {
            return found;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    panic!("the prompt never appeared on the display");
}

/// Click at a point on the screen.
fn click(conn: &impl Connection, root: Window, x: i16, y: i16) {
    conn.warp_pointer(x11rb::NONE, root, 0, 0, 0, 0, x, y)
        .expect("warp")
        .check()
        .expect("warp");
    conn.flush().expect("flush");
    for down in [true, false] {
        conn.xtest_fake_input(if down { 4 } else { 5 }, 1, 0, root, x, y, 0)
            .expect("fake button")
            .check()
            .expect("fake button");
    }
    conn.flush().expect("flush");
}

/// The keycode that produces `keysym` with no modifiers on this layout.
fn keycode_for(conn: &impl Connection, keysym: u32) -> u8 {
    let setup = conn.setup();
    let first = setup.min_keycode;
    let count = setup.max_keycode - first + 1;
    let reply = conn
        .get_keyboard_mapping(first, count)
        .expect("mapping")
        .reply()
        .expect("mapping");
    let per = usize::from(reply.keysyms_per_keycode);
    for (i, chunk) in reply.keysyms.chunks(per.max(1)).enumerate() {
        if chunk.first() == Some(&keysym) {
            return first + u8::try_from(i).expect("a keycode fits in a byte");
        }
    }
    panic!("this layout has no key for keysym {keysym:#x}");
}

/// Press or release a key through the server's own test extension.
fn key(conn: &impl Connection, root: Window, code: u8, down: bool) {
    conn.xtest_fake_input(if down { 2 } else { 3 }, code, 0, root, 0, 0, 0)
        .expect("fake key")
        .check()
        .expect("fake key");
    conn.flush().expect("flush");
}

/// Ask in a thread, so the test can be the person answering.
fn asking(within: Duration) -> std::thread::JoinHandle<Result<Answer, String>> {
    std::thread::spawn(move || esper_ask_x11::ask(&question(), within))
}

/// The first thing to establish: a display this can be asked on is recognised.
#[test]
fn a_display_is_somewhere_a_question_can_be_asked() {
    if !may_disturb_the_display() {
        return;
    }
    let _lock = exclusive();
    assert!(
        esper_ask_x11::can_ask(),
        "there is a display and can_ask said no"
    );
}

/// Nobody answering is a refusal, and it is the only thing a timeout can be.
#[test]
fn nobody_answering_is_not_a_yes() {
    if !may_disturb_the_display() {
        return;
    }
    let _lock = exclusive();
    let answer = esper_ask_x11::ask(&question(), Duration::from_millis(900)).expect("a display");
    assert_eq!(answer, Answer::Silence);
}

/// A click on Allow is a yes, and a click on Refuse is a no.
///
/// The two together, because a prompt where every click means the same thing
/// would pass either half on its own.
#[test]
fn the_two_answers_are_where_the_layout_says_they_are() {
    if !may_disturb_the_display() {
        return;
    }
    let _lock = exclusive();
    let (conn, root) = beside();
    let painted = esper_dialog::paint(&question());

    for (target, want) in [(painted.allow, Answer::Yes), (painted.refuse, Answer::No)] {
        let handle = asking(Duration::from_secs(10));
        let (wx, wy, _, _) = wait_for_the_prompt(&conn, root);
        // After settling, because before it nothing counts — which is the
        // next test.
        std::thread::sleep(esper_ask_x11::SETTLING + Duration::from_millis(200));
        click(
            &conn,
            root,
            wx + i16::try_from(target.x + target.w / 2).unwrap(),
            wy + i16::try_from(target.y + target.h / 2).unwrap(),
        );
        let answer = handle
            .join()
            .expect("the asking thread")
            .expect("a display");
        assert_eq!(answer, want, "clicking {target:?}");
    }
}

/// A click on its way when the prompt appeared is not an answer to it.
///
/// The same rule as the terminal prompt, where anything already typed is
/// thrown away first: somebody who was clicking at something else has not
/// consented to anything.
#[test]
fn a_click_that_lands_before_the_prompt_has_settled_does_not_count() {
    if !may_disturb_the_display() {
        return;
    }
    let _lock = exclusive();
    let (conn, root) = beside();
    let painted = esper_dialog::paint(&question());

    let handle = asking(Duration::from_millis(1_400));
    let (wx, wy, _, _) = wait_for_the_prompt(&conn, root);
    let x = wx + i16::try_from(painted.allow.x + painted.allow.w / 2).unwrap();
    let y = wy + i16::try_from(painted.allow.y + painted.allow.h / 2).unwrap();
    // Immediately, and again well inside the settling window.
    click(&conn, root, x, y);
    std::thread::sleep(Duration::from_millis(100));
    click(&conn, root, x, y);

    let answer = handle
        .join()
        .expect("the asking thread")
        .expect("a display");
    assert_eq!(
        answer,
        Answer::Silence,
        "a click inside the settling window granted a session"
    );
}

/// A key held down from before the prompt appeared is not an answer either.
///
/// Auto-repeat sends a press and a release over and over, so a `y` held from
/// before the prompt existed produces something indistinguishable from
/// somebody typing one — unless the server's own key state is what decides.
#[test]
fn a_key_already_held_when_the_prompt_appeared_does_not_count() {
    if !may_disturb_the_display() {
        return;
    }
    let _lock = exclusive();
    let (conn, root) = beside();
    let y = keycode_for(&conn, 0x0079);

    key(&conn, root, y, true);
    let handle = asking(Duration::from_millis(1_400));
    let answer = handle.join().expect("the asking thread");
    // Released whatever happened: a key left down on a shared display breaks
    // every test after this one.
    key(&conn, root, y, false);

    assert_eq!(
        answer.expect("a display"),
        Answer::Silence,
        "a `y` held from before the prompt granted a session"
    );
}

/// And a `y` typed while the prompt is up is a yes.
///
/// Its own test, because a prompt that ignored every key would pass the one
/// above while being unanswerable on a machine with no mouse.
#[test]
fn a_key_pressed_while_the_prompt_is_up_answers_it() {
    if !may_disturb_the_display() {
        return;
    }
    let _lock = exclusive();
    let (conn, root) = beside();
    let code = keycode_for(&conn, 0x0079);

    let handle = asking(Duration::from_secs(10));
    wait_for_the_prompt(&conn, root);
    std::thread::sleep(esper_ask_x11::SETTLING + Duration::from_millis(200));
    key(&conn, root, code, true);
    key(&conn, root, code, false);

    let answer = handle
        .join()
        .expect("the asking thread")
        .expect("a display");
    assert_eq!(answer, Answer::Yes);
}

/// Escape refuses, so a prompt somebody wants gone goes the safe way.
#[test]
fn escape_refuses() {
    if !may_disturb_the_display() {
        return;
    }
    let _lock = exclusive();
    let (conn, root) = beside();
    let code = keycode_for(&conn, 0xff1b);

    let handle = asking(Duration::from_secs(10));
    wait_for_the_prompt(&conn, root);
    std::thread::sleep(esper_ask_x11::SETTLING + Duration::from_millis(200));
    key(&conn, root, code, true);
    key(&conn, root, code, false);

    let answer = handle
        .join()
        .expect("the asking thread")
        .expect("a display");
    assert_eq!(answer, Answer::No);
}

/// A click anywhere but on the two answers is not an answer.
#[test]
fn a_click_somewhere_else_is_not_an_answer() {
    if !may_disturb_the_display() {
        return;
    }
    let _lock = exclusive();
    let (conn, root) = beside();

    let handle = asking(Duration::from_millis(1_600));
    let (wx, wy, _, _) = wait_for_the_prompt(&conn, root);
    std::thread::sleep(esper_ask_x11::SETTLING + Duration::from_millis(200));
    // The title, and then a long way off the window entirely.
    click(&conn, root, wx + 40, wy + 20);
    click(&conn, root, 2, 2);

    let answer = handle
        .join()
        .expect("the asking thread")
        .expect("a display");
    assert_eq!(
        answer,
        Answer::Silence,
        "a click that was not on an answer answered the question"
    );
}

/// A window put over the prompt does not stay over it.
///
/// Another client can make its own override-redirect window and raise it.
/// What it cannot do is keep it there: the prompt is told it has been
/// obscured, and puts itself back.
#[test]
fn a_window_put_over_the_prompt_does_not_keep_it_covered() {
    if !may_disturb_the_display() {
        return;
    }
    let _lock = exclusive();
    let (conn, root) = beside();

    let handle = asking(Duration::from_secs(10));
    let (wx, wy, ww, wh) = wait_for_the_prompt(&conn, root);

    let over = conn.generate_id().expect("an id");
    conn.create_window(
        x11rb::COPY_DEPTH_FROM_PARENT,
        over,
        root,
        wx,
        wy,
        ww,
        wh,
        0,
        x11rb::protocol::xproto::WindowClass::INPUT_OUTPUT,
        x11rb::COPY_FROM_PARENT,
        &x11rb::protocol::xproto::CreateWindowAux::new()
            .override_redirect(1)
            .background_pixel(0x00ff_00ff),
    )
    .expect("a window")
    .check()
    .expect("a window");
    conn.map_window(over).expect("map").check().expect("map");
    conn.flush().expect("flush");

    // Give the prompt a moment to notice and raise itself, then check the
    // pixels: the middle of the prompt must not be the colour of the window
    // that was put over it.
    std::thread::sleep(Duration::from_millis(600));
    let middle = conn
        .get_image(
            x11rb::protocol::xproto::ImageFormat::Z_PIXMAP,
            root,
            wx + i16::try_from(ww / 2).unwrap(),
            wy + i16::try_from(wh / 2).unwrap(),
            1,
            1,
            !0,
        )
        .expect("read a pixel")
        .reply()
        .expect("read a pixel");
    let seen = u32::from_le_bytes([
        middle.data.first().copied().unwrap_or(0),
        middle.data.get(1).copied().unwrap_or(0),
        middle.data.get(2).copied().unwrap_or(0),
        0,
    ]);

    let _ = conn.destroy_window(over);
    let _ = conn.flush();
    // End the prompt so the thread does not outlive the test.
    let (wx2, wy2, _, _) = (wx, wy, ww, wh);
    let code = keycode_for(&conn, 0xff1b);
    std::thread::sleep(esper_ask_x11::SETTLING);
    key(&conn, root, code, true);
    key(&conn, root, code, false);
    let _ = (wx2, wy2);
    let _ = handle.join().expect("the asking thread");

    assert_ne!(
        seen, 0x00ff_00ff,
        "the window put over the prompt was still covering it"
    );
}
