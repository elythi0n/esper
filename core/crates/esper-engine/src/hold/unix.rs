//! The signal that holds a session's picture: `SIGUSR1`.
//!
//! A machine with no screen has no hotkey, and a script has no keyboard at
//! all. `kill -USR1` is the trigger both of them have, and it lands as the
//! same `HostEvent::Pause` a key press does — so there is one place that
//! decides what pausing means.

/// A listener for the hold signal.
#[derive(Debug)]
pub struct Hold(Option<tokio::signal::unix::Signal>);

impl Hold {
    /// Start listening, and say what to tell the operator if it did not work.
    ///
    /// The line is returned rather than printed so that the caller keeps its
    /// own report's shape — and so that a platform with no such signal says
    /// something different from a platform where the call failed.
    pub fn listen() -> (Self, Option<String>) {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::user_defined1()) {
            Ok(signal) => (Self(Some(signal)), None),
            // Said, not swallowed: a host whose pause signal is unavailable
            // still works, and somebody who tried it would otherwise be left
            // wondering which end was broken.
            Err(e) => (
                Self(None),
                Some(format!("`kill -USR1` will not hold this session: {e}")),
            ),
        }
    }

    /// Wait for the signal, or for ever when there is none to wait for.
    ///
    /// A `select!` branch needs a future either way, which is why the
    /// unavailable case pends rather than returning.
    pub async fn wait(&mut self) {
        match &mut self.0 {
            Some(signal) => {
                signal.recv().await;
            }
            None => std::future::pending().await,
        }
    }
}
