//! Holding a session's picture on a platform with no signal for it.
//!
//! `SIGUSR1` is the trigger a script and a screenless machine both have on
//! Unix. **Windows has no equivalent**, and the near-misses are worse than
//! nothing: `CTRL_BREAK_EVENT` is a console gesture that also reaches whatever
//! else shares the console, and it means "interrupt", not "hold the picture".
//! Sending a made-up meaning down a signal people already use for something
//! else would be a surprise on somebody's machine.
//!
//! So this arm listens for nothing and says so once, which is the honest
//! answer until the real one exists. The real one is the local API — a request
//! to hold a session is a thing a dashboard can make and a script can `curl`,
//! it works the same on every platform, and it is authenticated, which
//! `kill -USR1` is not. Recorded as a Phase 6 item.

/// A listener for a signal this platform does not have.
#[derive(Debug)]
pub struct Hold;

impl Hold {
    /// Start listening — which here means not listening, and saying so.
    pub fn listen() -> (Self, Option<String>) {
        (
            Self,
            Some(
                "no signal holds a session on this platform; use a hotkey at the \
                 screen, or the local API once it can ask"
                    .to_owned(),
            ),
        )
    }

    /// Pend for ever: nothing will arrive.
    ///
    /// A `select!` branch needs a future either way, and a branch that
    /// returned immediately would spin the session's loop.
    pub async fn wait(&mut self) {
        std::future::pending().await
    }
}
