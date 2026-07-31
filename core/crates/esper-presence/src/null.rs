//! A presence that shows nothing and remembers everything.
//!
//! How the indicator's *lifecycle* is tested without a display: that it goes
//! up when a session starts, changes when the grant changes, and comes down
//! when the session ends — and, most of all, that nothing a controller sends
//! can make it come down early.

use crate::{HostEvent, Presence, PresenceError, Watching};

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
}
