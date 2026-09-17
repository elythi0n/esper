//! The kernel's `uinput` ABI, declared rather than bound.
//!
//! `linux-raw-sys` does not carry `<linux/uinput.h>` — there is no feature for
//! it — so these are written out. That is less alarming than it sounds: this
//! is the **stable** kernel/userspace boundary, the one Linux promises not to
//! break, and the structures here have not changed since `uinput_setup`
//! replaced the old write-a-struct interface in 4.5 (2016).
//!
//! What is worth being careful about is `input_event`'s timestamp. It is a
//! `struct timeval` of two `__kernel_long_t`, which is 8 bytes each on a
//! 64-bit kernel and 4 on a 32-bit one — so the struct is 24 bytes or 16
//! depending on the architecture, and a hard-coded 24 would write garbage on
//! 32-bit ARM. `c_long` is the type that tracks that.
//!
//! The kernel ignores the timestamp on the way *in* and stamps events itself,
//! so these are written as zero. It is in the struct because the layout
//! requires it, not because anything reads it.

use core::ffi::c_long;

/// `UINPUT_MAX_NAME_SIZE`.
pub const MAX_NAME: usize = 80;

/// Event types, from `input-event-codes.h`.
pub const EV_SYN: u16 = 0x00;
pub const EV_KEY: u16 = 0x01;
pub const EV_REL: u16 = 0x02;
pub const EV_ABS: u16 = 0x03;

/// `SYN_REPORT`: everything since the last one is one atomic change.
pub const SYN_REPORT: u16 = 0;

/// Relative axes. The wheel is in notches; the high-resolution axes are
/// deliberately not used — see the sink's note on scrolling.
pub const REL_HWHEEL: u16 = 0x06;
pub const REL_WHEEL: u16 = 0x08;

/// Absolute axes.
pub const ABS_X: u16 = 0x00;
pub const ABS_Y: u16 = 0x01;

/// Buttons, the same five the protocol names.
pub const BTN_LEFT: u16 = 0x110;
pub const BTN_RIGHT: u16 = 0x111;
pub const BTN_MIDDLE: u16 = 0x112;
pub const BTN_SIDE: u16 = 0x113;
pub const BTN_EXTRA: u16 = 0x114;

/// `BUS_VIRTUAL`. **Honest about what this is**: a device that says it is on
/// the USB bus would be telling `libinput` to apply quirks written for real
/// hardware, and telling a person reading `libinput list-devices` something
/// untrue about their machine.
pub const BUS_VIRTUAL: u16 = 0x06;

/// `struct input_id`.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct InputId {
    pub bustype: u16,
    pub vendor: u16,
    pub product: u16,
    pub version: u16,
}

/// `struct uinput_setup`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Setup {
    pub id: InputId,
    /// NUL-padded, not NUL-terminated by the type: the kernel reads up to the
    /// first NUL or the end of the array.
    pub name: [u8; MAX_NAME],
    pub ff_effects_max: u32,
}

/// `struct input_absinfo`.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct AbsInfo {
    pub value: i32,
    pub minimum: i32,
    pub maximum: i32,
    /// Noise the kernel should smooth away. Zero: there is no noise in a
    /// number that came off a network.
    pub fuzz: i32,
    /// A dead zone around the centre, for a joystick. Zero for a pointer.
    pub flat: i32,
    /// Units per millimetre. Zero means "do not know", which is the truth for
    /// a device that has no physical size.
    pub resolution: i32,
}

/// `struct uinput_abs_setup`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct AbsSetup {
    pub code: u16,
    pub absinfo: AbsInfo,
}

/// `struct input_event`.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct Event {
    /// `struct timeval`, ignored by the kernel on input and stamped by it.
    pub tv_sec: c_long,
    pub tv_usec: c_long,
    pub kind: u16,
    pub code: u16,
    pub value: i32,
}

impl Event {
    /// One event, with the timestamp the kernel is going to overwrite.
    #[must_use]
    pub const fn new(kind: u16, code: u16, value: i32) -> Self {
        Self {
            tv_sec: 0,
            tv_usec: 0,
            kind,
            code,
            value,
        }
    }

    /// The event that ends a batch. **Everything before it is applied at
    /// once**, which is what stops a pointer move being seen as two separate
    /// axis changes and a diagonal drag as a staircase.
    #[must_use]
    pub const fn sync() -> Self {
        Self::new(EV_SYN, SYN_REPORT, 0)
    }
}
