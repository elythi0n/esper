//! Input on Wayland, through the `RemoteDesktop` portal.
//!
//! # Why this shape
//!
//! Wayland removed X11's ability for any client to synthesise input into any
//! other client's window, on purpose. A remote desktop therefore needs a path
//! the compositor sanctions, and on GNOME and KDE that is
//! `org.freedesktop.portal.RemoteDesktop`: the user approves a session at a
//! dialog the *compositor* owns, and the grant can be revoked there.
//!
//! Measured on 2026-09-12 against a real GNOME session, the portal offers two
//! ways to deliver an event once granted:
//!
//! - `ConnectToEIS`, which hands back a socket for **libei**;
//! - ten plain `Notify*` methods, which are ordinary D-Bus calls.
//!
//! This uses the second. It needs no second library, and the consent prompt —
//! the part that matters — is the portal's either way, so the cheaper one is
//! taken first.
//!
//! # What it deliberately does not do
//!
//! **It does not fall back.** A compositor with no `RemoteDesktop` portal — every
//! wlroots one, since `xdg-desktop-portal-wlr` implements ScreenCast and
//! nothing else — is not this backend's problem to solve quietly. It says so,
//! and the ladder above it decides what to try next. A backend that silently
//! becomes a different backend is how a session ends up injecting through
//! `uinput` while its audit trail says otherwise.

// Unix only, and empty elsewhere.
//
// A session bus is a unix socket, the layer shell and the virtual-device
// protocols are Wayland, and none of that exists on Windows or in a macOS
// application. The engine already takes these as Linux-only dependencies; this
// is what lets the *workspace* type-check for another target without a stack
// of `cfg` inside every function.
#![cfg(unix)]
use std::time::Duration;

use esper_dbus::{BusError, Connection, Value};
use esper_input::{Held, InputError, InputSink, Point, evdev_for_usage};
use esper_proto::{Button, KeyEvent, PointerButton, Scroll};

/// The name this backend reports, in the log and the session HUD.
pub const BACKEND: &str = "wayland-portal";

const PORTAL: &str = "org.freedesktop.portal.Desktop";
const PORTAL_PATH: &str = "/org/freedesktop/portal/desktop";
const REMOTE_DESKTOP: &str = "org.freedesktop.portal.RemoteDesktop";

/// How long to wait for a person to answer the consent dialog.
///
/// Long, because it is a human at a prompt and not a machine. Not infinite,
/// because a host that waits for ever on a dialog nobody is looking at is a
/// host that appears to have hung.
const CONSENT_PATIENCE: Duration = Duration::from_secs(120);

/// Device types, as the portal counts them.
const KEYBOARD: u32 = 1;
const POINTER: u32 = 2;

/// evdev button codes, which is what `NotifyPointerButton` takes.
const BTN_LEFT: i32 = 0x110;
const BTN_RIGHT: i32 = 0x111;
const BTN_MIDDLE: i32 = 0x112;
const BTN_SIDE: i32 = 0x113;
const BTN_EXTRA: i32 = 0x114;

/// An input channel the compositor granted.
#[derive(Debug)]
pub struct PortalInput {
    bus: Connection,
    session: String,
    held: Held,
    /// The stream the compositor gave us to position against, if any.
    ///
    /// Absolute pointer motion is expressed **relative to a screen-cast
    /// stream**, not to a desktop, because the portal will not tell a client
    /// about screens it was not granted. Without one, only relative motion is
    /// possible — which is why this is an `Option` rather than an assumption.
    stream: Option<u32>,
    /// Where the pointer was last put, so relative motion can be used to reach
    /// an absolute position when there is no stream to address.
    at: Option<Point>,
}

impl PortalInput {
    /// Ask the compositor for an input channel, and wait for the person to say
    /// yes.
    ///
    /// # Errors
    ///
    /// If there is no session bus, no `RemoteDesktop` portal, or the request is
    /// refused — including by a person declining it, which is a refusal and not
    /// a fault.
    pub fn open() -> Result<Self, InputError> {
        Self::open_on(Connection::session().map_err(no_backend)?, CONSENT_PATIENCE)
    }

    /// The same, on a connection somebody else made.
    ///
    /// # Errors
    ///
    /// As [`Self::open`].
    pub fn open_on(mut bus: Connection, consent: Duration) -> Result<Self, InputError> {
        // Signals are broadcast and a connection receives only what it asked
        // for. Without this the portal answers and nothing hears it, which
        // looks exactly like a portal that never answered.
        bus.add_match(&format!(
            "type='signal',interface='org.freedesktop.portal.Request',sender='{PORTAL}'"
        ))
        .map_err(no_backend)?;

        let session = Self::create_session(&mut bus)?;
        Self::select_devices(&mut bus, &session)?;
        let stream = Self::start(&mut bus, &session, consent)?;

        Ok(Self {
            bus,
            session,
            held: Held::new(),
            stream,
            at: None,
        })
    }

    /// The object path the portal will answer this request on.
    ///
    /// The portal derives it from our unique bus name and a token we choose,
    /// and it must be listened to **before** the call is made — the reply and
    /// the signal race, and the signal usually wins.
    fn request_path(bus: &Connection, token: &str) -> String {
        let sender = bus.unique_name().trim_start_matches(':').replace('.', "_");
        format!("/org/freedesktop/portal/desktop/request/{sender}/{token}")
    }

    /// Make a portal call and read the `Response` signal it answers with.
    ///
    /// Every portal method that needs a person works this way: the method
    /// returns a request handle immediately, and the answer arrives later as a
    /// signal carrying a code and a dictionary. A caller that read only the
    /// method's return value would see a path and think it had succeeded.
    fn ask(
        bus: &mut Connection,
        member: &str,
        mut args: Vec<Value>,
        token: &str,
        extra: Vec<(&str, Value)>,
        patience: Duration,
    ) -> Result<Value, InputError> {
        let path = Self::request_path(bus, token);

        // The options dictionary is always the last argument, and the token
        // goes in it.
        let mut options = vec![("handle_token", Value::Str(token.to_owned()))];
        options.extend(extra);
        args.push(Value::dict(options));

        bus.call(PORTAL, PORTAL_PATH, REMOTE_DESKTOP, member, &args)
            .map_err(|e| match e {
                BusError::Call { name, message } => InputError::NoBackend {
                    tried: format!("the RemoteDesktop portal refused {member}: {name} — {message}"),
                },
                other => no_backend(other),
            })?;

        let signal =
            bus.await_signal(&path, "Response", patience)
                .map_err(|e| InputError::NoBackend {
                    tried: match e {
                        // The commonest outcome on a machine with nobody at it,
                        // and it is not a fault: the prompt is waiting.
                        BusError::TimedOut(waited) => format!(
                            "nobody answered the compositor's prompt for {member} in {waited:?}"
                        ),
                        other => format!("the portal never answered {member}: {other}"),
                    },
                })?;

        let code = signal.args.first().and_then(Value::as_u32).unwrap_or(2);
        let results = signal
            .args
            .get(1)
            .cloned()
            .unwrap_or_else(Value::no_options);

        match code {
            0 => Ok(results),
            // 1 is "the user said no", 2 is "it ended some other way". They
            // read very differently to somebody trying to start a session, and
            // conflating them is how "cancelled" becomes "broken".
            1 => Err(InputError::NoBackend {
                tried: format!("{member} was declined at the compositor's prompt"),
            }),
            other => Err(InputError::NoBackend {
                tried: format!("{member} ended with portal response {other}"),
            }),
        }
    }

    fn create_session(bus: &mut Connection) -> Result<String, InputError> {
        // **`session_handle_token` is a second token and it is not optional.**
        // One names the question, the other names the session the question
        // creates, and omitting the second does not produce an error: it
        // reaches `xdp_session_initable_init`, fails an assertion, and takes
        // `xdg-desktop-portal` down with `SIGABRT` — so the caller sees
        // "Message recipient disconnected from message bus without replying"
        // and no hint of what it left out. Found by running this against a
        // real GNOME portal in a container.
        let results = Self::ask(
            bus,
            "CreateSession",
            vec![],
            "esper_create",
            vec![(
                "session_handle_token",
                Value::Str("esper_session".to_owned()),
            )],
            Duration::from_secs(10),
        )?;
        results
            .get("session_handle")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
            .ok_or_else(|| InputError::NoBackend {
                tried: "the portal created a session and did not say which".to_owned(),
            })
    }

    fn select_devices(bus: &mut Connection, session: &str) -> Result<(), InputError> {
        Self::ask(
            bus,
            "SelectDevices",
            vec![Value::Path(session.to_owned())],
            "esper_devices",
            vec![],
            Duration::from_secs(10),
        )?;
        Ok(())
    }

    fn start(
        bus: &mut Connection,
        session: &str,
        consent: Duration,
    ) -> Result<Option<u32>, InputError> {
        let results = Self::ask(
            bus,
            "Start",
            vec![
                Value::Path(session.to_owned()),
                // No parent window: this host has no window of its own to
                // attach the prompt to, and an empty string is what the portal
                // documents for that.
                Value::Str(String::new()),
            ],
            "esper_start",
            vec![],
            consent,
        )?;

        let granted = results.get("devices").and_then(Value::as_u32).unwrap_or(0);
        if granted & (KEYBOARD | POINTER) == 0 {
            return Err(InputError::NoBackend {
                tried: format!(
                    "the portal granted device types {granted}, which is neither a keyboard nor a pointer"
                ),
            });
        }

        // A stream, if the session was also granted one. Absolute pointer
        // positions are expressed against a stream, so its absence is what
        // decides whether this backend can point at a place or only shove.
        let stream = results
            .get("streams")
            .and_then(|streams| match streams {
                Value::Array { items, .. } => items.first().cloned(),
                _ => None,
            })
            .and_then(|first| match first {
                Value::Struct(fields) => fields.first().and_then(Value::as_u32),
                _ => None,
            });
        Ok(stream)
    }

    /// Whether this channel can put the pointer at a place rather than nudge it.
    #[must_use]
    pub const fn can_point_absolutely(&self) -> bool {
        self.stream.is_some()
    }

    fn notify(&mut self, member: &str, mut args: Vec<Value>) -> Result<(), InputError> {
        // `Notify*` takes the session first and the options second, before its
        // own arguments — the opposite order from the session-creating calls,
        // which is a thing to get wrong exactly once.
        let mut full = vec![Value::Path(self.session.clone()), Value::no_options()];
        full.append(&mut args);
        self.bus
            .call(PORTAL, PORTAL_PATH, REMOTE_DESKTOP, member, &full)
            .map_err(|e| InputError::Backend {
                backend: BACKEND,
                detail: format!("{member}: {e}"),
            })?;
        Ok(())
    }
}

/// evdev's code for a button, which is what the portal wants.
const fn evdev_button(button: Button) -> i32 {
    match button {
        Button::Left => BTN_LEFT,
        Button::Right => BTN_RIGHT,
        Button::Middle => BTN_MIDDLE,
        Button::Back => BTN_SIDE,
        Button::Forward => BTN_EXTRA,
    }
}

fn no_backend(e: BusError) -> InputError {
    InputError::NoBackend {
        tried: format!("the RemoteDesktop portal over D-Bus: {e}"),
    }
}

impl InputSink for PortalInput {
    fn backend(&self) -> &'static str {
        BACKEND
    }

    fn key(&mut self, event: KeyEvent) -> Result<(), InputError> {
        let Some(code) = evdev_for_usage(event.usage) else {
            return Err(InputError::NoSuchKey { usage: event.usage });
        };
        self.notify(
            "NotifyKeyboardKeycode",
            vec![
                Value::Int32(i32::from(code.0)),
                Value::Uint32(u32::from(event.pressed)),
            ],
        )?;
        self.held.key(event.usage, event.pressed);
        Ok(())
    }

    fn pointer(&mut self, at: Point) -> Result<(), InputError> {
        if let Some(stream) = self.stream {
            self.notify(
                "NotifyPointerMotionAbsolute",
                vec![
                    Value::Uint32(stream),
                    Value::Double(f64::from(at.x)),
                    Value::Double(f64::from(at.y)),
                ],
            )?;
        } else {
            // No stream to address, so the only motion on offer is relative.
            // Tracked from the last known position, which is a guess the first
            // time and exact afterwards — and is honest about being a guess:
            // `can_point_absolutely` tells the caller which it is getting.
            let from = self.at.unwrap_or(Point { x: 0, y: 0 });
            self.notify(
                "NotifyPointerMotion",
                vec![
                    Value::Double(f64::from(at.x - from.x)),
                    Value::Double(f64::from(at.y - from.y)),
                ],
            )?;
        }
        self.at = Some(at);
        Ok(())
    }

    fn button(&mut self, event: PointerButton) -> Result<(), InputError> {
        self.notify(
            "NotifyPointerButton",
            vec![
                Value::Int32(evdev_button(event.button)),
                Value::Uint32(u32::from(event.pressed)),
            ],
        )?;
        self.held.button(event.button, event.pressed);
        Ok(())
    }

    fn scroll(&mut self, scroll: Scroll) -> Result<(), InputError> {
        // Notches, because they are the mandatory half of the protocol's
        // scroll and the portal has a call that takes exactly them. The sign
        // is the protocol's: positive is down, which is what
        // `NotifyPointerAxisDiscrete` also means.
        if scroll.dy_notches != 0 {
            self.notify(
                "NotifyPointerAxisDiscrete",
                vec![Value::Uint32(0), Value::Int32(i32::from(scroll.dy_notches))],
            )?;
        }
        if scroll.dx_notches != 0 {
            self.notify(
                "NotifyPointerAxisDiscrete",
                vec![Value::Uint32(1), Value::Int32(i32::from(scroll.dx_notches))],
            )?;
        }
        Ok(())
    }

    fn release_all(&mut self) -> Result<(), InputError> {
        // Taken by value first, so a failure part-way cannot leave this
        // believing it still holds what it has already let go of.
        let (keys, buttons) = self.held.to_release();
        self.held.clear();
        for usage in keys {
            let Some(code) = evdev_for_usage(usage) else {
                // A key with no evdev code cannot have been pressed through
                // here, so there is nothing to let go of.
                continue;
            };
            self.notify(
                "NotifyKeyboardKeycode",
                vec![Value::Int32(i32::from(code.0)), Value::Uint32(0)],
            )?;
        }
        for button in buttons {
            self.notify(
                "NotifyPointerButton",
                vec![Value::Int32(evdev_button(button)), Value::Uint32(0)],
            )?;
        }
        Ok(())
    }

    fn held(&self) -> &Held {
        &self.held
    }
}
