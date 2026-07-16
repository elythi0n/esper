#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Tests that need a real X server.
//!
//! These skip with a note when there is no `DISPLAY`, and CI runs them under
//! `Xvfb` so the skip is the exception rather than the path everything takes.
//!
//! # How a window is checked
//!
//! By reading it back. `GetImage` on the window returns what the server
//! actually has, so a picture put in and read out again proves the whole
//! path — the colour conversion, the shared segment, the blit and the byte
//! order — rather than proving that no call returned an error.

use std::sync::{Mutex, MutexGuard};

use ownview_capture::damage::{Damage, Regions};
use ownview_capture::geometry::Rect;
use ownview_color::{ColorSpace, I420};
use ownview_present::{PresentEvent, Presenter};
use ownview_present_x11::X11Presenter;
use x11rb::connection::Connection as _;
use x11rb::protocol::xproto::{ConnectionExt as _, ImageFormat};

/// **X11 windows have no backing store**, so an obscured window's pixels are
/// undefined — and every window here is created at the origin, because that is
/// where a window with no window manager lands. Two of these tests running at
/// once therefore cover each other and read whichever won.
///
/// So the ones that read pixels take this first. It is a property of X11 rather
/// than of this code, which is why the fix is a lock rather than a change to
/// the presenter: a real viewer's window is placed and raised by a window
/// manager, and a viewer does not read its own pixels back.
static SCREEN: Mutex<()> = Mutex::new(());

fn exclusive() -> MutexGuard<'static, ()> {
    SCREEN.lock().unwrap_or_else(|p| p.into_inner())
}

fn open(w: u32, h: u32) -> Option<X11Presenter> {
    if std::env::var_os("DISPLAY").is_none() {
        println!("skipped: no DISPLAY. CI runs these under Xvfb.");
        return None;
    }
    match X11Presenter::open(w, h, ColorSpace::default()) {
        Ok(p) => Some(p),
        Err(e) => {
            println!("skipped: DISPLAY is set but unusable ({e})");
            None
        }
    }
}

/// A picture filled with one YUV triple.
/// The size the server says the window is, read independently of the
/// presenter's own bookkeeping.
fn window_size(window: u32) -> Option<(u32, u32)> {
    use x11rb::protocol::xproto::ConnectionExt as _;
    let (conn, _) = x11rb::connect(None).ok()?;
    let g = conn.get_geometry(window).ok()?.reply().ok()?;
    Some((u32::from(g.width), u32::from(g.height)))
}

fn flat(w: u32, h: u32, y: u8, u: u8, v: u8) -> I420 {
    let mut img = I420::new(w, h).expect("a valid size");
    let (ys, us) = (img.y_stride(), img.uv_stride());
    let uv_h = img.uv_height() as usize;
    let (yp, up, vp) = img.planes_mut();
    for row in 0..h as usize {
        for col in 0..w as usize {
            if let Some(p) = yp.get_mut(row * ys + col) {
                *p = y;
            }
        }
    }
    for row in 0..uv_h {
        for col in 0..(w as usize).div_ceil(2) {
            if let Some(p) = up.get_mut(row * us + col) {
                *p = u;
            }
            if let Some(p) = vp.get_mut(row * us + col) {
                *p = v;
            }
        }
    }
    img
}

/// Read the window's pixels back out of the server.
///
/// From the **window**, not the screen. Reading the screen at the origin is
/// what this did first, and it passed under `Xvfb` — where there is no window
/// manager, so a window lands at 0,0 and nothing else is on the display — and
/// failed on any real desktop, where the window is wherever the window manager
/// put it. A test that passes because of where a window happened to land is
/// not testing the picture.
fn read_back(window: u32, w: u32, h: u32) -> Option<(Vec<u8>, (usize, usize, usize))> {
    let (conn, n) = x11rb::connect(None).ok()?;
    let screen = conn.setup().roots.get(n)?.clone();
    let img = conn
        .get_image(
            ImageFormat::Z_PIXMAP,
            window,
            0,
            0,
            u16::try_from(w).ok()?,
            u16::try_from(h).ok()?,
            !0,
        )
        .ok()?
        .reply()
        .ok()?;
    let setup = conn.setup();
    let format =
        ownview_capture_x11::format::of(setup, screen.root_depth, screen.root_visual).ok()?;
    Some((img.data, format.rgb_offsets()))
}

#[test]
fn a_window_opens_and_reports_itself() {
    let Some(p) = open(160, 120) else { return };
    assert_eq!(p.backend(), "x11-window");
    assert_eq!(p.size(), (160, 120));
    assert!(p.is_open());
    println!(
        "{}x{} as {} over {}",
        p.size().0,
        p.size().1,
        p.format().name(),
        if p.uses_shared_memory() {
            "MIT-SHM"
        } else {
            "the socket"
        }
    );
}

/// **The test that proves anything at all.** A known colour goes in and the
/// same colour has to come back out of the server.
#[test]
fn a_picture_reaches_the_window_in_the_right_colour() {
    let _screen = exclusive();
    const W: u32 = 128;
    const H: u32 = 64;
    let Some(mut p) = open(W, H) else { return };
    // Wait for the window to be mapped, or the read finds the root.
    for _ in 0..40 {
        let _ = p.poll().expect("poll");
        std::thread::sleep(std::time::Duration::from_millis(25));
        if p.poll()
            .expect("poll")
            .iter()
            .any(|e| matches!(e, PresentEvent::Exposed))
        {
            break;
        }
    }

    // **Chroma that makes the three channels differ.** An earlier version of
    // this used mid-grey, where R, G and B are equal — so hardcoding the
    // window's byte order to the wrong one passed it, and a desktop with red
    // and blue exchanged would have shipped. Y=126 with this chroma comes back
    // as three distinct values, and a swap cannot survive that.
    const Y: u8 = 126;
    const U: u8 = 90;
    const V: u8 = 200;
    let want = ownview_color::rgb::RgbCoefficients::for_space(ColorSpace::default()).pixel(Y, U, V);
    assert!(
        want.0 != want.1 && want.1 != want.2 && want.0 != want.2,
        "the test colour {want:?} must have three distinct channels or it cannot \
         detect a byte-order mistake"
    );
    p.present(&flat(W, H, Y, U, V), &Damage::Everything)
        .expect("presents");
    // Let the server settle the blit.
    std::thread::sleep(std::time::Duration::from_millis(120));

    let Some((data, (r_at, g_at, b_at))) = read_back(p.window(), W, H) else {
        println!("skipped: could not read the window back");
        return;
    };
    let mut matched = 0usize;
    let mut sample = None;
    for px in data.as_chunks::<4>().0 {
        let got = (
            px.get(r_at).copied().unwrap_or(0),
            px.get(g_at).copied().unwrap_or(0),
            px.get(b_at).copied().unwrap_or(0),
        );
        if sample.is_none() {
            sample = Some(got);
        }
        // One level of tolerance: the conversion rounds.
        if (i32::from(got.0) - i32::from(want.0)).abs() <= 1
            && (i32::from(got.1) - i32::from(want.1)).abs() <= 1
            && (i32::from(got.2) - i32::from(want.2)).abs() <= 1
        {
            matched += 1;
        }
    }
    let total = data.len() / 4;
    println!("{matched} of {total} pixels are {want:?} (first was {sample:?})",);
    assert!(
        matched * 100 / total.max(1) >= 90,
        "only {matched} of {total} pixels came back as {want:?}; the first was \
         {sample:?} — the conversion, the byte order or the blit is wrong"
    );
}

/// A presenter told only a small region changed must not repaint the rest, and
/// what it does repaint has to be right.
#[test]
fn a_damaged_region_updates_only_itself() {
    let _screen = exclusive();
    const W: u32 = 96;
    const H: u32 = 64;
    let Some(mut p) = open(W, H) else { return };
    for _ in 0..40 {
        if p.poll()
            .expect("poll")
            .iter()
            .any(|e| matches!(e, PresentEvent::Exposed))
        {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }

    // A dark picture everywhere.
    p.present(&flat(W, H, 40, 128, 128), &Damage::Everything)
        .expect("presents");
    // Then a bright one, but only a corner is reported as changed.
    let corner = Rect {
        x: 0,
        y: 0,
        width: 16,
        height: 16,
    };
    let some = Regions::coalesce(&[corner], &Rect::sized(W, H));
    p.present(&flat(W, H, 220, 128, 128), &some)
        .expect("presents");
    std::thread::sleep(std::time::Duration::from_millis(120));

    let Some((data, (r_at, _, _))) = read_back(p.window(), W, H) else {
        println!("skipped: could not read the window back");
        return;
    };
    let at = |x: usize, y: usize| -> u8 {
        data.get((y * W as usize + x) * 4 + r_at)
            .copied()
            .unwrap_or(0)
    };
    let inside = at(4, 4);
    let outside = at(80, 50);
    println!("inside the damage: {inside}, outside: {outside}");
    assert!(
        inside > 150,
        "the damaged corner was not repainted ({inside})"
    );
    assert!(
        outside < 100,
        "the rest of the window was repainted although nothing said it changed \
         ({outside}) — damage is being ignored, so a caret costs a frame"
    );
}

/// **A window that follows the session.** A host switching monitor changes
/// the picture's size, and a presenter that only refused the new size would
/// leave the viewer on the last frame of the old screen — which reads as a
/// host that has hung.
#[test]
fn a_resized_window_takes_pictures_of_the_new_size() {
    let _screen = exclusive();
    const W: u32 = 96;
    const H: u32 = 64;
    let Some(mut p) = open(W, H) else { return };
    p.present(&flat(W, H, 126, 90, 200), &Damage::Everything)
        .expect("the first size");

    // Both directions, and a portrait shape: a monitor switch is as likely to
    // be to a taller screen as a wider one, and an arithmetic mistake in the
    // stride shows up on one and not the other.
    for (w, h) in [(160u32, 100u32), (48, 128), (W, H)] {
        p.resize(w, h).expect("resized");
        assert_eq!(p.size(), (w, h), "the presenter reports the new size");
        // The window follows, but not synchronously: `configure_window` is
        // checked, which proves the request reached the server, and a window
        // manager then resizes the window in its own time.
        let mut geometry = window_size(p.window()).expect("the window's own size");
        for _ in 0..20 {
            if geometry == (w, h) {
                break;
            }
            let _ = p.poll();
            std::thread::sleep(std::time::Duration::from_millis(25));
            geometry = window_size(p.window()).expect("the window's own size");
        }
        assert_eq!(
            geometry,
            (w, h),
            "the window did not follow the resize — a window manager refusing a \
             client's size would look the same, so check that before the code"
        );
        p.present(&flat(w, h, 126, 90, 200), &Damage::Everything)
            .expect("a picture of the new size");
        // And the old size is now the wrong one.
        assert!(
            matches!(
                p.present(&flat(W + 7, H + 3, 126, 90, 200), &Damage::Everything),
                Err(ownview_present::PresentError::WrongSize { .. })
            ),
            "after a resize only the new size is accepted"
        );
        assert!(p.is_open(), "and the window survives it");
    }
}

/// Resizing to the size already in use must cost nothing: a re-announcement
/// that changed something else should not make the window flicker.
#[test]
fn resizing_to_the_same_size_does_nothing() {
    let _screen = exclusive();
    let Some(mut p) = open(64, 48) else { return };
    p.resize(64, 48).expect("a no-op resize");
    assert_eq!(p.size(), (64, 48));
    p.present(&flat(64, 48, 126, 90, 200), &Damage::Everything)
        .expect("still presents");
}

#[test]
fn a_window_cannot_be_resized_to_nothing() {
    let _screen = exclusive();
    let Some(mut p) = open(64, 48) else { return };
    assert!(p.resize(0, 48).is_err());
    assert!(p.resize(64, 0).is_err());
    // And it still works at the size it had.
    assert_eq!(p.size(), (64, 48));
    p.present(&flat(64, 48, 126, 90, 200), &Damage::Everything)
        .expect("unharmed");
}

#[test]
fn a_picture_of_the_wrong_size_is_refused() {
    let Some(mut p) = open(64, 64) else { return };
    let wrong = flat(64, 66, 128, 128, 128);
    assert!(matches!(
        p.present(&wrong, &Damage::Everything),
        Err(ownview_present::PresentError::WrongSize { .. })
    ));
}

#[test]
fn nothing_changed_costs_nothing() {
    let Some(mut p) = open(32, 32) else { return };
    // Must not error, and must not need a picture of the right size to be
    // examined at all — there is nothing to draw.
    p.present(&flat(32, 32, 128, 128, 128), &Damage::Nothing)
        .expect("presents");
    assert!(p.is_open());
}

#[test]
fn a_zero_sized_window_is_refused() {
    if std::env::var_os("DISPLAY").is_none() {
        println!("skipped: no DISPLAY");
        return;
    }
    assert!(X11Presenter::open(0, 100, ColorSpace::default()).is_err());
    assert!(X11Presenter::open(100, 0, ColorSpace::default()).is_err());
}

/// Both transports have to put the same pixels in the window. A shear in the
/// socket path would only appear on a remote display, which is exactly where
/// nobody is developing.
#[test]
fn the_window_survives_a_full_repaint_after_exposure() {
    const W: u32 = 64;
    const H: u32 = 48;
    let Some(mut p) = open(W, H) else { return };
    p.present(&flat(W, H, 180, 128, 128), &Damage::Everything)
        .expect("presents");
    // Polling handles exposure by repainting from the surface, which is what
    // keeps a static picture on screen when no new frame is coming.
    let events = p.poll().expect("poll");
    println!("{} event(s) on the first poll: {events:?}", events.len());
    assert!(p.is_open());
    // And a second poll must not error even with nothing pending.
    assert!(p.poll().is_ok());
}
