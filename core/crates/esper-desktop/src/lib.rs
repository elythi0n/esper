//! Which display server this machine is running.
//!
//! # Why this exists
//!
//! Esper on a Wayland desktop refuses to serve, which is right, and explains
//! itself like this:
//!
//! ```text
//! presence   ⚠ NONE (no way to show a session indicator:
//!            X11 via DISPLAY: $DISPLAY variable not set
//!            and no value was provided explicitly)
//! ```
//!
//! Every word of that is true and it names the wrong thing. The machine is not
//! missing a variable; it is running Wayland, where the X11 path was never
//! going to work. A person on Fedora or Ubuntu — which is GNOME on Wayland by
//! default — reads that message, sets `DISPLAY`, and gets a different error.
//!
//! Measured in a real sway session on 2026-09-12, in
//! `scripts/rigs/wayland.Containerfile`, because the machine this is developed
//! on runs X11 under XFCE and cannot produce the failure it is about.
//!
//! # What this is for, and what it is not
//!
//! It is for **explaining a failure**, not for choosing a backend. That
//! distinction is the whole design, and the reason is XWayland: a Wayland
//! session usually runs an X server too, so `DISPLAY` and `WAYLAND_DISPLAY`
//! are both set and the X11 backend **works**. A function that read
//! `WAYLAND_DISPLAY` and concluded "do not try X11" would break the case that
//! currently works, on the desktops where most people would meet it.
//!
//! So nothing here is consulted before an attempt. It is consulted after one
//! has failed, to say what kind of machine it failed on.

/// What the environment says this machine is running.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Session {
    /// An X server, and nothing saying Wayland.
    X11,
    /// Wayland, with no X server offered alongside it.
    Wayland,
    /// Wayland with XWayland: both are there, and the X11 path should work.
    WaylandWithX11,
    /// Neither. A server over SSH, a container, a machine with no seat.
    Headless,
}

/// What the environment says, from the three variables that say it.
///
/// Taken as arguments rather than read here so that a test can ask about a
/// machine it is not running on — which is every machine that matters, since
/// this exists to describe desktops the development machine is not.
#[must_use]
pub fn session_from(
    display: Option<&str>,
    wayland_display: Option<&str>,
    session_type: Option<&str>,
) -> Session {
    // Empty is unset. A shell that exports `DISPLAY=` has not given anybody a
    // display, and treating the empty string as a value is how a machine ends
    // up reported as X11 with nothing behind it.
    let display = display.filter(|s| !s.is_empty());
    let wayland = wayland_display.filter(|s| !s.is_empty());

    // `XDG_SESSION_TYPE` is the session manager's own word for it, and it is
    // the only one of the three that is present when a compositor has not
    // exported anything into this particular process's environment.
    let says_wayland = session_type.is_some_and(|t| t.eq_ignore_ascii_case("wayland"));
    let says_x11 = session_type.is_some_and(|t| t.eq_ignore_ascii_case("x11"));

    match (display.is_some(), wayland.is_some() || says_wayland) {
        (true, true) => Session::WaylandWithX11,
        (true, false) => Session::X11,
        (false, true) => Session::Wayland,
        // `XDG_SESSION_TYPE=x11` with no `DISPLAY` is a login session that has
        // an X server somewhere this process was not told about — a user
        // service started before the session, most often. Reported as X11 so
        // the advice is "this process was not given DISPLAY" rather than
        // "this machine has no display", which are different problems.
        (false, false) if says_x11 => Session::X11,
        (false, false) => Session::Headless,
    }
}

/// What this process's own environment says.
#[must_use]
pub fn session() -> Session {
    session_from(
        std::env::var("DISPLAY").ok().as_deref(),
        std::env::var("WAYLAND_DISPLAY").ok().as_deref(),
        std::env::var("XDG_SESSION_TYPE").ok().as_deref(),
    )
}

/// A sentence to add when an X11 backend has just failed to start.
///
/// `None` where the environment adds nothing — on a machine that really is
/// X11, the X11 error is already the whole story, and a second sentence saying
/// "this is X11" is noise in front of the useful one.
#[must_use]
pub fn why_x11_failed(session: Session) -> Option<&'static str> {
    match session {
        Session::Wayland => Some(
            "this is a Wayland session with no X server; input and the session \
             indicator have Wayland paths on wlroots, and capture still goes \
             through X11",
        ),
        Session::WaylandWithX11 => Some(
            "this is a Wayland session; XWayland is running, so X11 should have \
             worked and something else is wrong",
        ),
        Session::Headless => Some(
            "no display server of either kind is running, or this process was \
             started without being told about one",
        ),
        Session::X11 => None,
    }
}

/// What a failed X11 attempt should report, in one place.
///
/// Five crates start an X11 connection and five wrote the same sentence, so
/// the sentence lives here: `esper-input-x11`, `esper-capture-x11` twice,
/// `esper-presence-x11` and `esper-present-x11`. Five copies is five chances
/// for a machine to be described differently depending on which subsystem
/// happened to fail first.
#[must_use]
pub fn x11_failed(error: &str) -> String {
    match why_x11_failed(session()) {
        Some(why) => format!("X11 via DISPLAY: {error} — {why}"),
        None => format!("X11 via DISPLAY: {error}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The case this crate exists for: GNOME or KDE on Wayland, where the old
    /// message blamed a missing variable.
    #[test]
    fn wayland_alone_is_wayland() {
        assert_eq!(
            session_from(None, Some("wayland-1"), Some("wayland")),
            Session::Wayland
        );
        assert!(why_x11_failed(Session::Wayland).is_some_and(|s| s.contains("Wayland")));
    }

    /// **The case that must not regress.** Under XWayland both variables are
    /// set and the X11 backend works; a reading of "Wayland, so do not try
    /// X11" would break the desktops where this is most likely to be met.
    #[test]
    fn xwayland_keeps_x11_on_the_table() {
        let under_xwayland = session_from(Some(":0"), Some("wayland-1"), Some("wayland"));
        assert_eq!(under_xwayland, Session::WaylandWithX11);
        // Named as its own variant rather than lumped in with `Wayland`,
        // because the X11 backend works here and the whole point is that it
        // keeps being tried.
        assert_ne!(under_xwayland, Session::Wayland);
    }

    #[test]
    fn plain_x11_says_nothing_extra() {
        let plain = session_from(Some(":0"), None, Some("x11"));
        assert_eq!(plain, Session::X11);
        assert_eq!(why_x11_failed(plain), None, "the X11 error is the story");
    }

    #[test]
    fn nothing_at_all_is_headless() {
        assert_eq!(session_from(None, None, None), Session::Headless);
        assert!(why_x11_failed(Session::Headless).is_some_and(|s| s.contains("no display server")));
    }

    /// An exported-but-empty variable is not a display. A shell script that
    /// does `DISPLAY=` and exports it is common enough to matter, and reading
    /// it as a value reports the machine as X11 with nothing behind it.
    #[test]
    fn an_empty_variable_is_not_a_display() {
        assert_eq!(session_from(Some(""), Some(""), None), Session::Headless);
        assert_eq!(
            session_from(Some(""), Some("wayland-1"), None),
            Session::Wayland
        );
    }

    /// A session type with no display: the X server exists and this process
    /// was not told where. Different advice from "there is no display".
    #[test]
    fn a_session_type_without_a_display_still_means_x11() {
        assert_eq!(session_from(None, None, Some("x11")), Session::X11);
    }

    /// `XDG_SESSION_TYPE` alone is enough, because a user service can be
    /// started before the compositor exports anything into its environment.
    #[test]
    fn the_session_type_alone_can_say_wayland() {
        assert_eq!(session_from(None, None, Some("wayland")), Session::Wayland);
        assert_eq!(session_from(None, None, Some("WAYLAND")), Session::Wayland);
    }
}
