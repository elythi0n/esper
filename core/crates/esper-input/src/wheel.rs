//! Turning a protocol scroll into what `SendInput` wants.
//!
//! # The sign is inverted, and that is most of this module
//!
//! The protocol says positive `dy_notches` is **down**: content moving up,
//! which is the direction a wheel rolls towards the person. Windows'
//! `MOUSEEVENTF_WHEEL` says positive `mouseData` is **away** from the person,
//! which is up. The two disagree, so a backend that passed the number through
//! would scroll every Windows session backwards.
//!
//! Inverted here, in one place, with a test that says which way round it goes
//! — because "scrolling is backwards" is exactly the bug that gets fixed twice
//! in opposite directions by two people who each tried it once.
//!
//! Horizontal does **not** invert. `MOUSEEVENTF_HWHEEL` is positive to the
//! right and so is the protocol, and inverting both because one needed it
//! would be the same bug wearing a hat.
//!
//! # Why notches and not pixels
//!
//! [`esper_proto::Scroll`] carries both, and pixels are the better unit — the
//! protocol's own note says a host that can scroll by pixels should. Windows
//! cannot, quite: `mouseData` is documented in multiples of `WHEEL_DELTA`, one
//! notch, and there is no pixel unit to put there. Smooth scrolling on Windows
//! is the *application* interpreting sub-notch deltas, and converting pixels
//! into them needs the host's own `SPI_GETWHEELSCROLLLINES` and a line height
//! — a number this cannot invent without being wrong on somebody's machine.
//!
//! So pixels are ignored and notches are used, which is what the protocol
//! guarantees is always present and what X11 does for the same reason. Doing
//! better is a Phase 6 item with a real API call in it, not an arithmetic
//! choice to make here.
//!
//! # This module is here rather than in the Windows backend
//!
//! Because the Windows backend is `cfg(target_os = "windows")` and so none of
//! its tests run on the machine this is written on. Arithmetic that decides
//! which way a session scrolls should not be the part nobody can check.

use esper_proto::Scroll;

/// One notch, in the units `SendInput` reads from `mouseData`.
pub const WHEEL_DELTA: i32 = 120;

/// The most notches one message may turn into wheel events.
///
/// The protocol bounds a scroll at [`esper_proto::MAX_NOTCHES`] and refuses
/// anything larger, so this is a second wall rather than the first. It exists
/// because the multiplication below is the one place a bounded input becomes
/// an unbounded number, and `64 * 120` fitting in an `i32` is a fact worth a
/// test rather than a glance.
pub const MOST_NOTCHES: i32 = esper_proto::MAX_NOTCHES as i32;

/// What to hand `SendInput`, in `mouseData` units.
///
/// Zero in a field means no wheel event for that axis — not an event of size
/// zero, which some applications treat as a scroll and others as a reset.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Wheel {
    /// `MOUSEEVENTF_WHEEL`'s `mouseData`. Positive is away from the person.
    pub vertical: i32,
    /// `MOUSEEVENTF_HWHEEL`'s `mouseData`. Positive is to the right.
    pub horizontal: i32,
}

impl Wheel {
    /// Whether there is anything to send.
    #[must_use]
    pub const fn is_nothing(&self) -> bool {
        self.vertical == 0 && self.horizontal == 0
    }
}

/// Convert a protocol scroll into wheel deltas.
///
/// Clamps to [`MOST_NOTCHES`] on each axis rather than saturating the
/// multiplication, so an out-of-range message scrolls a lot rather than
/// wrapping into a scroll the other way.
#[must_use]
pub fn from_scroll(scroll: Scroll) -> Wheel {
    Wheel {
        // Negated: the protocol's down is Windows' negative.
        vertical: -notches(scroll.dy_notches),
        horizontal: notches(scroll.dx_notches),
    }
}

fn notches(count: i16) -> i32 {
    i32::from(count).clamp(-MOST_NOTCHES, MOST_NOTCHES) * WHEEL_DELTA
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    fn scroll(dx: i16, dy: i16) -> Scroll {
        Scroll {
            dx_notches: dx,
            dy_notches: dy,
            dx_pixels: None,
            dy_pixels: None,
        }
    }

    #[test]
    fn scrolling_down_scrolls_down() {
        // The whole point of the module. The protocol's positive dy is down,
        // and Windows' negative mouseData is down, so one notch down must come
        // out as -120. Getting this backwards is the bug people fix twice.
        let got = from_scroll(scroll(0, 1));
        assert_eq!(got.vertical, -WHEEL_DELTA, "one notch down is -120");
        assert_eq!(
            from_scroll(scroll(0, -1)).vertical,
            WHEEL_DELTA,
            "up is +120"
        );
    }

    #[test]
    fn scrolling_right_scrolls_right() {
        // And horizontal does *not* invert: both agree that positive is right.
        assert_eq!(from_scroll(scroll(1, 0)).horizontal, WHEEL_DELTA);
        assert_eq!(from_scroll(scroll(-1, 0)).horizontal, -WHEEL_DELTA);
    }

    #[test]
    fn the_two_axes_do_not_leak_into_each_other() {
        let got = from_scroll(scroll(3, 0));
        assert_eq!(
            got.vertical, 0,
            "a horizontal scroll moves nothing vertically"
        );
        let got = from_scroll(scroll(0, 3));
        assert_eq!(
            got.horizontal, 0,
            "a vertical scroll moves nothing horizontally"
        );
    }

    #[test]
    fn nothing_is_nothing() {
        // A wheel event of size zero is read as a scroll by some applications
        // and as a reset by others, so the caller must be able to tell there
        // is nothing to send.
        assert!(from_scroll(scroll(0, 0)).is_nothing());
        assert!(!from_scroll(scroll(0, 1)).is_nothing());
        assert!(!from_scroll(scroll(1, 0)).is_nothing());
    }

    #[test]
    fn a_scroll_larger_than_the_protocol_allows_does_not_wrap() {
        // i16::MIN * 120 overflows an i32, and a scroll that wrapped would go
        // the other way — which is worse than one that goes too far.
        for absurd in [i16::MAX, i16::MIN, 30_000, -30_000] {
            let got = from_scroll(scroll(absurd, absurd));
            assert_eq!(got.horizontal.abs(), MOST_NOTCHES * WHEEL_DELTA);
            assert_eq!(got.vertical.abs(), MOST_NOTCHES * WHEEL_DELTA);
            assert_eq!(
                got.horizontal.signum(),
                i32::from(absurd).signum(),
                "clamping must not change the direction"
            );
            assert_eq!(
                got.vertical.signum(),
                -i32::from(absurd).signum(),
                "and the vertical inversion still applies at the limit"
            );
        }
    }

    #[test]
    fn the_bound_fits_in_the_type_it_is_sent_in() {
        // 64 * 120 is 7680. Worth asserting rather than glancing at, because
        // this is the one place a bounded input becomes a large number.
        assert!(MOST_NOTCHES.checked_mul(WHEEL_DELTA).is_some());
        assert_eq!(MOST_NOTCHES * WHEEL_DELTA, 7680);
    }

    #[test]
    fn every_scroll_the_protocol_accepts_converts() {
        // The protocol refuses anything outside MAX_NOTCHES, so everything
        // inside it must come through without clamping.
        for n in -esper_proto::MAX_NOTCHES..=esper_proto::MAX_NOTCHES {
            let got = from_scroll(scroll(n, n));
            assert_eq!(got.horizontal, i32::from(n) * WHEEL_DELTA, "dx {n}");
            assert_eq!(got.vertical, -i32::from(n) * WHEEL_DELTA, "dy {n}");
        }
    }

    #[test]
    fn pixels_are_ignored_and_that_is_deliberate() {
        // Recorded as a test rather than only a comment: if somebody wires
        // pixels up, this fails and points at the module note explaining what
        // it takes to do it correctly.
        let with_pixels = Scroll {
            dx_notches: 1,
            dy_notches: 1,
            dx_pixels: Some(400),
            dy_pixels: Some(400),
        };
        assert_eq!(from_scroll(with_pixels), from_scroll(scroll(1, 1)));
    }
}
