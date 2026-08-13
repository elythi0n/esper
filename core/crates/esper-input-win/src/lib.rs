//! Injecting input on Windows, through `SendInput`.
//!
//! # What this crate is and is not
//!
//! It is the thin part. Everything decidable without Windows lives in
//! [`esper_input`] and is compiled and tested on every platform: the HID to
//! scan-code table with its extended flags ([`esper_input::set1`]), the
//! absolute-pointer arithmetic ([`esper_input::absolute`]), and the wheel
//! conversion with its inverted sign ([`esper_input::wheel`]). What is left
//! here is the calls.
//!
//! That split is deliberate and it is the reason this crate is small. A
//! `cfg(target_os = "windows")` crate's tests do not run on the machine this
//! was written on, so anything that could be got wrong quietly was put where
//! it could be checked.
//!
//! # Scan codes, not virtual keys
//!
//! `SendInput` takes either. A virtual key is what a character *means* after
//! the layout has spoken, and the wire carries where a key *is* — so injecting
//! a virtual key would ask Windows to re-derive a position from a meaning
//! using the host's layout, and hand back a different key. See
//! [`esper_input::set1`], which is where that argument is made properly.
//!
//! # Per-monitor DPI awareness, declared before anything is measured
//!
//! `GetSystemMetrics` lies to a process that has not said it understands DPI.
//! A process left at the default gets **virtualised** metrics — the desktop
//! reported as though every monitor were at the primary one's scale — so on a
//! 150% display the virtual desktop comes back a third too small and every
//! absolute pointer position lands short of where it was aimed.
//!
//! So [`WinInjector::open`] sets per-monitor-v2 awareness first. It is a
//! process-wide setting and it can only be set once, so a failure means
//! somebody set it already — which is fine if they set the same thing, and the
//! only honest response is to carry on and let the geometry be read as it is.
//!
//! # Unsafe inventory
//!
//! Four calls, all of them Win32 functions that are `unsafe` because they are
//! `extern "system"` and for no other reason. There are no raw pointers held
//! across a call, no lifetimes asserted, and no aliasing claims — the
//! structures handed to `SendInput` are stack arrays this function owns and
//! Windows copies before returning.
//!
//! - **`SetProcessDpiAwarenessContext`** — takes a context constant by value
//!   and returns a `BOOL`. The invariant is that the constant is one Windows
//!   defines, which the `windows` crate's own type supplies. Nothing is
//!   allocated and nothing outlives the call.
//! - **`GetSystemMetrics`** — takes an index constant and returns `i32`. The
//!   invariant is that the index is one Windows defines, again supplied by a
//!   typed constant. It cannot fail in a way that needs freeing; a zero return
//!   means "no such metric or no display", which the caller treats as no
//!   desktop rather than as a measurement.
//! - **`SendInput`** — passed a one-element slice this function just built,
//!   and `size_of::<INPUT>()`. The invariant Windows documents is that the
//!   size argument matches the structure's actual size, which `size_of`
//!   establishes by construction rather than by a literal. The slice is
//!   borrowed for the duration of the call and Windows copies out of it
//!   before returning.
//!
//!   **One call site for keys, pointer moves, buttons and the wheel.** The
//!   union member differs between them, so the obvious shape is a call site
//!   each — but that would put four unsafe blocks where one will do, and the
//!   short-count check in four places. Each kind has a safe constructor and
//!   they all hand their `INPUT` to one function.
//!
//! Every one of them is checked for failure. `SendInput` returns how many
//! events it inserted, and **a short count is an error rather than a warning**:
//! it means UIPI blocked the injection — a session on the secure desktop, or a
//! window running elevated when this process is not — and a keystroke that
//! silently went nowhere is the failure mode this whole path exists to avoid.

#![cfg(target_os = "windows")]

use esper_input::{Held, InputError, InputSink, Point, absolute, set1, wheel};
use esper_proto::{Button, KeyEvent, PointerButton, Scroll};
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT, KEYEVENTF_EXTENDEDKEY,
    KEYEVENTF_KEYUP, KEYEVENTF_SCANCODE, MOUSE_EVENT_FLAGS, MOUSEEVENTF_ABSOLUTE,
    MOUSEEVENTF_HWHEEL, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEDOWN,
    MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_MOVE, MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP,
    MOUSEEVENTF_VIRTUALDESK, MOUSEEVENTF_WHEEL, MOUSEEVENTF_XDOWN, MOUSEEVENTF_XUP, MOUSEINPUT,
    SendInput, VIRTUAL_KEY,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
    SYSTEM_METRICS_INDEX, XBUTTON1, XBUTTON2,
};

/// The name this backend goes by in a log and in the session indicator.
pub const BACKEND: &str = "windows sendinput";

/// Injecting input through `SendInput`.
#[derive(Debug)]
pub struct WinInjector {
    /// What is held, so teardown can let go of exactly it.
    held: Held,
}

impl WinInjector {
    /// Declare DPI awareness and take a look at the desktop.
    ///
    /// # Errors
    ///
    /// If there is no desktop to inject into — no monitors, or a session with
    /// no window station this process can reach, which is what a service
    /// running in session 0 sees.
    pub fn open() -> Result<Self, InputError> {
        set_dpi_awareness();
        let desk = desktop();
        if !desk.is_sane() {
            return Err(InputError::NoBackend {
                tried: format!(
                    "{BACKEND}: the virtual desktop is {}x{}, so there is no screen to \
                     inject into. A service in session 0 sees this, and so does a \
                     machine with every monitor asleep",
                    desk.width, desk.height
                ),
            });
        }
        Ok(Self { held: Held::new() })
    }
}

/// Ask for per-monitor DPI awareness, before anything measures the desktop.
///
/// Failure is not an error: the setting is process-wide and one-shot, so it
/// fails when somebody has already set it. Logged rather than refused, because
/// refusing to start a session over it would be worse than a pointer that is
/// off on a scaled display — and the log line is what makes that diagnosable.
fn set_dpi_awareness() {
    #[allow(unsafe_code)]
    // SAFETY: a Win32 call taking a context constant the `windows` crate
    // defines and returning a BOOL. Nothing is allocated, nothing outlives the
    // call. See the crate's unsafe inventory.
    let already =
        unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
    if already.is_err() {
        tracing::debug!(
            "per-monitor DPI awareness was already set by something else; the desktop \
             geometry is read as this process sees it"
        );
    }
}

/// The virtual desktop, in physical pixels.
///
/// Read on every pointer move rather than cached. Monitors are unplugged,
/// docked, rotated and resized during sessions, and a cached rectangle would
/// put the pointer somewhere that used to be a screen. The call is a read of a
/// value the kernel already has.
fn desktop() -> absolute::Desk {
    absolute::Desk {
        left: metric(SM_XVIRTUALSCREEN),
        top: metric(SM_YVIRTUALSCREEN),
        width: metric(SM_CXVIRTUALSCREEN),
        height: metric(SM_CYVIRTUALSCREEN),
    }
}

fn metric(which: SYSTEM_METRICS_INDEX) -> i32 {
    #[allow(unsafe_code)]
    // SAFETY: a Win32 call taking an index constant the `windows` crate
    // defines and returning i32. See the crate's unsafe inventory.
    unsafe {
        GetSystemMetrics(which)
    }
}

/// Hand one event to `SendInput` and insist it went in.
///
/// **A short count is an error.** `SendInput` returns how many events it
/// inserted, and zero means something refused it — UIPI, because the focused
/// window is elevated and this process is not, or because the secure desktop
/// is up. A keystroke that silently went nowhere is precisely the failure this
/// path exists to prevent, so it is reported and the session says so.
fn send(mut event: INPUT, what: &str) -> Result<(), InputError> {
    let events = std::slice::from_mut(&mut event);
    let size = i32::try_from(size_of::<INPUT>()).unwrap_or(i32::MAX);
    #[allow(unsafe_code)]
    // SAFETY: `events` is a slice of one `INPUT` this function owns, borrowed
    // for the duration of the call, and Windows copies out of it before
    // returning. `size` is `size_of::<INPUT>()` rather than a literal, which
    // is the invariant Windows documents. See the crate's unsafe inventory.
    let inserted = unsafe { SendInput(events, size) };
    if inserted == 1 {
        return Ok(());
    }
    Err(InputError::Backend {
        backend: BACKEND,
        detail: format!(
            "{what} was refused: SendInput inserted {inserted} of 1. Something with a \
             higher integrity level has the foreground — an elevated window, or the \
             secure desktop a UAC prompt runs on, neither of which a process at this \
             level may type into"
        ),
    })
}

/// A keyboard event carrying a scan code.
fn key_input(code: set1::Set1, pressed: bool) -> INPUT {
    let mut flags = KEYEVENTF_SCANCODE;
    if code.extended {
        flags |= KEYEVENTF_EXTENDEDKEY;
    }
    if !pressed {
        flags |= KEYEVENTF_KEYUP;
    }
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                // Zero, because the scan code is what is being sent. A virtual
                // key here would be the layout-dependent path this backend
                // exists to avoid.
                wVk: VIRTUAL_KEY(0),
                wScan: u16::from(code.code),
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

/// A mouse event.
fn mouse_input(flags: MOUSE_EVENT_FLAGS, dx: i32, dy: i32, data: i32) -> INPUT {
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx,
                dy,
                mouseData: data as u32,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

/// The flag and `mouseData` for pressing a button.
///
/// Back and forward are the two X buttons, told apart by `mouseData` rather
/// than by the flag — which is why this returns a pair and not just a flag.
fn down_for(button: Button) -> (MOUSE_EVENT_FLAGS, i32) {
    match button {
        Button::Left => (MOUSEEVENTF_LEFTDOWN, 0),
        Button::Middle => (MOUSEEVENTF_MIDDLEDOWN, 0),
        Button::Right => (MOUSEEVENTF_RIGHTDOWN, 0),
        Button::Back => (MOUSEEVENTF_XDOWN, i32::from(XBUTTON1)),
        Button::Forward => (MOUSEEVENTF_XDOWN, i32::from(XBUTTON2)),
    }
}

/// The flag and `mouseData` for releasing one.
///
/// Kept beside [`down_for`] rather than inlined at the two call sites, because
/// a button that presses as one and releases as another leaves a button held
/// down on somebody else's machine.
fn up_for(button: Button) -> (MOUSE_EVENT_FLAGS, i32) {
    match button {
        Button::Left => (MOUSEEVENTF_LEFTUP, 0),
        Button::Middle => (MOUSEEVENTF_MIDDLEUP, 0),
        Button::Right => (MOUSEEVENTF_RIGHTUP, 0),
        Button::Back => (MOUSEEVENTF_XUP, i32::from(XBUTTON1)),
        Button::Forward => (MOUSEEVENTF_XUP, i32::from(XBUTTON2)),
    }
}

impl InputSink for WinInjector {
    fn backend(&self) -> &'static str {
        BACKEND
    }

    fn key(&mut self, event: KeyEvent) -> Result<(), InputError> {
        let code = set1::set1_for_usage(event.usage)
            .ok_or(InputError::NoSuchKey { usage: event.usage })?;
        send(key_input(code, event.pressed), "a key")?;
        // Recorded only once the injection succeeded, so a refused press is
        // not something teardown later tries to release.
        self.held.key(event.usage, event.pressed);
        Ok(())
    }

    fn pointer(&mut self, at: Point) -> Result<(), InputError> {
        let desk = desktop();
        let Some(where_to) = absolute::onto(desk, at.x, at.y) else {
            return Err(InputError::Backend {
                backend: BACKEND,
                detail: "the virtual desktop has no area, so there is nowhere to put \
                         the pointer"
                    .to_owned(),
            });
        };
        // **`VIRTUALDESK` as well as `ABSOLUTE`.** Without it the coordinates
        // span the primary monitor alone, and every position on a second
        // monitor lands on the first.
        send(
            mouse_input(
                MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
                where_to.x,
                where_to.y,
                0,
            ),
            "a pointer move",
        )
    }

    fn button(&mut self, event: PointerButton) -> Result<(), InputError> {
        let (flags, data) = if event.pressed {
            down_for(event.button)
        } else {
            up_for(event.button)
        };
        send(mouse_input(flags, 0, 0, data), "a button")?;
        self.held.button(event.button, event.pressed);
        Ok(())
    }

    fn scroll(&mut self, scroll: Scroll) -> Result<(), InputError> {
        let turn = wheel::from_scroll(scroll);
        if turn.is_nothing() {
            // Not an error and not an event: a wheel event of size zero is
            // read as a scroll by some applications and as a reset by others.
            return Ok(());
        }
        if turn.vertical != 0 {
            send(
                mouse_input(MOUSEEVENTF_WHEEL, 0, 0, turn.vertical),
                "a scroll",
            )?;
        }
        if turn.horizontal != 0 {
            send(
                mouse_input(MOUSEEVENTF_HWHEEL, 0, 0, turn.horizontal),
                "a sideways scroll",
            )?;
        }
        Ok(())
    }

    fn release_all(&mut self) -> Result<(), InputError> {
        let (keys, buttons) = self.held.to_release();
        // **Cleared first, and every release attempted even if one fails.**
        // The same shape as the X11 backend and for the same reason: a single
        // refused release must not leave the rest held, and stopping at the
        // first error would leave exactly that. A key left held on somebody
        // else's machine is the worst failure this path has.
        self.held.clear();
        let mut first_error = None;
        for usage in keys {
            if let Some(code) = set1::set1_for_usage(usage)
                && let Err(e) = send(key_input(code, false), "a key release")
            {
                first_error = first_error.or(Some(e));
            }
        }
        for button in buttons {
            let (flags, data) = up_for(button);
            if let Err(e) = send(mouse_input(flags, 0, 0, data), "a button release") {
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
