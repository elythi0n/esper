#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
// The crate under test is Linux-only; so is its test.
#![cfg(target_os = "linux")]

//! What a pointer costs, measured rather than argued about.
//!
//! # The claim being tested
//!
//! A pointer composited on the *host* costs a round trip, an encode and a
//! decode for every movement. A pointer worn by the viewer's window costs
//! nothing at all — the server draws it at the local pointer. Between them
//! sits the third case, a pointer composited on the *controller* at the
//! position the host reported, which costs a round trip but no encode.
//!
//! The numbers that matter are not all measurable here — a round trip belongs
//! to a network — but the *local* cost of each option is, and that is what
//! decides whether compositing is a reasonable fallback or a last resort.
//!
//! Run with `cargo test -p esper-present-x11 --test cursor_cost -- --nocapture`.

use std::time::{Duration, Instant};

use esper_capture::damage::{Damage, Regions};
use esper_capture::geometry::Rect;
use esper_color::{ColorSpace, I420};
use esper_present::Presenter;
use esper_present_x11::X11Presenter;

const W: u32 = 1920;
const H: u32 = 1080;

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

fn pointer(edge: u16) -> esper_capture::cursor::CursorImage {
    esper_capture::cursor::CursorImage {
        width: edge,
        height: edge,
        hotspot_x: edge / 2,
        hotspot_y: edge / 2,
        // Half transparent, which is the expensive case: every pixel is
        // actually blended rather than copied or skipped.
        rgba: (0..u32::from(edge) * u32::from(edge))
            .flat_map(|_| [10u8, 200, 40, 128])
            .collect(),
    }
}

fn median(mut times: Vec<Duration>) -> Duration {
    times.sort_unstable();
    times.get(times.len() / 2).copied().unwrap_or_default()
}

/// **What each way of showing a pointer costs on the controller.**
#[test]
fn what_a_pointer_costs() {
    if std::env::var_os("DISPLAY").is_none() {
        println!("skipped: no DISPLAY");
        return;
    }
    if std::env::var_os("ESPER_DISPLAY_TESTS").is_none() {
        println!("skipped: set ESPER_DISPLAY_TESTS=1 — this draws on the display");
        return;
    }
    let _lock = esper_display_lock::DisplayLock::take();
    let Ok(mut p) = X11Presenter::open(W, H, ColorSpace::default()) else {
        println!("skipped: no window here");
        return;
    };
    let picture = flat(W, H, 126, 90, 200);
    p.present(&picture, &Damage::Everything).expect("a picture");

    // A caret-sized change, which is what a real session mostly sends.
    let caret = Regions::coalesce(
        &[Rect {
            x: 800,
            y: 400,
            width: 8,
            height: 18,
        }],
        &Rect::sized(W, H),
    );

    const ROUNDS: usize = 200;

    // 1. Worn: the pointer is the window's cursor and moving it costs the
    //    viewer nothing at all — the server draws it wherever the hand is.
    //    What is measured here is a *frame* with a worn pointer.
    p.draw_cursor_at(None).expect("worn");
    p.set_cursor(Some(&pointer(24))).expect("a shape");
    let mut worn = Vec::with_capacity(ROUNDS);
    for _ in 0..ROUNDS {
        let started = Instant::now();
        p.present(&picture, &caret).expect("a picture");
        worn.push(started.elapsed());
    }

    // 2. Composited: the same frame, with the pointer drawn into it.
    p.draw_cursor_at(Some((900, 500))).expect("drawn");
    let mut drawn = Vec::with_capacity(ROUNDS);
    for _ in 0..ROUNDS {
        let started = Instant::now();
        p.present(&picture, &caret).expect("a picture");
        drawn.push(started.elapsed());
    }

    // 3. Moving a composited pointer, which is the cost a *watched* session
    //    pays per position: uncover, cover, and two small blits.
    let mut moves = Vec::with_capacity(ROUNDS);
    for i in 0..ROUNDS {
        let x = 200 + u32::try_from(i).unwrap_or(0) % 1400;
        let started = Instant::now();
        p.draw_cursor_at(Some((x, 500))).expect("moved");
        moves.push(started.elapsed());
    }

    // 4. And the same for a large pointer, since a 24-square cursor is the
    //    common case and a 64-square one is what an accessibility setting or
    //    a resize handle produces.
    p.set_cursor(Some(&pointer(64))).expect("a big shape");
    let mut big_moves = Vec::with_capacity(ROUNDS);
    for i in 0..ROUNDS {
        let x = 200 + u32::try_from(i).unwrap_or(0) % 1400;
        let started = Instant::now();
        p.draw_cursor_at(Some((x, 600))).expect("moved");
        big_moves.push(started.elapsed());
    }

    let worn = median(worn);
    let drawn = median(drawn);
    let moved = median(moves);
    let big = median(big_moves);
    println!();
    println!("  a {W}x{H} frame with a caret's worth of damage:");
    println!("    pointer worn by the window      {worn:>10.2?}");
    println!("    pointer composited into it      {drawn:>10.2?}");
    println!("  moving the pointer, no new frame:");
    println!("    24-square pointer              {moved:>10.2?}");
    println!("    64-square pointer              {big:>10.2?}");
    println!();

    // The property worth holding to: compositing must not cost so much that
    // it stops being a reasonable fallback. A frame is 16 ms at 60 fps; a
    // pointer must be a rounding error next to that.
    assert!(
        drawn < Duration::from_millis(4),
        "compositing a pointer into a frame took {drawn:?}, which is a large \
         fraction of a frame"
    );
    assert!(
        moved < Duration::from_millis(2),
        "moving a composited pointer took {moved:?}"
    );
}
