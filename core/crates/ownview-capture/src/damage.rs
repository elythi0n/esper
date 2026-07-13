//! What changed since the last frame.
//!
//! Damage is the mechanism the whole pipeline rests on: with software encode,
//! not encoding an unchanged screen is not an optimisation, it is what makes
//! the product possible. So the shape damage arrives in matters.
//!
//! # Why regions are decomposed rather than passed through
//!
//! Backends report damage in their own terms and none of them are tidy.
//! XDamage will happily report a hundred small rectangles for one text cursor
//! blink and a scroll. DXGI reports dirty and move rectangles separately.
//! PipeWire reports a video-crop-style region per buffer. All of them can
//! report rectangles that overlap each other and rectangles outside the frame.
//!
//! Overlap is the problem. A consumer that adds up region areas to decide how
//! much of the screen changed gets a number larger than the screen, and a
//! consumer that encodes each region encodes the shared pixels twice.
//!
//! The obvious fix — merge any two regions that overlap — is wrong, and wrong
//! in a case that happens constantly. A scrolling window and a moving cursor
//! produce two thin strips crossing at right angles: twelve thousand changed
//! pixels whose bounding box is two million. Merging them turns a nearly-free
//! update into a full-screen one.
//!
//! So regions are **decomposed** instead, into the canonical set of
//! non-overlapping rectangles covering exactly the same pixels. The crossing
//! strips become three rectangles totalling twelve thousand pixels, and the
//! area is exact rather than approximate. This is the standard band
//! decomposition every region algebra uses, and it is the only approach that
//! is both exact and free of double work.
//!
//! # Why there is a cap
//!
//! Without one, the region list is an unbounded allocation driven by whatever
//! the compositor happens to do. Past some point the honest answer is "a lot
//! of it changed" rather than an exact map of what — and the reduction to get
//! there must keep the set non-overlapping, which is why it works on whole
//! horizontal bands rather than on arbitrary pairs.

use crate::geometry::Rect;

/// Most regions kept before merging is forced.
///
/// Thirty-two is well above what a normal screen produces — a typing cursor
/// and a scrolling window is two or three — and well below the point where
/// iterating the list costs more than the pixels in it.
pub const MAX_REGIONS: usize = 32;

/// The fraction of the frame past which regions stop being worth tracking.
///
/// Expressed as a percentage to keep it integer arithmetic. Past this, a whole
/// frame is cheaper than the regions plus their overhead, and pretending
/// otherwise means paying for precision that buys nothing.
pub const FULL_FRAME_PERCENT: u64 = 60;

/// What changed.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Damage {
    /// Nothing changed. The frame need not be encoded at all.
    #[default]
    Nothing,
    /// Everything changed, or the backend cannot say what did.
    ///
    /// The first frame of a session, a resolution change, a backend with no
    /// damage reporting, and "so much changed that regions are pointless" all
    /// arrive here. A consumer treats them identically, which is why they are
    /// one variant.
    Everything,
    /// These regions changed.
    Regions(Regions),
}

impl Damage {
    /// Whether anything changed at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        match self {
            Self::Nothing => true,
            Self::Everything => false,
            Self::Regions(r) => r.is_empty(),
        }
    }

    /// How many pixels changed, for a frame of this size.
    ///
    /// `Everything` is the whole frame. Regions are their combined area with
    /// overlaps counted once, because after coalescing there are none.
    #[must_use]
    pub fn changed_pixels(&self, frame: &Rect) -> u64 {
        match self {
            Self::Nothing => 0,
            Self::Everything => frame.area(),
            Self::Regions(r) => r.area(),
        }
    }

    /// The regions to encode, or the whole frame.
    ///
    /// A consumer that just wants "which rectangles" should use this rather
    /// than matching, so that `Everything` cannot be forgotten.
    #[must_use]
    pub fn rects(&self, frame: &Rect) -> Vec<Rect> {
        match self {
            Self::Nothing => Vec::new(),
            Self::Everything => vec![*frame],
            Self::Regions(r) => r.as_slice().to_vec(),
        }
    }
}

/// A bounded, coalesced set of changed rectangles.
///
/// Built through [`Regions::coalesce`], which is the only way to get one whose
/// invariants hold: every region is inside the frame, non-empty, and does not
/// overlap any other.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Regions {
    rects: Vec<Rect>,
    area: u64,
}

impl Regions {
    /// Reduce raw backend rectangles to a bounded, non-overlapping set.
    ///
    /// Returns [`Damage::Everything`] when the result covers enough of the
    /// frame that regions have stopped being worth the bookkeeping, and
    /// [`Damage::Nothing`] when nothing survives clamping.
    ///
    /// # What the result guarantees
    ///
    /// - **Never fewer pixels than the input covered.** Losing one leaves a
    ///   stale patch on a viewer's screen, which is the one failure a damage
    ///   system must not have.
    /// - **Exactly the input's pixels**, unless the region cap had to bind.
    ///   Enforcing the cap collapses whole bands to their hulls, which grows
    ///   coverage on purpose: a bound that could be exceeded is not a bound,
    ///   and paying for a few unchanged pixels is the cheaper mistake.
    /// - **No two regions overlap**, so a caller may add their areas.
    #[must_use]
    pub fn coalesce(raw: &[Rect], frame: &Rect) -> Damage {
        let clamped: Vec<Rect> = raw
            .iter()
            .filter_map(|r| r.clamp_to(frame))
            .filter(|r| !r.is_empty())
            .collect();

        if clamped.is_empty() {
            return Damage::Nothing;
        }

        let mut bands = bands_of(&clamped);
        coalesce_vertically(&mut bands);
        enforce_cap(&mut bands);

        let rects = emit(&bands);
        let area: u64 = rects.iter().map(Rect::area).sum();
        let frame_area = frame.area();

        // Past the threshold, say so plainly.
        if frame_area > 0
            && area.saturating_mul(100) >= frame_area.saturating_mul(FULL_FRAME_PERCENT)
        {
            return Damage::Everything;
        }

        Damage::Regions(Self { rects, area })
    }

    /// A single region, clamped to the frame.
    #[must_use]
    pub fn one(rect: Rect, frame: &Rect) -> Damage {
        Self::coalesce(&[rect], frame)
    }

    /// The regions. None overlaps any other.
    #[must_use]
    pub fn as_slice(&self) -> &[Rect] {
        &self.rects
    }

    /// How many regions.
    #[must_use]
    pub fn len(&self) -> usize {
        self.rects.len()
    }

    /// Whether there are none.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rects.is_empty()
    }

    /// Total pixels covered.
    ///
    /// Exact, because the regions do not overlap — which is the reason the
    /// decomposition exists.
    #[must_use]
    pub const fn area(&self) -> u64 {
        self.area
    }

    /// The smallest rectangle covering all of them.
    #[must_use]
    pub fn bounds(&self) -> Rect {
        self.rects
            .iter()
            .copied()
            .reduce(|a, b| a.union(&b))
            .unwrap_or_default()
    }
}

/// A horizontal slice of the frame, and the x ranges changed within it.
///
/// Bands never overlap each other, which is what makes every reduction below
/// safe: whatever is done inside a band, or to a pair of adjacent bands,
/// cannot make two regions share a pixel.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Band {
    top: i64,
    bottom: i64,
    /// Half-open `[start, end)` ranges, sorted and non-overlapping.
    spans: Vec<(i64, i64)>,
}

impl Band {
    fn height(&self) -> i64 {
        self.bottom.saturating_sub(self.top)
    }

    /// Pixels this band covers.
    fn area(&self) -> u64 {
        let h = self.height().max(0) as u64;
        self.spans
            .iter()
            .map(|(a, b)| (b.saturating_sub(*a).max(0) as u64).saturating_mul(h))
            .sum()
    }

    /// The single span covering all of them.
    fn hull(&self) -> Option<(i64, i64)> {
        let first = self.spans.first()?;
        let last = self.spans.last()?;
        Some((first.0, last.1))
    }
}

/// Split rectangles into horizontal bands at every distinct top and bottom
/// edge, so that within a band each rectangle either covers it entirely or
/// does not touch it at all.
fn bands_of(rects: &[Rect]) -> Vec<Band> {
    let mut edges: Vec<i64> = Vec::with_capacity(rects.len().saturating_mul(2));
    for r in rects {
        edges.push(i64::from(r.y));
        edges.push(r.bottom());
    }
    edges.sort_unstable();
    edges.dedup();

    let mut bands = Vec::new();
    for pair in edges.windows(2) {
        let (Some(&top), Some(&bottom)) = (pair.first(), pair.get(1)) else {
            continue;
        };

        // A rectangle either spans the whole band or misses it, because the
        // band's edges came from the rectangles' own edges.
        let mut spans: Vec<(i64, i64)> = rects
            .iter()
            .filter(|r| i64::from(r.y) <= top && r.bottom() >= bottom)
            .map(|r| (i64::from(r.x), r.right()))
            .collect();

        if spans.is_empty() {
            continue;
        }
        spans.sort_unstable();
        merge_spans(&mut spans);
        bands.push(Band { top, bottom, spans });
    }
    bands
}

/// Merge overlapping and touching ranges in a sorted list.
fn merge_spans(spans: &mut Vec<(i64, i64)>) {
    let mut out: Vec<(i64, i64)> = Vec::with_capacity(spans.len());
    for &(start, end) in spans.iter() {
        match out.last_mut() {
            // Touching counts: two ranges that meet describe one run of
            // pixels, and splitting it would be per-region overhead for
            // nothing.
            Some(last) if start <= last.1 => last.1 = last.1.max(end),
            _ => out.push((start, end)),
        }
    }
    *spans = out;
}

/// Merge vertically adjacent bands whose spans are identical.
///
/// This is what keeps the ordinary cases at one rectangle. A single changed
/// region produces several bands with the same spans — one per distinct edge
/// in the input — and without this step a plain window update would arrive as
/// three or four rectangles stacked on each other.
fn coalesce_vertically(bands: &mut Vec<Band>) {
    let mut out: Vec<Band> = Vec::with_capacity(bands.len());
    for band in bands.drain(..) {
        match out.last_mut() {
            Some(prev) if prev.bottom == band.top && prev.spans == band.spans => {
                prev.bottom = band.bottom;
            }
            _ => out.push(band),
        }
    }
    *bands = out;
}

/// Reduce until the emitted rectangle count is within [`MAX_REGIONS`].
///
/// Two moves, in order, both of which keep the set non-overlapping:
///
/// 1. Collapse the busiest band to its hull. Strictly reduces the total number
///    of spans, and a band's own hull cannot reach into another band.
/// 2. When every band holds a single span, merge the adjacent pair whose union
///    costs the fewest extra pixels. Strictly reduces the band count.
///
/// Each move strictly decreases a non-negative quantity, so this terminates.
fn enforce_cap(bands: &mut Vec<Band>) {
    while count(bands) > MAX_REGIONS {
        let busiest = bands
            .iter()
            .enumerate()
            .filter(|(_, b)| b.spans.len() > 1)
            .max_by_key(|(_, b)| b.spans.len())
            .map(|(i, _)| i);

        if let Some(i) = busiest {
            if let Some(band) = bands.get_mut(i)
                && let Some(hull) = band.hull()
            {
                band.spans = vec![hull];
            }
            coalesce_vertically(bands);
            continue;
        }

        // Every band is a single span now, so the count is the band count.
        let Some(at) = cheapest_adjacent_merge(bands) else {
            break;
        };
        let Some(next) = bands.get(at.saturating_add(1)).cloned() else {
            break;
        };
        let _ = bands.remove(at.saturating_add(1));
        if let Some(band) = bands.get_mut(at) {
            let merged_span = match (band.hull(), next.hull()) {
                (Some(a), Some(b)) => (a.0.min(b.0), a.1.max(b.1)),
                (Some(a), None) => a,
                (None, Some(b)) => b,
                (None, None) => break,
            };
            band.bottom = next.bottom;
            band.spans = vec![merged_span];
        }
        coalesce_vertically(bands);
    }
}

/// How many rectangles these bands will emit.
fn count(bands: &[Band]) -> usize {
    bands.iter().map(|b| b.spans.len()).sum()
}

/// The adjacent band pair whose merge adds the fewest pixels.
///
/// Adjacent only: merging two bands with a gap between them would swallow the
/// unchanged rows in between, and those rows are the reason damage tracking
/// pays for itself.
fn cheapest_adjacent_merge(bands: &[Band]) -> Option<usize> {
    let mut best: Option<(usize, u64)> = None;
    for (i, pair) in bands.windows(2).enumerate() {
        let (Some(a), Some(b)) = (pair.first(), pair.get(1)) else {
            continue;
        };
        let (Some(ha), Some(hb)) = (a.hull(), b.hull()) else {
            continue;
        };
        let width = (ha.1.max(hb.1).saturating_sub(ha.0.min(hb.0))).max(0) as u64;
        let height = (b.bottom.saturating_sub(a.top)).max(0) as u64;
        let cost = width
            .saturating_mul(height)
            .saturating_sub(a.area().saturating_add(b.area()));
        if best.is_none_or(|(_, c)| cost < c) {
            best = Some((i, cost));
        }
    }
    best.map(|(i, _)| i)
}

/// Turn bands back into rectangles.
fn emit(bands: &[Band]) -> Vec<Rect> {
    let mut out = Vec::with_capacity(count(bands));
    for band in bands {
        let height = u32::try_from(band.height()).unwrap_or(u32::MAX);
        for &(start, end) in &band.spans {
            let width = u32::try_from(end.saturating_sub(start)).unwrap_or(u32::MAX);
            if width == 0 || height == 0 {
                continue;
            }
            out.push(Rect {
                x: i32::try_from(start).unwrap_or(i32::MIN),
                y: i32::try_from(band.top).unwrap_or(i32::MIN),
                width,
                height,
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const FRAME: Rect = Rect::sized(1920, 1080);

    fn regions(d: &Damage) -> Vec<Rect> {
        match d {
            Damage::Regions(r) => r.as_slice().to_vec(),
            other => panic!("expected regions, got {other:?}"),
        }
    }

    /// Rasterise rectangles into a grid of changed pixels.
    ///
    /// The oracle. Whatever the decomposition does with bands and spans, the
    /// only thing that matters is which pixels it says changed — and a bitmap
    /// answers that with no shared logic to be wrong in the same way.
    fn raster(rects: &[Rect], w: i64, h: i64) -> Vec<bool> {
        let mut grid = vec![false; (w * h).max(0) as usize];
        for r in rects {
            for y in i64::from(r.y).max(0)..r.bottom().min(h) {
                for x in i64::from(r.x).max(0)..r.right().min(w) {
                    if let Some(cell) = grid.get_mut((y * w + x) as usize) {
                        *cell = true;
                    }
                }
            }
        }
        grid
    }

    #[test]
    fn nothing_reported_is_nothing_changed() {
        assert_eq!(Regions::coalesce(&[], &FRAME), Damage::Nothing);
        assert!(Damage::Nothing.is_empty());
        assert_eq!(Damage::Nothing.changed_pixels(&FRAME), 0);
    }

    #[test]
    fn empty_and_out_of_frame_rectangles_are_dropped() {
        let d = Regions::coalesce(
            &[
                Rect::new(0, 0, 0, 100),       // zero width
                Rect::new(5000, 5000, 10, 10), // outside
                Rect::new(-100, -100, 10, 10), // outside, negative
            ],
            &FRAME,
        );
        assert_eq!(d, Damage::Nothing, "none of those changed a pixel");
    }

    #[test]
    fn a_single_small_change_stays_a_single_region() {
        let d = Regions::one(Rect::new(100, 100, 50, 20), &FRAME);
        assert_eq!(regions(&d), vec![Rect::new(100, 100, 50, 20)]);
        assert_eq!(d.changed_pixels(&FRAME), 1000);
    }

    /// A change straddling the frame edge is trimmed, not rejected — a window
    /// dragged off-screen produces exactly this.
    #[test]
    fn a_change_straddling_the_edge_is_trimmed() {
        let d = Regions::one(Rect::new(1900, 1070, 100, 100), &FRAME);
        assert_eq!(regions(&d), vec![Rect::new(1900, 1070, 20, 10)]);
    }

    /// Adjacent regions always merge: the union is no bigger than the two
    /// together, and one region costs less overhead than two.
    #[test]
    fn adjacent_regions_merge() {
        let d = Regions::coalesce(&[Rect::new(0, 0, 10, 10), Rect::new(10, 0, 10, 10)], &FRAME);
        assert_eq!(regions(&d), vec![Rect::new(0, 0, 20, 10)]);
    }

    /// Overlapping regions are decomposed so no pixel is covered twice, and
    /// the total is the true area rather than the sum of the inputs.
    #[test]
    fn overlapping_regions_are_decomposed_not_double_counted() {
        let a = Rect::new(0, 0, 100, 100);
        let b = Rect::new(10, 10, 100, 100);
        let d = Regions::coalesce(&[a, b], &FRAME);
        let got = regions(&d);

        // 20000 would be the naive sum; 8100 of it is the shared square.
        assert_eq!(
            d.changed_pixels(&FRAME),
            20_000 - 8_100,
            "the area must be what actually changed, not the sum of the inputs"
        );
        // And it must be less than the bounding box, which is the thing a
        // merge would have produced.
        assert!(d.changed_pixels(&FRAME) < a.union(&b).area(), "{got:?}");
    }

    /// A region entirely inside another must not survive as a separate one.
    #[test]
    fn a_contained_region_is_absorbed() {
        let d = Regions::coalesce(
            &[Rect::new(0, 0, 200, 200), Rect::new(50, 50, 10, 10)],
            &FRAME,
        );
        assert_eq!(regions(&d), vec![Rect::new(0, 0, 200, 200)]);
    }

    /// **The case a "merge if they overlap" rule gets wrong.** Two thin strips
    /// crossing at right angles — a scrolling window and a moving cursor — have
    /// a union of nearly the whole frame and cost almost nothing separately.
    #[test]
    fn crossing_thin_strips_stay_cheap() {
        let horizontal = Rect::new(0, 500, 1920, 4);
        let vertical = Rect::new(900, 0, 4, 1080);
        let d = Regions::coalesce(&[horizontal, vertical], &FRAME);
        let got = regions(&d);

        let separate = horizontal.area() + vertical.area();
        let shared = horizontal.intersect(&vertical).map_or(0, |r| r.area());
        assert_eq!(
            d.changed_pixels(&FRAME),
            separate - shared,
            "exactly the changed pixels, no more: {got:?}"
        );
        assert!(
            d.changed_pixels(&FRAME) < FRAME.area() / 100,
            "merging these would turn twelve thousand pixels into two million"
        );
    }

    /// Two far-apart small changes stay separate for the same reason.
    #[test]
    fn distant_small_changes_stay_separate() {
        let a = Rect::new(0, 0, 10, 10);
        let b = Rect::new(1900, 1070, 10, 10);
        let d = Regions::coalesce(&[a, b], &FRAME);
        assert_eq!(regions(&d).len(), 2);
        assert_eq!(d.changed_pixels(&FRAME), 200);
    }

    /// The bound. However many rectangles a compositor reports, the list stays
    /// bounded — the bookkeeping must not be driven by what the screen does.
    #[test]
    #[cfg_attr(
        miri,
        ignore = "an exhaustive sweep: MIRI interprets rather than executes, so this takes minutes. This crate forbids unsafe code and has no FFI, so what MIRI is here for is the buffer pool's Drop, which the cheaper tests exercise"
    )]
    fn the_region_count_is_bounded_however_many_arrive() {
        // 400 scattered small changes, none of them adjacent.
        let raw: Vec<Rect> = (0..400u32)
            .map(|i| Rect::new(((i % 20) * 90) as i32, ((i / 20) * 50) as i32, 4, 4))
            .collect();
        let d = Regions::coalesce(&raw, &FRAME);
        match d {
            Damage::Regions(r) => assert!(
                r.len() <= MAX_REGIONS,
                "{} regions survived a cap of {MAX_REGIONS}",
                r.len()
            ),
            // Also acceptable: so much changed that regions stopped paying.
            Damage::Everything => {}
            Damage::Nothing => panic!("400 changes is not nothing"),
        }
    }

    /// Forcing merges must still cover every original pixel. A bound that
    /// dropped damage would leave stale pixels on the viewer's screen, which
    /// is the one failure mode a damage system must not have.
    #[test]
    #[cfg_attr(
        miri,
        ignore = "an exhaustive sweep: MIRI interprets rather than executes, so this takes minutes. This crate forbids unsafe code and has no FFI, so what MIRI is here for is the buffer pool's Drop, which the cheaper tests exercise"
    )]
    fn forced_merging_never_loses_a_changed_pixel() {
        let raw: Vec<Rect> = (0..300u32)
            .map(|i| Rect::new(((i * 7) % 1900) as i32, ((i * 13) % 1070) as i32, 3, 3))
            .collect();
        let d = Regions::coalesce(&raw, &FRAME);
        let covered = d.rects(&FRAME);

        for original in &raw {
            let inside = original.clamp_to(&FRAME).expect("all are inside");
            assert!(
                covered.iter().any(|c| c.contains(&inside)),
                "region {inside:?} was dropped by coalescing — stale pixels \
                 would be left on the viewer's screen"
            );
        }
    }

    /// Past the threshold the honest answer is "a lot of it", and a whole
    /// frame is cheaper than regions plus their overhead.
    #[test]
    fn enough_change_becomes_a_whole_frame() {
        // Two thirds of the frame, in one region.
        let two_thirds = Rect::new(0, 0, 1920, 720);
        assert_eq!(Regions::one(two_thirds, &FRAME), Damage::Everything);

        // And just under the threshold does not.
        let half = Rect::new(0, 0, 1920, 540);
        assert!(matches!(Regions::one(half, &FRAME), Damage::Regions(_)));
    }

    #[test]
    fn everything_covers_the_whole_frame() {
        assert!(!Damage::Everything.is_empty());
        assert_eq!(Damage::Everything.changed_pixels(&FRAME), FRAME.area());
        assert_eq!(Damage::Everything.rects(&FRAME), vec![FRAME]);
    }

    /// **The decomposition, checked against a bitmap.** Random rectangles in,
    /// and the pixels the result claims changed must be exactly the pixels the
    /// input covered — no fewer, or stale pixels are left on the viewer's
    /// screen, and no more, or work is wasted.
    ///
    /// **At most four rectangles**, so the region cap cannot bind: `n`
    /// rectangles give at most `2n - 1` bands of at most `n` spans, and
    /// `4 * 7` is under the cap of 32. Enforcing the cap collapses bands to
    /// their hulls and so grows coverage on purpose, which would make an
    /// exactness assertion wrong rather than the code. The capped case is the
    /// test below.
    #[test]
    #[cfg_attr(
        miri,
        ignore = "an exhaustive sweep: MIRI interprets rather than executes, so this takes minutes. This crate forbids unsafe code and has no FFI, so what MIRI is here for is the buffer pool's Drop, which the cheaper tests exercise"
    )]
    fn the_decomposition_covers_exactly_the_pixels_that_changed() {
        const W: i64 = 64;
        const H: i64 = 48;
        let frame = Rect::sized(W as u32, H as u32);

        let mut x = 0x9E88_1B25_C0FF_EE01u64;
        for round in 0..2000 {
            let mut next = || {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                x
            };
            let raw: Vec<Rect> = (0..(next() % 4) + 1)
                .map(|_| {
                    Rect::new(
                        (next() % (W as u64 + 8)) as i32 - 4,
                        (next() % (H as u64 + 8)) as i32 - 4,
                        (next() % 20) as u32,
                        (next() % 20) as u32,
                    )
                })
                .collect();

            let want = raster(&raw, W, H);
            let d = Regions::coalesce(&raw, &frame);

            // `Everything` is allowed to be an over-approximation — it is the
            // deliberate "a lot of it changed" answer — so it only has to
            // cover what changed.
            let got = raster(&d.rects(&frame), W, H);
            match d {
                Damage::Everything => {
                    for (i, w) in want.iter().enumerate() {
                        assert!(
                            !*w || got.get(i).copied().unwrap_or(false),
                            "round {round}: a whole frame must still cover pixel {i}"
                        );
                    }
                }
                _ => assert_eq!(
                    got, want,
                    "round {round}: decomposing {raw:?} changed which pixels are covered"
                ),
            }
        }
    }

    /// Once the cap binds, coverage may grow — but it may never shrink. This
    /// is the direction that matters: covering extra pixels wastes encoding,
    /// covering fewer leaves a stale patch on someone's screen.
    #[test]
    #[cfg_attr(
        miri,
        ignore = "an exhaustive sweep: MIRI interprets rather than executes, so this takes minutes. This crate forbids unsafe code and has no FFI, so what MIRI is here for is the buffer pool's Drop, which the cheaper tests exercise"
    )]
    fn once_the_cap_binds_coverage_grows_but_never_shrinks() {
        const W: i64 = 48;
        const H: i64 = 32;
        let frame = Rect::sized(W as u32, H as u32);

        let mut x = 0x1234_5678_9ABC_DEF1u64;
        let mut ever_capped = false;
        for round in 0..500 {
            let mut next = || {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                x
            };
            // Enough rectangles that the cap has to bind sometimes.
            let raw: Vec<Rect> = (0..24)
                .map(|_| {
                    Rect::new(
                        (next() % 56) as i32 - 4,
                        (next() % 40) as i32 - 4,
                        (next() % 6) as u32,
                        (next() % 6) as u32,
                    )
                })
                .collect();

            let want = raster(&raw, W, H);
            let d = Regions::coalesce(&raw, &frame);
            let covered = raster(&d.rects(&frame), W, H);

            for (i, w) in want.iter().enumerate() {
                assert!(
                    !*w || covered.get(i).copied().unwrap_or(false),
                    "round {round}: pixel {i} changed but is not covered"
                );
            }
            if let Damage::Regions(r) = &d {
                assert!(r.len() <= MAX_REGIONS);
                if r.len() == MAX_REGIONS {
                    ever_capped = true;
                }
            }
        }
        assert!(
            ever_capped,
            "this test is supposed to reach the cap; if it never does it is \
             not testing the capped path at all"
        );
    }

    /// The invariant callers rely on: no two regions share a pixel, so their
    /// areas can simply be added.
    #[test]
    #[cfg_attr(
        miri,
        ignore = "an exhaustive sweep: MIRI interprets rather than executes, so this takes minutes. This crate forbids unsafe code and has no FFI, so what MIRI is here for is the buffer pool's Drop, which the cheaper tests exercise"
    )]
    fn coalesced_regions_never_overlap() {
        let mut x = 0xD1B5_4A32_D192_ED03u64;
        for _ in 0..300 {
            let mut next = || {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                x
            };
            let raw: Vec<Rect> = (0..(next() % 40) + 1)
                .map(|_| {
                    Rect::new(
                        (next() % 2000) as i32 - 40,
                        (next() % 1200) as i32 - 40,
                        (next() % 300) as u32,
                        (next() % 300) as u32,
                    )
                })
                .collect();

            if let Damage::Regions(r) = Regions::coalesce(&raw, &FRAME) {
                let rs = r.as_slice();
                for (i, a) in rs.iter().enumerate() {
                    for b in rs.iter().skip(i.saturating_add(1)) {
                        assert!(
                            !a.overlaps(b),
                            "coalescing left {a:?} overlapping {b:?}, so areas \
                             cannot be added"
                        );
                    }
                }
                assert_eq!(
                    r.area(),
                    rs.iter().map(Rect::area).sum::<u64>(),
                    "the cached area must match the regions"
                );
                assert!(rs.iter().all(|x| FRAME.contains(x)), "{rs:?}");
                assert!(rs.iter().all(|x| !x.is_empty()));
            }
        }
    }

    #[test]
    fn bounds_covers_every_region() {
        let d = Regions::coalesce(
            &[Rect::new(10, 10, 5, 5), Rect::new(1000, 900, 5, 5)],
            &FRAME,
        );
        if let Damage::Regions(r) = d {
            let b = r.bounds();
            assert!(r.as_slice().iter().all(|x| b.contains(x)));
            assert_eq!(b, Rect::new(10, 10, 995, 895));
        } else {
            panic!("expected regions");
        }
    }

    /// A zero-size frame must not divide by zero on the threshold check.
    #[test]
    fn a_zero_size_frame_does_not_panic() {
        let empty = Rect::sized(0, 0);
        assert_eq!(
            Regions::one(Rect::new(0, 0, 10, 10), &empty),
            Damage::Nothing
        );
    }

    /// Arbitrary input must never panic and must always terminate.
    #[test]
    #[cfg_attr(
        miri,
        ignore = "an exhaustive sweep: MIRI interprets rather than executes, so this takes minutes. This crate forbids unsafe code and has no FFI, so what MIRI is here for is the buffer pool's Drop, which the cheaper tests exercise"
    )]
    fn arbitrary_damage_never_panics() {
        let mut x = 0x2FE1_3B99_7C4D_5A67u64;
        for _ in 0..400 {
            let mut next = || {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                x
            };
            let raw: Vec<Rect> = (0..(next() % 80))
                .map(|_| Rect::new(next() as i32, next() as i32, next() as u32, next() as u32))
                .collect();
            let frame = Rect::sized((next() % 4000) as u32, (next() % 3000) as u32);
            let d = Regions::coalesce(&raw, &frame);
            let _ = d.changed_pixels(&frame);
            let _ = d.rects(&frame);
        }
    }
}
