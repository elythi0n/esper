//! Taking this machine's own keyboard and pointer out of the picture while a
//! session runs.
//!
//! # What this is for
//!
//! The other half of the privacy screen. That one stops somebody walking past
//! from *seeing* a session; this stops them typing into it. The case is a
//! machine in a shared room, being worked on remotely by the person who owns
//! it — and the two halves are wanted together or not at all.
//!
//! # Why detaching, and not a grab
//!
//! A grab was the obvious answer and it cannot work. An active grab of the
//! keyboard redirects key events to the grabbing client, and XTEST's injected
//! events go the same way — so a session would block the remote control it
//! exists to protect. Measured, in
//! `tests/blocking_local_input.rs`: one injected press before the grab, **none
//! during**, one after.
//!
//! Detaching a physical device from its master leaves injection working,
//! because injection is delivered *as the master*. Measured in the same file:
//! one press while floating.
//!
//! # What it costs the person at the machine
//!
//! Their panic key. It is a passive grab of a key *combination* on the root,
//! and a grabbed key arrives from the **master** keyboard — which a floating
//! device no longer has. So the same fact this feature is built on, seen from
//! the other side: the input that reaches nothing includes the input that
//! would have stopped the session. While this is in force a session ends from
//! the far end, on its own clock, or when the host stops.
//!
//! Not fixable from here, and not hidden either: the host says it every time
//! it engages, because somebody who inherited the setting from a policy would
//! otherwise find out at the moment they needed the key.
//!
//! # What is measured here, and what is definitional
//!
//! **Measured**: that the devices are floating afterwards, read back from the
//! server; that injection still works while they are; and that they are
//! attached again when the session ends or the host dies.
//!
//! **Definitional**: that a floating device's input does not reach the
//! desktop. That is what "floating" *is* in XInput2 — a slave with no master
//! has nowhere to send events — and nothing here can prove it by experiment,
//! because nothing here can produce physical input. XTEST is the only
//! injection available and this server delivers a faked event as the master
//! whatever device id it is given.
//!
//! That division is the same one the privacy screen ships with: it verifies
//! that a CRTC is off, and takes "an output that is not scanning out shows
//! nothing" from the layer below. This verifies the state it set, and takes
//! the consequence from the protocol.
//!
//! # Why this cannot exist without the keeper
//!
//! A floating device is server state that outlives the process. A host that
//! dies leaves the person at that machine with no keyboard and no pointer and
//! nothing running to give them back — which is worse than a dark monitor,
//! where somebody can still type blind. So every device is registered with a
//! keeper *before* it is detached, exactly as the screens are, and a keeper
//! that cannot be started means nothing is detached at all.

use esper_input::Floating;
use esper_keeper::Undo;
use x11rb::connection::Connection;
use x11rb::protocol::xinput::{
    self, ConnectionExt as _, DeviceType, HierarchyChange, HierarchyChangeData,
    HierarchyChangeDataAttachSlave, HierarchyChangeDataDetachSlave,
};

/// Which physical devices to detach, and from which master.
///
/// # What counts as one
///
/// A *slave* keyboard or pointer that is attached to a master. Masters are the
/// virtual devices injection is delivered as, and detaching one would take the
/// pointer away from the session as well as from the person. XTEST's own
/// devices are slaves too and are left alone: they are how the far end types.
///
/// # Errors
///
/// If the display cannot be reached, XInput2 is not there, or nothing on this
/// machine could be detached — which is not a state to detach *some* of.
pub fn plan() -> Result<Floating, String> {
    let (conn, _) = x11rb::connect(None).map_err(|e| e.to_string())?;
    let devices = conn
        .xinput_xi_query_device(0u16)
        .map_err(|e| format!("asking what input devices there are: {e}"))?
        .reply()
        .map_err(|e| format!("asking what input devices there are: {e:?}"))?;

    let mut undo = Vec::new();
    let mut names = Vec::new();
    for device in &devices.infos {
        if !is_a_physical_device(device) {
            continue;
        }
        let name = String::from_utf8_lossy(&device.name).into_owned();
        // XTEST's own devices are how the far end types. Detaching them would
        // block the session rather than the person at the machine.
        if name.contains("XTEST") {
            continue;
        }
        undo.push(Undo::Slave {
            device: device.deviceid,
            master: device.attachment,
            what: format!("this machine's {name}"),
        });
        names.push(name);
    }
    if undo.is_empty() {
        return Err("no physical keyboard or pointer on this machine could be detached".to_owned());
    }
    Ok(Floating {
        what: format!("{} local input device(s)", names.len()),
        undo,
    })
}

/// Whether a device is a physical one attached to a master.
fn is_a_physical_device(device: &xinput::XIDeviceInfo) -> bool {
    matches!(
        device.type_,
        DeviceType::SLAVE_KEYBOARD | DeviceType::SLAVE_POINTER
    ) && device.enabled
}

/// Detach every device in the plan.
///
/// **Registered with a keeper first, by the caller.** This does not check
/// that: it cannot, and the one place that calls it is the one place that
/// knows. See the module note.
///
/// # Errors
///
/// If any device will not detach. Everything already detached is put back
/// before returning, because a half-detached machine whose caller got an error
/// is exactly the state nobody is holding the way out of.
pub fn detach(plan: &Floating) -> Result<(), String> {
    let (conn, _) = x11rb::connect(None).map_err(|e| e.to_string())?;
    let mut done: Vec<&Undo> = Vec::new();
    for undo in &plan.undo {
        let Undo::Slave { device, .. } = undo else {
            continue;
        };
        let change = HierarchyChange {
            // Length in four-byte units, which the encoder does not work out
            // for a variant it cannot see the size of.
            len: 2,
            data: HierarchyChangeData::DetachSlave(HierarchyChangeDataDetachSlave {
                deviceid: *device,
            }),
        };
        let asked = conn
            .xinput_xi_change_hierarchy(&[change])
            .map_err(|e| e.to_string())
            .and_then(|c| c.check().map_err(|e| format!("{e:?}")));
        if let Err(e) = asked {
            for back in &done {
                let _ = attach(&conn, back);
            }
            return Err(format!("detaching an input device: {e}"));
        }
        done.push(undo);
    }
    conn.flush().map_err(|e| e.to_string())?;

    // **And read it back.** The claim is that these devices are floating, and
    // the server is the only thing that can say so — a request the server
    // accepted and did not act on would otherwise be a privacy guarantee
    // nobody had.
    let still_attached = attached_of(&conn, &plan.undo)?;
    if !still_attached.is_empty() {
        for back in &plan.undo {
            let _ = attach(&conn, back);
        }
        return Err(format!(
            "the server kept {} device(s) attached after being asked to detach them",
            still_attached.len()
        ));
    }
    Ok(())
}

/// Put every device in the plan back.
///
/// Best effort and in order: one device that will not come back must not stop
/// the others, because the person at the machine wants a keyboard more than
/// they want a tidy error.
pub fn reattach(plan: &Floating) -> Vec<String> {
    let Ok((conn, _)) = x11rb::connect(None) else {
        return vec!["the display could not be reached to give the input back".to_owned()];
    };
    let mut trouble = Vec::new();
    for undo in &plan.undo {
        if let Err(e) = attach(&conn, undo) {
            trouble.push(e);
        }
    }
    let _ = conn.flush();
    trouble
}

/// Which of these devices the server still says have a master.
///
/// Public because the check that matters is "are they floating", and a test
/// should be able to ask the same question the code does rather than a
/// question that looks like it.
///
/// # Errors
///
/// If the display cannot be asked.
pub fn still_attached(plan: &Floating) -> Result<Vec<u16>, String> {
    let (conn, _) = x11rb::connect(None).map_err(|e| e.to_string())?;
    attached_of(&conn, &plan.undo)
}

/// The same, over a connection somebody already has.
fn attached_of<C: Connection>(conn: &C, undo: &[Undo]) -> Result<Vec<u16>, String> {
    let devices = conn
        .xinput_xi_query_device(0u16)
        .map_err(|e| e.to_string())?
        .reply()
        .map_err(|e| format!("{e:?}"))?;
    let wanted: Vec<u16> = undo
        .iter()
        .filter_map(|u| match u {
            Undo::Slave { device, .. } => Some(*device),
            _ => None,
        })
        .collect();
    Ok(devices
        .infos
        .iter()
        .filter(|d| wanted.contains(&d.deviceid))
        // **A floating device is a slave whose type says so.** The server
        // reports `FLOATING_SLAVE` for one with no master, so this asks what
        // it is rather than comparing an attachment number against a master
        // that may itself have gone.
        .filter(|d| d.type_ != DeviceType::FLOATING_SLAVE)
        .map(|d| d.deviceid)
        .collect())
}

/// Attach one device back to its master.
///
/// # Errors
///
/// If the server refuses.
pub fn attach<C: Connection>(conn: &C, undo: &Undo) -> Result<(), String> {
    let Undo::Slave {
        device,
        master,
        what,
    } = undo
    else {
        return Err("this is not an input device to put back".to_owned());
    };
    let change = HierarchyChange {
        // Two four-byte units: the header, then the device and its master.
        // Three was tried and the server answered `Length` — which is the
        // reason to read the *neighbouring* measurement's numbers rather than
        // work them out again.
        len: 2,
        data: HierarchyChangeData::AttachSlave(HierarchyChangeDataAttachSlave {
            deviceid: *device,
            master: *master,
        }),
    };
    conn.xinput_xi_change_hierarchy(&[change])
        .map_err(|e| format!("{what}: {e}"))?
        .check()
        .map_err(|e| format!("{what}: {e:?}"))?;
    Ok(())
}

/// Put one device back, from a process that has nothing else.
///
/// The keeper's half: it runs after the host is gone, so it opens its own
/// connection and asks for nothing it cannot get from the record.
///
/// # Errors
///
/// If the display cannot be reached or the server refuses.
pub fn put_back(undo: &Undo) -> Result<(), String> {
    let (conn, _) = x11rb::connect(None).map_err(|e| e.to_string())?;
    attach(&conn, undo)?;
    conn.flush().map_err(|e| e.to_string())?;
    Ok(())
}
