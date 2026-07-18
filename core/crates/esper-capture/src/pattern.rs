//! A synthetic screen, designed to fail loudly.
//!
//! The pipeline needs a frame source that works with no display, so that CI
//! can run it and so that a bug can be reproduced without a screen in front of
//! it. That much is obvious. What is worth spending effort on is *which*
//! picture, because a moving square would let every one of the following pass
//! unnoticed.
//!
//! Each band exists to break a specific, known bug:
//!
//! | Band | Catches |
//! |---|---|
//! | Colour bars | Red and blue swapped — the commonest capture bug, and it looks like a colour-space problem rather than a byte-order one |
//! | Frame counter | Which frame is on screen, so latency is measurable with no external clock and a dropped or repeated frame is visible |
//! | One-pixel stripes | Chroma subsampling — 4:2:0 turns these to mush, which is what makes small text unreadable |
//! | Grid lines | A wrong stride, which shears the image; and a row offset, which a stripe pattern alone would hide |
//! | Static gradient | Banding from quantisation, and it is the region that proves damage tracking works, because it never changes |
//! | Moving box | Damage, at an exactly known position every frame |
//! | Corner markers | A vertical flip — the classic origin disagreement — and cropping. Four different colours, so no symmetry can hide it |
//!
//! The whole thing is a pure function of `(x, y, frame)`. A test can therefore
//! compute what any pixel should be without a reference image, and the damage
//! between any two frames is exactly known rather than approximated.

use crate::geometry::Rect;

/// Smallest synthetic screen that still contains every band.
///
/// The frame counter needs a block per bit and the bands need a row each.
pub const MIN_WIDTH: u32 = 32;
/// Smallest synthetic screen height.
pub const MIN_HEIGHT: u32 = 16;

/// Bits of frame counter drawn into the picture.
///
/// Sixteen: enough for thirty-six minutes at thirty frames a second before it
/// wraps, and narrow enough that each bit's block is several pixels wide even
/// on a small test frame.
pub const COUNTER_BITS: u32 = 16;

/// Most rows the frame counter occupies.
///
/// Enough that the centre of a cell survives a lossy encode, and few enough
/// that a flipped bit is a small amount of damage on a large screen.
pub const COUNTER_ROWS: u32 = 12;

/// Widest a single counter cell gets.
///
/// Bounded for the same reason as the row count, and it matters more here. A
/// binary counter's worst step is a carry cascade — 511 to 512 flips ten bits
/// at once — so a full-width counter on a 4K screen would make one ordinary
/// frame advance the largest damage event in the picture. The instrument would
/// be generating the signal.
pub const COUNTER_CELL_MAX: u32 = 24;

/// The eight colour bars, in `[r, g, b]`.
///
/// Every combination of full-on channels, so a channel swap cannot go
/// unnoticed: exchanging red and blue exchanges yellow with cyan and red with
/// blue, and both are visible side by side.
const BARS: [[u8; 3]; 8] = [
    [255, 255, 255], // white
    [255, 255, 0],   // yellow
    [0, 255, 255],   // cyan
    [0, 255, 0],     // green
    [255, 0, 255],   // magenta
    [255, 0, 0],     // red
    [0, 0, 255],     // blue
    [0, 0, 0],       // black
];

/// Corner marker colours, clockwise from the top left.
///
/// All different, so a vertical flip, a horizontal flip and a 180° rotation
/// each produce a distinguishable wrong answer.
const CORNERS: [[u8; 3]; 4] = [
    [255, 0, 0],   // top left: red
    [0, 255, 0],   // top right: green
    [0, 0, 255],   // bottom right: blue
    [255, 255, 0], // bottom left: yellow
];

/// Size of a corner marker, in pixels.
const MARKER: u32 = 2;

/// A synthetic screen of a fixed size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pattern {
    width: u32,
    height: u32,
}

/// Why a pattern could not be made.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PatternError {
    /// Too small to contain the bands.
    #[error("a synthetic screen must be at least {MIN_WIDTH}x{MIN_HEIGHT}, got {width}x{height}")]
    TooSmall { width: u32, height: u32 },
}

impl Pattern {
    /// A pattern for a screen of this size.
    pub fn new(width: u32, height: u32) -> Result<Self, PatternError> {
        if width < MIN_WIDTH || height < MIN_HEIGHT {
            return Err(PatternError::TooSmall { width, height });
        }
        Ok(Self { width, height })
    }

    /// Width in pixels.
    #[must_use]
    pub const fn width(&self) -> u32 {
        self.width
    }

    /// Height in pixels.
    #[must_use]
    pub const fn height(&self) -> u32 {
        self.height
    }

    /// The whole screen as a rectangle.
    #[must_use]
    pub const fn bounds(&self) -> Rect {
        Rect::sized(self.width, self.height)
    }

    /// Where the colour bars end and the counter begins.
    fn bars_bottom(&self) -> u32 {
        (self.height / 8).max(1)
    }

    /// Where the counter ends and the stripes begin.
    ///
    /// Capped in absolute rows rather than taken as a fraction of the height.
    /// The counter is an instrument, not content: at 1080p a proportional band
    /// would make one flipped bit sixteen thousand changed pixels, and the
    /// damage a "static screen" reports would be dominated by the measuring
    /// device.
    fn counter_bottom(&self) -> u32 {
        let top = self.bars_bottom();
        let rows = (self.height / 8).clamp(1, COUNTER_ROWS);
        top.saturating_add(rows)
    }

    /// Where the stripes end and the grid begins.
    fn stripes_bottom(&self) -> u32 {
        (self.height * 3 / 8).max(3)
    }

    /// Where the grid ends and the gradient begins.
    fn grid_bottom(&self) -> u32 {
        (self.height / 2).max(4)
    }

    /// Side length of the moving box.
    ///
    /// A sixteenth of the shorter side. Large enough to see and to carry
    /// motion for an encoder, small enough that two positions of it are well
    /// under a percent of the screen — because a test asserting that a mostly
    /// static screen reports little damage should be measuring the pipeline,
    /// not the size of the test's own box.
    fn box_size(&self) -> u32 {
        let smaller = if self.width < self.height {
            self.width
        } else {
            self.height
        };
        (smaller / 16).max(2)
    }

    /// Where the moving box is on a given frame.
    ///
    /// Confined to the gradient band and kept clear of the corner markers, so
    /// nothing is ever drawn over it and the damage between two frames is
    /// exactly the two box positions plus whichever counter bits flipped.
    ///
    /// The two axes advance by different amounts, which are coprime with
    /// nothing in particular — the point is only that the box does not retrace
    /// a short cycle, so successive frames keep producing fresh damage.
    #[must_use]
    pub fn box_at(&self, frame: u64) -> Rect {
        let size = self.box_size();
        let top = self.grid_bottom();
        // Leave room for the bottom corner markers.
        let bottom = self.height.saturating_sub(MARKER);

        let span_x = self.width.saturating_sub(size).max(1);
        let span_y = bottom.saturating_sub(top).saturating_sub(size).max(1);

        let x = (frame.wrapping_mul(7) % u64::from(span_x)) as u32;
        let y = top.saturating_add((frame.wrapping_mul(3) % u64::from(span_y)) as u32);
        Rect::new(
            i32::try_from(x).unwrap_or(0),
            i32::try_from(y).unwrap_or(0),
            size,
            size,
        )
    }

    /// Width of one counter cell.
    fn counter_cell_width(&self) -> u32 {
        (self.width / COUNTER_BITS).clamp(1, COUNTER_CELL_MAX)
    }

    /// How far across the frame the counter reaches.
    fn counter_width(&self) -> u32 {
        self.counter_cell_width().saturating_mul(COUNTER_BITS)
    }

    /// The block of pixels holding one bit of the frame counter.
    #[must_use]
    pub fn counter_cell(&self, bit: u32) -> Rect {
        let cell = self.counter_cell_width();
        let top = self.bars_bottom();
        let height = self.counter_bottom().saturating_sub(top).max(1);
        let x = cell.saturating_mul(bit.min(COUNTER_BITS.saturating_sub(1)));
        Rect::new(
            i32::try_from(x).unwrap_or(0),
            i32::try_from(top).unwrap_or(0),
            cell,
            height,
        )
    }

    /// One pixel of the screen on a given frame, as `[r, g, b]`.
    ///
    /// A pure function, which is the property the tests are built on: any
    /// pixel of any frame can be computed without rendering, so a round trip
    /// can be checked against the source rather than against a previous run.
    #[must_use]
    pub fn pixel(&self, x: u32, y: u32, frame: u64) -> [u8; 3] {
        // Corner markers win over everything, so orientation is always
        // readable however the rest of the picture is arranged.
        if let Some(c) = self.corner(x, y) {
            return c;
        }

        // The moving box, over the gradient.
        if self.box_at(frame).contains(&Rect::new(
            i32::try_from(x).unwrap_or(0),
            i32::try_from(y).unwrap_or(0),
            1,
            1,
        )) {
            return [255, 128, 0];
        }

        if y < self.bars_bottom() {
            let bar = (x.saturating_mul(8) / self.width.max(1)).min(7) as usize;
            return BARS.get(bar).copied().unwrap_or([0, 0, 0]);
        }

        if y < self.counter_bottom() && x < self.counter_width() {
            let bit = (x / self.counter_cell_width()).min(COUNTER_BITS.saturating_sub(1));
            // Most significant bit on the left, so the picture reads like a
            // number.
            let shift = COUNTER_BITS.saturating_sub(1).saturating_sub(bit);
            let on = (frame >> shift) & 1 == 1;
            return if on { [255, 255, 255] } else { [16, 16, 16] };
        }

        // The counter band past the counter, and the band below it, are
        // stripes. Extending the stripes rather than inventing a filler keeps
        // the band boundaries simple and gives the chroma test more rows.
        if y < self.stripes_bottom() {
            // One-pixel vertical stripes: what 4:2:0 chroma subsampling
            // destroys, and the reason small text goes mushy.
            return if x.is_multiple_of(2) {
                [255, 255, 255]
            } else {
                [0, 0, 0]
            };
        }

        if y < self.grid_bottom() {
            // One-pixel grid lines. A wrong stride shears these into
            // diagonals; a row offset shifts them.
            return if x.is_multiple_of(8) || y.is_multiple_of(8) {
                [255, 255, 255]
            } else {
                [32, 32, 48]
            };
        }

        // A static gradient. Never changes, so it is what proves damage
        // tracking is doing anything at all — and smooth ramps are where
        // quantisation banding shows.
        let r = (x.saturating_mul(255) / self.width.max(1)).min(255) as u8;
        let g = (y.saturating_mul(255) / self.height.max(1)).min(255) as u8;
        [r, g, 96]
    }

    /// The corner marker at this pixel, if any.
    fn corner(&self, x: u32, y: u32) -> Option<[u8; 3]> {
        let right = self.width.saturating_sub(MARKER);
        let bottom = self.height.saturating_sub(MARKER);
        let index = match (x < MARKER, y < MARKER, x >= right, y >= bottom) {
            (true, true, _, _) => 0,
            (_, true, true, _) => 1,
            (_, _, true, true) => 2,
            (true, _, _, true) => 3,
            _ => return None,
        };
        CORNERS.get(index).copied()
    }

    /// Exactly what changed between two frames.
    ///
    /// The whole reason the pattern is a pure function: a test can check that
    /// a backend's damage covers this, and that a damage-driven pipeline
    /// encoded no more than it needed to.
    #[must_use]
    pub fn damage_between(&self, from: u64, to: u64) -> Vec<Rect> {
        if from == to {
            return Vec::new();
        }
        let mut rects = vec![self.box_at(from), self.box_at(to)];

        // Only the counter bits that actually flipped.
        let flipped = from ^ to;
        for bit in 0..COUNTER_BITS {
            let shift = COUNTER_BITS.saturating_sub(1).saturating_sub(bit);
            if (flipped >> shift) & 1 == 1 {
                rects.push(self.counter_cell(bit));
            }
        }
        rects
    }

    /// Read the frame number back out of a rendered picture.
    ///
    /// `at` returns the `[r, g, b]` of a pixel. Thresholded on brightness, so
    /// it survives a lossy encode — which is the point: after a round trip
    /// through H.264 this still says which frame is on screen, and that is how
    /// glass-to-glass latency gets measured without a second clock.
    pub fn read_counter<F>(&self, mut at: F) -> u64
    where
        F: FnMut(u32, u32) -> [u8; 3],
    {
        let mut n = 0u64;
        for bit in 0..COUNTER_BITS {
            let cell = self.counter_cell(bit);
            // The centre of the cell, which is furthest from any ringing at
            // the edges of a block a lossy codec has smeared.
            let x = (cell.x as u32).saturating_add(cell.width / 2);
            let y = (cell.y as u32).saturating_add(cell.height / 2);
            let [r, g, b] = at(
                x.min(self.width.saturating_sub(1)),
                y.min(self.height.saturating_sub(1)),
            );
            let luma = u32::from(r)
                .saturating_add(u32::from(g))
                .saturating_add(u32::from(b));
            let shift = COUNTER_BITS.saturating_sub(1).saturating_sub(bit);
            if luma > 3 * 128 {
                n |= 1u64 << shift;
            }
        }
        n
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p() -> Pattern {
        Pattern::new(320, 240).expect("valid")
    }

    #[test]
    fn a_screen_too_small_for_the_bands_is_refused() {
        assert_eq!(
            Pattern::new(16, 16),
            Err(PatternError::TooSmall {
                width: 16,
                height: 16
            })
        );
        assert!(Pattern::new(MIN_WIDTH, MIN_HEIGHT).is_ok());
    }

    /// Four different colours, so no flip or rotation can produce the same
    /// picture. This is the test that catches a vertically flipped capture.
    #[test]
    fn every_corner_is_a_different_colour() {
        let p = p();
        let tl = p.pixel(0, 0, 0);
        let tr = p.pixel(319, 0, 0);
        let br = p.pixel(319, 239, 0);
        let bl = p.pixel(0, 239, 0);
        let all = [tl, tr, br, bl];
        for (i, a) in all.iter().enumerate() {
            for b in all.iter().skip(i + 1) {
                assert_ne!(a, b, "two corners share a colour: {all:?}");
            }
        }
        assert_eq!(tl, [255, 0, 0], "top left is red");
    }

    /// Red and blue swapped is the commonest capture bug there is, and it
    /// looks like a colour-space problem rather than a byte-order one. The
    /// bars make it unmistakable.
    #[test]
    fn the_colour_bars_distinguish_a_red_blue_swap() {
        let p = p();
        let y = 2; // inside the bars, past the corner markers
        let mut seen = Vec::new();
        for bar in 0..8u32 {
            let x = bar * 40 + 20;
            seen.push(p.pixel(x, y, 0));
        }
        assert_eq!(seen, BARS.to_vec(), "the bars must be in order");

        // Swapping red and blue must produce a different sequence.
        let swapped: Vec<[u8; 3]> = seen.iter().map(|c| [c[2], c[1], c[0]]).collect();
        assert_ne!(swapped, seen, "the bars must not be symmetric under a swap");
    }

    #[test]
    fn the_counter_reads_back_what_was_drawn() {
        let p = p();
        for frame in [0u64, 1, 2, 255, 256, 1000, 40000, 65535] {
            let read = p.read_counter(|x, y| p.pixel(x, y, frame));
            assert_eq!(read, frame, "frame {frame} did not read back");
        }
    }

    /// It has to survive a lossy codec, because that is the point: after a
    /// round trip it still says which frame is on screen.
    #[test]
    fn the_counter_survives_heavy_distortion() {
        let p = p();
        for frame in [7u64, 1234, 54321] {
            // Crush every channel towards mid-grey, then add a wobble.
            let read = p.read_counter(|x, y| {
                let [r, g, b] = p.pixel(x, y, frame);
                let squash = |v: u8, k: i32| {
                    let mid = 128 + (i32::from(v) - 128) * 45 / 100;
                    (mid + k).clamp(0, 255) as u8
                };
                [squash(r, 9), squash(g, -7), squash(b, 4)]
            });
            assert_eq!(read, frame, "frame {frame} did not survive distortion");
        }
    }

    #[test]
    fn the_counter_wraps_rather_than_lying() {
        let p = p();
        // Above 16 bits, only the low bits are drawn — so that is what reads
        // back, rather than a wrong number.
        let read = p.read_counter(|x, y| p.pixel(x, y, 65536 + 5));
        assert_eq!(read, 5);
    }

    /// The band that 4:2:0 chroma subsampling destroys.
    #[test]
    fn the_stripe_band_alternates_every_single_pixel() {
        let p = p();
        let y = p.counter_bottom() + 1;
        for x in MARKER..40 {
            let want = if x.is_multiple_of(2) {
                [255, 255, 255]
            } else {
                [0, 0, 0]
            };
            assert_eq!(p.pixel(x, y, 0), want, "at x={x}");
        }
    }

    /// Nothing outside the box and the counter may change between frames.
    /// This is what makes the gradient band a real test of damage tracking.
    #[test]
    #[cfg_attr(
        miri,
        ignore = "an exhaustive sweep: MIRI interprets rather than executes, so this takes minutes. This crate forbids unsafe code and has no FFI, so what MIRI is here for is the buffer pool's Drop, which the cheaper tests exercise"
    )]
    fn only_the_box_and_the_counter_change_between_frames() {
        let p = Pattern::new(64, 48).expect("valid");
        for frame in 0..40u64 {
            let next = frame + 1;
            let claimed = p.damage_between(frame, next);
            for y in 0..48u32 {
                for x in 0..64u32 {
                    let a = p.pixel(x, y, frame);
                    let b = p.pixel(x, y, next);
                    if a == b {
                        continue;
                    }
                    let px = Rect::new(x as i32, y as i32, 1, 1);
                    assert!(
                        claimed.iter().any(|r| r.contains(&px)),
                        "pixel ({x},{y}) changed between frames {frame} and \
                         {next} but was not in the reported damage"
                    );
                }
            }
        }
    }

    /// And the damage must not be a lie in the other direction either: every
    /// rectangle it reports has to be a region that could have changed, or the
    /// pipeline is being asked to encode for nothing.
    ///
    /// Checked at 1080p, where the number matters and where computing it costs
    /// nothing because the pattern never has to be rendered.
    #[test]
    #[cfg_attr(
        miri,
        ignore = "an exhaustive sweep: MIRI interprets rather than executes, so this takes minutes. This crate forbids unsafe code and has no FFI, so what MIRI is here for is the buffer pool's Drop, which the cheaper tests exercise"
    )]
    fn one_frame_of_change_is_well_under_a_percent_of_a_1080p_screen() {
        let p = Pattern::new(1920, 1080).expect("valid");
        let screen = p.bounds().area();

        // The worst single step is the one that flips the most counter bits.
        let worst = (0..512u64)
            .map(|f| {
                p.damage_between(f, f + 1)
                    .iter()
                    .map(Rect::area)
                    .sum::<u64>()
            })
            .max()
            .unwrap_or(0);

        assert!(
            worst * 100 < screen,
            "the worst single frame of change claims {worst} pixels of \
             {screen} — over one per cent, and the measuring instrument \
             should not be the thing generating the damage"
        );
        assert!(worst > 0, "something has to change");
    }

    #[test]
    fn nothing_changes_between_a_frame_and_itself() {
        assert!(p().damage_between(9, 9).is_empty());
    }

    /// The box must stay inside the frame and clear of the corner markers, or
    /// the exact-damage property above quietly stops holding.
    #[test]
    #[cfg_attr(
        miri,
        ignore = "an exhaustive sweep: MIRI interprets rather than executes, so this takes minutes. This crate forbids unsafe code and has no FFI, so what MIRI is here for is the buffer pool's Drop, which the cheaper tests exercise"
    )]
    fn the_moving_box_stays_inside_the_gradient_band() {
        for (w, h) in [(32u32, 16u32), (64, 48), (320, 240), (1920, 1080)] {
            let p = Pattern::new(w, h).expect("valid");
            let frame_rect = p.bounds();
            for frame in 0..200u64 {
                let b = p.box_at(frame);
                assert!(
                    frame_rect.contains(&b),
                    "{w}x{h} frame {frame}: box {b:?} leaves the screen"
                );
                assert!(
                    b.y as u32 >= p.grid_bottom(),
                    "{w}x{h} frame {frame}: box {b:?} is above the gradient band"
                );
                assert!(
                    b.bottom() <= i64::from(h - MARKER),
                    "{w}x{h} frame {frame}: box {b:?} reaches the corner markers"
                );
                assert!(!b.is_empty());
            }
        }
    }

    /// A static region really is static, at every size.
    ///
    /// Compared between two specific frames, with only those two frames' box
    /// positions excluded — an earlier version excluded a *range* of box
    /// positions and then compared against a frame outside it, which passed
    /// for the wrong reason until it did not.
    #[test]
    #[cfg_attr(
        miri,
        ignore = "an exhaustive sweep: MIRI interprets rather than executes, so this takes minutes. This crate forbids unsafe code and has no FFI, so what MIRI is here for is the buffer pool's Drop, which the cheaper tests exercise"
    )]
    fn the_gradient_band_never_changes() {
        for (w, h) in [(64u32, 48u32), (320, 240)] {
            let p = Pattern::new(w, h).expect("valid");
            for (a, b) in [(0u64, 12_345u64), (7, 8), (1000, 999_999)] {
                let boxes = [p.box_at(a), p.box_at(b)];
                for y in p.grid_bottom()..h {
                    for x in 0..w {
                        let px = Rect::new(x as i32, y as i32, 1, 1);
                        if boxes.iter().any(|r| r.contains(&px)) || p.corner(x, y).is_some() {
                            continue;
                        }
                        assert_eq!(
                            p.pixel(x, y, a),
                            p.pixel(x, y, b),
                            "{w}x{h}: ({x},{y}) changed between frames {a} and {b}, \
                             but the gradient band is supposed to be static"
                        );
                    }
                }
            }
        }
    }

    /// The bands must stay in order and non-empty at every size, or a later
    /// band silently swallows an earlier one and the picture loses whichever
    /// bug that band was there to catch.
    #[test]
    #[cfg_attr(
        miri,
        ignore = "an exhaustive sweep: MIRI interprets rather than executes, so this takes minutes. This crate forbids unsafe code and has no FFI, so what MIRI is here for is the buffer pool's Drop, which the cheaper tests exercise"
    )]
    fn the_bands_are_ordered_and_non_empty_at_every_size() {
        for w in [MIN_WIDTH, 33, 64, 100, 320, 1920, 3840] {
            for h in [MIN_HEIGHT, 17, 24, 48, 101, 240, 1080, 2160] {
                let p = Pattern::new(w, h).expect("valid");
                let bands = [
                    ("bars", 0, p.bars_bottom()),
                    ("counter", p.bars_bottom(), p.counter_bottom()),
                    ("stripes", p.counter_bottom(), p.stripes_bottom()),
                    ("grid", p.stripes_bottom(), p.grid_bottom()),
                    ("gradient", p.grid_bottom(), h),
                ];
                for (name, top, bottom) in bands {
                    assert!(
                        bottom > top,
                        "{w}x{h}: the {name} band is empty ({top}..{bottom})"
                    );
                    assert!(bottom <= h, "{w}x{h}: the {name} band runs past the frame");
                }
                assert!(
                    p.grid_bottom() + p.box_size() + MARKER <= h,
                    "{w}x{h}: no room for the moving box between the gradient \
                     band and the corner markers"
                );
            }
        }
    }

    /// Every size must render without panicking, including the smallest.
    #[test]
    #[cfg_attr(
        miri,
        ignore = "an exhaustive sweep: MIRI interprets rather than executes, so this takes minutes. This crate forbids unsafe code and has no FFI, so what MIRI is here for is the buffer pool's Drop, which the cheaper tests exercise"
    )]
    fn every_size_renders_without_panicking() {
        for w in [MIN_WIDTH, 33, 64, 100, 1921] {
            for h in [MIN_HEIGHT, 17, 48, 101, 1081] {
                let p = Pattern::new(w, h).expect("valid");
                for frame in [0u64, 1, u64::MAX] {
                    for y in 0..h {
                        for x in 0..w {
                            let _ = p.pixel(x, y, frame);
                        }
                    }
                    let _ = p.damage_between(frame, frame.wrapping_add(1));
                }
                assert_eq!(
                    p.read_counter(|x, y| p.pixel(x, y, 12)),
                    12,
                    "{w}x{h} counter"
                );
            }
        }
    }
}
