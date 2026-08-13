//! Putting a pointer somewhere exact on a Windows desktop.
//!
//! # The problem this solves
//!
//! `SendInput` moves the pointer to an absolute position in a coordinate
//! system of its own: `0` to `65535` on each axis, spanning either the primary
//! monitor or the whole virtual desktop depending on a flag. Pixels go in one
//! end and that goes out, and the conversion is the entire correctness of
//! remote pointing.
//!
//! Two ways to get it wrong, and both look nearly right.
//!
//! **Dividing by the width instead of the width less one.** The range has
//! `65536` positions and the screen has `width` pixels, and the last pixel is
//! at `width - 1`. Scale by `width` and the far edge maps to just under
//! `65535`, so **the rightmost column and the bottom row cannot be reached** —
//! which on a desktop is exactly where the close button and the taskbar live.
//! The test below asserts the corners, because that is the assertion this bug
//! fails.
//!
//! **Forgetting that the virtual desktop does not start at zero.** A monitor
//! placed to the left of the primary one has negative coordinates, and
//! `SM_XVIRTUALSCREEN` is then negative. Treating the origin as `(0, 0)`
//! sends every pointer move to the wrong monitor, and only for people with
//! that arrangement — which is why it survives a demo.
//!
//! # What is verified here and what waits for a machine
//!
//! The arithmetic is pure, so all of it is tested on the development machine:
//! the corners, monotonicity, negative origins, a desk one pixel wide, and a
//! round trip that must land back within a pixel.
//!
//! What cannot be tested here is Windows' own rounding on the way back — the
//! exact pixel a given normalised value lands on. The round-trip test pins the
//! inverse *this* module believes in, so if the machine disagrees the
//! disagreement is a single number in one place rather than a mystery.

/// The rectangle the virtual desktop occupies, in physical pixels.
///
/// From `SM_XVIRTUALSCREEN`, `SM_YVIRTUALSCREEN`, `SM_CXVIRTUALSCREEN` and
/// `SM_CYVIRTUALSCREEN` — and those are only in physical pixels if the process
/// declared itself per-monitor DPI aware. A process that did not gets values
/// scaled by the primary monitor's factor, so on a 150% display every one of
/// them is a third too small and every pointer move lands short. The sink
/// declares awareness before it reads them; see its module note.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Desk {
    /// Leftmost pixel. **Negative** when a monitor sits left of the primary.
    pub left: i32,
    /// Topmost pixel. Negative when a monitor sits above the primary.
    pub top: i32,
    /// Width in pixels. Must be positive.
    pub width: i32,
    /// Height in pixels. Must be positive.
    pub height: i32,
}

/// A position in `SendInput`'s own coordinates, `0..=65535` on each axis.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Normalised {
    /// Horizontal, `0` at the desk's left edge and `65535` at its right.
    pub x: i32,
    /// Vertical, `0` at the desk's top edge and `65535` at its bottom.
    pub y: i32,
}

/// The top of the range. `65536` positions, so the last index is this.
pub const FULL: i32 = 65535;

impl Desk {
    /// Whether this rectangle could describe a real desktop.
    #[must_use]
    pub const fn is_sane(&self) -> bool {
        self.width > 0 && self.height > 0
    }

    /// The rightmost pixel that exists.
    #[must_use]
    pub const fn right(&self) -> i32 {
        self.left + self.width - 1
    }

    /// The bottommost pixel that exists.
    #[must_use]
    pub const fn bottom(&self) -> i32 {
        self.top + self.height - 1
    }
}

/// Put a pixel position into `SendInput`'s coordinates.
///
/// Returns `None` for a desk with no area, rather than dividing by zero —
/// `GetSystemMetrics` returning zero means the call failed or there is no
/// display, and inventing a position for a screen that is not there would
/// move a pointer somewhere arbitrary.
///
/// A position outside the desk is clamped to it. The far end sends coordinates
/// derived from a frame it captured, and a frame can describe a monitor that
/// has since been unplugged — clamping keeps the pointer on a screen somebody
/// can see, which is better than refusing to move and better than off-screen.
#[must_use]
pub fn onto(desk: Desk, x: i32, y: i32) -> Option<Normalised> {
    if !desk.is_sane() {
        return None;
    }
    Some(Normalised {
        x: axis(x - desk.left, desk.width),
        y: axis(y - desk.top, desk.height),
    })
}

/// One axis: a pixel offset from the desk's edge into `0..=FULL`.
///
/// **Divides by `span - 1`**, so the last pixel reaches `FULL` exactly. See
/// the module note for what dividing by `span` costs.
fn axis(offset: i32, span: i32) -> i32 {
    let last = span - 1;
    if last <= 0 {
        // A desk one pixel wide has one position, and it is the whole range.
        return 0;
    }
    let clamped = offset.clamp(0, last);
    // Widened before multiplying: 65535 * 32767 overflows an i32, and a
    // desktop that wide is a wall of monitors somebody actually has.
    let scaled = i64::from(clamped) * i64::from(FULL) / i64::from(last);
    // Rounding is not needed — the division is exact at both ends and the
    // error in between is under one pixel, which `back` asserts.
    i32::try_from(scaled).unwrap_or(FULL)
}

/// The inverse, as this module understands it: which pixel a normalised value
/// names.
///
/// Not used to inject anything. It exists so the round trip can be asserted,
/// which is the only way to check the scaling without a Windows machine.
#[must_use]
pub fn back(desk: Desk, at: Normalised) -> Option<(i32, i32)> {
    if !desk.is_sane() {
        return None;
    }
    Some((
        desk.left + unaxis(at.x, desk.width),
        desk.top + unaxis(at.y, desk.height),
    ))
}

fn unaxis(value: i32, span: i32) -> i32 {
    let last = span - 1;
    if last <= 0 {
        return 0;
    }
    let clamped = value.clamp(0, FULL);
    let scaled = (i64::from(clamped) * i64::from(last) + i64::from(FULL) / 2) / i64::from(FULL);
    i32::try_from(scaled).unwrap_or(last)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    /// The desks worth testing: one ordinary, one with the second monitor to
    /// the left so the origin is negative, one tall stack, one huge wall, and
    /// two degenerate ones.
    fn desks() -> Vec<(&'static str, Desk)> {
        vec![
            (
                "a single 1080p monitor",
                Desk {
                    left: 0,
                    top: 0,
                    width: 1920,
                    height: 1080,
                },
            ),
            (
                "a second monitor to the left of the primary",
                Desk {
                    left: -1920,
                    top: 0,
                    width: 3840,
                    height: 1080,
                },
            ),
            (
                "a monitor above the primary",
                Desk {
                    left: 0,
                    top: -1440,
                    width: 2560,
                    height: 2880,
                },
            ),
            (
                "a mixed-DPI pair, the 4K one scaled",
                Desk {
                    left: 0,
                    top: 0,
                    width: 3840 + 1920,
                    height: 2160,
                },
            ),
            (
                "a wall of monitors wider than the normalised range",
                Desk {
                    left: -10_000,
                    top: 0,
                    width: 70_000,
                    height: 2160,
                },
            ),
            (
                "one pixel",
                Desk {
                    left: 7,
                    top: 9,
                    width: 1,
                    height: 1,
                },
            ),
        ]
    }

    #[test]
    fn the_corners_are_exact() {
        // The assertion the divide-by-width bug fails. If the right edge does
        // not reach 65535 the rightmost column of the desktop cannot be
        // clicked, and that column holds every window's close button.
        for (what, desk) in desks() {
            let top_left = onto(desk, desk.left, desk.top).expect(what);
            assert_eq!(top_left, Normalised { x: 0, y: 0 }, "{what}: top left");

            let bottom_right = onto(desk, desk.right(), desk.bottom()).expect(what);
            let want = if desk.width == 1 { 0 } else { FULL };
            let want_y = if desk.height == 1 { 0 } else { FULL };
            assert_eq!(
                bottom_right,
                Normalised { x: want, y: want_y },
                "{what}: bottom right — the last pixel must reach the end of the range"
            );
        }
    }

    #[test]
    fn a_point_outside_the_desk_is_clamped_onto_it() {
        let desk = Desk {
            left: 0,
            top: 0,
            width: 1920,
            height: 1080,
        };
        assert_eq!(
            onto(desk, -500, -500).expect("clamped"),
            Normalised { x: 0, y: 0 }
        );
        assert_eq!(
            onto(desk, 99_999, 99_999).expect("clamped"),
            Normalised { x: FULL, y: FULL }
        );
    }

    #[test]
    fn a_desk_with_no_area_is_refused_rather_than_divided_by() {
        for bad in [
            Desk {
                left: 0,
                top: 0,
                width: 0,
                height: 1080,
            },
            Desk {
                left: 0,
                top: 0,
                width: 1920,
                height: 0,
            },
            Desk {
                left: 0,
                top: 0,
                width: -1920,
                height: -1080,
            },
        ] {
            assert!(onto(bad, 10, 10).is_none(), "{bad:?}");
            assert!(back(bad, Normalised { x: 0, y: 0 }).is_none(), "{bad:?}");
        }
    }

    #[test]
    fn moving_right_never_moves_left() {
        for (what, desk) in desks() {
            let mut last = -1;
            let step = (desk.width / 97).max(1);
            let mut x = desk.left;
            while x <= desk.right() {
                let got = onto(desk, x, desk.top).expect(what).x;
                assert!(
                    got >= last,
                    "{what}: x={x} normalised to {got} after {last}"
                );
                last = got;
                x += step;
            }
        }
    }

    #[test]
    fn a_pixel_survives_the_round_trip() {
        // The real proof of the scaling. Every pixel on a desk no wider than
        // the range must come back as itself; on a wider one it must come back
        // within a pixel, because the range cannot name them all.
        for (what, desk) in desks() {
            let step = (desk.width / 401).max(1);
            let mut x = desk.left;
            while x <= desk.right() {
                let there = onto(desk, x, desk.top).expect(what);
                let (again, _) = back(desk, there).expect(what);
                let slack = if desk.width > FULL + 1 {
                    desk.width / (FULL + 1) + 1
                } else {
                    0
                };
                assert!(
                    (again - x).abs() <= slack,
                    "{what}: x={x} came back as {again}, slack {slack}"
                );
                x += step;
            }
        }
    }

    #[test]
    fn the_negative_origin_is_not_treated_as_zero() {
        // A monitor left of the primary. Treating the origin as (0, 0) puts
        // every move on the wrong monitor, and only for people with this
        // arrangement — which is why it survives being demonstrated.
        let desk = Desk {
            left: -1920,
            top: 0,
            width: 3840,
            height: 1080,
        };
        // The primary monitor's own top-left is halfway across the desk —
        // within one pixel's worth of the range, which on a 3840-wide desk is
        // 17 counts. Not exact, and correctly so: the midpoint pixel of an
        // even-width desk is at offset 1919.5, so x=1920 is a fraction past
        // the middle.
        let primary_origin = onto(desk, 0, 0).expect("on the desk");
        let a_pixel = FULL / desk.width + 1;
        assert!(
            (primary_origin.x - FULL / 2).abs() <= a_pixel,
            "the primary monitor's origin should be mid-desk, got {} (slack {a_pixel})",
            primary_origin.x
        );
        // And the property that actually matters: it comes back as itself.
        assert_eq!(back(desk, primary_origin).expect("on the desk"), (0, 0));
        // And the far monitor's top-left is the start of the range.
        assert_eq!(onto(desk, -1920, 0).expect("on the desk").x, 0);
    }

    #[test]
    fn the_middle_of_a_desk_is_the_middle_of_the_range() {
        let desk = Desk {
            left: 0,
            top: 0,
            width: 1921,
            height: 1081,
        };
        let got = onto(desk, 960, 540).expect("on the desk");
        assert_eq!(
            got,
            Normalised {
                x: FULL / 2,
                y: FULL / 2
            }
        );
    }
}
