//! Choosing a capture backend at runtime.
//!
//! # Why this is a decision and not a `cfg`
//!
//! Which backend works is a property of the machine the binary is running on,
//! not of the machine it was compiled on. One Linux build has to serve a
//! desktop running X11, a desktop running Wayland, and a headless server with
//! no display at all — and the last of those is a first-class target, not a
//! degraded one.
//!
//! So the backends are tried in order and the choice is **reported**. An
//! operator whose session is showing a test pattern instead of their screen
//! needs to know that the X11 backend was tried and why it declined, and
//! "capture failed" tells them nothing they can act on.

use std::fmt;

use esper_capture::capturer::{CaptureError, Capturer, MonitorId};

/// Which backend to use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Choice {
    /// Try the real ones, fall back to synthetic.
    #[default]
    Auto,
    /// X11, or fail saying why.
    X11,
    /// A wlroots compositor's own copy, or fail saying why.
    Wlr,
    /// DXGI Desktop Duplication, or fail saying why.
    Dxgi,
    /// The known moving pattern. Always available, never a real screen.
    Synthetic,
}

impl Choice {
    /// Parse a `--backend` value.
    pub fn parse(s: &str) -> Result<Self, String> {
        match s {
            "auto" => Ok(Self::Auto),
            "x11" => Ok(Self::X11),
            "wlr" => Ok(Self::Wlr),
            "dxgi" => Ok(Self::Dxgi),
            "synthetic" => Ok(Self::Synthetic),
            other => Err(format!(
                "--backend wants auto, x11, wlr, dxgi or synthetic, got {other}"
            )),
        }
    }
}

impl fmt::Display for Choice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Auto => write!(f, "auto"),
            Self::X11 => write!(f, "x11"),
            Self::Wlr => write!(f, "wlr"),
            Self::Dxgi => write!(f, "dxgi"),
            Self::Synthetic => write!(f, "synthetic"),
        }
    }
}

/// A backend, and the story of how it was picked.
pub struct Selected {
    pub capturer: Box<dyn Capturer>,
    /// What was tried and what happened, in order. Printed on startup,
    /// because a session showing a test pattern needs to say why.
    pub attempts: Vec<String>,
}

impl fmt::Debug for Selected {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Selected")
            .field("backend", &self.capturer.backend())
            .field("attempts", &self.attempts)
            .finish()
    }
}

/// Open a capturer, honouring `choice`.
///
/// What the synthetic screen is doing, from `host.synthetic_motion`.
///
/// An unknown word cannot reach here — the setting is a choice, so the config
/// refuses a typo rather than defaulting it — so falling back to the ordinary
/// screen is the answer for "not set" rather than for "set to nonsense".
fn motion() -> esper_capture::pattern::Motion {
    use esper_capture::pattern::Motion;
    match crate::settings::text("host.synthetic_motion").as_str() {
        "still" => Motion::Still,
        "video" => Motion::Everything,
        _ => Motion::Screen,
    }
}

/// `size` is the synthetic screen's size, and is ignored by a real backend:
/// a real screen is whatever size it is.
/// Open a wlroots capturer, if this platform has one compiled in.
#[cfg(target_os = "linux")]
fn open_wlr(monitor: Option<MonitorId>) -> Result<Option<Box<dyn Capturer>>, CaptureError> {
    let mut capturer = esper_capture_wlr::WlrCapturer::open()?;
    if let Some(id) = monitor {
        capturer.serve(&[id])?;
    }
    Ok(Some(Box::new(capturer)))
}

#[cfg(not(target_os = "linux"))]
const fn open_wlr(_monitor: Option<MonitorId>) -> Result<Option<Box<dyn Capturer>>, CaptureError> {
    Ok(None)
}

/// A backend somebody named that this build does not have.
///
/// **An error, like one that failed to open**, and for the same reason: the
/// alternative is the synthetic pattern, handed to somebody who said which
/// screen they wanted. On a platform without the backend that fall-through was
/// silent, because "not compiled in" was only ever noted as an attempt.
fn not_here(backend: &'static str) -> CaptureError {
    CaptureError::Backend {
        backend,
        detail: "not compiled in for this platform".to_owned(),
    }
}

pub fn open(
    choice: Choice,
    monitor: Option<MonitorId>,
    size: (u32, u32),
) -> Result<Selected, CaptureError> {
    let mut attempts = Vec::new();

    if matches!(choice, Choice::Auto | Choice::X11) {
        match open_x11(monitor) {
            Ok(Some(c)) => {
                attempts.push("x11: in use".to_owned());
                return Ok(Selected {
                    capturer: c,
                    attempts,
                });
            }
            Ok(None) => {
                if choice == Choice::X11 {
                    return Err(not_here("x11"));
                }
                attempts.push("x11: not compiled in for this platform".to_owned());
            }
            Err(e) => {
                // Asking for x11 explicitly and not getting it is an error.
                // Falling back would hand back a test pattern to somebody who
                // said which screen they wanted.
                if choice == Choice::X11 {
                    return Err(e);
                }
                attempts.push(format!("x11: {e}"));
            }
        }
    }

    // Then a wlroots compositor's own copy.
    //
    // After X11 for the same reason the input ladder is: X11 works under
    // XWayland, which is most Wayland sessions, and it is the complete path.
    // This is for the sessions with no X server at all, where the alternative
    // is the ScreenCast portal and a PipeWire dependency.
    if matches!(choice, Choice::Auto | Choice::Wlr) {
        match open_wlr(monitor) {
            Ok(Some(c)) => {
                attempts.push("wlr: in use".to_owned());
                return Ok(Selected {
                    capturer: c,
                    attempts,
                });
            }
            Ok(None) => {
                if choice == Choice::Wlr {
                    return Err(not_here("wlr"));
                }
                attempts.push("wlr: not compiled in for this platform".to_owned());
            }
            Err(e) => {
                if choice == Choice::Wlr {
                    return Err(e);
                }
                attempts.push(format!("wlr: {e}"));
            }
        }
    }

    if matches!(choice, Choice::Auto | Choice::Dxgi) {
        match open_dxgi(monitor) {
            Ok(Some(c)) => {
                attempts.push("dxgi: in use".to_owned());
                return Ok(Selected {
                    capturer: c,
                    attempts,
                });
            }
            Ok(None) => {
                if choice == Choice::Dxgi {
                    return Err(not_here("dxgi"));
                }
                attempts.push("dxgi: not compiled in for this platform".to_owned());
            }
            Err(e) => {
                // Asking for it explicitly and not getting it is an error, for
                // the same reason as x11: falling back hands a test pattern to
                // somebody who said which screen they wanted.
                if choice == Choice::Dxgi {
                    return Err(e);
                }
                attempts.push(format!("dxgi: {e}"));
            }
        }
    }

    if choice == Choice::Auto {
        attempts.push("synthetic: in use, because nothing else was available".to_owned());
    } else {
        attempts.push("synthetic: in use, as asked".to_owned());
    }
    let mut synthetic = esper_capture::synthetic::SyntheticCapturer::new(size.0, size.1, 1_000_000)
        .map_err(|e| CaptureError::Backend {
            backend: "synthetic",
            detail: e.to_string(),
        })?
        .with_motion(motion());
    // **Each half the size of the one before**, so a switch between them shows
    // up in the picture rather than being taken on trust — the same reason the
    // test fixtures do it. A screen too small to render stops the list rather
    // than failing the host: a machine that would not start because somebody
    // asked for eight synthetic monitors is worse than one that offers five.
    let wanted = crate::settings::whole("host.synthetic_monitors").unwrap_or(1);
    let (mut w, mut h) = size;
    for _ in 1..wanted {
        (w, h) = (w / 2, h / 2);
        // A screen too small to render ends the list here with what was
        // built so far.
        if synthetic.add_monitor(w, h).is_err() {
            break;
        }
    }
    if synthetic.monitors().len() > 1 {
        attempts.push(format!(
            "synthetic: {} screens, as asked",
            synthetic.monitors().len()
        ));
    }
    // **`--monitor` means the same thing here as it does on a real backend.**
    // The X11 one takes it at construction; this one is built and then pointed,
    // and without this a host asked for its second synthetic screen quietly
    // sent its first.
    if let Some(id) = monitor
        && let Err(e) = esper_capture::Capturer::select(&mut synthetic, id)
    {
        return Err(e);
    }
    Ok(Selected {
        capturer: Box::new(synthetic),
        attempts,
    })
}

/// `Ok(None)` when this platform has no X11 backend compiled in.
#[cfg(target_os = "linux")]
fn open_x11(monitor: Option<MonitorId>) -> Result<Option<Box<dyn Capturer>>, CaptureError> {
    let c = esper_capture_x11::X11Capturer::open(monitor)?;
    Ok(Some(Box::new(c)))
}

#[cfg(not(target_os = "linux"))]
fn open_x11(_monitor: Option<MonitorId>) -> Result<Option<Box<dyn Capturer>>, CaptureError> {
    Ok(None)
}

/// `Ok(None)` when this platform has no Desktop Duplication compiled in.
#[cfg(target_os = "windows")]
fn open_dxgi(monitor: Option<MonitorId>) -> Result<Option<Box<dyn Capturer>>, CaptureError> {
    let c = esper_capture_win::DxgiCapturer::open(monitor)?;
    Ok(Some(Box::new(c)))
}

#[cfg(not(target_os = "windows"))]
fn open_dxgi(_monitor: Option<MonitorId>) -> Result<Option<Box<dyn Capturer>>, CaptureError> {
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_backend_names_round_trip() {
        for c in [Choice::Auto, Choice::X11, Choice::Dxgi, Choice::Synthetic] {
            assert_eq!(Choice::parse(&c.to_string()), Ok(c));
        }
    }

    #[test]
    fn an_unknown_backend_says_what_it_accepts() {
        // "wayland" is deliberately not a backend name: there are two Wayland
        // capture paths and they share nothing, so the name would have to mean
        // one of them. `wlr` is the compositor's own copy; the ScreenCast
        // portal, when it exists, will need its own name.
        let e = Choice::parse("wayland").expect_err("not a backend, and not a family either");
        assert!(e.contains("auto"), "{e}");
        assert!(e.contains("x11"), "{e}");
        assert!(e.contains("wlr"), "{e}");
        assert!(e.contains("dxgi"), "{e}");
        assert!(e.contains("synthetic"), "{e}");
    }

    /// Synthetic must always work, because a headless server is a first-class
    /// target rather than a degraded one.
    #[test]
    fn synthetic_is_always_available() {
        let s = open(Choice::Synthetic, None, (320, 240)).expect("synthetic opens");
        assert_eq!(s.capturer.backend(), "synthetic");
        assert!(s.attempts.iter().any(|a| a.contains("as asked")));
    }

    /// **Asking for a real backend and silently getting a test pattern is the
    /// failure this prevents.** Somebody who named a backend gets it or an
    /// error, never a substitute.
    #[test]
    fn naming_a_backend_that_cannot_open_is_an_error_not_a_fallback() {
        if std::env::var_os("DISPLAY").is_some() {
            println!("skipped: there is a display, so x11 can open");
            return;
        }
        let r = open(Choice::X11, None, (320, 240));
        assert!(
            r.is_err(),
            "with no DISPLAY, --backend x11 must fail rather than hand back synthetic"
        );
    }

    /// Auto, with no display, must still produce something.
    #[test]
    fn auto_falls_back_and_says_so() {
        let s = open(Choice::Auto, None, (320, 240)).expect("auto always opens something");
        // On a machine with a display this is x11; without one it is
        // synthetic. Either way the attempts have to explain the choice.
        assert!(!s.attempts.is_empty(), "auto chose silently");
        assert!(
            s.attempts.iter().any(|a| a.contains("in use")),
            "no attempt is marked as the one in use: {:?}",
            s.attempts
        );
    }
}
