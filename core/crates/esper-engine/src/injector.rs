//! Choosing something to inject input with.
//!
//! The same shape as [`crate::backend`]: try the platform's backends in order,
//! keep what was tried, and say what happened. A host that cannot inject is
//! still a host — it can be *viewed* — so this returns the reason rather than
//! refusing to start, and the session says "view only" out loud.

use esper_input::{InputSink, NullSink};

/// What was chosen, and what was tried on the way.
pub struct Chosen {
    pub sink: Box<dyn InputSink>,
    /// One line per attempt, for the session's own log.
    pub attempts: Vec<String>,
    /// Whether anything real was found. A recording sink is not injection.
    pub real: bool,
}

impl std::fmt::Debug for Chosen {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Chosen")
            .field("backend", &self.sink.backend())
            .field("real", &self.real)
            .finish_non_exhaustive()
    }
}

/// Open an input sink for this machine.
///
/// **Never falls back to a sink that silently discards input.** A host that
/// accepted keystrokes and dropped them would look like a working session
/// where nothing happens, which is the hardest kind of fault to diagnose from
/// the other end. When nothing real is available the caller is told, and it
/// tells the controller.
#[must_use]
pub fn open() -> Chosen {
    let mut attempts = Vec::new();

    #[cfg(target_os = "linux")]
    {
        match esper_input_x11::X11Injector::open() {
            Ok(injector) => {
                attempts.push("x11 xtest: in use".to_owned());
                return Chosen {
                    sink: Box::new(injector),
                    attempts,
                    real: true,
                };
            }
            Err(e) => attempts.push(format!("x11 xtest: {e}")),
        }
    }
    #[cfg(target_os = "windows")]
    {
        match esper_input_win::WinInjector::open() {
            Ok(injector) => {
                attempts.push(format!("{}: in use", esper_input_win::BACKEND));
                return Chosen {
                    sink: Box::new(injector),
                    attempts,
                    real: true,
                };
            }
            Err(e) => attempts.push(format!("{}: {e}", esper_input_win::BACKEND)),
        }
    }
    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    {
        attempts.push("no injection backend is compiled in for this platform".to_owned());
    }

    Chosen {
        sink: Box::new(NullSink::new()),
        attempts,
        real: false,
    }
}
