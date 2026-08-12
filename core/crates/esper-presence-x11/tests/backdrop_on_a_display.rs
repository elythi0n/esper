#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]
// The crate under test is Linux-only; so is its test.
#![cfg(target_os = "linux")]

//! **Making the desktop background plain, and putting it back.**
//!
//! # What is asserted here, and what is not
//!
//! This plays the desktop's part: it paints a pixmap, makes it the root's
//! background and publishes it the way a wallpaper setter does. Then it
//! asserts — by reading the root through the same call the capture pipeline
//! uses — that the picture went plain and came back.
//!
//! That is the whole mechanism, verified on a display where nothing else is
//! drawing, which is exactly what the product cannot do for itself: in
//! production any sample point may have a window over it, and X11 will not say
//! what a window's background is. So the host asks the honest question there
//! (*did anything change*) and this asks the complete one here.
//!
//! # It puts the display back
//!
//! These may run on somebody's real desktop — the flag is theirs to set — and
//! the root's background and the two properties that name it are **persistent
//! state**, not a window that vanishes when a test ends. So the rig reads both
//! properties as it found them and restores them, present or absent, whatever
//! the test did in between.

use esper_keeper::Undo;
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{
    AtomEnum, ChangeGCAux, ChangeWindowAttributesAux, ConnectionExt as _, CreateGCAux, ImageFormat,
    PropMode, Rectangle, Window,
};
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

/// The two properties a desktop publishes its background pixmap in.
const PUBLISHED_IN: [&str; 2] = ["_XROOTPMAP_ID", "ESETROOT_PMAP_ID"];

/// Whether this run may do visible things to the display it can see.
fn may_disturb_the_display() -> bool {
    if std::env::var_os("ESPER_DISPLAY_TESTS").is_none() {
        println!(
            "skipped: set ESPER_DISPLAY_TESTS=1 to allow changing this display's desktop \
             background. It is off unless asked for."
        );
        return false;
    }
    true
}

/// A display, and the desktop background exactly as it was found.
struct Rig {
    conn: RustConnection,
    root: Window,
    depth: u8,
    width: u16,
    height: u16,
    /// What each property said when this started: `None` for absent.
    found: Vec<(&'static str, Option<u32>)>,
    /// One display, one root — and one lock, shared with every other suite in
    /// the workspace that touches a display.
    _lock: esper_display_lock::DisplayLock,
}

impl Rig {
    fn open() -> Self {
        let lock = esper_display_lock::DisplayLock::take();
        let (conn, num) = esper_display_lock::connect().expect("a display");
        let screen = conn.setup().roots.get(num).expect("a screen").clone();
        let mut found = Vec::new();
        for name in PUBLISHED_IN {
            found.push((name, read_published(&conn, screen.root, name)));
        }
        Self {
            conn,
            root: screen.root,
            depth: screen.root_depth,
            width: screen.width_in_pixels,
            height: screen.height_in_pixels,
            found,
            _lock: lock,
        }
    }

    /// Paint eight bands, make them the background, and publish them.
    ///
    /// Bands rather than one colour, so "the background changed" is a fact
    /// about a picture rather than about one pixel that happened to differ.
    ///
    /// The pixmap is deliberately never freed: it is the wallpaper for as long
    /// as this connection lives, and freeing it while the root's background
    /// names it is a different test — see the last one in this file.
    fn paints_a_wallpaper(&self) {
        let pixmap = self.conn.generate_id().expect("an id");
        self.conn
            .create_pixmap(self.depth, pixmap, self.root, self.width, self.height)
            .expect("a pixmap")
            .check()
            .expect("a pixmap");
        let gc = self.conn.generate_id().expect("an id");
        self.conn
            .create_gc(gc, pixmap, &CreateGCAux::new())
            .expect("a graphics context")
            .check()
            .expect("a graphics context");
        for band in 0..8u16 {
            let colour = 0x0020_2020u32 * u32::from(band + 1);
            self.conn
                .change_gc(gc, &ChangeGCAux::new().foreground(colour))
                .expect("colour")
                .check()
                .expect("colour");
            let top = i16::try_from(self.height / 8 * band).expect("a small screen");
            self.conn
                .poly_fill_rectangle(
                    pixmap,
                    gc,
                    &[Rectangle {
                        x: 0,
                        y: top,
                        width: self.width,
                        height: self.height / 8,
                    }],
                )
                .expect("fill")
                .check()
                .expect("fill");
        }
        self.becomes_the_background(pixmap);
        for name in PUBLISHED_IN {
            self.publishes(name, Some(pixmap));
        }
        self.conn.flush().expect("flush");
    }

    fn becomes_the_background(&self, pixmap: u32) {
        self.conn
            .change_window_attributes(
                self.root,
                &ChangeWindowAttributesAux::new().background_pixmap(pixmap),
            )
            .expect("background")
            .check()
            .expect("background");
        self.conn
            .clear_area(false, self.root, 0, 0, 0, 0)
            .expect("clear")
            .check()
            .expect("clear");
    }

    /// Say — or stop saying — that a pixmap is the background.
    fn publishes(&self, name: &str, pixmap: Option<u32>) {
        let atom = self
            .conn
            .intern_atom(false, name.as_bytes())
            .expect("atom")
            .reply()
            .expect("atom")
            .atom;
        match pixmap {
            Some(id) => {
                self.conn
                    .change_property32(PropMode::REPLACE, self.root, atom, AtomEnum::PIXMAP, &[id])
                    .expect("property")
                    .check()
                    .expect("property");
            }
            None => {
                let _ = self
                    .conn
                    .delete_property(self.root, atom)
                    .expect("delete")
                    .check();
            }
        }
    }

    /// The colours down the middle of the root, read the way capture reads.
    fn stripe(&self) -> Vec<u32> {
        let x = i16::try_from(self.width / 2).expect("a small screen");
        (1..=8)
            .filter_map(|n| {
                let y = i16::try_from(self.height * n / 9).ok()?;
                let reply = self
                    .conn
                    .get_image(ImageFormat::Z_PIXMAP, self.root, x, y, 1, 1, !0)
                    .ok()?
                    .reply()
                    .ok()?;
                let mut value = 0u32;
                for (i, byte) in reply.data.iter().take(4).enumerate() {
                    value |= u32::from(*byte) << (8 * i);
                }
                Some(value)
            })
            .collect()
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        // Both properties back to what they said, present or absent.
        for (name, was) in self.found.clone() {
            self.publishes(name, was);
        }
        // And the background to whatever the desktop had published, if it had.
        if let Some(id) = self
            .found
            .iter()
            .find_map(|(_, was)| *was)
            .filter(|id| *id != 0)
        {
            self.becomes_the_background(id);
        }
        let _ = self.conn.flush();
    }
}

/// What a property says now, or `None` if it says nothing.
fn read_published(conn: &RustConnection, root: Window, name: &str) -> Option<u32> {
    let atom = conn
        .intern_atom(true, name.as_bytes())
        .ok()?
        .reply()
        .ok()?
        .atom;
    if atom == 0 {
        return None;
    }
    let reply = conn
        .get_property(false, root, atom, AtomEnum::PIXMAP, 0, 1)
        .ok()?
        .reply()
        .ok()?;
    reply.value32().and_then(|mut v| v.next())
}

/// **A published wallpaper goes plain, and comes back exactly.**
#[test]
fn a_wallpaper_goes_plain_for_a_session_and_comes_back() {
    if !may_disturb_the_display() {
        return;
    }
    let rig = Rig::open();
    rig.paints_a_wallpaper();

    let before = rig.stripe();
    assert!(
        before
            .iter()
            .collect::<std::collections::HashSet<_>>()
            .len()
            > 1,
        "the wallpaper this test painted is not on the root, so nothing below proves \
         anything: {before:?}"
    );

    let plan = esper_presence_x11::backdrop::plan().expect("the desktop published one");
    assert!(!plan.undo.is_empty());
    let changed = esper_presence_x11::backdrop::plain(&plan).expect("made plain");
    let during = rig.stripe();

    // Put it back whatever the answers were: a display left black breaks
    // every test after this one, and the person running it.
    let trouble: Vec<String> = plan
        .undo
        .iter()
        .filter_map(|u| esper_presence_x11::backdrop::put_back(u).err())
        .collect();
    let after = rig.stripe();

    assert!(
        changed.anything(),
        "the host would have reported that nothing changed: {changed:?}"
    );
    assert_eq!(
        during
            .iter()
            .collect::<std::collections::HashSet<_>>()
            .len(),
        1,
        "the background is not one colour after being made plain: {during:?}"
    );
    assert!(trouble.is_empty(), "putting it back: {trouble:?}");
    assert_eq!(
        after, before,
        "the wallpaper did not come back as it was: {after:?} against {before:?}"
    );
}

/// **A desktop that published nothing is refused**, because a background
/// nothing can put back may not be taken away.
#[test]
fn a_desktop_that_published_nothing_is_refused() {
    if !may_disturb_the_display() {
        return;
    }
    let rig = Rig::open();
    // The state of a desktop that draws its wallpaper in a window of its own,
    // or of a bare X server. Restored by the rig.
    for name in PUBLISHED_IN {
        rig.publishes(name, None);
    }
    rig.conn.flush().expect("flush");

    let refused = esper_presence_x11::backdrop::plan();
    let why = match refused {
        Err(why) => why,
        Ok(plan) => panic!("a desktop with no published background was not refused: {plan:?}"),
    };
    assert!(
        why.contains("put back"),
        "the refusal should say why it is one: {why:?}"
    );
}

/// **A zero is not a pixmap.** It is what a stale property looks like, and it
/// is what was on the machine this was written on: `_XROOTPMAP_ID` present and
/// zero, the real one in `ESETROOT_PMAP_ID`.
#[test]
fn a_stale_property_is_not_mistaken_for_a_background() {
    if !may_disturb_the_display() {
        return;
    }
    let rig = Rig::open();
    rig.paints_a_wallpaper();
    let real = read_published(&rig.conn, rig.root, "ESETROOT_PMAP_ID").expect("just published");
    rig.publishes("_XROOTPMAP_ID", Some(0));
    rig.conn.flush().expect("flush");

    let plan = esper_presence_x11::backdrop::plan().expect("the other property still names one");
    let Some(Undo::Backdrop { pixmap, .. }) = plan.undo.first() else {
        panic!("the plan carried something else: {plan:?}");
    };
    assert_eq!(
        *pixmap, real,
        "a zero in the first property was taken for the background"
    );
}

/// **A record for something the server will not accept is reported**, not
/// swallowed — a desktop that has gone away leaves a pixmap id nothing owns.
#[test]
fn a_pixmap_that_no_longer_exists_is_reported() {
    if !may_disturb_the_display() {
        return;
    }
    let _rig = Rig::open();
    let nonsense = Undo::Backdrop {
        // An id no client has been given: the server refuses it.
        pixmap: 0x00ff_fffe,
        what: "a wallpaper that has gone".to_owned(),
    };
    assert!(
        esper_presence_x11::backdrop::put_back(&nonsense).is_err(),
        "the server accepted a pixmap that does not exist, so nothing here can tell a \
         restore that worked from one that did not"
    );
}
