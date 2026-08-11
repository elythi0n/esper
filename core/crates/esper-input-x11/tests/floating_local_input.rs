#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]
// The crate under test is Linux-only; so is its test.
#![cfg(target_os = "linux")]

//! **Taking this machine's own input out of the picture, and giving it back.**
//!
//! The companion to `blocking_local_input.rs`, which measured *why* this is
//! done by detaching rather than grabbing. This one is about the thing that
//! was built on that measurement.
//!
//! # What is asserted here, and what is not
//!
//! Asserted, by reading the server back: that the devices are floating after
//! detaching, that remote injection still lands while they are, and that they
//! are attached again afterwards.
//!
//! # The rig has to keep somebody connected
//!
//! `Rig` holds a connection open for the whole of the first test and that is
//! load-bearing, not decoration. A display server with no clients left
//! *resets*, and a reset reinitialises the input hierarchy — so a detach comes
//! undone by itself, within milliseconds, if the last client goes away.
//! Measured while writing this on a bare `Xvfb`: floating with one client
//! held open, attached again with none. A reader who moves the rig below the
//! read-backs gets a test that passes on the server's housekeeping. It cost a
//! smoke check its meaning before it was found.
//!
//! Not asserted: that a person at the keyboard cannot type. Nothing here can
//! produce physical input — XTEST is the only injection available and this
//! server delivers a faked event as the master whatever device id it is
//! given, which `blocking_local_input.rs` measures. A floating slave has no
//! master to route events to, which is what floating *is* in XInput2 rather
//! than an inference about it. The same division the privacy screen ships
//! with: verify the state you set, and take the consequence from the layer
//! below.

use std::time::Duration;

use x11rb::connection::Connection;
use x11rb::protocol::Event;
use x11rb::protocol::xinput::{self, ConnectionExt as _, DeviceType};
use x11rb::protocol::xproto::{
    ConnectionExt as _, CreateWindowAux, EventMask, InputFocus, WindowClass,
};
use x11rb::protocol::xtest::ConnectionExt as _;

/// The display these may disturb is somebody's session unless they said so.
fn may_disturb_the_display() -> bool {
    if std::env::var_os("ESPER_DISPLAY_TESTS").is_none() {
        println!(
            "skipped: set ESPER_DISPLAY_TESTS=1 to allow detaching this display's input \
             devices. It is off unless asked for."
        );
        return false;
    }
    true
}

/// One display, one input hierarchy — and one lock, shared with every other
/// suite in the workspace that touches a display.
fn exclusive() -> esper_display_lock::DisplayLock {
    esper_display_lock::DisplayLock::take()
}

/// A window with the focus, so an injected key has somewhere to land.
struct Rig {
    conn: x11rb::rust_connection::RustConnection,
    root: x11rb::protocol::xproto::Window,
    window: x11rb::protocol::xproto::Window,
}

impl Rig {
    fn open() -> Self {
        let (conn, num) = esper_display_lock::connect().expect("a display");
        let root = conn.setup().roots.get(num).expect("a screen").root;
        let window = conn.generate_id().expect("an id");
        conn.create_window(
            x11rb::COPY_DEPTH_FROM_PARENT,
            window,
            root,
            0,
            0,
            64,
            64,
            0,
            WindowClass::INPUT_OUTPUT,
            x11rb::COPY_FROM_PARENT,
            &CreateWindowAux::new().event_mask(EventMask::KEY_PRESS | EventMask::KEY_RELEASE),
        )
        .expect("a window")
        .check()
        .expect("a window");
        conn.map_window(window).expect("map").check().expect("map");
        conn.set_input_focus(InputFocus::PARENT, window, x11rb::CURRENT_TIME)
            .expect("focus")
            .check()
            .expect("focus");
        conn.flush().expect("flush");
        Self { conn, root, window }
    }

    /// Tap a key through XTEST and say whether the window heard it.
    fn injected_press_lands(&self) -> bool {
        // Drain whatever is queued, so an earlier press cannot answer for
        // this one.
        while self.conn.poll_for_event().ok().flatten().is_some() {}
        let code = 38; // `a` on every layout this runs on.
        for down in [true, false] {
            self.conn
                .xtest_fake_input(if down { 2 } else { 3 }, code, 0, self.root, 0, 0, 0)
                .expect("fake key")
                .check()
                .expect("fake key");
        }
        self.conn.flush().expect("flush");
        for _ in 0..40 {
            while let Ok(Some(event)) = self.conn.poll_for_event() {
                if matches!(event, Event::KeyPress(_)) {
                    return true;
                }
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        false
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        let _ = self.conn.destroy_window(self.window);
        let _ = self.conn.flush();
    }
}

/// Which devices the server says are floating.
fn floating_now(conn: &impl Connection) -> Vec<u16> {
    let devices = conn
        .xinput_xi_query_device(0u16)
        .expect("ask")
        .reply()
        .expect("ask");
    devices
        .infos
        .iter()
        .filter(|d| d.type_ == DeviceType::FLOATING_SLAVE)
        .map(|d| d.deviceid)
        .collect()
}

/// **The devices float, injection keeps working, and they come back.**
///
/// Three assertions and the middle one is the reason this mechanism was chosen
/// over a grab: a grab of the keyboard takes injected events with it, so it
/// would block the remote control the feature exists to protect.
#[test]
fn detaching_floats_the_devices_and_leaves_injection_working() {
    if !may_disturb_the_display() {
        return;
    }
    let _lock = exclusive();
    let rig = Rig::open();

    let plan = match esper_input_x11::floating::plan() {
        Ok(plan) => plan,
        Err(why) => {
            // A display with no physical devices at all — which an `Xvfb`
            // without them is — has nothing to detach, and that is a refusal
            // rather than a silent success.
            println!("skipped: {why}");
            return;
        }
    };
    assert!(!plan.undo.is_empty());

    // Nothing is floating to begin with, or the rest proves nothing.
    let before = floating_now(&rig.conn);
    assert!(
        esper_input_x11::floating::still_attached(&plan)
            .expect("ask")
            .len()
            == plan.undo.len(),
        "some of these devices were already floating: {before:?}"
    );
    assert!(
        rig.injected_press_lands(),
        "injection did not work before anything was detached, so this rig proves nothing"
    );

    esper_input_x11::floating::detach(&plan).expect("detach");
    let attached = esper_input_x11::floating::still_attached(&plan).expect("ask");
    let floating = floating_now(&rig.conn);
    let injected_while_floating = rig.injected_press_lands();

    // Put it back whatever the answers were: a display left with no keyboard
    // breaks every test after this one, and the person running it.
    let trouble = esper_input_x11::floating::reattach(&plan);
    let after = esper_input_x11::floating::still_attached(&plan).expect("ask");

    assert!(
        attached.is_empty(),
        "the server kept {attached:?} attached after being asked to detach them"
    );
    assert!(
        !floating.is_empty(),
        "nothing was floating after detaching: {floating:?}"
    );
    assert!(
        injected_while_floating,
        "remote injection stopped working while the local devices were floating, which is \
         the whole reason a grab was rejected"
    );
    assert!(trouble.is_empty(), "putting the input back: {trouble:?}");
    assert_eq!(
        after.len(),
        plan.undo.len(),
        "the devices did not all come back attached"
    );
}

/// **The plan carries the master each device belonged to**, because by the
/// time somebody has to put one back, nothing can ask what it used to be.
#[test]
fn the_plan_remembers_which_master_each_device_had() {
    if !may_disturb_the_display() {
        return;
    }
    let _lock = exclusive();
    let (conn, _) = esper_display_lock::connect().expect("a display");

    let Ok(plan) = esper_input_x11::floating::plan() else {
        println!("skipped: nothing on this display to detach");
        return;
    };
    let devices = conn
        .xinput_xi_query_device(0u16)
        .expect("ask")
        .reply()
        .expect("ask");
    for undo in &plan.undo {
        let esper_keeper::Undo::Slave { device, master, .. } = undo else {
            panic!("a floating plan carried something that is not a device: {undo:?}");
        };
        let found = devices
            .infos
            .iter()
            .find(|d| d.deviceid == *device)
            .expect("the device the plan named");
        assert_eq!(
            found.attachment,
            *master,
            "the plan remembers the wrong master for {}",
            String::from_utf8_lossy(&found.name)
        );
        assert_ne!(
            found.type_,
            DeviceType::MASTER_KEYBOARD,
            "a master device is in the plan, and detaching one would take the session's input \
             away as well as the person's"
        );
        assert_ne!(found.type_, DeviceType::MASTER_POINTER);
    }
}

/// XTEST's own devices are how the far end types, so they are left alone.
#[test]
fn the_injection_devices_are_not_in_the_plan() {
    if !may_disturb_the_display() {
        return;
    }
    let _lock = exclusive();
    let (conn, _) = esper_display_lock::connect().expect("a display");

    let Ok(plan) = esper_input_x11::floating::plan() else {
        println!("skipped: nothing on this display to detach");
        return;
    };
    let devices = conn
        .xinput_xi_query_device(0u16)
        .expect("ask")
        .reply()
        .expect("ask");
    let named: Vec<String> = plan
        .undo
        .iter()
        .filter_map(|u| match u {
            esper_keeper::Undo::Slave { device, .. } => devices
                .infos
                .iter()
                .find(|d| d.deviceid == *device)
                .map(|d| String::from_utf8_lossy(&d.name).into_owned()),
            _ => None,
        })
        .collect();
    for name in &named {
        assert!(
            !name.contains("XTEST"),
            "{name} is in the plan, and detaching it would block the far end rather than the \
             person at the machine"
        );
    }
    // And it did not simply skip everything.
    assert!(!named.is_empty(), "the plan named nothing: {named:?}");
    let _ = xinput::DeviceType::SLAVE_KEYBOARD;
}
