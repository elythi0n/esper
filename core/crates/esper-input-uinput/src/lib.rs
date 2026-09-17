//! Injecting input through the kernel's own virtual device.
//!
//! # Where this sits in the ladder, and why it is last
//!
//! Every other backend asks something: X11 asks an X server, wlroots asks a
//! compositor, the portal asks a person. This one asks the **kernel**, and the
//! kernel does not arbitrate — a `uinput` device is indistinguishable from a
//! keyboard somebody plugged in, and every display server on the machine will
//! take its events without knowing where they came from.
//!
//! That is exactly why it is the last rung and why it is not the first. It
//! works where nothing else does — a GNOME session with no XWayland, a kiosk,
//! a compositor that implements no remote-control protocol at all — and it
//! works by going *under* the thing that would otherwise have had a say.
//!
//! # It is not a way around a refusal
//!
//! A person who declines the portal's dialog has declined. This backend must
//! not turn that into a yes, and it does not: the ladder reaches here only when
//! the earlier rungs found **no backend at all**, not when one refused. The
//! distinction lives in `injector::open`, and it is the reason this crate has
//! no "try harder" entry point — there is one constructor and it does the same
//! thing every time.
//!
//! What makes that honest rather than a promise is that the machine's owner
//! decides whether it can work at all: a `uinput` node this process cannot
//! open is a rung that is simply not there. See [`Trouble::advice`].
//!
//! # Absolute, not relative
//!
//! A remote pointer is told *where to be*, never how far to move. Relative
//! motion would inherit the desktop's pointer acceleration — a curve applied
//! to a delta that never came from a hand — so the same remote movement would
//! land somewhere different depending on how fast the previous one was. The
//! device declares `ABS_X`/`ABS_Y` over a fixed `0..=65535` range and the
//! sink normalises pixels onto it with [`esper_input::absolute`], which is the
//! same arithmetic the Windows backend uses and the same reasoning about the
//! last pixel reaching the end of the range exactly.
//!
//! # Unsafe inventory
//!
//! Every block here is one `ioctl`, and they share a shape: `rustix`'s typed
//! wrappers carry the pointer and the length, so what each block asserts is
//! only that *this opcode goes with this argument type*. That is a fact about
//! `<linux/uinput.h>`, written beside each constant in [`abi`], and it is the
//! whole of what cannot be checked by the compiler. The file descriptor is
//! owned by this struct and outlives every call, and no pointer escapes one.
//!
//! One entry per block rather than one per opcode, because that is what makes
//! the count checkable — two calls of the same opcode are two places a mistake
//! can be.
//!
//! - **`UI_SET_EVBIT`, declaring the four event types.** `_IOW('U', 100, int)`,
//!   taking an event type by value. A type outside `EV_*` is refused by the
//!   kernel rather than being undefined.
//! - **`UI_SET_KEYBIT`, declaring every key in the HID table.**
//!   `_IOW('U', 101, int)`, taking a key code by value. The codes come from
//!   `esper_input::every_evdev`, which is the same table the injector maps
//!   through, so a code it can produce is a code the device declared.
//! - **`UI_SET_KEYBIT`, declaring the five buttons.** The same opcode and the
//!   same invariant; a button is a key code in the same space.
//! - **`UI_SET_RELBIT`, declaring the two scroll axes.**
//!   `_IOW('U', 102, int)`, taking a relative axis by value.
//! - **`UI_SET_ABSBIT`, declaring the two pointer axes.**
//!   `_IOW('U', 103, int)`, taking an absolute axis by value.
//! - **`UI_ABS_SETUP`, giving each pointer axis its range.**
//!   `_IOW('U', 4, struct uinput_abs_setup)`. [`abi::AbsSetup`] is
//!   `#[repr(C)]` and every field is written before the call.
//! - **`UI_DEV_SETUP`, naming the device.**
//!   `_IOW('U', 3, struct uinput_setup)`. [`abi::Setup`] is `#[repr(C)]`, and
//!   its `name` is a fixed array filled whole, so no uninitialised byte
//!   reaches the kernel.
//! - **`UI_DEV_CREATE`.** `_IO('U', 1)`, no argument. It is the point at which
//!   the device appears to the rest of the machine, and every capability above
//!   must already have been declared: the shape is frozen here.
//! - **`UI_DEV_DESTROY`, in `Drop`.** `_IO('U', 2)`, no argument. The
//!   descriptor is still open — the struct's fields are dropped after this
//!   body — and a device left behind would be a keyboard on somebody's machine
//!   that nothing owns.

#![cfg(target_os = "linux")]

pub mod abi;

use std::os::fd::AsFd as _;

use esper_input::absolute::{self, Desk};
use esper_input::{Evdev, Held, InputError, InputSink, Point, evdev_for_usage, every_evdev};
use esper_proto::{Button, KeyEvent, PointerButton, Scroll};
use rustix::ioctl;

/// The backend name, for the log and the session indicator.
pub const BACKEND: &str = "uinput";

/// What the device calls itself.
///
/// It appears in `libinput list-devices`, in `/proc/bus/input/devices`, and in
/// any desktop's list of input hardware — so it says what it is and which
/// program made it. A person who finds an unexplained keyboard on their
/// machine should be one search away from the answer.
const DEVICE_NAME: &str = "Esper remote input";

/// The node the kernel offers, and the one almost every distribution ships.
const NODE: &str = "/dev/uinput";

/// The older path, for kernels and distributions that still use it.
const OLD_NODE: &str = "/dev/input/uinput";

/// `_IOC` encoding, group `'U'` — `UINPUT_IOCTL_BASE`.
const UINPUT: u8 = b'U';

// The opcodes. Each is `_IO`/`_IOW` from `<linux/uinput.h>`; the number is the
// second argument and the payload type is what the safety note above pins.
const UI_DEV_CREATE: ioctl::Opcode = ioctl::opcode::none(UINPUT, 1);
const UI_DEV_DESTROY: ioctl::Opcode = ioctl::opcode::none(UINPUT, 2);
const UI_DEV_SETUP: ioctl::Opcode = ioctl::opcode::write::<abi::Setup>(UINPUT, 3);
const UI_ABS_SETUP: ioctl::Opcode = ioctl::opcode::write::<abi::AbsSetup>(UINPUT, 4);
const UI_SET_EVBIT: ioctl::Opcode = ioctl::opcode::write::<i32>(UINPUT, 100);
const UI_SET_KEYBIT: ioctl::Opcode = ioctl::opcode::write::<i32>(UINPUT, 101);
const UI_SET_RELBIT: ioctl::Opcode = ioctl::opcode::write::<i32>(UINPUT, 102);
const UI_SET_ABSBIT: ioctl::Opcode = ioctl::opcode::write::<i32>(UINPUT, 103);

/// Why the kernel's virtual device could not be used.
///
/// Separate from [`InputError`] because the *reason* decides what somebody
/// should do about it, and only this crate knows which reason it was.
#[derive(Debug, thiserror::Error)]
pub enum Trouble {
    /// No `uinput` node at all.
    #[error("this kernel has no {NODE}")]
    NoNode,
    /// The node is there and this process may not open it.
    #[error("{NODE} is there and this user may not open it")]
    NotPermitted,
    /// The node opened and the kernel refused to configure the device.
    #[error("the kernel refused to set up a virtual device: {0}")]
    Refused(String),
    /// Opening failed for a reason that is neither of the first two.
    #[error("could not open {NODE}: {0}")]
    Unopenable(String),
}

impl Trouble {
    /// What somebody at this machine would have to do, if anything can be done.
    ///
    /// **The permission case is the one worth spelling out.** `uinput` is
    /// root-only on most distributions, and the two ways to change that are
    /// not equivalent: adding a group is a decision about this machine that
    /// survives reboots and is visible in `/etc/group`, and a udev rule is the
    /// thing that makes the group mean anything. Telling somebody only half of
    /// it leaves them with a group that changes nothing and no idea why.
    #[must_use]
    pub fn advice(&self) -> Option<String> {
        match self {
            Self::NotPermitted => Some(format!(
                "`{NODE}` exists and this user cannot open it, which is the \
                 default on most distributions.\n\
                 \x20 To allow it, on the host:\n\
                 \x20   sudo groupadd -f uinput\n\
                 \x20   sudo usermod -aG uinput $USER\n\
                 \x20   echo 'KERNEL==\"uinput\", GROUP=\"uinput\", MODE=\"0660\", \
                 OPTIONS+=\"static_node=uinput\"' \\\n\
                 \x20     | sudo tee /etc/udev/rules.d/60-esper-uinput.rules\n\
                 \x20   sudo udevadm control --reload-rules && sudo udevadm trigger\n\
                 \x20 Then log out and back in, so the new group is in effect.\n\
                 \x20 The group alone does nothing without the rule: the node is \
                 recreated at boot with the kernel's own ownership."
            )),
            Self::NoNode => Some(
                "The `uinput` module is not loaded. On the host: `sudo modprobe uinput`, \
                 and add it to `/etc/modules-load.d/` to survive a reboot."
                    .to_owned(),
            ),
            Self::Refused(_) | Self::Unopenable(_) => None,
        }
    }
}

/// Whether this machine would let a device be made, without making one.
///
/// **Opens the node and closes it.** A caller asking "could an unattended
/// session inject here" must not leave a keyboard on somebody's machine to
/// find out, and must not pay for `UI_DEV_CREATE` either — the question is
/// about permission, and permission is answered by `open`.
///
/// # Errors
///
/// [`Trouble`], whose [`Trouble::advice`] says what somebody at the machine
/// would have to do.
pub fn available() -> Result<(), Trouble> {
    UinputSink::open_node().map(drop)
}

/// A virtual keyboard and pointer, owned by this process.
#[derive(Debug)]
pub struct UinputSink {
    /// The open node. Dropping it after `UI_DEV_DESTROY` is what removes the
    /// device; dropping it *without* destroying would also remove it, and the
    /// explicit destroy is there so the order is stated rather than relied on.
    node: std::fs::File,
    /// The desktop these coordinates are in, for normalising onto the axes.
    desk: Desk,
    /// What is currently down, so it can be let go of.
    held: Held,
}

impl UinputSink {
    /// Open the node and create the device, for a desktop of this size.
    ///
    /// # Errors
    ///
    /// [`Trouble`], whose [`Trouble::advice`] says what can be done about it.
    pub fn open_with_size(width: i32, height: i32) -> Result<Self, Trouble> {
        let node = Self::open_node()?;
        let sink = Self {
            node,
            desk: Desk {
                left: 0,
                top: 0,
                width: width.max(1),
                height: height.max(1),
            },
            held: Held::default(),
        };
        sink.declare()?;
        Ok(sink)
    }

    /// The same, for a desktop whose size is not known yet.
    ///
    /// **1920x1080 is a guess and is documented as one.** It is only ever
    /// wrong by a scale factor — a pointer told to go to the middle goes to
    /// the middle — because the axes are a fixed `0..=65535` range and the
    /// guess only affects how pixels are mapped onto it. `injector::open` has
    /// no way to learn the desktop's extent; a caller that knows it should say
    /// so with [`Self::open_with_size`].
    ///
    /// # Errors
    ///
    /// As [`Self::open_with_size`].
    pub fn open() -> Result<Self, Trouble> {
        Self::open_with_size(1920, 1080)
    }

    /// Open `/dev/uinput`, or say precisely why not.
    fn open_node() -> Result<std::fs::File, Trouble> {
        let mut last = None;
        for path in [NODE, OLD_NODE] {
            match std::fs::OpenOptions::new().write(true).open(path) {
                Ok(file) => return Ok(file),
                Err(e) => last = Some(e),
            }
        }
        // **The kind is the whole diagnosis.** "Permission denied" is a
        // machine that could do this and has not been told to; "not found" is
        // a kernel without the module. Collapsing them into one message is how
        // somebody spends an afternoon loading a module they already have.
        Err(match last {
            Some(e) if e.kind() == std::io::ErrorKind::PermissionDenied => Trouble::NotPermitted,
            Some(e) if e.kind() == std::io::ErrorKind::NotFound => Trouble::NoNode,
            Some(e) => Trouble::Unopenable(e.to_string()),
            None => Trouble::NoNode,
        })
    }

    /// Tell the kernel what this device can do, then create it.
    ///
    /// **Every capability is declared before creation and none afterwards.**
    /// `UI_DEV_CREATE` freezes the device's shape: a key that was not declared
    /// is silently dropped for the life of the device, which from the far end
    /// looks like one key on the keyboard not working.
    fn declare(&self) -> Result<(), Trouble> {
        let fd = self.node.as_fd();
        fn refused(what: &'static str) -> impl Fn(rustix::io::Errno) -> Trouble {
            move |e| Trouble::Refused(format!("{what}: {e}"))
        }

        for kind in [abi::EV_KEY, abi::EV_REL, abi::EV_ABS, abi::EV_SYN] {
            // SAFETY: see the unsafe inventory. `UI_SET_EVBIT` is
            // `_IOW('U', 100, int)` and takes an event type by value.
            #[allow(unsafe_code)]
            unsafe {
                ioctl::ioctl(
                    fd,
                    ioctl::IntegerSetter::<UI_SET_EVBIT>::new_usize(kind as usize),
                )
            }
            .map_err(refused("declaring an event type"))?;
        }

        // **Every key the HID table can produce, plus the five buttons.** The
        // alternative — declaring keys lazily as they are first pressed — is
        // impossible, because the shape is frozen at creation. So the device
        // claims the whole table, and a key nobody ever presses costs a bit in
        // a bitmap.
        for code in every_evdev() {
            // SAFETY: see the unsafe inventory. `UI_SET_KEYBIT` is
            // `_IOW('U', 101, int)` and takes a key code by value.
            #[allow(unsafe_code)]
            unsafe {
                ioctl::ioctl(
                    fd,
                    ioctl::IntegerSetter::<UI_SET_KEYBIT>::new_usize(code.0 as usize),
                )
            }
            .map_err(refused("declaring a key"))?;
        }
        for button in [
            abi::BTN_LEFT,
            abi::BTN_RIGHT,
            abi::BTN_MIDDLE,
            abi::BTN_SIDE,
            abi::BTN_EXTRA,
        ] {
            // SAFETY: see the unsafe inventory, as above — a button is a key
            // code in the same space.
            #[allow(unsafe_code)]
            unsafe {
                ioctl::ioctl(
                    fd,
                    ioctl::IntegerSetter::<UI_SET_KEYBIT>::new_usize(button as usize),
                )
            }
            .map_err(refused("declaring a button"))?;
        }

        for axis in [abi::REL_WHEEL, abi::REL_HWHEEL] {
            // SAFETY: see the unsafe inventory. `UI_SET_RELBIT` is
            // `_IOW('U', 102, int)` and takes a relative axis by value.
            #[allow(unsafe_code)]
            unsafe {
                ioctl::ioctl(
                    fd,
                    ioctl::IntegerSetter::<UI_SET_RELBIT>::new_usize(axis as usize),
                )
            }
            .map_err(refused("declaring a scroll axis"))?;
        }

        for axis in [abi::ABS_X, abi::ABS_Y] {
            // SAFETY: see the unsafe inventory. `UI_SET_ABSBIT` is
            // `_IOW('U', 103, int)` and takes an absolute axis by value.
            #[allow(unsafe_code)]
            unsafe {
                ioctl::ioctl(
                    fd,
                    ioctl::IntegerSetter::<UI_SET_ABSBIT>::new_usize(axis as usize),
                )
            }
            .map_err(refused("declaring a pointer axis"))?;

            let setup = abi::AbsSetup {
                code: axis,
                absinfo: abi::AbsInfo {
                    minimum: 0,
                    maximum: absolute::FULL,
                    ..abi::AbsInfo::default()
                },
            };
            // SAFETY: see the unsafe inventory. `UI_ABS_SETUP` is
            // `_IOW('U', 4, struct uinput_abs_setup)`, and `setup` is a
            // `#[repr(C)]` value of exactly that type, fully initialised.
            #[allow(unsafe_code)]
            unsafe { ioctl::ioctl(fd, ioctl::Setter::<UI_ABS_SETUP, abi::AbsSetup>::new(setup)) }
                .map_err(refused("setting a pointer axis range"))?;
        }

        let mut name = [0u8; abi::MAX_NAME];
        for (slot, byte) in name.iter_mut().zip(DEVICE_NAME.bytes()) {
            *slot = byte;
        }
        let setup = abi::Setup {
            id: abi::InputId {
                bustype: abi::BUS_VIRTUAL,
                ..abi::InputId::default()
            },
            name,
            ff_effects_max: 0,
        };
        // SAFETY: see the unsafe inventory. `UI_DEV_SETUP` is
        // `_IOW('U', 3, struct uinput_setup)`, and `setup` is a `#[repr(C)]`
        // value of exactly that type whose every byte was written above.
        #[allow(unsafe_code)]
        unsafe { ioctl::ioctl(fd, ioctl::Setter::<UI_DEV_SETUP, abi::Setup>::new(setup)) }
            .map_err(refused("naming the device"))?;

        // SAFETY: see the unsafe inventory. `UI_DEV_CREATE` is `_IO('U', 1)`
        // and takes no argument.
        #[allow(unsafe_code)]
        unsafe { ioctl::ioctl(fd, ioctl::NoArg::<UI_DEV_CREATE>::new()) }
            .map_err(refused("creating the device"))?;
        Ok(())
    }

    /// Write one batch, ending with the sync that applies it.
    fn emit(&mut self, events: &[abi::Event]) -> Result<(), InputError> {
        let mut bytes = Vec::with_capacity((events.len() + 1) * size_of::<abi::Event>());
        for event in events.iter().copied().chain([abi::Event::sync()]) {
            bytes.extend_from_slice(&as_bytes(event));
        }
        // **One write for the whole batch.** The kernel reads whole events and
        // a short write would leave half of one in the pipe, which desynchronises
        // every event after it for the life of the device.
        rustix::io::write(self.node.as_fd(), &bytes)
            .map_err(|e| InputError::Backend {
                backend: BACKEND,
                detail: e.to_string(),
            })
            .and_then(|written| {
                if written == bytes.len() {
                    Ok(())
                } else {
                    Err(InputError::Backend {
                        backend: BACKEND,
                        detail: format!("wrote {written} of {} bytes", bytes.len()),
                    })
                }
            })
    }
}

/// One event as the bytes the kernel reads.
///
/// A byte copy of a `#[repr(C)]` value with no padding holes that carry
/// meaning — every field is written before this is called, so nothing
/// uninitialised is observed.
fn as_bytes(event: abi::Event) -> [u8; size_of::<abi::Event>()] {
    let mut out = [0u8; size_of::<abi::Event>()];
    let (time, rest) = out.split_at_mut(2 * size_of::<core::ffi::c_long>());
    let (secs, usecs) = time.split_at_mut(size_of::<core::ffi::c_long>());
    secs.copy_from_slice(&event.tv_sec.to_ne_bytes());
    usecs.copy_from_slice(&event.tv_usec.to_ne_bytes());
    if let Some(chunk) = rest.get_mut(0..2) {
        chunk.copy_from_slice(&event.kind.to_ne_bytes());
    }
    if let Some(chunk) = rest.get_mut(2..4) {
        chunk.copy_from_slice(&event.code.to_ne_bytes());
    }
    if let Some(chunk) = rest.get_mut(4..8) {
        chunk.copy_from_slice(&event.value.to_ne_bytes());
    }
    out
}

/// The evdev code for one of the protocol's five buttons.
const fn button_code(button: Button) -> u16 {
    match button {
        Button::Left => abi::BTN_LEFT,
        Button::Right => abi::BTN_RIGHT,
        Button::Middle => abi::BTN_MIDDLE,
        Button::Back => abi::BTN_SIDE,
        Button::Forward => abi::BTN_EXTRA,
    }
}

impl InputSink for UinputSink {
    fn backend(&self) -> &'static str {
        BACKEND
    }

    fn key(&mut self, event: KeyEvent) -> Result<(), InputError> {
        let Some(Evdev(code)) = evdev_for_usage(event.usage) else {
            return Err(InputError::NoSuchKey { usage: event.usage });
        };
        self.emit(&[abi::Event::new(abi::EV_KEY, code, i32::from(event.pressed))])?;
        self.held.key(event.usage, event.pressed);
        Ok(())
    }

    fn pointer(&mut self, at: Point) -> Result<(), InputError> {
        // A desk that makes no sense is a bug in whoever set the size, not a
        // reason to put the pointer somewhere arbitrary.
        let Some(to) = absolute::onto(self.desk, at.x, at.y) else {
            return Err(InputError::Backend {
                backend: BACKEND,
                detail: format!(
                    "a desktop of {}x{} has no positions in it",
                    self.desk.width, self.desk.height
                ),
            });
        };
        self.emit(&[
            abi::Event::new(abi::EV_ABS, abi::ABS_X, to.x),
            abi::Event::new(abi::EV_ABS, abi::ABS_Y, to.y),
        ])
    }

    fn button(&mut self, event: PointerButton) -> Result<(), InputError> {
        self.emit(&[abi::Event::new(
            abi::EV_KEY,
            button_code(event.button),
            i32::from(event.pressed),
        )])?;
        self.held.button(event.button, event.pressed);
        Ok(())
    }

    fn scroll(&mut self, scroll: Scroll) -> Result<(), InputError> {
        // **Notches, and the vertical one is inverted here.** The protocol is
        // positive-down; evdev's `REL_WHEEL` is positive-*away*, the same
        // convention the Windows backend has to invert for. This is the only
        // place it happens on this path — see `esper_input::wheel`.
        let mut events = Vec::new();
        if scroll.dy_notches != 0 {
            events.push(abi::Event::new(
                abi::EV_REL,
                abi::REL_WHEEL,
                -i32::from(scroll.dy_notches),
            ));
        }
        if scroll.dx_notches != 0 {
            events.push(abi::Event::new(
                abi::EV_REL,
                abi::REL_HWHEEL,
                i32::from(scroll.dx_notches),
            ));
        }
        if events.is_empty() {
            return Ok(());
        }
        self.emit(&events)
    }

    fn release_all(&mut self) -> Result<(), InputError> {
        let (keys, buttons) = self.held.to_release();
        let mut events = Vec::new();
        for usage in keys {
            if let Some(Evdev(code)) = evdev_for_usage(usage) {
                events.push(abi::Event::new(abi::EV_KEY, code, 0));
            }
        }
        for button in buttons {
            events.push(abi::Event::new(abi::EV_KEY, button_code(button), 0));
        }
        if !events.is_empty() {
            self.emit(&events)?;
        }
        self.held.clear();
        Ok(())
    }

    fn held(&self) -> &Held {
        &self.held
    }
}

impl Drop for UinputSink {
    fn drop(&mut self) {
        // **Let go of anything still down, then take the device away.** A
        // device removed with a key held leaves the key held: the kernel has
        // no release to deliver and the desktop keeps repeating it, which is
        // the one failure here that a person cannot fix without a reboot or a
        // second keyboard.
        let _ = self.release_all();
        // SAFETY: see the unsafe inventory. `UI_DEV_DESTROY` is `_IO('U', 2)`
        // and takes no argument; the descriptor is still open until this
        // struct's fields are dropped after this body.
        #[allow(unsafe_code)]
        let gone =
            unsafe { ioctl::ioctl(self.node.as_fd(), ioctl::NoArg::<UI_DEV_DESTROY>::new()) };
        if let Err(e) = gone {
            tracing::warn!(error = %e, "a virtual input device could not be removed");
        }
    }
}
