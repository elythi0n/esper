#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]
#![cfg(target_os = "linux")]

//! **A host that makes the desktop background plain, killed, and the wallpaper
//! comes back.**
//!
//! # Why this exists as a test and not a smoke check
//!
//! The smoke suite can start a host and shoot it, and it cannot produce the
//! precondition: a desktop that has *published* a background pixmap. That
//! needs a client which creates the pixmap and stays alive to own it, and no
//! tool on the machine this was written on does that.
//!
//! This test is that client. It plays the desktop's part — paints a wallpaper,
//! publishes it the way a wallpaper setter does, and holds it — then starts the
//! real `esper-engine host` with `host.plain_background` on, opens a session,
//! and kills the engine with `-9`.
//!
//! What is asserted is what the whole arrangement is for: **the wallpaper is
//! back afterwards**, read off the root through the same call the capture
//! pipeline uses, and the keeper said it was the one who did it. Either alone
//! would be a check that can pass for the wrong reason — a display server
//! reset puts a root background back all by itself, which is how a check for
//! the input lockout once passed with the keeper taken out of the code.

use std::time::Duration;

use esper_proto::Permission;
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{
    AtomEnum, ChangeGCAux, ChangeWindowAttributesAux, ConnectionExt as _, CreateGCAux, ImageFormat,
    PropMode, Rectangle, Window,
};
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

mod common;
use common::{Host, Peer};

/// The two properties a desktop publishes its background pixmap in.
const PUBLISHED_IN: [&str; 2] = ["_XROOTPMAP_ID", "ESETROOT_PMAP_ID"];

/// The display this may disturb is somebody's session unless they said so.
fn may_disturb_the_display() -> bool {
    if std::env::var_os("ESPER_DISPLAY_TESTS").is_none() {
        println!("skipped: set ESPER_DISPLAY_TESTS=1 to allow changing this display's background");
        return false;
    }
    if std::env::var_os("DISPLAY").is_none() {
        println!("skipped: no display to change the background of");
        return false;
    }
    true
}

/// A desktop with a wallpaper, put back exactly as it was found.
struct Desktop {
    conn: RustConnection,
    root: Window,
    width: u16,
    height: u16,
    found: Vec<(&'static str, Option<u32>)>,
    _lock: esper_display_lock::DisplayLock,
}

impl Desktop {
    fn with_a_wallpaper() -> Self {
        let lock = esper_display_lock::DisplayLock::take();
        let (conn, num) = esper_display_lock::connect().expect("a display");
        let screen = conn.setup().roots.get(num).expect("a screen").clone();
        let found = PUBLISHED_IN
            .into_iter()
            .map(|name| (name, published(&conn, screen.root, name)))
            .collect();
        let desktop = Self {
            conn,
            root: screen.root,
            width: screen.width_in_pixels,
            height: screen.height_in_pixels,
            found,
            _lock: lock,
        };
        desktop.paint(screen.root_depth);
        desktop
    }

    /// Eight bands, so "it came back" is a fact about a picture.
    fn paint(&self, depth: u8) {
        let pixmap = self.conn.generate_id().expect("an id");
        self.conn
            .create_pixmap(depth, pixmap, self.root, self.width, self.height)
            .expect("a pixmap")
            .check()
            .expect("a pixmap");
        let gc = self.conn.generate_id().expect("an id");
        self.conn
            .create_gc(gc, pixmap, &CreateGCAux::new())
            .expect("a gc")
            .check()
            .expect("a gc");
        for band in 0..8u16 {
            self.conn
                .change_gc(
                    gc,
                    &ChangeGCAux::new().foreground(0x0020_2020u32 * u32::from(band + 1)),
                )
                .expect("colour")
                .check()
                .expect("colour");
            self.conn
                .poly_fill_rectangle(
                    pixmap,
                    gc,
                    &[Rectangle {
                        x: 0,
                        y: i16::try_from(self.height / 8 * band).expect("a small screen"),
                        width: self.width,
                        height: self.height / 8,
                    }],
                )
                .expect("fill")
                .check()
                .expect("fill");
        }
        self.background(pixmap);
        for name in PUBLISHED_IN {
            self.publish(name, Some(pixmap));
        }
        self.conn.flush().expect("flush");
    }

    fn background(&self, pixmap: u32) {
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

    fn publish(&self, name: &str, pixmap: Option<u32>) {
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
                    .expect("gone")
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

    /// Wait for the root to look like this again, or give up.
    fn waits_to_look_like(&self, wanted: &[u32], within: Duration) -> bool {
        let deadline = std::time::Instant::now() + within;
        while std::time::Instant::now() < deadline {
            if self.stripe() == wanted {
                return true;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        false
    }
}

impl Drop for Desktop {
    fn drop(&mut self) {
        for (name, was) in self.found.clone() {
            self.publish(name, was);
        }
        if let Some(id) = self
            .found
            .iter()
            .find_map(|(_, was)| *was)
            .filter(|id| *id != 0)
        {
            self.background(id);
        }
        let _ = self.conn.flush();
    }
}

fn published(conn: &RustConnection, root: Window, name: &str) -> Option<u32> {
    let atom = conn
        .intern_atom(true, name.as_bytes())
        .ok()?
        .reply()
        .ok()?
        .atom;
    if atom == 0 {
        return None;
    }
    conn.get_property(false, root, atom, AtomEnum::PIXMAP, 0, 1)
        .ok()?
        .reply()
        .ok()?
        .value32()
        .and_then(|mut v| v.next())
}

/// **A host killed with `-9` leaves the wallpaper as it found it.**
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_keeper_puts_the_wallpaper_back_after_the_host_is_killed() {
    if !may_disturb_the_display() {
        return;
    }
    let desktop = Desktop::with_a_wallpaper();
    let wallpaper = desktop.stripe();
    assert!(
        wallpaper
            .iter()
            .collect::<std::collections::HashSet<_>>()
            .len()
            > 1,
        "the wallpaper this test painted is not on the root, so nothing below proves \
         anything: {wallpaper:?}"
    );

    // **Not named "backdrop".** The home's path is printed in the host's own
    // output — the audit line names it — so a check that waited for the word
    // "backdrop" matched the *directory* and passed before the host had done
    // anything at all. Found by the assertion after it, which is the only
    // reason this test is not still lying.
    let (host_home, host_key, peer_home) = common::homes("plain-bg", &[Permission::View]);
    // Written before the host starts: the flag is what the whole test is
    // about, and a host that had never seen the file would pass every
    // assertion below for the wrong reason.
    std::fs::write(
        host_home.path().join("config.toml"),
        "host.plain_background = true\n",
    )
    .expect("a config");
    let mut host = Host::start_on_this_display(&host_home);

    let _peer = Peer::open(&peer_home, &host, host_key, &[Permission::View]).await;
    // The sentence, not the word: waited for, because the background is
    // changed after the session's other lines are printed.
    assert!(
        host.waits_to_say("is plain for this session"),
        "the background was not made plain, so the rest proves nothing: {:?}",
        host.everything()
    );
    assert!(
        desktop.stripe() != wallpaper,
        "the host said the background was plain and the root still shows the wallpaper"
    );

    host.killed_outright();

    assert!(
        desktop.waits_to_look_like(&wallpaper, Duration::from_secs(15)),
        "the wallpaper did not come back after the host was killed: {:?} against {wallpaper:?}",
        desktop.stripe()
    );
    // **And the keeper is who put it back.** The state alone is not enough:
    // a display server with no clients left resets and restores a root
    // background all by itself, which is how a check for the input lockout
    // once passed with the keeper taken out of the code. This test is a
    // client and stays connected, so no reset can happen — and the keeper
    // says what it did on the host's own stderr.
    assert!(
        host.waits_to_say("put back the desktop background after the host went away"),
        "nothing said it was the keeper: {:?}",
        host.everything()
    );
}
