//! Rectangles, and the arithmetic that must not overflow.
//!
//! # Why the arithmetic is in `i64`
//!
//! A rectangle's origin can be negative — a monitor to the left of the primary
//! one starts at a negative x in desktop coordinates — while its size cannot.
//! So the natural shape is a signed origin and an unsigned size, and the moment
//! those are added together the result no longer fits the type either of them
//! came from.
//!
//! This is not pedantry. Release builds have overflow checks on, damage
//! rectangles come from capture backends and eventually from peers, and
//! `x + width` on an `i32` near its limit is a panic. Every edge is computed in
//! `i64`, which cannot overflow for any `i32`/`u32` pair.

/// A rectangle with a signed origin and an unsigned size.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

impl Rect {
    /// A rectangle at an origin with a size.
    #[must_use]
    pub const fn new(x: i32, y: i32, width: u32, height: u32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    /// A rectangle at the origin with a size — a whole frame, usually.
    #[must_use]
    pub const fn sized(width: u32, height: u32) -> Self {
        Self::new(0, 0, width, height)
    }

    /// The x coordinate one past the right edge.
    #[must_use]
    pub const fn right(&self) -> i64 {
        self.x as i64 + self.width as i64
    }

    /// The y coordinate one past the bottom edge.
    #[must_use]
    pub const fn bottom(&self) -> i64 {
        self.y as i64 + self.height as i64
    }

    /// Whether this covers no pixels.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.width == 0 || self.height == 0
    }

    /// How many pixels this covers.
    ///
    /// `u64`, because a 65535×65535 region overflows `u32` and a caller
    /// comparing areas to decide whether to merge would get a wrapped answer.
    #[must_use]
    pub const fn area(&self) -> u64 {
        self.width as u64 * self.height as u64
    }

    /// The overlap, or `None` if they do not touch.
    ///
    /// Rectangles that merely share an edge do not overlap: an edge is not a
    /// pixel.
    #[must_use]
    pub fn intersect(&self, other: &Self) -> Option<Self> {
        let x = self.x.max(other.x);
        let y = self.y.max(other.y);
        let right = self.right().min(other.right());
        let bottom = self.bottom().min(other.bottom());

        let width = right.saturating_sub(i64::from(x));
        let height = bottom.saturating_sub(i64::from(y));
        if width <= 0 || height <= 0 {
            return None;
        }
        Some(Self {
            x,
            y,
            // Both are positive and bounded by the smaller input's size, so
            // they fit.
            width: u32::try_from(width).unwrap_or(u32::MAX),
            height: u32::try_from(height).unwrap_or(u32::MAX),
        })
    }

    /// Whether they share any pixel.
    #[must_use]
    pub fn overlaps(&self, other: &Self) -> bool {
        self.intersect(other).is_some()
    }

    /// The smallest rectangle covering both.
    ///
    /// An empty rectangle unions to the other one rather than dragging the
    /// result to the origin — a zero-size rect at (0,0) is the default value,
    /// and letting it contribute a corner would silently expand every union it
    /// took part in.
    #[must_use]
    pub fn union(&self, other: &Self) -> Self {
        if self.is_empty() {
            return *other;
        }
        if other.is_empty() {
            return *self;
        }
        let x = self.x.min(other.x);
        let y = self.y.min(other.y);
        let right = self.right().max(other.right());
        let bottom = self.bottom().max(other.bottom());
        Self {
            x,
            y,
            width: u32::try_from(right - i64::from(x)).unwrap_or(u32::MAX),
            height: u32::try_from(bottom - i64::from(y)).unwrap_or(u32::MAX),
        }
    }

    /// Whether this fully covers `other`.
    #[must_use]
    pub fn contains(&self, other: &Self) -> bool {
        if other.is_empty() {
            return true;
        }
        if self.is_empty() {
            return false;
        }
        self.x <= other.x
            && self.y <= other.y
            && self.right() >= other.right()
            && self.bottom() >= other.bottom()
    }

    /// This rectangle cut down to `bounds`, or `None` if nothing is left.
    ///
    /// Every rectangle from a capture backend goes through this. A backend
    /// reporting damage outside the frame is not hypothetical: it happens
    /// across a resolution change, when a monitor is unplugged, and whenever a
    /// backend reports in desktop coordinates where the caller expected frame
    /// coordinates. Without clamping, that becomes an out-of-bounds read in
    /// whatever consumes the region.
    #[must_use]
    pub fn clamp_to(&self, bounds: &Self) -> Option<Self> {
        self.intersect(bounds)
    }

    /// Moved by an offset.
    #[must_use]
    pub fn translate(&self, dx: i32, dy: i32) -> Self {
        Self {
            x: self.x.saturating_add(dx),
            y: self.y.saturating_add(dy),
            width: self.width,
            height: self.height,
        }
    }

    /// Grown outward on every side, clamped so the origin does not wrap.
    ///
    /// Used to align a region to a macroblock grid: an encoder given a region
    /// that starts mid-macroblock has to encode the whole macroblock anyway,
    /// so the caller may as well say so.
    #[must_use]
    pub fn inflate(&self, by: u32) -> Self {
        if self.is_empty() {
            return *self;
        }
        let by_i = i32::try_from(by).unwrap_or(i32::MAX);
        Self {
            x: self.x.saturating_sub(by_i),
            y: self.y.saturating_sub(by_i),
            width: self.width.saturating_add(by).saturating_add(by),
            height: self.height.saturating_add(by).saturating_add(by),
        }
    }

    /// Snapped outward to a grid of `step` pixels, in frame coordinates.
    ///
    /// `step` of zero returns the rectangle unchanged rather than dividing by
    /// zero, because a caller computing a step from a codec's macroblock size
    /// should not have to check it.
    #[must_use]
    pub fn align_out(&self, step: u32) -> Self {
        if step <= 1 || self.is_empty() {
            return *self;
        }
        let step_i = i64::from(step);
        let x0 = floor_to(i64::from(self.x), step_i);
        let y0 = floor_to(i64::from(self.y), step_i);
        let x1 = ceil_to(self.right(), step_i);
        let y1 = ceil_to(self.bottom(), step_i);
        Self {
            x: i32::try_from(x0).unwrap_or(i32::MIN),
            y: i32::try_from(y0).unwrap_or(i32::MIN),
            width: u32::try_from(x1 - x0).unwrap_or(u32::MAX),
            height: u32::try_from(y1 - y0).unwrap_or(u32::MAX),
        }
    }
}

/// Round down to a multiple of `step`, correctly for negative values.
///
/// Rust's `/` truncates toward zero, so `-1 / 16` is `0` and a naive
/// `v / step * step` rounds *up* for anything negative. A damage rectangle on a
/// monitor left of the primary one is negative, so this is reachable.
const fn floor_to(v: i64, step: i64) -> i64 {
    let m = v % step;
    if m == 0 {
        v
    } else if m < 0 {
        v - m - step
    } else {
        v - m
    }
}

/// Round up to a multiple of `step`, correctly for negative values.
const fn ceil_to(v: i64, step: i64) -> i64 {
    let f = floor_to(v, step);
    if f == v { v } else { f + step }
}

/// The first pair of screens that cover any of the same pixels.
///
/// **Two screens showing the same pixels cannot be tiled.** Mirrored displays
/// are the ordinary case, and a picture with both in it would show the same
/// desktop twice, at the same place, with one drawn over the other — while
/// costing the session the pixels of both. A caller refuses the request and
/// says which two, which is something a person can act on.
///
/// It is also the rule that keeps a tiled fetch bounded: the screens in a
/// picture cover no pixel twice, so reading all of them is never more work
/// than reading the picture once.
#[must_use]
pub fn first_overlap(screens: &[Rect]) -> Option<(usize, usize)> {
    for (i, a) in screens.iter().enumerate() {
        for (j, b) in screens.iter().enumerate().skip(i.saturating_add(1)) {
            if a.overlaps(b) {
                return Some((i, j));
            }
        }
    }
    None
}

/// Where each screen sits in a picture holding several of them.
///
/// **The desktop's own arrangement, moved to the origin.** Two screens
/// side by side arrive side by side; one above another arrives above another.
/// There is no layout policy to agree on because there is no layout decision:
/// a screen's place in the picture is its place on the desk, less the top-left
/// corner of the set. So both ends work it out from the same monitor list and
/// cannot disagree — and a person looking at the picture sees the arrangement
/// they would see standing at the machine.
///
/// Any gap between them — a hole in an L-shaped desktop, or a screen in the
/// middle that nobody asked for — is part of the picture and belongs to no
/// screen. **A caller must fill it rather than leave it,** which is why this
/// returns the tiles rather than only the size: the region that belongs to no
/// tile is exactly the region a capture must not show, and the unserved screen
/// sitting there is the reason.
///
/// Returns the bounding rectangle in desktop coordinates and, in the order
/// given, each screen's rectangle within the picture.
#[must_use]
pub fn tiles(screens: &[Rect]) -> (Rect, Vec<Rect>) {
    let bounds = screens
        .iter()
        .fold(Rect::sized(0, 0), |acc, r| acc.union(r));
    let placed = screens
        .iter()
        .map(|r| r.translate(-bounds.x, -bounds.y))
        .collect();
    (bounds, placed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_rectangle_covers_nothing() {
        assert!(Rect::new(5, 5, 0, 10).is_empty());
        assert!(Rect::new(5, 5, 10, 0).is_empty());
        assert!(!Rect::new(5, 5, 1, 1).is_empty());
        assert_eq!(Rect::new(5, 5, 0, 10).area(), 0);
    }

    /// The reason every edge is computed in `i64`. Release builds have
    /// overflow checks on, so a wrapped edge is a panic rather than a wrong
    /// number — and these values arrive from capture backends.
    #[test]
    fn edges_do_not_overflow_at_the_limits_of_the_types() {
        let r = Rect::new(i32::MAX, i32::MAX, u32::MAX, u32::MAX);
        assert_eq!(r.right(), i64::from(i32::MAX) + i64::from(u32::MAX));
        assert_eq!(r.bottom(), i64::from(i32::MAX) + i64::from(u32::MAX));
        assert_eq!(r.area(), u64::from(u32::MAX) * u64::from(u32::MAX));

        // And at the other end.
        let r = Rect::new(i32::MIN, i32::MIN, u32::MAX, u32::MAX);
        assert_eq!(r.right(), i64::from(i32::MIN) + i64::from(u32::MAX));
    }

    #[test]
    fn overlapping_rectangles_intersect_to_the_shared_part() {
        let a = Rect::new(0, 0, 100, 100);
        let b = Rect::new(50, 50, 100, 100);
        assert_eq!(a.intersect(&b), Some(Rect::new(50, 50, 50, 50)));
        assert_eq!(
            a.intersect(&b),
            b.intersect(&a),
            "intersection is symmetric"
        );
    }

    /// Sharing an edge is not sharing a pixel. Getting this wrong makes two
    /// side-by-side regions look like they overlap, and the coalescer then
    /// merges things it should not.
    #[test]
    fn rectangles_that_only_touch_do_not_intersect() {
        let a = Rect::new(0, 0, 10, 10);
        let right = Rect::new(10, 0, 10, 10);
        let below = Rect::new(0, 10, 10, 10);
        assert_eq!(a.intersect(&right), None);
        assert_eq!(a.intersect(&below), None);
        assert!(!a.overlaps(&right));
    }

    #[test]
    fn separate_rectangles_do_not_intersect() {
        let a = Rect::new(0, 0, 10, 10);
        let far = Rect::new(1000, 1000, 10, 10);
        assert_eq!(a.intersect(&far), None);
    }

    #[test]
    fn a_union_covers_both() {
        let a = Rect::new(0, 0, 10, 10);
        let b = Rect::new(90, 90, 10, 10);
        let u = a.union(&b);
        assert_eq!(u, Rect::new(0, 0, 100, 100));
        assert!(u.contains(&a) && u.contains(&b));
    }

    /// `Rect::default()` is a zero-size rectangle at the origin. If it
    /// contributed a corner, every union involving one would silently stretch
    /// to (0,0) — which on a multi-monitor desktop is not even a corner of the
    /// frame in question.
    #[test]
    fn an_empty_rectangle_does_not_drag_a_union_to_the_origin() {
        let real = Rect::new(500, 500, 10, 10);
        assert_eq!(Rect::default().union(&real), real);
        assert_eq!(real.union(&Rect::default()), real);
        assert_eq!(
            Rect::default().union(&Rect::default()),
            Rect::default(),
            "two empties are still empty"
        );
    }

    #[test]
    fn containment_is_inclusive_of_the_edges() {
        let outer = Rect::new(0, 0, 100, 100);
        assert!(outer.contains(&outer), "a rectangle contains itself");
        assert!(outer.contains(&Rect::new(0, 0, 1, 1)));
        assert!(outer.contains(&Rect::new(99, 99, 1, 1)));
        assert!(
            !outer.contains(&Rect::new(99, 99, 2, 2)),
            "one past the edge"
        );
    }

    /// Damage outside the frame is not hypothetical — it arrives across a
    /// resolution change and whenever a backend reports desktop coordinates
    /// where frame coordinates were expected.
    #[test]
    fn damage_outside_the_frame_is_clamped_away() {
        let frame = Rect::sized(1920, 1080);

        // Straddling the right edge.
        assert_eq!(
            Rect::new(1900, 0, 100, 10).clamp_to(&frame),
            Some(Rect::new(1900, 0, 20, 10))
        );
        // Negative origin, as a monitor to the left produces.
        assert_eq!(
            Rect::new(-50, -50, 100, 100).clamp_to(&frame),
            Some(Rect::new(0, 0, 50, 50))
        );
        // Entirely outside.
        assert_eq!(Rect::new(5000, 5000, 10, 10).clamp_to(&frame), None);
        assert_eq!(Rect::new(-100, 0, 10, 10).clamp_to(&frame), None);
    }

    #[test]
    fn translating_does_not_change_the_size() {
        let r = Rect::new(10, 20, 30, 40);
        assert_eq!(r.translate(5, -5), Rect::new(15, 15, 30, 40));
        // And it saturates rather than wrapping.
        assert_eq!(Rect::new(i32::MAX, 0, 1, 1).translate(1, 0).x, i32::MAX);
        assert_eq!(Rect::new(i32::MIN, 0, 1, 1).translate(-1, 0).x, i32::MIN);
    }

    #[test]
    fn inflating_grows_every_side() {
        assert_eq!(Rect::new(10, 10, 5, 5).inflate(2), Rect::new(8, 8, 9, 9));
        assert_eq!(
            Rect::new(10, 10, 0, 5).inflate(2),
            Rect::new(10, 10, 0, 5),
            "an empty rectangle stays empty rather than becoming a 4x9 one"
        );
    }

    /// An encoder handed a region starting mid-macroblock has to encode the
    /// whole macroblock, so aligning outward costs nothing and makes the
    /// region honest about what will be spent.
    #[test]
    fn aligning_snaps_outward_to_the_grid() {
        assert_eq!(
            Rect::new(5, 5, 3, 3).align_out(16),
            Rect::new(0, 0, 16, 16),
            "a 3x3 change still costs one macroblock"
        );
        assert_eq!(
            Rect::new(16, 32, 16, 16).align_out(16),
            Rect::new(16, 32, 16, 16),
            "an already-aligned region is unchanged"
        );
        assert_eq!(
            Rect::new(15, 15, 2, 2).align_out(16),
            Rect::new(0, 0, 32, 32),
            "a change straddling a boundary costs both"
        );
        assert_eq!(
            Rect::new(5, 5, 3, 3).align_out(0),
            Rect::new(5, 5, 3, 3),
            "a step of zero must not divide by zero"
        );
        assert_eq!(Rect::new(5, 5, 3, 3).align_out(1), Rect::new(5, 5, 3, 3));
    }

    /// Rust's `/` truncates toward zero, so the obvious `v / step * step`
    /// rounds the wrong way for negatives — and a monitor left of the primary
    /// one has negative coordinates.
    #[test]
    fn aligning_rounds_the_right_way_for_negative_coordinates() {
        assert_eq!(
            Rect::new(-5, -5, 3, 3).align_out(16),
            Rect::new(-16, -16, 16, 16),
            "must round down to -16, not up to 0"
        );
        assert_eq!(
            Rect::new(-32, -32, 16, 16).align_out(16),
            Rect::new(-32, -32, 16, 16)
        );
        assert_eq!(
            Rect::new(-17, 0, 1, 1).align_out(16),
            Rect::new(-32, 0, 16, 16)
        );
    }

    #[test]
    fn floor_and_ceil_agree_with_each_other_on_a_grid() {
        for v in -100i64..=100 {
            for step in [1i64, 2, 3, 16, 64] {
                let f = floor_to(v, step);
                let c = ceil_to(v, step);
                assert_eq!(f % step, 0, "floor_to({v}, {step}) = {f}");
                assert_eq!(c % step, 0, "ceil_to({v}, {step}) = {c}");
                assert!(f <= v && v <= c, "{f} <= {v} <= {c}");
                if v % step == 0 {
                    assert_eq!(
                        (f, c),
                        (v, v),
                        "a value on the grid is its own floor and ceiling"
                    );
                } else {
                    assert_eq!(
                        c - f,
                        step,
                        "off the grid, the floor and ceiling are exactly one step apart"
                    );
                }
            }
        }
    }

    /// Arbitrary rectangles must never panic, whatever a backend reports.
    #[test]
    #[cfg_attr(
        miri,
        ignore = "an exhaustive sweep: MIRI interprets rather than executes, so this takes minutes. This crate forbids unsafe code and has no FFI, so what MIRI is here for is the buffer pool's Drop, which the cheaper tests exercise"
    )]
    fn arbitrary_rectangles_never_panic() {
        let mut x = 0x853C_49E6_748F_EA9Bu64;
        let bounds = Rect::sized(1920, 1080);
        for _ in 0..20_000 {
            let mut next = || {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                x
            };
            let a = Rect::new(next() as i32, next() as i32, next() as u32, next() as u32);
            let b = Rect::new(next() as i32, next() as i32, next() as u32, next() as u32);
            let _ = a.area();
            let _ = a.right();
            let _ = a.bottom();
            let _ = a.intersect(&b);
            let _ = a.union(&b);
            let _ = a.contains(&b);
            let _ = a.clamp_to(&bounds);
            let _ = a.translate(next() as i32, next() as i32);
            let _ = a.inflate(next() as u32 % 64);
            let _ = a.align_out(next() as u32 % 64);
        }
    }

    /// Mirrored screens are found, and screens merely touching are not: a
    /// desktop's screens share an edge, and sharing an edge is not sharing a
    /// pixel.
    #[test]
    fn screens_that_share_pixels_are_found_and_neighbours_are_not() {
        let a = Rect::sized(1920, 1080);
        let same = Rect::sized(1920, 1080);
        let beside = Rect {
            x: 1920,
            y: 0,
            width: 1920,
            height: 1080,
        };
        let over = Rect {
            x: 1900,
            y: 0,
            width: 100,
            height: 100,
        };
        assert_eq!(first_overlap(&[a, same]), Some((0, 1)));
        assert_eq!(first_overlap(&[a, beside]), None, "an edge is not a pixel");
        assert_eq!(first_overlap(&[a, beside, over]), Some((0, 2)));
        assert_eq!(first_overlap(&[a]), None);
        assert_eq!(first_overlap(&[]), None);
    }

    /// One screen tiles to itself at the origin, whatever its position.
    #[test]
    fn one_screen_is_the_whole_picture() {
        let screen = Rect {
            x: -1920,
            y: 200,
            width: 1920,
            height: 1080,
        };
        let (bounds, placed) = tiles(&[screen]);
        assert_eq!(bounds, screen, "the picture is exactly that screen");
        assert_eq!(placed, vec![Rect::sized(1920, 1080)], "and it sits at 0,0");
    }

    /// **Two screens keep the arrangement the desk has**, including one to the
    /// left of the primary, which is where negative coordinates come from.
    #[test]
    fn two_screens_arrive_in_the_order_the_desk_has_them() {
        let left = Rect {
            x: -1920,
            y: 0,
            width: 1920,
            height: 1080,
        };
        let right = Rect {
            x: 0,
            y: 0,
            width: 2560,
            height: 1440,
        };
        // Asked for in the other order: the *order* is the caller's, the
        // *places* are the desk's.
        let (bounds, placed) = tiles(&[right, left]);
        assert_eq!(bounds.width, 4480);
        assert_eq!(bounds.height, 1440, "the taller screen sets the height");
        assert_eq!(placed.first().map(|r| (r.x, r.y)), Some((1920, 0)));
        assert_eq!(placed.get(1).map(|r| (r.x, r.y)), Some((0, 0)));
    }

    /// **A screen in the middle that nobody asked for leaves a hole**, and the
    /// hole is in the picture. What fills it is the capture's business; that it
    /// is not the middle screen is the whole point of returning the tiles.
    #[test]
    fn a_screen_left_out_of_the_middle_leaves_a_hole_in_the_picture() {
        let a = Rect {
            x: 0,
            y: 0,
            width: 1920,
            height: 1080,
        };
        let c = Rect {
            x: 3840,
            y: 0,
            width: 1920,
            height: 1080,
        };
        let (bounds, placed) = tiles(&[a, c]);
        assert_eq!(bounds.width, 5760);
        let covered: u64 = placed
            .iter()
            .map(|r| u64::from(r.width) * u64::from(r.height))
            .sum();
        let whole = u64::from(bounds.width) * u64::from(bounds.height);
        assert!(
            covered < whole,
            "the middle screen's place must be part of the picture and belong to no tile"
        );
        assert!(
            !placed.iter().any(|r| r.contains(&Rect {
                x: 1920,
                y: 0,
                width: 1920,
                height: 1080,
            })),
            "the hole must not be covered by either tile"
        );
    }

    /// Nothing served is a picture of nothing, rather than a panic or a
    /// rectangle at the origin that a caller would then try to encode.
    #[test]
    fn no_screens_tile_to_nothing() {
        let (bounds, placed) = tiles(&[]);
        assert!(bounds.is_empty());
        assert!(placed.is_empty());
    }
}
