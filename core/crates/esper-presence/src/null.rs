//! A presence that shows nothing and remembers everything.
//!
//! How the indicator's *lifecycle* is tested without a display: that it goes
//! up when a session starts, changes when the grant changes, and comes down
//! when the session ends — and, most of all, that nothing a controller sends
//! can make it come down early.

use crate::{HostEvent, Missing, Presence, PresenceError, Verdict, Watching};

/// One thing the indicator was asked to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Shown {
    Up(Watching),
    Down,
}

/// A presence that records rather than displays.
#[derive(Debug, Default)]
pub struct NullPresence {
    /// Everything asked for, in order.
    pub asked: Vec<Shown>,
    /// Events to hand out on the next poll, for testing what a caller does
    /// with a panic.
    pub pending: Vec<HostEvent>,
    /// What the next verification should find, when it should not find the
    /// indicator.
    ///
    /// **The whole point of this backend for the verifying half**: a session
    /// that stops because the indicator cannot be confirmed is the behaviour
    /// worth testing, and it cannot be provoked on a real display without
    /// something drawing over the screen.
    pub screen_says: Option<Missing>,
    /// How many times the screen was asked.
    pub verified: usize,
    showing: Option<Watching>,
}

impl NullPresence {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Pretend the person hit the panic key.
    pub fn panic_key(&mut self) {
        self.pending.push(HostEvent::Panic);
    }

    /// Pretend the person hit the pause key.
    pub fn pause_key(&mut self) {
        self.pending.push(HostEvent::Pause);
    }

    /// Pretend something is drawn over the frame.
    pub fn cover(&mut self) {
        self.screen_says = Some(Missing::Covered {
            wrong: 3,
            of: 12,
            saw: [0, 0, 0],
            at: (2, 2),
        });
    }

    /// Pretend a compositor destroyed the frame's windows.
    pub fn destroy(&mut self) {
        self.screen_says = Some(Missing::Gone);
    }

    /// Pretend the screen is showing what it was asked to.
    pub fn uncover(&mut self) {
        self.screen_says = None;
    }

    /// What the indicator currently says.
    #[must_use]
    pub const fn saying(&self) -> Option<&Watching> {
        self.showing.as_ref()
    }
}

impl Presence for NullPresence {
    fn backend(&self) -> &'static str {
        "null"
    }

    fn show(&mut self, watching: &Watching) -> Result<(), PresenceError> {
        // Showing again is what a caller does about windows that are gone, so
        // it clears that — but not something drawn *over* the frame, which
        // rebuilding does not fix on a real display either.
        if self
            .screen_says
            .as_ref()
            .is_some_and(Missing::worth_rebuilding)
        {
            self.screen_says = None;
        }
        self.showing = Some(watching.clone());
        self.asked.push(Shown::Up(watching.clone()));
        Ok(())
    }

    fn hide(&mut self) -> Result<(), PresenceError> {
        self.showing = None;
        self.asked.push(Shown::Down);
        Ok(())
    }

    fn poll(&mut self) -> Result<Vec<HostEvent>, PresenceError> {
        Ok(std::mem::take(&mut self.pending))
    }

    fn is_showing(&self) -> bool {
        self.showing.is_some()
    }

    fn verify(&mut self) -> Result<Verdict, PresenceError> {
        self.verified = self.verified.saturating_add(1);
        if self.showing.is_none() {
            return Ok(Verdict::Missing(Missing::NothingAsked));
        }
        // A rebuild is what a caller does about `Gone`, and this backend has
        // to let one succeed or the caller's recovery path is untestable: the
        // next `show` clears it, the same way building the windows again would.
        match self.screen_says.clone() {
            Some(missing) => Ok(Verdict::Missing(missing)),
            None => Ok(Verdict::OnScreen { checked: 12 }),
        }
    }
}
