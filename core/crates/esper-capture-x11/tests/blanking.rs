//! **Can the host's physical screen be blanked without blinding the session?**
//!
//! A privacy screen — blank the host's own display while somebody works on it
//! remotely, for a machine in a shared room — was measured once before and the
//! obvious mechanism was killed by it: X11 capture reads the *root*, and a
//! window mapped over the screen is part of what the root composites, so a
//! cover blanks the remote view too ([finding][1]).
//!
//! The mechanism left is turning the **output** off with RandR, which leaves
//! the framebuffer allocated and therefore leaves capture reading it. That
//! claim is the thing measured here, because the whole feature rests on it and
//! nobody had run it.
//!
//! # Why this needs a gate of its own
//!
//! `ESPER_DISPLAY_TESTS` means "you may disturb this display". Turning
//! somebody's monitor off is not a disturbance — if the restore fails they are
//! left looking at a dark screen with no way to see what they are typing. So
//! this refuses to run unless **every output reports no physical size**, which
//! is what a virtual display looks like: a real monitor reports its
//! millimetres from its EDID, and `Xvfb` reports `0mm x 0mm`.
//!
//! [1]: the finding of 2026-09-06, "what the camera sees"

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::time::Duration;

use x11rb::connection::Connection;
use x11rb::protocol::randr::{self, ConnectionExt as _};
use x11rb::protocol::xproto::ConnectionExt as _;

/// A display whose outputs are all virtual, or `None` with a reason.
fn scratch_display() -> Option<(x11rb::rust_connection::RustConnection, u32)> {
    if std::env::var_os("ESPER_DISPLAY_TESTS").is_none() {
        println!("skipped: set ESPER_DISPLAY_TESTS=1 to allow changing this display's outputs");
        return None;
    }
    let (conn, screen_num) = esper_display_lock::connect().ok()?;
    let root = conn.setup().roots.get(screen_num)?.root;
    let resources = conn
        .randr_get_screen_resources_current(root)
        .ok()?
        .reply()
        .ok()?;
    for output in &resources.outputs {
        let info = conn
            .randr_get_output_info(*output, resources.config_timestamp)
            .ok()?
            .reply()
            .ok()?;
        let name = String::from_utf8_lossy(&info.name).to_string();
        if info.mm_width != 0 || info.mm_height != 0 {
            // **The whole point of the gate.** An output that reports
            // millimetres has an EDID behind it, which means a monitor
            // somebody is looking at.
            println!(
                "skipped: output {name} reports {}mm x {}mm, so this is a real display and this \
                 test will not turn it off",
                info.mm_width, info.mm_height
            );
            return None;
        }
    }
    Some((conn, root))
}

/// The pixels at a point on the root, as the capture path reads them.
fn root_pixel<C: Connection>(conn: &C, root: u32, x: i16, y: i16) -> Option<[u8; 4]> {
    let image = conn
        .get_image(
            x11rb::protocol::xproto::ImageFormat::Z_PIXMAP,
            root,
            x,
            y,
            1,
            1,
            !0,
        )
        .ok()?
        .reply()
        .ok()?;
    image.data.first_chunk::<4>().copied()
}

/// **Turning the output off leaves the framebuffer readable.**
///
/// Which is the difference between a privacy screen and a broken session. If
/// this fails, the feature has no mechanism at all on X11 and the tracker's
/// entry should say so.
#[test]
fn an_output_that_is_off_still_leaves_a_capturable_framebuffer() {
    let Some((conn, root)) = scratch_display() else {
        return;
    };
    let resources = conn
        .randr_get_screen_resources_current(root)
        .expect("ask")
        .reply()
        .expect("resources");
    let Some(crtc_id) = resources.crtcs.first().copied() else {
        println!("skipped: this display has no CRTC");
        return;
    };
    let before = conn
        .randr_get_crtc_info(crtc_id, resources.config_timestamp)
        .expect("ask")
        .reply()
        .expect("crtc");
    println!(
        "crtc {crtc_id}: {}x{} at {},{} mode {} with {} output(s)",
        before.width,
        before.height,
        before.x,
        before.y,
        before.mode,
        before.outputs.len()
    );
    if before.mode == 0 {
        println!("skipped: this CRTC is already off");
        return;
    }

    // Paint something recognisable, so "capture still works" is not satisfied
    // by a black screen that was black anyway.
    let gc = conn.generate_id().expect("id");
    conn.create_gc(
        gc,
        root,
        &x11rb::protocol::xproto::CreateGCAux::new().foreground(0x00FF_00FF),
    )
    .expect("gc")
    .check()
    .expect("made");
    conn.poly_fill_rectangle(
        root,
        gc,
        &[x11rb::protocol::xproto::Rectangle {
            x: 0,
            y: 0,
            width: 64,
            height: 64,
        }],
    )
    .expect("fill")
    .check()
    .expect("filled");
    conn.flush().expect("flush");
    let painted = root_pixel(&conn, root, 8, 8).expect("a pixel");
    println!("painted pixel: {painted:?}");

    // Off.
    let off = conn
        .randr_set_crtc_config(
            crtc_id,
            x11rb::CURRENT_TIME,
            resources.config_timestamp,
            0,
            0,
            0, // no mode: the output stops scanning out
            randr::Rotation::ROTATE0,
            &[],
        )
        .expect("ask to turn it off")
        .reply();
    let status = match off {
        Ok(reply) => reply.status,
        Err(e) => {
            println!("skipped: this server refused to turn the CRTC off: {e:?}");
            return;
        }
    };
    println!("turning it off: {status:?}");
    std::thread::sleep(Duration::from_millis(200));

    let geometry = conn.get_geometry(root).expect("ask").reply().expect("geom");
    let while_off = root_pixel(&conn, root, 8, 8);
    println!(
        "while off: root is {}x{}, pixel {while_off:?}",
        geometry.width, geometry.height
    );

    // And back, exactly as it was — the part a dead host would not do.
    let back = conn
        .randr_set_crtc_config(
            crtc_id,
            x11rb::CURRENT_TIME,
            resources.config_timestamp,
            before.x,
            before.y,
            before.mode,
            before.rotation,
            &before.outputs,
        )
        .expect("ask to put it back")
        .reply();
    println!("putting it back: {:?}", back.map(|r| r.status));
    std::thread::sleep(Duration::from_millis(200));
    let after = conn
        .randr_get_crtc_info(crtc_id, resources.config_timestamp)
        .expect("ask")
        .reply()
        .expect("crtc");

    assert_eq!(
        while_off,
        Some(painted),
        "the framebuffer stopped being readable while the output was off, which would leave \
         this feature with no mechanism on X11"
    );
    assert!(
        geometry.width > 0 && geometry.height > 0,
        "the root window lost its size while the output was off"
    );
    assert_eq!(
        (after.mode, after.x, after.y),
        (before.mode, before.x, before.y),
        "the CRTC did not come back the way it was"
    );
}
