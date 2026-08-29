//! Input on a wlroots compositor, straight over the Wayland socket.
//!
//! # Why this is not the portal
//!
//! `xdg-desktop-portal-wlr` implements ScreenCast and nothing else, so on sway,
//! Hyprland and river there is no `RemoteDesktop` portal to ask. Measured on
//! 2026-09-12 in a real sway session: the compositor offers
//! `zwp_virtual_keyboard_manager_v1` and `zwlr_virtual_pointer_manager_v1`
//! directly, with no D-Bus, no privilege and no consent dialog.
//!
//! The absent dialog is the honest cost of this path. The compositor grants the
//! protocol to anything that can open its socket, which is any process running
//! as the same user — so there is no prompt to approve and nothing in the
//! desktop's own UI to revoke. That is weaker than the portal and stronger than
//! `uinput`, which needs no permission from the compositor at all, and it is
//! why the ladder tries the portal first where one exists.
//!
//! # The keymap
//!
//! `zwp_virtual_keyboard_v1` will not accept a key until it has been told what
//! the keycodes mean, and the protocol makes the *client* supply that.
//!
//! This project sends **where a key is** — a HID usage from page 0x07 — rather
//! than what it produces, so the host must interpret a keycode exactly as its
//! own keyboard's would. Shipping a fixed "us" keymap would quietly force that
//! layout on every host, and somebody typing on a Finnish keyboard would find
//! their remote session producing the wrong characters.
//!
//! So the seat's own keymap is read from `wl_keyboard` and handed straight
//! back. A seat with no keyboard — a headless machine with no input devices —
//! has no keymap to pass on, and there a minimal one is used and said to be in
//! use.

// Unix only, and empty elsewhere.
//
// A session bus is a unix socket, the layer shell and the virtual-device
// protocols are Wayland, and none of that exists on Windows or in a macOS
// application. The engine already takes these as Linux-only dependencies; this
// is what lets the *workspace* type-check for another target without a stack
// of `cfg` inside every function.
#![cfg(unix)]
use std::io::{Seek as _, Write as _};
use std::os::fd::AsFd as _;
use std::time::{SystemTime, UNIX_EPOCH};

use esper_input::{Held, InputError, InputSink, Point, evdev_for_usage};
use esper_proto::{Button, KeyEvent, PointerButton, Scroll};
use wayland_client::protocol::{wl_keyboard, wl_output, wl_pointer, wl_registry, wl_seat};
use wayland_client::{Connection, Dispatch, QueueHandle, WEnum, delegate_noop};
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::{
    zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1,
    zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1,
};
use wayland_protocols_wlr::virtual_pointer::v1::client::{
    zwlr_virtual_pointer_manager_v1::ZwlrVirtualPointerManagerV1,
    zwlr_virtual_pointer_v1::ZwlrVirtualPointerV1,
};

/// The name this backend reports, in the log and the session HUD.
pub const BACKEND: &str = "wayland-wlr";

/// A keymap for a seat that has none.
///
/// One key that produces nothing, because the file's *purpose* is to satisfy
/// the protocol rather than to decide characters: every key this host injects
/// is named by position, and a headless machine with no keyboard has no layout
/// to be faithful to. A host using this says so.
const NO_SEAT_KEYMAP: &str = "xkb_keymap {\n\
     xkb_keycodes { include \"evdev\" };\n\
     xkb_types { include \"complete\" };\n\
     xkb_compat { include \"complete\" };\n\
     xkb_symbols { include \"pc+us\" };\n\
     };\n";

/// evdev button codes, which is what the protocol takes.
const BTN_LEFT: u32 = 0x110;
const BTN_RIGHT: u32 = 0x111;
const BTN_MIDDLE: u32 = 0x112;
const BTN_SIDE: u32 = 0x113;
const BTN_EXTRA: u32 = 0x114;

/// What the registry handed us while it was being walked.
#[derive(Default)]
struct Found {
    seat: Option<wl_seat::WlSeat>,
    keyboards: Option<ZwpVirtualKeyboardManagerV1>,
    pointers: Option<ZwlrVirtualPointerManagerV1>,
    /// The seat's keymap, as the compositor sent it: the raw text and nothing
    /// interpreted, because the only thing this does with it is hand it back.
    keymap: Option<String>,
    /// Whether the seat said it has a keyboard at all.
    seat_has_keyboard: bool,
    /// The first output's size, as the compositor reports it.
    ///
    /// Absolute pointer motion is a position within an extent, and the extent
    /// has to be the screen the coordinates are in. Asking the compositor is
    /// the only way to be right about that — a size passed in by a caller who
    /// guessed puts every click at the wrong fraction of the screen.
    output_size: Option<(u32, u32)>,
}

/// Input into a wlroots compositor.
pub struct WlrInput {
    connection: Connection,
    queue: wayland_client::EventQueue<Found>,
    found: Found,
    keyboard: ZwpVirtualKeyboardV1,
    pointer: ZwlrVirtualPointerV1,
    held: Held,
    /// Whether the keymap came from the seat or is the placeholder.
    keymap_is_the_seat_s: bool,
    /// The last place the pointer was put, for the protocol's own bookkeeping.
    size: (u32, u32),
}

impl std::fmt::Debug for WlrInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WlrInput")
            .field("holding", &self.held.count())
            .field("seat_keymap", &self.keymap_is_the_seat_s)
            .finish_non_exhaustive()
    }
}

impl Dispatch<wl_registry::WlRegistry, ()> for Found {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        (): &(),
        _: &Connection,
        queue: &QueueHandle<Self>,
    ) {
        let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        else {
            return;
        };
        match interface.as_str() {
            "wl_seat" => {
                // Version 5 or whatever the compositor has, whichever is less:
                // asking for more than it offers is a protocol error, and this
                // needs only capabilities and a keymap.
                state.seat = Some(registry.bind(name, version.min(5), queue, ()));
            }
            "zwp_virtual_keyboard_manager_v1" => {
                state.keyboards = Some(registry.bind(name, version.min(1), queue, ()));
            }
            "zwlr_virtual_pointer_manager_v1" => {
                state.pointers = Some(registry.bind(name, version.min(2), queue, ()));
            }
            "wl_output" => {
                // Bound only to be told its size. Version 2 is where `mode`
                // carries the resolution.
                let _output: wl_output::WlOutput = registry.bind(name, version.min(2), queue, ());
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_seat::WlSeat, ()> for Found {
    fn event(
        state: &mut Self,
        seat: &wl_seat::WlSeat,
        event: wl_seat::Event,
        (): &(),
        _: &Connection,
        queue: &QueueHandle<Self>,
    ) {
        let wl_seat::Event::Capabilities {
            capabilities: WEnum::Value(capabilities),
        } = event
        else {
            return;
        };
        if capabilities.contains(wl_seat::Capability::Keyboard) {
            state.seat_has_keyboard = true;
            // Taking the seat's keyboard is what makes the compositor send its
            // keymap, which is the only reason this is bound at all.
            let _keyboard = seat.get_keyboard(queue, ());
        }
    }
}

impl Dispatch<wl_keyboard::WlKeyboard, ()> for Found {
    fn event(
        state: &mut Self,
        _: &wl_keyboard::WlKeyboard,
        event: wl_keyboard::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let wl_keyboard::Event::Keymap { fd, size, .. } = event else {
            return;
        };
        // Read rather than mapped: this is a few kilobytes once at start-up,
        // and a mapping would need `unsafe` for no gain.
        let file = std::fs::File::from(fd);
        let mut text = String::new();
        use std::io::Read as _;
        if file.take(u64::from(size)).read_to_string(&mut text).is_ok() {
            // The compositor's keymap is nul-terminated; the virtual keyboard
            // wants the same bytes, so the terminator is kept and only trailing
            // nothing is trimmed.
            state.keymap = Some(text);
        }
    }
}

impl Dispatch<wl_output::WlOutput, ()> for Found {
    fn event(
        state: &mut Self,
        _: &wl_output::WlOutput,
        event: wl_output::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let wl_output::Event::Mode {
            flags,
            width,
            height,
            ..
        } = event
        else {
            return;
        };
        // Only the current mode. An output reports every mode it can do, and
        // the others are sizes it is not showing.
        if !matches!(flags, WEnum::Value(f) if f.contains(wl_output::Mode::Current)) {
            return;
        }
        if state.output_size.is_none() {
            state.output_size = Some((
                u32::try_from(width).unwrap_or(0).max(1),
                u32::try_from(height).unwrap_or(0).max(1),
            ));
        }
    }
}

delegate_noop!(Found: ignore ZwpVirtualKeyboardManagerV1);
delegate_noop!(Found: ignore ZwpVirtualKeyboardV1);
delegate_noop!(Found: ignore ZwlrVirtualPointerManagerV1);
delegate_noop!(Found: ignore ZwlrVirtualPointerV1);

impl WlrInput {
    /// Connect to the compositor and take a virtual keyboard and pointer.
    ///
    /// # Errors
    ///
    /// If there is no Wayland display, or the compositor offers neither
    /// virtual-device protocol — which is every compositor that is not
    /// wlroots-based, and is a refusal rather than a fault.
    pub fn open() -> Result<Self, InputError> {
        Self::open_with_size(1920, 1080)
    }

    /// The same, told the size of the screen it is pointing at.
    ///
    /// The virtual pointer expresses absolute motion as a position within an
    /// extent it is given, so the extent has to be the one the coordinates are
    /// in — the host's own screen.
    ///
    /// # Errors
    ///
    /// As [`Self::open`].
    pub fn open_with_size(width: u32, height: u32) -> Result<Self, InputError> {
        let connection = Connection::connect_to_env().map_err(|e| InputError::NoBackend {
            tried: format!("the Wayland display: {e}"),
        })?;

        let mut queue = connection.new_event_queue();
        let handle = queue.handle();
        let display = connection.display();
        let _registry = display.get_registry(&handle, ());

        let mut found = Found::default();
        // Twice: the first pass learns what globals exist and binds the seat,
        // the second collects the seat's capabilities and its keymap, which
        // only arrive after the seat is bound.
        for _ in 0..2 {
            queue
                .roundtrip(&mut found)
                .map_err(|e| InputError::NoBackend {
                    tried: format!("talking to the compositor: {e}"),
                })?;
        }

        let keyboards = found
            .keyboards
            .clone()
            .ok_or_else(|| InputError::NoBackend {
                tried: "this compositor offers no zwp_virtual_keyboard_manager_v1, so it is not \
                    a wlroots one and its input path is the portal"
                    .to_owned(),
            })?;
        let pointers = found
            .pointers
            .clone()
            .ok_or_else(|| InputError::NoBackend {
                tried: "this compositor offers no zwlr_virtual_pointer_manager_v1".to_owned(),
            })?;
        let seat = found.seat.clone().ok_or_else(|| InputError::NoBackend {
            tried: "this compositor offers no seat to attach a keyboard to".to_owned(),
        })?;

        let keyboard = keyboards.create_virtual_keyboard(&seat, &handle, ());
        let pointer = pointers.create_virtual_pointer(Some(&seat), &handle, ());

        let output_size = found.output_size;
        let keymap_is_the_seat_s = found.keymap.is_some();
        let keymap = found
            .keymap
            .clone()
            .unwrap_or_else(|| NO_SEAT_KEYMAP.to_owned());
        send_keymap(&keyboard, &keymap)?;

        let mut input = Self {
            connection,
            queue,
            found,
            keyboard,
            pointer,
            held: Held::new(),
            keymap_is_the_seat_s,
            // The compositor's own answer where there is one; the caller's
            // guess only where the compositor offered no output.
            size: output_size.unwrap_or((width.max(1), height.max(1))),
        };
        input.flush()?;
        Ok(input)
    }

    /// Send a raw evdev keycode, bypassing the HID table.
    ///
    /// For diagnosing a compositor rather than for serving a session: the
    /// keycode convention is the one thing here that cannot be read off the
    /// protocol, and sweeping it against a compositor that reports what it
    /// received is how it was settled.
    ///
    /// # Errors
    ///
    /// If the compositor rejects the event.
    pub fn raw_key(&mut self, evdev_code: u32, pressed: bool) -> Result<(), InputError> {
        let time = self.now();
        self.keyboard.key(time, evdev_code, u32::from(pressed));
        self.flush()
    }

    /// Whether the keymap in use is the seat's own.
    ///
    /// False on a machine whose seat has no keyboard, where a placeholder is
    /// used — a caller that cares about which characters a key produces should
    /// know which of the two it got.
    #[must_use]
    pub const fn uses_the_seat_keymap(&self) -> bool {
        self.keymap_is_the_seat_s
    }

    /// Wait until the compositor has processed everything sent so far.
    ///
    /// **A flush is not a delivery.** Flushing writes bytes to the socket and
    /// returns; the compositor reads them whenever it gets to it. If the
    /// process exits in between, the events are lost — measured in a sway
    /// session, where an injected key that worked with two seconds of delay
    /// afterwards did nothing at all without it.
    ///
    /// The case that matters is not a lost keystroke mid-session, which the
    /// next one would paper over. It is **teardown**: `release_all` sends the
    /// releases for everything still held, and losing those leaves keys down
    /// on somebody's machine after the session that pressed them has gone.
    ///
    /// So the hot path flushes and this is called where it counts. Making
    /// every keystroke wait for a round trip would put the compositor's
    /// scheduling in front of every character somebody types.
    ///
    /// # Errors
    ///
    /// If the connection fails while waiting.
    pub fn sync(&mut self) -> Result<(), InputError> {
        self.queue
            .roundtrip(&mut self.found)
            .map(|_| ())
            .map_err(|e| InputError::Backend {
                backend: BACKEND,
                detail: format!("waiting for the compositor to catch up: {e}"),
            })
    }

    /// Milliseconds, which is what every Wayland input event is stamped with.
    ///
    /// The protocol only requires that it increase; the epoch is arbitrary, so
    /// the wall clock is used and truncated rather than a counter kept, which
    /// would be one more thing to get wrong across a reconnect.
    fn now(&self) -> u32 {
        SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| {
            u32::try_from(d.as_millis() & 0xFFFF_FFFF).unwrap_or(0)
        })
    }

    fn flush(&mut self) -> Result<(), InputError> {
        // A Wayland request is buffered until something flushes it, so an
        // injected key that is never flushed is a key the compositor never
        // hears — and nothing reports an error, because nothing went wrong.
        self.queue.flush().map_err(|e| InputError::Backend {
            backend: BACKEND,
            detail: format!("flushing to the compositor: {e}"),
        })?;
        // Dispatch anything pending so a protocol error surfaces here rather
        // than as silence later.
        self.queue
            .dispatch_pending(&mut self.found)
            .map_err(|e| InputError::Backend {
                backend: BACKEND,
                detail: format!("the compositor refused something: {e}"),
            })?;
        let _ = &self.connection;
        Ok(())
    }
}

/// Hand a keymap to the virtual keyboard, over a file descriptor.
fn send_keymap(keyboard: &ZwpVirtualKeyboardV1, keymap: &str) -> Result<(), InputError> {
    // Nul-terminated, with the size counting the nul, because that is what
    // `xkb_keymap_new_from_fd` documents.
    //
    // Measured: sway accepts it either way, so this is compliance rather than a
    // fix — it was added on a theory about why an injected key was not arriving
    // and it was the wrong theory. What was actually wrong is in `sync`.
    let mut bytes = keymap.as_bytes().to_vec();
    if bytes.last() != Some(&0) {
        bytes.push(0);
    }

    let mut file = tempfile()?;
    file.write_all(&bytes)
        .and_then(|()| file.flush())
        .and_then(|()| file.seek(std::io::SeekFrom::Start(0)))
        .map_err(|e| InputError::Backend {
            backend: BACKEND,
            detail: format!("writing the keymap out: {e}"),
        })?;

    // Format 1 is `xkb_v1`, the only one there is.
    keyboard.keymap(
        1,
        file.as_fd(),
        u32::try_from(bytes.len()).unwrap_or(u32::MAX),
    );
    Ok(())
}

/// A file nobody else can see, to pass a descriptor for.
///
/// Created in the runtime directory and unlinked at once, so it exists only as
/// an open descriptor: a keymap left on disk is a small thing, and a keymap
/// left on disk every time a session starts is a pile of them.
fn tempfile() -> Result<std::fs::File, InputError> {
    let directory = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".to_owned());
    let path =
        std::path::Path::new(&directory).join(format!("esper-keymap-{}", std::process::id()));
    let file = std::fs::File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(&path)
        .map_err(|e| InputError::Backend {
            backend: BACKEND,
            detail: format!("making a keymap file in {directory}: {e}"),
        })?;
    let _ = std::fs::remove_file(&path);
    Ok(file)
}

const fn evdev_button(button: Button) -> u32 {
    match button {
        Button::Left => BTN_LEFT,
        Button::Right => BTN_RIGHT,
        Button::Middle => BTN_MIDDLE,
        Button::Back => BTN_SIDE,
        Button::Forward => BTN_EXTRA,
    }
}

impl InputSink for WlrInput {
    fn backend(&self) -> &'static str {
        BACKEND
    }

    fn key(&mut self, event: KeyEvent) -> Result<(), InputError> {
        let Some(code) = evdev_for_usage(event.usage) else {
            return Err(InputError::NoSuchKey { usage: event.usage });
        };
        let time = self.now();
        self.keyboard
            .key(time, u32::from(code.0), u32::from(event.pressed));
        self.flush()?;
        self.held.key(event.usage, event.pressed);
        Ok(())
    }

    fn pointer(&mut self, at: Point) -> Result<(), InputError> {
        let time = self.now();
        let (width, height) = self.size;
        // Absolute motion is a position within an extent, and the extent is
        // the screen these coordinates are in. Negative coordinates are
        // clamped rather than cast, because `as u32` on a negative number is
        // an enormous positive one and the pointer would leap.
        self.pointer.motion_absolute(
            time,
            u32::try_from(at.x.max(0)).unwrap_or(0).min(width),
            u32::try_from(at.y.max(0)).unwrap_or(0).min(height),
            width,
            height,
        );
        self.pointer.frame();
        self.flush()
    }

    fn button(&mut self, event: PointerButton) -> Result<(), InputError> {
        let time = self.now();
        let state = if event.pressed {
            wl_pointer::ButtonState::Pressed
        } else {
            wl_pointer::ButtonState::Released
        };
        self.pointer.button(time, evdev_button(event.button), state);
        self.pointer.frame();
        self.flush()?;
        self.held.button(event.button, event.pressed);
        Ok(())
    }

    fn scroll(&mut self, scroll: Scroll) -> Result<(), InputError> {
        let time = self.now();
        // Notches, which are the mandatory half of this protocol's scroll.
        // `axis_discrete` carries both a continuous value and a step count,
        // and a compositor that only reads one of them gets a sensible answer
        // either way.
        if scroll.dy_notches != 0 {
            self.pointer.axis_discrete(
                time,
                wl_pointer::Axis::VerticalScroll,
                f64::from(scroll.dy_notches) * 15.0,
                i32::from(scroll.dy_notches),
            );
        }
        if scroll.dx_notches != 0 {
            self.pointer.axis_discrete(
                time,
                wl_pointer::Axis::HorizontalScroll,
                f64::from(scroll.dx_notches) * 15.0,
                i32::from(scroll.dx_notches),
            );
        }
        if scroll.dx_notches != 0 || scroll.dy_notches != 0 {
            self.pointer.frame();
            self.flush()?;
        }
        Ok(())
    }

    fn release_all(&mut self) -> Result<(), InputError> {
        let (keys, buttons) = self.held.to_release();
        self.held.clear();
        let time = self.now();
        for usage in keys {
            if let Some(code) = evdev_for_usage(usage) {
                self.keyboard.key(time, u32::from(code.0), 0);
            }
        }
        for button in buttons {
            self.pointer.button(
                time,
                evdev_button(button),
                wl_pointer::ButtonState::Released,
            );
        }
        self.pointer.frame();
        self.flush()?;
        // The one place a round trip is worth its cost: these are the releases
        // that stop a key being held down on somebody's machine, and they are
        // the last thing sent before a session goes away.
        self.sync()
    }

    fn held(&self) -> &Held {
        &self.held
    }
}
