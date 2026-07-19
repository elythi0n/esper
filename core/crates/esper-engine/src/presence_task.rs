//! The host's indicator and panic key, on a thread of their own.
//!
//! # Why a thread
//!
//! The indicator and the panic key belong to one X connection, and that
//! connection has to be watched continuously: **the panic key is the one thing
//! in this product where a delay is measured against somebody's alarm.** A
//! session loop that polled it between frames would answer it whenever the
//! next frame happened to arrive.
//!
//! So a thread owns the connection, polls it every few tens of milliseconds,
//! and talks to the rest of the engine through two channels: what to show goes
//! in, and the panic goes out. The polling costs a wake-up every thirty
//! milliseconds doing almost nothing, which is the price of not having to
//! reason about how long the session loop might be busy.

// Nothing in this module runs on a platform with no backend for it: `start`
// refuses and the session says so. It is still compiled everywhere, which is
// deliberate — the thread below is generic over the trait, so it is the part a
// new platform reuses rather than rewrites, and compiling it everywhere is
// what keeps it honest.
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

use std::sync::mpsc::{Receiver, Sender, TryRecvError};
use std::time::Duration;

use esper_presence::{HostEvent, Presence, Watching};

/// How often the indicator's connection is looked at.
const TICK: Duration = Duration::from_millis(30);

/// What the session wants the indicator to say.
#[derive(Debug)]
enum Command {
    Show(Watching),
    Hide,
    Stop,
}

/// The handle the rest of the engine holds.
#[derive(Debug)]
pub struct Indicator {
    to_thread: Sender<Command>,
    backend: &'static str,
    /// Whether the panic key is actually held by us.
    armed: bool,
    /// Whether an indicator can be shown at all.
    can_show: bool,
}

impl Indicator {
    /// Which backend is showing it.
    #[must_use]
    pub const fn backend(&self) -> &'static str {
        self.backend
    }

    /// Whether the panic key is working.
    #[must_use]
    pub const fn armed(&self) -> bool {
        self.armed
    }

    /// Whether an indicator can be shown.
    #[must_use]
    pub const fn can_show(&self) -> bool {
        self.can_show
    }

    /// Show that a session is happening.
    pub fn show(&self, watching: Watching) {
        // Best effort by design: the thread is gone only when the process is
        // going, and a session that cannot raise an indicator has already
        // been told so by `can_show`.
        let _ = self.to_thread.send(Command::Show(watching));
    }

    /// Take the indicator away.
    pub fn hide(&self) {
        let _ = self.to_thread.send(Command::Hide);
    }
}

impl Drop for Indicator {
    fn drop(&mut self) {
        let _ = self.to_thread.send(Command::Stop);
    }
}

/// Each press of the panic key, once.
///
/// **A queue, not a flag.** A flag was the first version and it was wrong: a
/// latched "it happened" makes every loop that waits on it return immediately
/// for ever, so the session ended, the accept loop saw the panic again, and
/// again — four and a half million times in three seconds, measured, filling
/// a log with 1.5 GB and burning a core.
///
/// A press is an *event*, acted on once by whoever is waiting. Only one loop
/// waits at a time — while a session is running the accept loop is inside it —
/// so one receiver is the whole mechanism.
pub type Presses = tokio::sync::mpsc::Receiver<()>;

/// Start the indicator and grab the panic key.
///
/// `Err` when this machine has no way to do either. **A caller must not treat
/// that as ordinary**: a host that cannot show it is being watched is failing
/// to provide something a person relies on, and it should say so loudly rather
/// than serve a session quietly.
pub fn start() -> Result<(Indicator, Presses), String> {
    #[cfg(target_os = "linux")]
    {
        let presence = esper_presence_x11::X11Presence::open().map_err(|e| e.to_string())?;
        let armed = presence.panic_key_grabbed();
        let can_show = presence.can_show();
        let backend = esper_presence::Presence::backend(&presence);
        let (to_thread, commands) = std::sync::mpsc::channel();
        // Four deep: a person hitting the key repeatedly should not have
        // presses dropped, and nobody presses it more than a handful of times.
        let (panic_tx, panic_rx) = tokio::sync::mpsc::channel(4);
        std::thread::spawn(move || run(presence, commands, panic_tx));
        Ok((
            Indicator {
                to_thread,
                backend,
                armed,
                can_show,
            },
            panic_rx,
        ))
    }
    #[cfg(not(target_os = "linux"))]
    {
        Err("no presence backend is compiled in for this platform".to_owned())
    }
}

/// The thread: show what it is told, and report the panic key.
fn run<P: Presence>(
    mut presence: P,
    commands: Receiver<Command>,
    presses: tokio::sync::mpsc::Sender<()>,
) {
    loop {
        // **Polled before the commands are read**, so a panic pressed at the
        // same moment the host is shutting down is still reported. Acting on
        // it costs nothing and reporting it is what closes the sessions; the
        // other order drops it on the floor.
        match presence.poll() {
            Ok(events) => {
                for _ in events.iter().filter(|e| **e == HostEvent::Panic) {
                    // `try_send`, and a full queue is dropped on purpose: four
                    // presses nobody has acted on yet means the fifth adds
                    // nothing, and blocking this thread would stop the
                    // indicator being kept on top.
                    let _ = presses.try_send(());
                }
            }
            Err(e) => {
                tracing::error!("the session indicator's display went away: {e}");
                break;
            }
        }
        match commands.try_recv() {
            Ok(Command::Show(watching)) => {
                if let Err(e) = presence.show(&watching) {
                    tracing::error!("could not show the session indicator: {e}");
                }
            }
            Ok(Command::Hide) => {
                if let Err(e) = presence.hide() {
                    tracing::error!("could not take the session indicator down: {e}");
                }
            }
            Ok(Command::Stop) | Err(TryRecvError::Disconnected) => break,
            Err(TryRecvError::Empty) => {}
        }
        std::thread::sleep(TICK);
    }
    let _ = presence.hide();
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;
    use esper_presence::{NullPresence, Shown};
    use std::sync::mpsc::channel;

    fn watching(controlling: bool) -> Watching {
        Watching {
            peer: "somebody".to_owned(),
            controlling,
        }
    }

    /// The thread does what it is told and reports what happened, checked with
    /// no display in the way.
    #[test]
    fn the_thread_shows_hides_and_reports_a_panic() {
        let mut presence = NullPresence::new();
        presence.panic_key();
        let (to_thread, commands) = channel();
        let (panic_tx, mut panic_rx) = tokio::sync::mpsc::channel(4);

        to_thread.send(Command::Show(watching(true))).expect("send");
        to_thread.send(Command::Hide).expect("send");
        to_thread.send(Command::Stop).expect("send");

        // Run it to completion on this thread: the commands are already
        // queued, so there is nothing to wait for.
        run(presence, commands, panic_tx);

        assert_eq!(
            panic_rx.try_recv(),
            Ok(()),
            "the press must have been reported"
        );
    }

    /// **One press is one event.** The first version latched a flag, so every
    /// loop waiting on it returned for ever: four and a half million panics in
    /// three seconds, a 1.5 GB log and a busy core. A queue is delivered once.
    #[test]
    fn a_press_is_delivered_once_and_not_for_ever() {
        let mut presence = NullPresence::new();
        presence.panic_key();
        let (to_thread, commands) = channel();
        let (panic_tx, mut panic_rx) = tokio::sync::mpsc::channel(4);
        to_thread.send(Command::Stop).expect("send");
        run(presence, commands, panic_tx);

        assert_eq!(panic_rx.try_recv(), Ok(()), "one press arrives");
        assert!(
            panic_rx.try_recv().is_err(),
            "and there is not a second one waiting"
        );
    }

    /// Two presses are two events, so a person who hits it again during the
    /// next session is heard again.
    #[test]
    fn two_presses_are_two_events() {
        let mut presence = NullPresence::new();
        presence.panic_key();
        presence.panic_key();
        let (to_thread, commands) = channel();
        let (panic_tx, mut panic_rx) = tokio::sync::mpsc::channel(4);
        to_thread.send(Command::Stop).expect("send");
        run(presence, commands, panic_tx);
        assert_eq!(panic_rx.try_recv(), Ok(()));
        assert_eq!(panic_rx.try_recv(), Ok(()));
        assert!(panic_rx.try_recv().is_err());
    }

    /// **A panic is reported even if nothing was ever shown.** The key works
    /// from the moment the host is running, including while a session is only
    /// being offered.
    #[test]
    fn a_panic_with_no_session_is_still_reported() {
        let mut presence = NullPresence::new();
        presence.panic_key();
        let (to_thread, commands) = channel();
        let (panic_tx, mut panic_rx) = tokio::sync::mpsc::channel(4);
        to_thread.send(Command::Stop).expect("send");
        run(presence, commands, panic_tx);
        assert_eq!(panic_rx.try_recv(), Ok(()));
    }

    /// Dropping the handle stops the thread, so a host that finishes does not
    /// leave a frame on somebody's screen.
    #[test]
    fn the_indicator_comes_down_when_the_thread_stops() {
        let presence = NullPresence::new();
        let (to_thread, commands) = channel();
        let (panic_tx, _panic_rx) = tokio::sync::mpsc::channel(4);
        to_thread
            .send(Command::Show(watching(false)))
            .expect("send");
        drop(to_thread);
        // A disconnected channel ends the thread, which hides on the way out.
        run(presence, commands, panic_tx);
        // The recording presence is consumed by `run`, so what is checked here
        // is that it terminated at all rather than looping for ever — a
        // disconnected command channel is how the process going away looks
        // from in here.
    }

    /// What the recording presence saw, when it is kept rather than moved into
    /// the thread.
    #[test]
    fn showing_twice_with_different_grants_says_both_things() {
        let mut presence = NullPresence::new();
        Presence::show(&mut presence, &watching(false)).expect("shown");
        Presence::show(&mut presence, &watching(true)).expect("shown");
        Presence::hide(&mut presence).expect("hidden");
        assert_eq!(
            presence.asked,
            vec![
                Shown::Up(watching(false)),
                Shown::Up(watching(true)),
                Shown::Down
            ]
        );
        assert!(!presence.is_showing());
    }
}
