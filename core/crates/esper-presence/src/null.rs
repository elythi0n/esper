//! A presence that shows nothing and remembers everything.
//!
//! How the indicator's *lifecycle* is tested without a display: that it goes
//! up when a session starts, changes when the grant changes, and comes down
//! when the session ends — and, most of all, that nothing a controller sends
//! can make it come down early.

use crate::{Blanking, HostEvent, Missing, Presence, PresenceError, Verdict, Watching};

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
    /// Whether blanking should fail, for testing what a caller does about it.
    pub blanking_fails: bool,
    blanked: bool,
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

    /// Whether the screen is pretending to be blank.
    #[must_use]
    pub const fn blanked(&self) -> bool {
        self.blanked
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

    fn blanking_plan(&mut self) -> Result<Blanking, PresenceError> {
        Ok(Blanking {
            what: "a screen that is not there".to_owned(),
            undo: vec![esper_keeper::Undo::Crtc {
                crtc: 1,
                x: 0,
                y: 0,
                mode: 2,
                rotation: 1,
                outputs: vec![3],
                what: "a screen that is not there".to_owned(),
            }],
        })
    }

    fn blank(&mut self, _plan: &Blanking) -> Result<(), PresenceError> {
        if self.blanking_fails {
            return Err(PresenceError::Backend {
                backend: "null",
                detail: "this screen refuses to blank".to_owned(),
            });
        }
        self.blanked = true;
        Ok(())
    }

    fn unblank(&mut self) -> Result<(), PresenceError> {
        self.blanked = false;
        Ok(())
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
