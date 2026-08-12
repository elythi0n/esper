//! Making the host's desktop background plain for the length of a session.
//!
//! # What this is for, in bytes
//!
//! A desktop is windows over a background, and the windows are what the person
//! at the far end came for. If the background is a photograph, they are paying
//! for it: **63% of a keyframe at 1280×720** on a desktop whose windows cover
//! a tenth of the screen, 48% with a quarter covered, 25% with half — measured
//! in `esper-encode/tests/background_cost.rs`.
//!
//! And it is a keyframe cost, which is where it hurts. Capture is
//! damage-driven, so a still background is sent once and never again; what a
//! wallpaper costs is the **first picture** and every keyframe after a change
//! of size. That is precisely where this product measured its worst behaviour:
//! a link whose buffer could not hold one keyframe showed no picture at all
//! for as long as a session ran.
//!
//! # What the desktop has to have said
//!
//! X11 has no way to ask a window what its background is. So this can only put
//! back a background the desktop **published**, and there is a convention for
//! that: `_XROOTPMAP_ID`, and `ESETROOT_PMAP_ID` from the tool that started it.
//! Both name a pixmap on the root window.
//!
//! Where neither is there, or both are zero, nothing is touched — a background
//! this cannot put back is one it may not take away. That is not a rare case
//! and it is not a bug: a desktop that draws its wallpaper in a window of its
//! own publishes no pixmap, and on the machine this was written on
//! `_XROOTPMAP_ID` was present and **zero** while `ESETROOT_PMAP_ID` held the
//! real one — which is why both are read and a zero is not a pixmap.
//!
//! # Black, on purpose
//!
//! The screen's own `black_pixel`, which is a colour every server is certain
//! to have: no colormap allocation, no assumption about the visual, and the
//! cheapest thing an encoder can be given. A person at the machine sees their
//! wallpaper go black for the session, which is why this is off unless they
//! asked for it.
//!
//! # What is verified, and what cannot be
//!
//! **Verified**: that the picture changed. Pixels are read from the root
//! before and after — through `GetImage`, the same path the capture pipeline
//! reads — and the caller is told how many of them moved. A suppression that
//! changed nothing is reported as such and put straight back, rather than
//! leaving somebody's wallpaper hidden for nothing.
//!
//! **Not verifiable**: that the background *is* now the colour asked for.
//! X11 will not say what a window's background is, and any sample point may
//! have a window over it. So this asks the honest question — did anything
//! change — rather than a question the server cannot answer.

use esper_keeper::Undo;
use esper_presence::Backdrop;
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{
    AtomEnum, ChangeWindowAttributesAux, ConnectionExt as _, ImageFormat, Window,
};

/// The two properties a desktop publishes its background pixmap in.
///
/// Both, because they disagree in practice: the tool that sets one usually
/// sets the other, and either can be left stale.
const PUBLISHED_IN: [&str; 2] = ["_XROOTPMAP_ID", "ESETROOT_PMAP_ID"];

/// How many places to sample when asking whether the picture changed.
///
/// A grid rather than a line: a window covering the top half of the screen
/// would otherwise answer for the whole of it.
const SAMPLES: u16 = 5;

/// What changed, so a caller can say whether it was worth doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Changed {
    /// How many sampled places on the root are a different colour now.
    pub moved: usize,
    /// How many were sampled.
    pub of: usize,
}

impl Changed {
    /// Whether anything at all changed.
    #[must_use]
    pub const fn anything(self) -> bool {
        self.moved > 0
    }
}

/// What the desktop says its background is, and how to put it back.
///
/// # Errors
///
/// If the display cannot be reached, or the desktop has published no
/// background pixmap — which is a refusal rather than a silent success,
/// because the alternative is taking away something nothing can restore.
pub fn plan() -> Result<Backdrop, String> {
    let (conn, screen_num) = x11rb::connect(None).map_err(|e| e.to_string())?;
    let root = conn
        .setup()
        .roots
        .get(screen_num)
        .ok_or_else(|| "the display has no screen".to_owned())?
        .root;
    let (pixmap, named_in) = published(&conn, root).ok_or_else(|| {
        "this desktop has not published a background pixmap, so there is nothing that could be \
         put back afterwards"
            .to_owned()
    })?;
    Ok(Backdrop {
        what: format!("the desktop background ({named_in})"),
        undo: vec![Undo::Backdrop {
            pixmap,
            what: "the desktop background".to_owned(),
        }],
    })
}

/// The background pixmap the desktop published, and which property said so.
fn published<C: Connection>(conn: &C, root: Window) -> Option<(u32, &'static str)> {
    for name in PUBLISHED_IN {
        let atom = conn
            .intern_atom(true, name.as_bytes())
            .ok()?
            .reply()
            .ok()?
            .atom;
        if atom == 0 {
            continue;
        }
        let reply = conn
            .get_property(false, root, atom, AtomEnum::PIXMAP, 0, 1)
            .ok()?
            .reply()
            .ok()?;
        let Some(id) = reply.value32().and_then(|mut v| v.next()) else {
            continue;
        };
        // **Zero is not a pixmap.** It is what a stale property looks like,
        // and it is what was actually on the machine this was written on.
        if id != 0 {
            return Some((id, name));
        }
    }
    None
}

/// Make the background plain, and say whether the picture changed.
///
/// **Registered with a keeper first, by the caller.** This does not check
/// that: it cannot, and the one place that calls it is the one place that
/// knows — the same arrangement the privacy screen and the input lockout have.
///
/// # Errors
///
/// If the display cannot be reached or the server refuses the change.
pub fn plain(plan: &Backdrop) -> Result<Changed, String> {
    // **The plan is the ordering, not the data.** Nothing here needs anything
    // out of it — the colour is the screen's own black — and it is taken by
    // value anyway so that this cannot be called by somebody who has not
    // been handed something to register first. The privacy screen's `blank`
    // has the same shape for the same reason, and there it happens to need
    // the contents too.
    let _ = plan;
    let (conn, screen_num) = x11rb::connect(None).map_err(|e| e.to_string())?;
    let screen = conn
        .setup()
        .roots
        .get(screen_num)
        .ok_or_else(|| "the display has no screen".to_owned())?;
    let (root, black) = (screen.root, screen.black_pixel);
    let (width, height) = (screen.width_in_pixels, screen.height_in_pixels);
    let before = sampled(&conn, root, width, height);

    conn.change_window_attributes(
        root,
        &ChangeWindowAttributesAux::new().background_pixel(black),
    )
    .map_err(|e| e.to_string())?
    .check()
    .map_err(|e| format!("setting the root's background: {e:?}"))?;
    // **Setting the background paints nothing.** X11 uses it the next time the
    // window is cleared, so a caller that stopped here would have changed a
    // number and nothing else — which is the shape of a mistake this tree has
    // made before with `xsetroot`.
    conn.clear_area(false, root, 0, 0, 0, 0)
        .map_err(|e| e.to_string())?
        .check()
        .map_err(|e| format!("clearing the root: {e:?}"))?;
    conn.flush().map_err(|e| e.to_string())?;

    let after = sampled(&conn, root, width, height);
    let moved = before
        .iter()
        .zip(after.iter())
        .filter(|(was, now)| was != now)
        .count();
    Ok(Changed {
        moved,
        of: before.len().min(after.len()),
    })
}

/// The colour at a grid of places on the root, as the server's own integers.
///
/// Compared as integers rather than converted to RGB: the question is whether
/// they are the same value, and a conversion is a second place for two answers
/// to disagree. The same reasoning the indicator's own read-back is built on.
fn sampled<C: Connection>(conn: &C, root: Window, width: u16, height: u16) -> Vec<u32> {
    let mut seen = Vec::new();
    for row in 1..=SAMPLES {
        for column in 1..=SAMPLES {
            let x = i32::from(width) * i32::from(column) / i32::from(SAMPLES + 1);
            let y = i32::from(height) * i32::from(row) / i32::from(SAMPLES + 1);
            let (Ok(x), Ok(y)) = (i16::try_from(x), i16::try_from(y)) else {
                continue;
            };
            let Ok(cookie) = conn.get_image(ImageFormat::Z_PIXMAP, root, x, y, 1, 1, !0) else {
                continue;
            };
            let Ok(reply) = cookie.reply() else { continue };
            let mut value = 0u32;
            for (i, byte) in reply.data.iter().take(4).enumerate() {
                value |= u32::from(*byte) << (8 * i);
            }
            seen.push(value);
        }
    }
    seen
}

/// Put the desktop's own background back.
///
/// The keeper's half: it runs after the host is gone, so it opens its own
/// connection and asks for nothing it cannot get from the record.
///
/// # Errors
///
/// If the display cannot be reached, or the server refuses the pixmap — which
/// is what a desktop that has since gone away looks like, and is worth saying
/// rather than hiding.
pub fn put_back(undo: &Undo) -> Result<(), String> {
    let Undo::Backdrop { pixmap, .. } = undo else {
        return Err("this is not a desktop background to put back".to_owned());
    };
    let (conn, screen_num) = x11rb::connect(None).map_err(|e| e.to_string())?;
    let root = conn
        .setup()
        .roots
        .get(screen_num)
        .ok_or_else(|| "the display has no screen".to_owned())?
        .root;
    conn.change_window_attributes(
        root,
        &ChangeWindowAttributesAux::new().background_pixmap(*pixmap),
    )
    .map_err(|e| e.to_string())?
    .check()
    .map_err(|e| format!("putting the desktop's background back: {e:?}"))?;
    conn.clear_area(false, root, 0, 0, 0, 0)
        .map_err(|e| e.to_string())?
        .check()
        .map_err(|e| format!("clearing the root: {e:?}"))?;
    conn.flush().map_err(|e| e.to_string())?;
    Ok(())
}
