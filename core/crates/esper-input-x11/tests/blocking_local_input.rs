//! **Why the host's local input is not blocked during a session.**
//!
//! Phase 15 asks for it: block the person at the machine from fighting the
//! person at the far end, with a local override that always wins. This file is
//! the measurement that decided how — and, in the end, that it is not built
//! yet. It is a test rather than a note because the reasoning rests on how a
//! particular X server behaves, and a server that starts behaving differently
//! should tell somebody rather than leave a stale paragraph in a document.
//!
//! # What was measured
//!
//! | mechanism | remote injection | verdict |
//! |---|---|---|
//! | `XIGrabDevice` on the physical slave keyboard | **stops working** | disqualified |
//! | detaching that slave from its master ("floating" it) | keeps working | the only candidate |
//!
//! The first is the obvious implementation and it cannot work: an active grab
//! of the keyboard redirects key events to the grabbing client, and XTEST's
//! injected events go the same way — so a session would block the remote
//! control it exists to protect. That is the useful negative result.
//!
//! # What could not be measured, and why it matters
//!
//! Whether floating a device blocks *physical* input. Nothing here can produce
//! physical input: XTEST is the only injection this environment has, and the
//! control in [`the_device_id_in_a_faked_event_is_ignored`] shows this server
//! accepts a device id that does not exist and delivers the press anyway — so
//! "type as the physical keyboard" is not a thing that can be simulated. Any
//! claim that floating blocks local input would be a claim about the protocol
//! specification, not a measurement, and this project does not ship those.
//!
//! # And the hazard that settles it
//!
//! A floated device is **server state that outlives the process**. A host that
//! is killed leaves the person at that machine with no keyboard and no
//! pointer, and nothing running to give them back — which is worse than the
//! same problem in the privacy screen, where a dark monitor still leaves
//! somebody able to type blind or reboot cleanly. Both features need one
//! missing thing first: something that restores server-side state when the
//! host dies.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::time::Duration;

use x11rb::connection::Connection;
use x11rb::protocol::Event;
use x11rb::protocol::xinput::{self, ConnectionExt as _};
use x11rb::protocol::xproto::{
    ConnectionExt as _, CreateWindowAux, EventMask, GrabMode, GrabStatus, InputFocus, WindowClass,
};
use x11rb::protocol::xtest::ConnectionExt as _;

/// The display these may disturb is somebody's session unless they said so.
fn may_disturb_the_display() -> bool {
    if std::env::var_os("ESPER_DISPLAY_TESTS").is_none() {
        println!("skipped: set ESPER_DISPLAY_TESTS=1 to allow injecting into this display");
        return false;
    }
    true
}

/// A focused window, and a way to inject and count what reaches it.
struct Rig {
    conn: x11rb::rust_connection::RustConnection,
    root: x11rb::protocol::xproto::Window,
    window: x11rb::protocol::xproto::Window,
}

impl Rig {
    fn open() -> Option<Self> {
        let (conn, screen_num) = esper_display_lock::connect().ok()?;
        let screen = &conn.setup().roots[screen_num];
        let (root, depth, visual) = (screen.root, screen.root_depth, screen.root_visual);
        let white = screen.white_pixel;
        let window = conn.generate_id().ok()?;
        conn.create_window(
            depth,
            window,
            root,
            0,
            0,
            200,
            200,
            0,
            WindowClass::INPUT_OUTPUT,
            visual,
            &CreateWindowAux::new()
                .background_pixel(white)
                .event_mask(EventMask::KEY_PRESS | EventMask::KEY_RELEASE),
        )
        .ok()?
        .check()
        .ok()?;
        conn.map_window(window).ok()?.check().ok()?;
        conn.set_input_focus(InputFocus::PARENT, window, x11rb::CURRENT_TIME)
            .ok()?
            .check()
            .ok()?;
        conn.flush().ok()?;
        std::thread::sleep(Duration::from_millis(100));
        Some(Self { conn, root, window })
    }

    /// Inject one keypress as `device`, and count what arrives at the window.
    ///
    /// Errors are **reported** rather than dropped: the first version of this
    /// ignored them, and a refused request looked exactly like an honoured
    /// one — which would have turned every measurement here into a
    /// measurement of the default XTEST device.
    fn press_as(&self, device: u8) -> Result<usize, String> {
        while let Ok(Some(_)) = self.conn.poll_for_event() {}
        self.conn
            .xtest_fake_input(2, 38, 0, self.root, 0, 0, device)
            .map_err(|e| e.to_string())?
            .check()
            .map_err(|e| format!("{e:?}"))?;
        let _ = self
            .conn
            .xtest_fake_input(3, 38, 0, self.root, 0, 0, device)
            .map(|c| c.check());
        self.conn.flush().map_err(|e| e.to_string())?;
        let mut presses = 0;
        for _ in 0..40 {
            while let Ok(Some(event)) = self.conn.poll_for_event() {
                if matches!(event, Event::KeyPress(_)) {
                    presses += 1;
                }
            }
            if presses > 0 {
                break;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        Ok(presses)
    }

    /// Every physical slave keyboard: the XTEST ones are not physical.
    fn physical_keyboards(&self) -> Vec<u16> {
        let Ok(devices) = self
            .conn
            .xinput_xi_query_device(0u16)
            .map_err(|e| e.to_string())
            .and_then(|c| c.reply().map_err(|e| e.to_string()))
        else {
            return Vec::new();
        };
        devices
            .infos
            .iter()
            .filter(|d| {
                d.type_ == xinput::DeviceType::SLAVE_KEYBOARD
                    && !String::from_utf8_lossy(&d.name).contains("XTEST")
            })
            .map(|d| d.deviceid)
            .collect()
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        let _ = self.conn.destroy_window(self.window);
        let _ = self.conn.flush();
    }
}

fn detach(rig: &Rig, id: u16) -> bool {
    let change = xinput::HierarchyChange {
        len: 2,
        data: xinput::HierarchyChangeData::DetachSlave(xinput::HierarchyChangeDataDetachSlave {
            deviceid: id,
        }),
    };
    rig.conn
        .xinput_xi_change_hierarchy(&[change])
        .is_ok_and(|c| c.check().is_ok())
}

fn attach(rig: &Rig, id: u16, master: u16) {
    let change = xinput::HierarchyChange {
        len: 2,
        data: xinput::HierarchyChangeData::AttachSlave(xinput::HierarchyChangeDataAttachSlave {
            deviceid: id,
            master,
        }),
    };
    let _ = rig
        .conn
        .xinput_xi_change_hierarchy(&[change])
        .map(|c| c.check());
    let _ = rig.conn.flush();
}

/// **Grabbing the physical keyboard stops the remote end typing.**
///
/// The obvious way to stop the person at the machine interfering is to grab
/// their keyboard. It cannot be used: an active grab redirects key events to
/// the grabbing client, and XTEST's injected events are redirected with them —
/// so the session would lose exactly the ability it exists to provide.
///
/// If a future X server delivers injected events through a grab, this test
/// fails and the mechanism is back on the table.
#[test]
fn grabbing_the_physical_keyboard_also_blocks_the_remote_end() {
    if !may_disturb_the_display() {
        return;
    }
    let _display = esper_display_lock::DisplayLock::take();
    let Some(rig) = Rig::open() else {
        println!("skipped: no display");
        return;
    };
    let physical = rig.physical_keyboards();
    if physical.is_empty() {
        println!("skipped: this server reports no physical slave keyboard");
        return;
    }
    assert_eq!(
        rig.press_as(0),
        Ok(1),
        "injection has to work before a grab, or this measures nothing"
    );

    let mask =
        [u32::from(xinput::XIEventMask::KEY_PRESS) | u32::from(xinput::XIEventMask::KEY_RELEASE)];
    let mut grabbed = Vec::new();
    for id in &physical {
        let asked = rig
            .conn
            .xinput_xi_grab_device(
                rig.root,
                x11rb::CURRENT_TIME,
                0u32,
                *id,
                GrabMode::ASYNC,
                GrabMode::ASYNC,
                xinput::GrabOwner::OWNER,
                &mask,
            )
            .map_err(|e| e.to_string())
            .and_then(|c| c.reply().map_err(|e| e.to_string()));
        if asked.is_ok_and(|reply| reply.status == GrabStatus::SUCCESS) {
            grabbed.push(*id);
        }
    }
    if grabbed.is_empty() {
        println!("skipped: no physical keyboard could be grabbed");
        return;
    }

    let during = rig.press_as(0);
    for id in &grabbed {
        let _ = rig
            .conn
            .xinput_xi_ungrab_device(x11rb::CURRENT_TIME, *id)
            .map(|c| c.check());
    }
    let _ = rig.conn.flush();
    std::thread::sleep(Duration::from_millis(100));

    assert_eq!(
        during,
        Ok(0),
        "a grab of the physical keyboard let injected events through, which would make \
         device grabs a usable way to block local input"
    );
    assert_eq!(
        rig.press_as(0),
        Ok(1),
        "and injection has to come back once the grab is released"
    );
}

/// **Detaching the physical keyboard leaves the remote end working.**
///
/// The only candidate mechanism, and the reason it is a candidate. What this
/// does *not* show is that local input is blocked — see this file's own note,
/// and the control below.
#[test]
fn detaching_the_physical_keyboard_leaves_injection_alone() {
    if !may_disturb_the_display() {
        return;
    }
    let _display = esper_display_lock::DisplayLock::take();
    let Some(rig) = Rig::open() else {
        println!("skipped: no display");
        return;
    };
    let physical = rig.physical_keyboards();
    let Some(id) = physical.first().copied() else {
        println!("skipped: this server reports no physical slave keyboard");
        return;
    };
    // Which master it belongs to, so it can be put back exactly there.
    let master = rig
        .conn
        .xinput_xi_query_device(id)
        .map_err(|e| e.to_string())
        .and_then(|c| c.reply().map_err(|e| e.to_string()))
        .ok()
        .and_then(|r| r.infos.first().map(|d| d.attachment))
        .unwrap_or(3);

    if !detach(&rig, id) {
        println!("skipped: this server would not detach the device");
        return;
    }
    let while_floating = rig.press_as(0);
    attach(&rig, id, master);
    std::thread::sleep(Duration::from_millis(100));
    let after = rig.press_as(0);

    assert_eq!(
        while_floating,
        Ok(1),
        "injection stopped while the physical device was floating, which would rule out the \
         one remaining mechanism"
    );
    assert_eq!(after, Ok(1), "and it still works once the device is back");
}

/// **The control that stopped a vacuous measurement from shipping.**
///
/// XTEST's `FakeInput` carries a device id, which looks like a way to simulate
/// "the person at the machine typed". It is not: this server accepts a device
/// id that does not exist, reports no error, and delivers the press through its
/// own virtual device. So no test here can distinguish local input from
/// injected input, and the claim that floating a device blocks the person at
/// the keyboard cannot be made from this environment.
///
/// A server that starts refusing a nonexistent device would make the
/// simulation worth trying again — which is why this is a test and not a
/// sentence in a document.
#[test]
fn the_device_id_in_a_faked_event_is_ignored() {
    if !may_disturb_the_display() {
        return;
    }
    let _display = esper_display_lock::DisplayLock::take();
    let Some(rig) = Rig::open() else {
        println!("skipped: no display");
        return;
    };
    let nonsense = rig.press_as(99);
    assert_eq!(
        nonsense,
        Ok(1),
        "this server distinguished a device id that does not exist — which means faking input \
         as a named device may now be possible, and the blocking half of this feature could be \
         measured after all"
    );
}
