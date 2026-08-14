//! The rule that wedges Desktop Duplication when it is broken.
//!
//! # One release per acquire, and never an acquire while holding
//!
//! `AcquireNextFrame` hands over a frame and a lock on the desktop image.
//! `ReleaseFrame` gives it back. Between them the duplication is **holding**,
//! and calling `AcquireNextFrame` again in that state fails — and keeps
//! failing, because the state never changes on its own. The session does not
//! crash; it stops producing frames and goes on looking connected.
//!
//! That is the single most common way to get Desktop Duplication wrong, and it
//! is the kind of bug that survives review: the release is *there*, on some
//! path, and the path that misses it is an error return somebody added later.
//!
//! # Why it is a type here rather than a comment there
//!
//! Because this part can be tested and the calls cannot. The backend that
//! makes the calls is `cfg(target_os = "windows")`, so none of its tests run on
//! the machine this was written on — so the sequencing lives here, in the
//! platform-neutral crate, compiled and tested everywhere, and the backend
//! reports outcomes to it rather than remembering the rule itself.
//!
//! The other half of the rule is the one nobody writes down: **a timeout must
//! not be released.** `AcquireNextFrame` returning `DXGI_ERROR_WAIT_TIMEOUT`
//! did not hand anything over, and releasing then is an error the API reports
//! and most callers ignore — after which the *next* real frame is the one that
//! goes unreleased, and the wedge appears one frame later than its cause.

/// What `AcquireNextFrame` said.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Acquired {
    /// A frame arrived, and the desktop image is now held.
    Frame,
    /// Nothing changed before the deadline. **Nothing is held.**
    Timeout,
    /// `DXGI_ERROR_ACCESS_LOST`. The duplication is finished — a session
    /// switch, a mode change, another process taking exclusive full screen, or
    /// the secure desktop coming up. Nothing is held and nothing will be
    /// again until it is recreated.
    AccessLost,
    /// Anything else. Treated as fatal for this duplication for the same
    /// reason as `AccessLost`: an error nobody enumerated is not one to guess
    /// the lock state from.
    Failed,
}

/// What the caller must do next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Next {
    /// Read the frame, then call `ReleaseFrame` and say so with
    /// [`Frames::released`].
    Read,
    /// Nothing arrived. Ask again; do **not** release.
    AskAgain,
    /// This duplication is over. Build another before asking again.
    Recreate,
}

/// Why a call was refused before it reached the API.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum OutOfTurn {
    /// An acquire while a frame is still held — the wedge.
    #[error(
        "a frame is still held: `AcquireNextFrame` while holding fails and keeps \
         failing, and the session goes on looking connected while producing nothing"
    )]
    StillHolding,

    /// A release with nothing held.
    #[error(
        "nothing is held, so there is nothing to release: releasing after a timeout \
         is an error the API reports and most callers ignore, and it leaves the next \
         real frame unreleased instead"
    )]
    HoldingNothing,

    /// Anything at all after the duplication was lost.
    #[error("this duplication is finished and has to be recreated before it is asked again")]
    Finished,
}

/// The frame lifecycle of one duplication.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Frames {
    holding: bool,
    finished: bool,
    /// How many frames were acquired and released, for a report that wants it.
    completed: u64,
}

impl Frames {
    /// A fresh duplication, holding nothing.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            holding: false,
            finished: false,
            completed: 0,
        }
    }

    /// Whether it is legal to call `AcquireNextFrame` right now.
    ///
    /// # Errors
    ///
    /// If a frame is still held, or the duplication is finished.
    pub const fn may_acquire(&self) -> Result<(), OutOfTurn> {
        if self.finished {
            return Err(OutOfTurn::Finished);
        }
        if self.holding {
            return Err(OutOfTurn::StillHolding);
        }
        Ok(())
    }

    /// Record what `AcquireNextFrame` said, and get the next step.
    ///
    /// # Errors
    ///
    /// If the call should not have been made — see [`Self::may_acquire`]. The
    /// outcome is **not** recorded in that case: a call that never legally
    /// happened must not move the state.
    pub fn acquired(&mut self, outcome: Acquired) -> Result<Next, OutOfTurn> {
        self.may_acquire()?;
        Ok(match outcome {
            Acquired::Frame => {
                self.holding = true;
                Next::Read
            }
            Acquired::Timeout => Next::AskAgain,
            Acquired::AccessLost | Acquired::Failed => {
                self.finished = true;
                Next::Recreate
            }
        })
    }

    /// Record that `ReleaseFrame` was called.
    ///
    /// # Errors
    ///
    /// If nothing was held, or the duplication is finished.
    pub fn released(&mut self) -> Result<(), OutOfTurn> {
        if self.finished {
            return Err(OutOfTurn::Finished);
        }
        if !self.holding {
            return Err(OutOfTurn::HoldingNothing);
        }
        self.holding = false;
        self.completed = self.completed.saturating_add(1);
        Ok(())
    }

    /// Record that the duplication was lost outside an acquire.
    ///
    /// `GetFrameDirtyRects` and the readback can fail with `ACCESS_LOST` too,
    /// after the acquire succeeded — so the lock is dropped along with the
    /// duplication, and the next thing that happens must be a rebuild rather
    /// than a release into an object that is gone.
    pub const fn lost(&mut self) {
        self.finished = true;
        self.holding = false;
    }

    /// Whether the desktop image is held.
    #[must_use]
    pub const fn is_holding(&self) -> bool {
        self.holding
    }

    /// Whether this duplication is over.
    #[must_use]
    pub const fn is_finished(&self) -> bool {
        self.finished
    }

    /// How many frames were acquired and released.
    #[must_use]
    pub const fn completed(&self) -> u64 {
        self.completed
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    #[test]
    fn a_frame_is_read_then_released() {
        let mut frames = Frames::new();
        assert_eq!(frames.acquired(Acquired::Frame), Ok(Next::Read));
        assert!(frames.is_holding());
        assert_eq!(frames.released(), Ok(()));
        assert!(!frames.is_holding());
        assert_eq!(frames.completed(), 1);
    }

    #[test]
    fn acquiring_while_holding_is_refused_before_it_reaches_the_api() {
        // The wedge. The API would fail and keep failing, and the session
        // would go on looking connected while producing nothing.
        let mut frames = Frames::new();
        assert_eq!(frames.acquired(Acquired::Frame), Ok(Next::Read));
        assert_eq!(
            frames.acquired(Acquired::Frame),
            Err(OutOfTurn::StillHolding)
        );
        // And the refused call did not move the state: still holding one frame.
        assert!(frames.is_holding());
        assert_eq!(frames.released(), Ok(()));
        assert_eq!(frames.completed(), 1, "the refused acquire is not a frame");
    }

    #[test]
    fn a_refused_acquire_moves_nothing_whatever_it_would_have_reported() {
        // Found by sabotage. The test above only tries `Frame`, where setting
        // `holding` on an already-holding machine changes nothing — so a
        // version that recorded the outcome *before* refusing passed it.
        //
        // A refused acquire carrying a fatal outcome is the case that shows
        // it: recording that would finish a duplication over a call the API
        // never saw, and the next legal acquire would be refused as
        // `Finished` on a duplication that is perfectly alive.
        for outcome in [
            Acquired::Frame,
            Acquired::Timeout,
            Acquired::AccessLost,
            Acquired::Failed,
        ] {
            let mut frames = Frames::new();
            frames.acquired(Acquired::Frame).expect("a frame");
            let before = Frames {
                holding: true,
                finished: false,
                completed: 0,
            };
            assert_eq!(frames, before, "the setup itself moved something");

            assert_eq!(
                frames.acquired(outcome),
                Err(OutOfTurn::StillHolding),
                "{outcome:?} while holding"
            );
            assert_eq!(
                frames, before,
                "{outcome:?} was refused and still moved the state"
            );
        }
    }

    #[test]
    fn a_timeout_holds_nothing_and_must_not_be_released() {
        // The half nobody writes down. Releasing after a timeout is an error
        // the API reports and most callers ignore, and the cost is that the
        // *next* real frame goes unreleased — so the wedge shows up one frame
        // after its cause.
        let mut frames = Frames::new();
        assert_eq!(frames.acquired(Acquired::Timeout), Ok(Next::AskAgain));
        assert!(!frames.is_holding());
        assert_eq!(frames.released(), Err(OutOfTurn::HoldingNothing));
        // And asking again is fine.
        assert_eq!(frames.acquired(Acquired::Frame), Ok(Next::Read));
    }

    #[test]
    fn many_timeouts_are_the_ordinary_case() {
        // A screen showing a document changes a few times a minute, so a
        // damage-driven pipeline times out far more often than it captures.
        let mut frames = Frames::new();
        for _ in 0..1_000 {
            assert_eq!(frames.acquired(Acquired::Timeout), Ok(Next::AskAgain));
        }
        assert_eq!(frames.completed(), 0);
        assert!(!frames.is_finished());
    }

    #[test]
    fn access_lost_finishes_it_and_nothing_more_is_allowed() {
        // A session switch, a mode change, another process going exclusive
        // full screen, or the secure desktop coming up.
        for fatal in [Acquired::AccessLost, Acquired::Failed] {
            let mut frames = Frames::new();
            assert_eq!(frames.acquired(fatal), Ok(Next::Recreate), "{fatal:?}");
            assert!(frames.is_finished());
            assert!(!frames.is_holding(), "a lost duplication holds nothing");
            assert_eq!(frames.acquired(Acquired::Frame), Err(OutOfTurn::Finished));
            assert_eq!(frames.released(), Err(OutOfTurn::Finished));
            assert_eq!(frames.may_acquire(), Err(OutOfTurn::Finished));
        }
    }

    #[test]
    fn losing_it_while_holding_drops_the_lock_with_it() {
        // `GetFrameDirtyRects` and the readback can fail with ACCESS_LOST
        // *after* the acquire succeeded. The duplication is gone, so the lock
        // went with it — releasing into an object that no longer exists is not
        // the next step, rebuilding is.
        let mut frames = Frames::new();
        assert_eq!(frames.acquired(Acquired::Frame), Ok(Next::Read));
        assert!(frames.is_holding());
        frames.lost();
        assert!(frames.is_finished());
        assert!(!frames.is_holding());
        assert_eq!(frames.released(), Err(OutOfTurn::Finished));
    }

    #[test]
    fn releasing_with_nothing_held_is_refused_from_the_start() {
        let mut frames = Frames::new();
        assert_eq!(frames.released(), Err(OutOfTurn::HoldingNothing));
        assert_eq!(frames.completed(), 0);
    }

    #[test]
    fn releasing_twice_is_refused() {
        // Which is what a caller does when an error path releases and the
        // happy path releases again.
        let mut frames = Frames::new();
        frames.acquired(Acquired::Frame).expect("a frame");
        assert_eq!(frames.released(), Ok(()));
        assert_eq!(frames.released(), Err(OutOfTurn::HoldingNothing));
        assert_eq!(frames.completed(), 1, "the second release is not a frame");
    }

    #[test]
    fn a_long_session_of_mixed_outcomes_stays_balanced() {
        // The property over a whole session: every frame that was acquired was
        // released exactly once, whatever the timeouts did in between.
        let mut frames = Frames::new();
        let mut acquired = 0u64;
        // A pattern with runs of timeouts, which is what a real screen does.
        for step in 0..5_000u32 {
            let outcome = if step % 7 == 0 {
                Acquired::Frame
            } else {
                Acquired::Timeout
            };
            match frames.acquired(outcome).expect("in turn") {
                Next::Read => {
                    acquired += 1;
                    frames.released().expect("held");
                }
                Next::AskAgain => {}
                Next::Recreate => panic!("nothing here loses the duplication"),
            }
        }
        assert_eq!(frames.completed(), acquired);
        assert!(!frames.is_holding(), "nothing left held at the end");
        assert!(acquired > 0);
    }

    #[test]
    fn the_next_step_says_which_of_the_three_things_to_do() {
        // Exhaustive over the outcomes, so a fourth one added later has to
        // decide rather than inherit.
        for (outcome, want) in [
            (Acquired::Frame, Next::Read),
            (Acquired::Timeout, Next::AskAgain),
            (Acquired::AccessLost, Next::Recreate),
            (Acquired::Failed, Next::Recreate),
        ] {
            let mut frames = Frames::new();
            assert_eq!(frames.acquired(outcome), Ok(want), "{outcome:?}");
        }
    }
}
