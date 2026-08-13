//! What Desktop Duplication says changed, turned into damage.
//!
//! # Why this is the reason DXGI was chosen
//!
//! Windows Graphics Capture is the newer API and the one Microsoft points at.
//! It does not report damage. Desktop Duplication does, and damage is what
//! makes software encode viable here — an idle screen costing nothing is the
//! difference between a session that works on a laptop and one that does not.
//! So the whole argument for the older API rests on the metadata this module
//! reads, which is a good reason for it to be the part that is tested rather
//! than the part that is typed once and hoped over.
//!
//! It lives in `esper-capture` rather than in a Windows-only crate for that
//! reason: here it is compiled and tested on every platform, and there its
//! tests would run nowhere until somebody had a machine.
//!
//! # The two kinds of rectangle, and why moves are not free
//!
//! Duplication reports **dirty rectangles** — pixels that were redrawn — and
//! **move rectangles**, which say a block was copied from one place to
//! another. Moves are how a scroll or a dragged window is described cheaply:
//! one small record instead of a screenful of dirt.
//!
//! Cheaply for a consumer that can *act* on a copy. This one cannot. The
//! encoder downstream takes changed regions and re-encodes them, so a move has
//! to become damage, and the only question is which pixels to name.
//!
//! **The destination is not optional.** Those pixels now hold what used to be
//! somewhere else, so they differ from the previous frame, and a damage set
//! that omitted them would leave the old content on the viewer's screen — the
//! one failure a damage system must not have.
//!
//! **The source is included too, and that is a deliberate cost.** In theory it
//! is unnecessary: after a true move the vacated area holds something new, and
//! Duplication reports that as dirty. In practice that makes correctness
//! depend on every driver reporting the vacated area, for every kind of move,
//! and the failure when one does not is a stale band that sits on screen until
//! something else happens to overwrite it.
//!
//! What it costs is bounded and usually nothing: for a scroll the source and
//! destination overlap heavily, and [`Regions::coalesce`] merges them into one
//! rectangle. For a window dragged clear across the screen it is two
//! rectangles instead of one. Paying that to remove a dependency on driver
//! behaviour nobody here can test is the cheaper mistake.
//!
//! # Where the coordinates are
//!
//! Both kinds are in the coordinate space of the **duplicated output's desktop
//! image**, not the virtual desktop — so a second monitor's rectangles start at
//! zero, and the `frame` passed here is that output's own bounds.
//!
//! A rotated output is the trap: the desktop image stays in the unrotated
//! orientation, so the rectangles are too, while the *image* the capturer hands
//! on has been rotated. Nothing here rotates anything, and a rotated monitor is
//! recorded as a Phase 6 item rather than guessed at — a wrong rotation puts
//! damage on the wrong side of the screen, which looks like a driver bug and is
//! not one.

use crate::damage::{Damage, Regions};
use crate::geometry::Rect;

/// A block Duplication says was copied from one place to another.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Moved {
    /// Where it came from. `DXGI_OUTDUPL_MOVE_RECT::SourcePoint`, which is a
    /// point rather than a rectangle because the size is the destination's.
    pub from_x: i32,
    /// The other half of that point.
    pub from_y: i32,
    /// Where it is now, size included.
    pub to: Rect,
}

impl Moved {
    /// The region it came from: the destination's size at the source point.
    #[must_use]
    pub const fn source(&self) -> Rect {
        Rect::new(self.from_x, self.from_y, self.to.width, self.to.height)
    }

    /// Whether this move moves anything anywhere.
    ///
    /// A record whose source and destination are the same block is a copy onto
    /// itself, which changes no pixel. Drivers do emit them.
    #[must_use]
    pub fn is_still(&self) -> bool {
        self.to.is_empty() || (self.from_x == self.to.x && self.from_y == self.to.y)
    }
}

/// What `DXGI_OUTDUPL_FRAME_INFO` says, reduced to what damage needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Report {
    /// Whether the desktop image itself changed.
    ///
    /// `LastPresentTime` is zero when the only thing that moved was the
    /// pointer, and Duplication hands back a frame anyway. Encoding it would
    /// spend a keyframe on a mouse moving, which is the single most common
    /// thing that happens on an idle screen.
    pub image_changed: bool,
    /// Whether every rectangle fitted in the metadata buffer.
    ///
    /// `TotalMetadataBufferSize` says how much is needed and the two `Get…Rects`
    /// calls fill a buffer the caller sizes. If it was too small, some
    /// rectangles were not read — and **a partial damage list is worse than
    /// none**, because it looks complete.
    pub metadata_complete: bool,
    /// `AccumulatedFrames`: how many desktop updates this one frame covers.
    ///
    /// Reported rather than acted on. Duplication documents the rectangles as
    /// describing the accumulated result, so more than one is not by itself a
    /// reason to give up on regions — but it is the number that says a
    /// consumer is falling behind, and the pipeline logs it.
    pub accumulated: u32,
}

impl Report {
    /// A frame that changed the image and whose metadata was complete.
    #[must_use]
    pub const fn complete(accumulated: u32) -> Self {
        Self {
            image_changed: true,
            metadata_complete: true,
            accumulated,
        }
    }
}

/// Turn one frame's metadata into damage.
///
/// # The four answers, and why each is the safe one
///
/// - **The image did not change** — nothing. The pointer moved and Duplication
///   handed back a frame regardless.
/// - **The metadata was incomplete** — everything. Some rectangles were not
///   read, and there is no way to know which.
/// - **The image changed and nothing says what** — everything. Duplication can
///   report a new frame with no rectangles at all, for a change in a surface it
///   will not describe. Trusting the empty list would freeze the viewer's
///   screen on stale content.
/// - **Otherwise** — the dirty rectangles, plus both ends of every move, handed
///   to [`Regions::coalesce`] to clamp, merge, cap and decide whether regions
///   are still worth the bookkeeping.
#[must_use]
pub fn damage_of(report: &Report, moves: &[Moved], dirty: &[Rect], frame: &Rect) -> Damage {
    if !report.image_changed {
        return Damage::Nothing;
    }
    if !report.metadata_complete {
        return Damage::Everything;
    }

    let mut raw: Vec<Rect> = Vec::with_capacity(dirty.len() + moves.len() * 2);
    raw.extend(dirty.iter().copied());
    for moved in moves {
        if moved.is_still() {
            continue;
        }
        raw.push(moved.to);
        raw.push(moved.source());
    }

    if raw.is_empty() {
        // Something changed and nothing said what. Not `Nothing`: that would
        // leave the viewer looking at the previous frame for ever.
        return Damage::Everything;
    }
    Regions::coalesce(&raw, frame)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    /// A 1080p output, which is what the rectangles are relative to.
    fn output() -> Rect {
        Rect::sized(1920, 1080)
    }

    fn covered(damage: &Damage) -> Vec<Rect> {
        match damage {
            Damage::Regions(r) => r.as_slice().to_vec(),
            _ => Vec::new(),
        }
    }

    /// Whether every pixel of `wanted` is inside some region of `damage`.
    ///
    /// The property the whole module exists for: a pixel that changed and is
    /// not named is a stale pixel on somebody's screen.
    ///
    /// Measured by area rather than pixel by pixel, which is exact here and
    /// not an approximation: `coalesce` guarantees no two regions overlap, so
    /// the areas of their intersections with `wanted` add up without
    /// double-counting. A pixel-by-pixel version of this was seven million
    /// comparisons for one assertion.
    fn covers(damage: &Damage, wanted: &Rect) -> bool {
        match damage {
            Damage::Everything => true,
            Damage::Nothing => wanted.is_empty(),
            Damage::Regions(regions) => {
                let inside: u64 = regions
                    .as_slice()
                    .iter()
                    .filter_map(|r| r.intersect(wanted))
                    .map(|r| r.area())
                    .sum();
                inside == wanted.area()
            }
        }
    }

    /// The area assertion above is only exact while regions do not overlap, so
    /// that is asserted rather than assumed.
    fn regions_do_not_overlap(damage: &Damage) -> bool {
        match damage {
            Damage::Regions(regions) => {
                let rects = regions.as_slice();
                rects.iter().enumerate().all(|(i, a)| {
                    rects
                        .iter()
                        .skip(i + 1)
                        .all(|b| a.intersect(b).is_none_or(|o| o.is_empty()))
                })
            }
            _ => true,
        }
    }

    #[test]
    fn a_pointer_moving_is_not_a_frame() {
        // The commonest thing that happens on an idle screen. Encoding it
        // would spend bytes on a mouse.
        let report = Report {
            image_changed: false,
            metadata_complete: true,
            accumulated: 1,
        };
        assert_eq!(
            damage_of(&report, &[], &[Rect::new(0, 0, 8, 8)], &output()),
            Damage::Nothing,
            "and the rectangles are ignored, because there is no new image"
        );
    }

    #[test]
    fn incomplete_metadata_is_everything() {
        // Some rectangles were not read and there is no way to know which, so
        // a partial list must not be treated as a complete one.
        let report = Report {
            image_changed: true,
            metadata_complete: false,
            accumulated: 1,
        };
        assert_eq!(
            damage_of(&report, &[], &[Rect::new(0, 0, 8, 8)], &output()),
            Damage::Everything
        );
    }

    #[test]
    fn a_new_image_with_no_rectangles_is_everything() {
        // Duplication can report a frame and describe nothing. Believing the
        // empty list freezes the viewer on the previous frame.
        assert_eq!(
            damage_of(&Report::complete(1), &[], &[], &output()),
            Damage::Everything
        );
    }

    #[test]
    fn a_dirty_rectangle_is_the_damage() {
        let dirt = Rect::new(100, 200, 300, 40);
        let got = damage_of(&Report::complete(1), &[], &[dirt], &output());
        assert!(covers(&got, &dirt), "got {got:?}");
        assert_eq!(covered(&got), vec![dirt], "and nothing more");
    }

    #[test]
    fn a_move_names_its_destination() {
        // The pixels that must not be missed: they hold what used to be
        // elsewhere, so they differ from the previous frame.
        let moved = Moved {
            from_x: 0,
            from_y: 40,
            to: Rect::new(0, 0, 400, 300),
        };
        let got = damage_of(&Report::complete(1), &[moved], &[], &output());
        assert!(
            covers(&got, &moved.to),
            "the destination must be damaged: {got:?}"
        );
    }

    #[test]
    fn a_move_names_its_source_as_well() {
        // Deliberate, and the module note says why: it removes a dependency on
        // every driver reporting the vacated area as dirty, and the failure it
        // prevents is a stale band nothing overwrites.
        let moved = Moved {
            from_x: 1000,
            from_y: 0,
            to: Rect::new(0, 0, 200, 200),
        };
        let got = damage_of(&Report::complete(1), &[moved], &[], &output());
        assert!(covers(&got, &moved.to), "destination: {got:?}");
        assert!(covers(&got, &moved.source()), "source: {got:?}");
    }

    #[test]
    fn a_scroll_costs_one_region_rather_than_two() {
        // The common case, and the reason including the source is cheap: a
        // scroll's source and destination overlap, so coalescing merges them.
        let moved = Moved {
            from_x: 0,
            from_y: 20,
            to: Rect::new(0, 0, 600, 400),
        };
        let got = damage_of(&Report::complete(1), &[moved], &[], &output());
        assert_eq!(
            covered(&got).len(),
            1,
            "an overlapping source and destination should merge: {got:?}"
        );
        assert!(covers(&got, &moved.to));
        assert!(covers(&got, &moved.source()));
    }

    #[test]
    fn a_copy_onto_itself_changes_nothing() {
        // Drivers emit these. Treating one as damage would re-encode a region
        // every frame for no reason.
        let still = Moved {
            from_x: 40,
            from_y: 40,
            to: Rect::new(40, 40, 100, 100),
        };
        assert!(still.is_still());
        assert_eq!(
            damage_of(&Report::complete(1), &[still], &[], &output()),
            Damage::Everything,
            "with nothing else to report, a still move leaves no rectangles at \
             all — which is the new-image-with-no-rectangles case"
        );
        // With real dirt beside it, the still move must not add anything.
        let dirt = Rect::new(0, 0, 10, 10);
        let got = damage_of(&Report::complete(1), &[still], &[dirt], &output());
        assert_eq!(covered(&got), vec![dirt]);
    }

    #[test]
    fn an_empty_move_is_ignored() {
        let empty = Moved {
            from_x: 0,
            from_y: 0,
            to: Rect::new(500, 500, 0, 0),
        };
        assert!(empty.is_still());
        let dirt = Rect::new(0, 0, 10, 10);
        assert_eq!(
            covered(&damage_of(
                &Report::complete(1),
                &[empty],
                &[dirt],
                &output()
            )),
            vec![dirt]
        );
    }

    #[test]
    fn rectangles_outside_the_output_are_clamped_away() {
        // A rectangle that does not touch the output cannot be damage on it.
        // `coalesce` does the clamping; this asserts it is reached.
        let outside = Rect::new(5000, 5000, 100, 100);
        assert_eq!(
            damage_of(&Report::complete(1), &[], &[outside], &output()),
            Damage::Nothing
        );
    }

    #[test]
    fn a_move_from_outside_the_output_still_damages_its_destination() {
        // The source is clamped away and the destination is not, which is the
        // asymmetry that matters: the destination is the half that cannot be
        // dropped.
        let moved = Moved {
            from_x: -400,
            from_y: 0,
            to: Rect::new(0, 0, 300, 300),
        };
        let got = damage_of(&Report::complete(1), &[moved], &[], &output());
        assert!(covers(&got, &moved.to), "{got:?}");
    }

    #[test]
    fn enough_movement_becomes_a_whole_frame() {
        // A window dragged across most of the screen. Past the threshold the
        // damage system says so plainly rather than tracking regions that
        // cover the frame anyway.
        let moved = Moved {
            from_x: 0,
            from_y: 0,
            to: Rect::new(0, 0, 1920, 900),
        };
        assert_eq!(
            damage_of(&Report::complete(1), &[moved], &[], &output()),
            Damage::Everything
        );
    }

    #[test]
    fn many_small_rectangles_stay_bounded() {
        // The region cap is `coalesce`'s to enforce; this asserts a realistic
        // burst of dirt reaches it rather than being passed on raw.
        let dirty: Vec<Rect> = (0..200)
            .map(|i| Rect::new(i * 9, (i % 7) * 130, 4, 4))
            .collect();
        let got = damage_of(&Report::complete(1), &[], &dirty, &output());
        match got {
            Damage::Everything => {}
            Damage::Regions(ref r) => assert!(
                r.len() <= crate::damage::MAX_REGIONS,
                "{} regions, cap is {}",
                r.len(),
                crate::damage::MAX_REGIONS
            ),
            Damage::Nothing => panic!("two hundred dirty rectangles is not nothing"),
        }
        for rect in &dirty {
            assert!(covers(&got, rect), "lost {rect:?} from {got:?}");
        }
    }

    #[test]
    fn every_dirty_pixel_survives_a_mixture() {
        // The property, over dirt and moves together: nothing named as changed
        // may be dropped.
        let dirty = [
            Rect::new(0, 0, 40, 40),
            Rect::new(900, 500, 60, 20),
            Rect::new(1880, 1060, 40, 20),
        ];
        let moves = [
            Moved {
                from_x: 100,
                from_y: 100,
                to: Rect::new(100, 60, 200, 200),
            },
            Moved {
                from_x: 1500,
                from_y: 900,
                to: Rect::new(1400, 800, 120, 120),
            },
        ];
        let got = damage_of(&Report::complete(1), &moves, &dirty, &output());
        assert!(
            regions_do_not_overlap(&got),
            "coalesce promises non-overlapping regions, and the coverage check              above is only exact while that holds: {got:?}"
        );
        for rect in &dirty {
            assert!(covers(&got, rect), "lost dirt {rect:?}");
        }
        for moved in &moves {
            assert!(covers(&got, &moved.to), "lost destination of {moved:?}");
            let source = moved
                .source()
                .clamp_to(&output())
                .expect("a source inside the output");
            assert!(covers(&got, &source), "lost source of {moved:?}");
        }
    }

    #[test]
    fn the_accumulated_count_is_carried_and_not_acted_on() {
        // Duplication documents the rectangles as describing the accumulated
        // result, so more than one update is not by itself a reason to give up
        // on regions. Asserted so that changing the rule is a deliberate act
        // rather than a drift — and it is the rule most worth checking on a
        // machine.
        let dirt = Rect::new(10, 10, 50, 50);
        let one = damage_of(&Report::complete(1), &[], &[dirt], &output());
        let many = damage_of(&Report::complete(9), &[], &[dirt], &output());
        assert_eq!(one, many);
    }
}
