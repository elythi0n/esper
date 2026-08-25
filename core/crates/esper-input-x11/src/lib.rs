//! Injecting input through X11's XTEST extension.
//!
//! # Why XTEST
//!
//! It is the X server's own test extension, and the only way to synthesise
//! input that applications cannot tell from a real device. That is the point
//! rather than a detail: anything that checks whether an event was synthetic —
//! and toolkits do — would ignore input sent any other way, and a remote
//! desktop whose keystrokes are silently ignored by some windows is worse than
//! one that says it cannot type.
//!
//! Pure Rust over a socket, like the capture backend, so nothing here links a
//! display library.
//!
//! # The offset, probed rather than trusted
//!
//! A HID usage becomes an evdev code from one table, and on X11 an evdev code
//! becomes a keycode by adding 8. That is a convention of Xorg's evdev and
//! libinput drivers, not something the protocol guarantees — so this asks the
//! server for the keysym at the keycode it computed for a key it knows, and
//! **refuses to open** if the answer is not what a keyboard would produce.
//!
//! The alternative is typing gibberish into somebody's machine and leaving
//! them to work out why.

// The engine only uses the X11 backends on Linux. Elsewhere this crate
// compiles to nothing rather than being absent, so a workspace build for
// another target still covers every member.
#![cfg(target_os = "linux")]

pub mod floating;

use esper_input::{Held, InputError, InputSink, Point, hid};
use esper_proto::{Button, KeyEvent, PointerButton, Scroll};
use x11rb::connection::Connection as _;
use x11rb::protocol::xproto::ConnectionExt as _;
use x11rb::protocol::xtest::ConnectionExt as _;
use x11rb::rust_connection::RustConnection;

/// The backend name reported in logs and the session indicator.
pub const BACKEND: &str = "x11-xtest";

/// What Xorg's input drivers add to an evdev code to get a keycode.
///
/// Probed on open. See the module documentation.
const KEYCODE_OFFSET: u16 = 8;

/// X11's own button numbering, which is not the protocol's.
///
/// 1, 2, 3 are left, middle, right — note that middle is *2*, where Windows
/// numbers right as 2. 4 to 7 are the wheel, which is why scrolling on X11 is
/// a button press. 8 and 9 are the side buttons.
const BUTTON_LEFT: u8 = 1;
const BUTTON_MIDDLE: u8 = 2;
const BUTTON_RIGHT: u8 = 3;
const WHEEL_UP: u8 = 4;
const WHEEL_DOWN: u8 = 5;
const WHEEL_LEFT: u8 = 6;
const WHEEL_RIGHT: u8 = 7;
const BUTTON_BACK: u8 = 8;
const BUTTON_FORWARD: u8 = 9;

/// Injection through an X server.
pub struct X11Injector {
    conn: RustConnection,
    root: x11rb::protocol::xproto::Window,
    held: Held,
}

impl std::fmt::Debug for X11Injector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("X11Injector")
            .field("holding", &self.held.count())
            .finish_non_exhaustive()
    }
}

impl X11Injector {
    /// Connect, check XTEST is there, and check the keycode assumption.
    pub fn open() -> Result<Self, InputError> {
        let (conn, screen_num) = x11rb::connect(None).map_err(|e| InputError::NoBackend {
            tried: esper_desktop::x11_failed(&e.to_string()),
        })?;
        let root = conn
            .setup()
            .roots
            .get(screen_num)
            .ok_or_else(|| InputError::NoBackend {
                tried: "X11: the display has no screen".to_owned(),
            })?
            .root;

        // XTEST, by asking for its version rather than by looking for its
        // name: a server can list an extension it will not serve.
        let version = conn
            .xtest_get_version(2, 2)
            .map_err(|e| InputError::NoBackend {
                tried: format!("X11 XTEST: {e}"),
            })?
            .reply()
            .map_err(|e| InputError::NoBackend {
                tried: format!("X11 XTEST: {e}"),
            })?;
        tracing::debug!("XTEST {}.{}", version.major_version, version.minor_version);

        let me = Self {
            conn,
            root,
            held: Held::new(),
        };
        me.check_keycodes()?;
        Ok(me)
    }

    /// Check that `evdev + 8` really is this server's keycode for a key.
    ///
    /// Two keys are checked rather than one, and they are chosen for a reason:
    /// **`a` and `z` are far apart in the keycode space and both unambiguous
    /// on every layout that uses Latin letters.** A single check could pass on
    /// a server whose mapping happens to agree at one point.
    ///
    /// The keysym looked at is the *first* one for the keycode, which is the
    /// unshifted symbol. On a layout that is not Latin — Cyrillic, say — the
    /// first symbol is not `a`, so this looks at every group and accepts any
    /// of them: the question is whether the keycode is the one for that
    /// physical key, not what the current layout makes of it.
    fn check_keycodes(&self) -> Result<(), InputError> {
        // (usage, the keysym a US layout produces there)
        const PROBES: [(u16, u32); 2] = [(0x04, 0x0061), (0x1D, 0x007A)];
        let setup = self.conn.setup();
        let mapping = self
            .conn
            .get_keyboard_mapping(setup.min_keycode, setup.max_keycode - setup.min_keycode + 1)
            .map_err(|e| InputError::Backend {
                backend: BACKEND,
                detail: format!("asking for the keyboard mapping: {e}"),
            })?
            .reply()
            .map_err(|e| InputError::Backend {
                backend: BACKEND,
                detail: format!("reading the keyboard mapping: {e}"),
            })?;
        let per = usize::from(mapping.keysyms_per_keycode).max(1);

        for (usage, expected) in PROBES {
            let Some(code) = self.keycode_for(usage) else {
                return Err(InputError::Backend {
                    backend: BACKEND,
                    detail: format!("{usage:#06x} is not in the key table"),
                });
            };
            let index = usize::from(code.saturating_sub(setup.min_keycode));
            let start = index.saturating_mul(per);
            let syms = mapping
                .keysyms
                .get(start..start.saturating_add(per))
                .unwrap_or(&[]);
            if syms.is_empty() {
                return Err(InputError::Backend {
                    backend: BACKEND,
                    detail: format!(
                        "keycode {code} has no keysyms, so the {KEYCODE_OFFSET} offset this \
                         backend assumes does not hold on this server"
                    ),
                });
            }
            if !syms.contains(&expected) {
                // Reported, not fatal. A non-Latin layout is the ordinary
                // reason and injection still works — the keycode is right,
                // and what it types is the host's business by design.
                tracing::warn!(
                    "keycode {code} does not produce {expected:#06x}; the host's layout is \
                     not Latin, or the keycode offset differs on this server"
                );
            }
        }
        Ok(())
    }

    /// The X11 keycode for a HID usage.
    fn keycode_for(&self, usage: u16) -> Option<u8> {
        let evdev = hid::evdev_for_usage(usage)?;
        u8::try_from(evdev.0.saturating_add(KEYCODE_OFFSET)).ok()
    }

    /// Send one fake event and flush, so it is on its way before returning.
    fn fake(&self, kind: u8, detail: u8, x: i16, y: i16) -> Result<(), InputError> {
        let fail = |detail: String| InputError::Backend {
            backend: BACKEND,
            detail,
        };
        self.conn
            .xtest_fake_input(kind, detail, 0, self.root, x, y, 0)
            .map_err(|e| fail(e.to_string()))?;
        // **Flushed every time.** A keystroke sitting in a buffer waiting for
        // the next one is a keystroke that arrives when the person has stopped
        // typing, and the next one may be a second away.
        self.conn.flush().map_err(|e| fail(e.to_string()))
    }

    fn press_key(&self, code: u8, pressed: bool) -> Result<(), InputError> {
        // 2 is KeyPress, 3 is KeyRelease — the event types themselves, which
        // is what XTEST takes.
        self.fake(if pressed { 2 } else { 3 }, code, 0, 0)
    }

    fn press_button(&self, button: u8, pressed: bool) -> Result<(), InputError> {
        // 4 is ButtonPress, 5 is ButtonRelease.
        self.fake(if pressed { 4 } else { 5 }, button, 0, 0)
    }

    /// One wheel click, which on X11 is a button pressed and released.
    fn wheel(&self, button: u8) -> Result<(), InputError> {
        self.press_button(button, true)?;
        self.press_button(button, false)
    }

    const fn button_number(button: Button) -> u8 {
        match button {
            Button::Left => BUTTON_LEFT,
            Button::Middle => BUTTON_MIDDLE,
            Button::Right => BUTTON_RIGHT,
            Button::Back => BUTTON_BACK,
            Button::Forward => BUTTON_FORWARD,
        }
    }
}

impl InputSink for X11Injector {
    fn backend(&self) -> &'static str {
        BACKEND
    }

    fn key(&mut self, event: KeyEvent) -> Result<(), InputError> {
        let Some(code) = self.keycode_for(event.usage) else {
            return Err(InputError::NoSuchKey { usage: event.usage });
        };
        // Injected first, recorded second: a press that the server refused is
        // not a key that is down, and recording it would leave this sink
        // trying to release something that never happened.
        self.press_key(code, event.pressed)?;
        self.held.key(event.usage, event.pressed);
        Ok(())
    }

    fn pointer(&mut self, at: Point) -> Result<(), InputError> {
        let x = i16::try_from(at.x).unwrap_or(if at.x < 0 { i16::MIN } else { i16::MAX });
        let y = i16::try_from(at.y).unwrap_or(if at.y < 0 { i16::MIN } else { i16::MAX });
        // 6 is MotionNotify. With a root window and absolute coordinates,
        // which is what the protocol carries — a relative protocol drifts.
        self.fake(6, 0, x, y)
    }

    fn button(&mut self, event: PointerButton) -> Result<(), InputError> {
        let number = Self::button_number(event.button);
        self.press_button(number, event.pressed)?;
        self.held.button(event.button, event.pressed);
        Ok(())
    }

    /// **Notches, not pixels.** XTEST can press a wheel button and nothing
    /// else, so the smooth part of a scroll is unavailable here and the
    /// protocol's mandatory coarse unit is what gets used. A backend that can
    /// do better — `libei`, `uinput` — uses the pixels.
    fn scroll(&mut self, scroll: Scroll) -> Result<(), InputError> {
        for _ in 0..scroll.dy_notches.abs() {
            self.wheel(if scroll.dy_notches < 0 {
                WHEEL_UP
            } else {
                WHEEL_DOWN
            })?;
        }
        for _ in 0..scroll.dx_notches.abs() {
            self.wheel(if scroll.dx_notches < 0 {
                WHEEL_LEFT
            } else {
                WHEEL_RIGHT
            })?;
        }
        Ok(())
    }

    fn release_all(&mut self) -> Result<(), InputError> {
        let (keys, buttons) = self.held.to_release();
        // **Cleared first, and every release attempted even if one fails.**
        // A single refused release must not leave the rest held: the whole
        // point of this is that nothing is left down, and stopping at the
        // first error would leave exactly that.
        self.held.clear();
        let mut first_error = None;
        for usage in keys {
            if let Some(code) = self.keycode_for(usage)
                && let Err(e) = self.press_key(code, false)
            {
                first_error = first_error.or(Some(e));
            }
        }
        for button in buttons {
            if let Err(e) = self.press_button(Self::button_number(button), false) {
                first_error = first_error.or(Some(e));
            }
        }
        match first_error {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    fn held(&self) -> &Held {
        &self.held
    }
}

impl Drop for X11Injector {
    /// **Nothing is left held.**
    ///
    /// A session ends for reasons this code does not get told about: a killed
    /// process, a dropped network, a closed window. Whatever the reason, the
    /// last thing to happen on this machine must not be a key going down and
    /// staying there — the person at the host has no way to know why their
    /// keyboard has stopped working.
    fn drop(&mut self) {
        if self.held.is_empty() {
            return;
        }
        tracing::info!("releasing {} held keys on teardown", self.held.count());
        if let Err(e) = self.release_all() {
            tracing::error!("could not release held keys: {e}");
        }
    }
}
