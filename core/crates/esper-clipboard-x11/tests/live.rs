#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
// The crate under test is Linux-only; so is its test.
#![cfg(target_os = "linux")]

//! Tests that need a real X server, and that exercise the path the
//! conformance suite cannot reach.
//!
//! # Why two clipboards
//!
//! A backend reading back what it just wrote never touches the wire: it owns
//! the selection, so it answers itself. That is correct and it is fast, and it
//! means the conformance suite — run against one clipboard — proves nothing
//! about the part of this backend that is hard: taking ownership, answering
//! somebody else's request, and handing a large paste over one piece at a
//! time.
//!
//! So these open two, and make one paste from the other. That is a real
//! conversation between two X clients through the display server, which is
//! what happens every time a person copies in one program and pastes in
//! another.

use std::time::{Duration, Instant};

use esper_clipboard::{Clipboard, ClipboardKind, conformance};
use esper_clipboard_x11::X11Clipboard;

/// Whether this run may take the clipboard of the display it can see.
///
/// **Off unless asked for.** Copying replaces whatever the person at this
/// machine last copied, which they will discover when they paste and get
/// something else. CI sets this, where the display is an `Xvfb` nobody is
/// using.
fn may_disturb_the_display() -> bool {
    if std::env::var_os("ESPER_DISPLAY_TESTS").is_none() {
        println!(
            "skipped: set ESPER_DISPLAY_TESTS=1 to allow taking this display's clipboard. \
             It is off unless asked for."
        );
        return false;
    }
    true
}

/// Wait for a paste to become visible to another client.
///
/// Ownership is asynchronous: the write returns as soon as the server has
/// taken it, and the other client learns of it on its own event loop. A test
/// that read once and gave up would be flaky for a reason that has nothing to
/// do with what it is testing.
fn read_until(clipboard: &mut X11Clipboard, kind: ClipboardKind, wanted: &[u8]) -> Option<Vec<u8>> {
    let deadline = Instant::now() + PATIENCE;
    let mut last = None;
    while Instant::now() < deadline {
        match clipboard.read(kind) {
            Ok(Some(got)) if got == wanted => return Some(got),
            Ok(other) => last = other,
            Err(e) => println!("  (read said: {e})"),
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    last
}

/// And wait for something to stop being there.
///
/// A separate helper because waiting for an absence by waiting for a presence
/// that never comes means waiting the whole deadline every time — which is a
/// slow suite for no information.
fn read_until_gone(
    clipboard: &mut X11Clipboard,
    kind: ClipboardKind,
    unwanted: &[u8],
) -> Option<Vec<u8>> {
    let deadline = Instant::now() + PATIENCE;
    loop {
        let got = clipboard.read(kind).unwrap_or(None);
        if got.as_deref() != Some(unwanted) {
            return got;
        }
        if Instant::now() >= deadline {
            return got;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// How long to wait for one client to see what another copied.
const PATIENCE: Duration = Duration::from_secs(20);

fn a_png(bytes: usize) -> Vec<u8> {
    let mut v = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    v.extend((0..bytes).map(|i| u8::try_from(i % 251).unwrap_or(0)));
    v
}

#[test]
fn the_x11_backend_conforms() {
    if !may_disturb_the_display() {
        return;
    }
    let _display = esper_display_lock::DisplayLock::take();
    let mut clipboard = match X11Clipboard::open() {
        Ok(c) => c,
        Err(e) => {
            println!("skipped: no clipboard to test ({e})");
            return;
        }
    };
    let report = conformance::check(&mut clipboard);
    assert!(report.passed(), "\n{report}");
    println!("{report}");
}

/// The whole conversation: one client copies, another pastes.
#[test]
fn a_paste_crosses_from_one_client_to_another() {
    if !may_disturb_the_display() {
        return;
    }
    let _display = esper_display_lock::DisplayLock::take();
    let (Ok(mut writer), Ok(mut reader)) = (X11Clipboard::open(), X11Clipboard::open()) else {
        println!("skipped: no clipboard to test");
        return;
    };

    // Text, with the things that break clipboards in it.
    let text = "esper-live ünïcödé 日本語 🎛\ttabbed\nlined\u{0}and after a nul".as_bytes();
    writer.write(ClipboardKind::Text, text).expect("copied");
    assert_eq!(
        read_until(&mut reader, ClipboardKind::Text, text).as_deref(),
        Some(text),
        "the paste did not arrive as it was copied"
    );

    // An image, which is a different target and a different answer.
    let png = a_png(4096);
    writer.write(ClipboardKind::Image, &png).expect("copied");
    assert_eq!(
        read_until(&mut reader, ClipboardKind::Image, &png).as_deref(),
        Some(png.as_slice()),
        "the image did not arrive as it was copied"
    );

    // And asking for a kind the owner does not hold gets nothing rather than
    // the other kind converted into it.
    writer.clear().expect("gave it up");
    let mut fresh = X11Clipboard::open().expect("a third client");
    let only_text = b"only text here";
    fresh.write(ClipboardKind::Text, only_text).expect("copied");
    // Wait for the new owner to be the one answering before concluding
    // anything about what it does not hold.
    assert_eq!(
        read_until(&mut reader, ClipboardKind::Text, only_text).as_deref(),
        Some(only_text.as_slice()),
        "the third client never became the owner"
    );
    let image = reader.read(ClipboardKind::Image).expect("a read");
    assert!(
        image.is_none(),
        "asking for an image got {} bytes of something else",
        image.map_or(0, |v| v.len())
    );
}

/// **The piece-by-piece path, which is the one that is easy to get wrong.**
///
/// Anything past a single property has to be handed over in pieces, with the
/// sender waiting for each one to be taken. Both halves are here, and neither
/// is exercised by anything else in the suite.
#[test]
fn a_large_paste_crosses_one_piece_at_a_time() {
    if !may_disturb_the_display() {
        return;
    }
    let _display = esper_display_lock::DisplayLock::take();
    let (Ok(mut writer), Ok(mut reader)) = (X11Clipboard::open(), X11Clipboard::open()) else {
        println!("skipped: no clipboard to test");
        return;
    };

    // Comfortably past the size of one property, and not a round number, so an
    // off-by-one in the chunking shows up as a difference rather than as a
    // shorter but plausible result.
    let big: Vec<u8> = b"esper-incr-"
        .iter()
        .copied()
        .cycle()
        .take(700_003)
        .collect();
    writer.write(ClipboardKind::Text, &big).expect("copied");

    let started = Instant::now();
    let got = read_until(&mut reader, ClipboardKind::Text, &big);
    let took = started.elapsed();
    match got {
        Some(got) => {
            assert_eq!(got.len(), big.len(), "a piece was lost or repeated");
            assert_eq!(got, big, "the pieces were put back together wrongly");
        }
        None => panic!("a {} byte paste never arrived", big.len()),
    }
    println!("  {} bytes crossed in {took:?}", big.len());

    // And the transfer left nothing behind that stops the next one.
    let after = b"a small one after a large one";
    writer.write(ClipboardKind::Text, after).expect("copied");
    assert_eq!(
        read_until(&mut reader, ClipboardKind::Text, after).as_deref(),
        Some(after.as_slice()),
        "the clipboard stopped working after a large paste"
    );
}

/// A session that ends stops being the owner of anything.
#[test]
fn dropping_a_clipboard_gives_up_what_it_was_answering_for() {
    if !may_disturb_the_display() {
        return;
    }
    let _display = esper_display_lock::DisplayLock::take();
    let mut reader = match X11Clipboard::open() {
        Ok(c) => c,
        Err(e) => {
            println!("skipped: no clipboard to test ({e})");
            return;
        }
    };
    let secret = b"esper-should-not-outlive-the-session";
    {
        let mut writer = X11Clipboard::open().expect("a second client");
        writer.write(ClipboardKind::Text, secret).expect("copied");
        assert_eq!(
            read_until(&mut reader, ClipboardKind::Text, secret).as_deref(),
            Some(secret.as_slice()),
            "the paste was never there to begin with"
        );
    }
    // Two things make this true and only one of them is this crate's: the
    // thread gives the selection up on its way out, and the server drops it
    // anyway when the connection closes. The second is why this test cannot
    // tell whether the first happened — the property is what matters, and it
    // is checked immediately rather than after a wait, because "eventually not
    // on the clipboard" is not the promise.
    let after = reader.read(ClipboardKind::Text).expect("a read");
    assert_ne!(
        after.as_deref(),
        Some(secret.as_slice()),
        "what a finished session copied is still on the clipboard"
    );
    // And it stays gone.
    let later = read_until_gone(&mut reader, ClipboardKind::Text, secret);
    assert_ne!(later.as_deref(), Some(secret.as_slice()));
}
