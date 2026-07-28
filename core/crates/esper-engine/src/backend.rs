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
    /// The known moving pattern. Always available, never a real screen.
    Synthetic,
}

impl Choice {
    /// Parse a `--backend` value.
    pub fn parse(s: &str) -> Result<Self, String> {
        match s {
            "auto" => Ok(Self::Auto),
            "x11" => Ok(Self::X11),
            "synthetic" => Ok(Self::Synthetic),
            other => Err(format!(
                "--backend wants auto, x11 or synthetic, got {other}"
            )),
        }
    }
}

impl fmt::Display for Choice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Auto => write!(f, "auto"),
            Self::X11 => write!(f, "x11"),
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
/// `size` is the synthetic screen's size, and is ignored by a real backend:
/// a real screen is whatever size it is.
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
            Ok(None) => attempts.push("x11: not compiled in for this platform".to_owned()),
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

    if choice == Choice::Auto {
        attempts.push("synthetic: in use, because nothing else was available".to_owned());
    } else {
        attempts.push("synthetic: in use, as asked".to_owned());
    }
    let mut synthetic = esper_capture::synthetic::SyntheticCapturer::new(size.0, size.1, 1_000_000)
        .map_err(|e| CaptureError::Backend {
            backend: "synthetic",
            detail: e.to_string(),
        })?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_backend_names_round_trip() {
        for c in [Choice::Auto, Choice::X11, Choice::Synthetic] {
            assert_eq!(Choice::parse(&c.to_string()), Ok(c));
        }
    }

    #[test]
    fn an_unknown_backend_says_what_it_accepts() {
        let e = Choice::parse("wayland").expect_err("not a backend yet");
        assert!(e.contains("auto"), "{e}");
        assert!(e.contains("x11"), "{e}");
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
