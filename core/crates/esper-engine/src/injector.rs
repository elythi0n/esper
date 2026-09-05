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

/// Keep what the portal handed back, when it is not what we already had.
///
/// Written only on a change: a portal that returns the same token every
/// session would otherwise rewrite the file on every session, and a file
/// rewritten for no reason is a file that can be lost for no reason.
#[cfg(target_os = "linux")]
fn remember(kept: Option<&str>, held: Option<&str>, attempts: &mut Vec<String>) {
    let Some(token) = kept else {
        return;
    };
    if held == Some(token) {
        return;
    }
    if let Err(e) = crate::portal_grant::keep(token) {
        // Not fatal: the session works, and only the *next* one pays for it.
        attempts.push(format!("the grant could not be kept: {e}"));
    }
}

/// What an unattended session would be able to do here, without asking anybody.
///
/// **Asked without opening the portal**, which is the whole point: the portal
/// is the rung that can put a dialog on somebody's screen, and a host that
/// probed it at startup would be a host that prompts every time it starts.
///
/// `Ok(backend)` when input needs nobody's agreement — X11's XTEST, or a
/// wlroots compositor's virtual devices, either of which an unattended session
/// can use at three in the morning. `Err(())` when the only way in is a portal
/// and this machine holds no grant for it, which is a promise the host must
/// not make.
#[cfg(target_os = "linux")]
pub fn unattended_input() -> Result<String, ()> {
    // Opened and dropped: what is being established is whether it *can* be
    // opened, and a session that starts a moment later opens its own.
    if let Ok(injector) = esper_input_x11::X11Injector::open() {
        let name = injector.backend().to_owned();
        drop(injector);
        return Ok(name);
    }
    if let Ok(injector) = esper_input_wlr::WlrInput::open() {
        let name = injector.backend().to_owned();
        drop(injector);
        return Ok(name);
    }
    // A grant somebody gave once is as good as a backend that never asks.
    match crate::portal_grant::held() {
        Some(_) => Ok(format!(
            "{} (a grant somebody gave once)",
            esper_input_portal::BACKEND
        )),
        None => Err(()),
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

        // Then wlroots' own virtual devices.
        //
        // Second rather than first because X11 works under XWayland, which is
        // most Wayland sessions, and it is the complete path. This one exists
        // for the sessions where there is no X server at all — and on wlroots
        // there is no RemoteDesktop portal either, so without it a sway host
        // is view-only.
        //
        // It asks nobody's permission: the compositor grants these protocols
        // to anything that can open its socket. That is weaker than the portal
        // and is why the portal is preferred where one exists — but a wlroots
        // compositor has none to prefer.
        match esper_input_wlr::WlrInput::open() {
            Ok(injector) => {
                attempts.push(format!("{}: in use", esper_input_wlr::BACKEND));
                return Chosen {
                    sink: Box::new(injector),
                    attempts,
                    real: true,
                };
            }
            Err(e) => attempts.push(format!("{}: {e}", esper_input_wlr::BACKEND)),
        }

        /*
            And the portal, last.

            Last because it is the one that can put a dialog on somebody's
            screen. X11 works under XWayland and wlroots grants its virtual
            devices to anything that can open the compositor's socket, so a
            machine that reaches this rung is one where neither was there —
            GNOME without XWayland, most often — and there the portal is the
            only way in at all.

            **The grant this machine already holds is offered first.** Without
            it every session asks, which is a dialog nobody is there to answer
            on exactly the machines unattended access is for. A token the
            compositor no longer honours is let go of rather than kept: a
            token that is refused is worse than none, because it is a reason
            to believe a session will start when it will not.
        */
        let held = crate::portal_grant::held();
        match esper_input_portal::PortalInput::reopen(held.as_deref()) {
            Ok(injector) => {
                let kept = injector.restore_token().map(str::to_owned);
                attempts.push(format!(
                    "{}: in use{}",
                    esper_input_portal::BACKEND,
                    match &kept {
                        Some(_) => ", and the grant is kept",
                        None =>
                            ", and the compositor keeps no grant, so the next session asks again",
                    }
                ));
                remember(kept.as_deref(), held.as_deref(), &mut attempts);
                return Chosen {
                    sink: Box::new(injector),
                    attempts,
                    real: true,
                };
            }
            Err(e) => {
                attempts.push(format!("{}: {e}", esper_input_portal::BACKEND));
                // A grant that did not get us in is not a grant. Kept, it
                // would go on making this machine look reachable.
                if held.is_some()
                    && let Err(why) = crate::portal_grant::forget()
                {
                    attempts.push(format!("could not let go of a refused grant: {why}"));
                }
            }
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
