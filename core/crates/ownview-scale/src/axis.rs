//! Per-axis resampling weights, built once when the size changes.
//!
//! # Exact area coverage, in integers
//!
//! Each destination sample is the average of the source samples it covers,
//! weighted by how much of each it covers. For a 4→3 reduction the first
//! destination sample covers source pixel 0 entirely and a third of source
//! pixel 1, so the weights are 3/4 and 1/4.
//!
//! The arithmetic is integer throughout, and that is not fussiness. Positions
//! are held in units of 1/`dst` of a source pixel, so a boundary that falls
//! exactly on a pixel edge — every boundary, at the ratios that matter most —
//! lands on an exact integer instead of `0.9999999999999998`. A `floor()` of
//! that in `f64` picks the wrong source pixel, which is a one-pixel shift in
//! the output that appears at some sizes and not others. Integers cannot do
//! that, and it makes a scaled frame bit-identical on every platform.
//!
//! # The weights sum to exactly one
//!
//! Rounding each weight independently leaves the sum a little under or over,
//! and the error is not noise: it is a constant multiplier on that whole
//! row or column. A sum of 16383/16384 darkens every frame by one part in
//! sixteen thousand, forever, and a flat grey wall scales to a slightly
//! different flat grey. So after rounding, the residue is pushed into the
//! largest weight, where it is proportionally smallest.

/// Fractional bits in a weight.
///
/// Ten, and the number is load-bearing. Two passes multiply by a weight each,
/// so a sample ends up scaled by `ONE * ONE`:
///
/// ```text
/// 255 * 1024 * 1024 = 267,386,880   <   4,294,967,295 = u32::MAX
/// ```
///
/// which means the whole resampling runs in `u32` with sixteen times the
/// headroom it needs. At fourteen bits it does not fit, and the horizontal
/// pass has to accumulate in `u64` — which measured 96% *slower* than simply
/// converting the frame at full size, making the resolution rung cost more
/// CPU than the pixels it removed.
///
/// The precision given up is a thousandth of a weight. The tests hold the
/// output to within one level of a floating-point area average, which is
/// where it has to be regardless of how the weights are stored.
pub const SHIFT: u32 = 10;

/// A weight of one.
pub const ONE: u32 = 1 << SHIFT;

/// What went wrong building an axis.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum AxisError {
    /// A zero-length axis has no samples to average.
    #[error("an axis cannot be {0} samples long")]
    Empty(u32),
    /// Upscaling is not implemented, because nothing asks for it.
    #[error("this is a downscaler: {dst} is larger than {src}")]
    Upscale { src: u32, dst: u32 },
}

/// The source samples and weights for every destination sample on one axis.
///
/// Stored flat rather than as a vector of vectors: the spans are walked once
/// per row of every frame, and a pointer chase per destination pixel is a
/// cache miss per destination pixel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Axis {
    src: u32,
    dst: u32,
    /// First source sample covered by each destination sample.
    starts: Vec<u32>,
    /// How many source samples each destination sample covers.
    lens: Vec<u32>,
    /// Where each destination sample's weights begin in `weights`.
    offsets: Vec<u32>,
    /// Weights, `SHIFT`-bit fixed point, summing to `ONE` per destination.
    weights: Vec<u32>,
}

impl Axis {
    /// Build the table for a reduction from `src` samples to `dst`.
    pub fn new(src: u32, dst: u32) -> Result<Self, AxisError> {
        if src == 0 {
            return Err(AxisError::Empty(src));
        }
        if dst == 0 {
            return Err(AxisError::Empty(dst));
        }
        if dst > src {
            return Err(AxisError::Upscale { src, dst });
        }

        let mut starts = Vec::with_capacity(dst as usize);
        let mut lens = Vec::with_capacity(dst as usize);
        let mut offsets = Vec::with_capacity(dst as usize);
        let mut weights = Vec::new();

        let src64 = u64::from(src);
        let dst64 = u64::from(dst);

        for i in 0..u64::from(dst) {
            // Positions in units of 1/dst of a source pixel. The span is
            // exactly `src` of those units wide, for every i.
            let lo = i * src64;
            let hi = (i + 1) * src64;

            let first = lo / dst64;
            let last = hi.div_ceil(dst64).min(src64);
            // `last > first` always: the span is src/dst >= 1 pixels wide.
            let len = last - first;

            offsets.push(u32::try_from(weights.len()).unwrap_or(u32::MAX));
            starts.push(u32::try_from(first).unwrap_or(u32::MAX));
            lens.push(u32::try_from(len).unwrap_or(u32::MAX));

            // Exact weight of source sample j is overlap/src. Held as a
            // floor and a remainder so the rounding can be apportioned
            // rather than each weight rounded on its own.
            let base = weights.len();
            let mut remainders = Vec::with_capacity(len as usize);
            for j in first..last {
                let cover_lo = lo.max(j * dst64);
                let cover_hi = hi.min((j + 1) * dst64);
                let overlap = cover_hi.saturating_sub(cover_lo);
                let exact = overlap * u64::from(ONE);
                weights.push(u32::try_from(exact / src64).unwrap_or(ONE));
                remainders.push(exact % src64);
            }

            Self::apportion(weights.get_mut(base..).unwrap_or_default(), &remainders);
        }

        Ok(Self {
            src,
            dst,
            starts,
            lens,
            offsets,
            weights,
        })
    }

    /// Hand the missing units to the weights that were cut by the most.
    ///
    /// The floors sum to at most `ONE`; the shortfall is exactly how many
    /// weights deserve one more unit. Giving it to those with the largest
    /// discarded remainders leaves every weight within a single unit of its
    /// exact value, which is the best any integer split can do — and unlike
    /// rounding each weight on its own, the total is still exactly `ONE`.
    ///
    /// Ties go to the earlier sample, so the table is a function of the two
    /// sizes and nothing else.
    fn apportion(span: &mut [u32], remainders: &[u64]) {
        let sum: u32 = span.iter().copied().sum();
        let mut deficit = ONE.saturating_sub(sum);
        if deficit == 0 {
            return;
        }
        let mut order: Vec<usize> = (0..span.len()).collect();
        order.sort_by(|&a, &b| {
            let ra = remainders.get(a).copied().unwrap_or(0);
            let rb = remainders.get(b).copied().unwrap_or(0);
            rb.cmp(&ra).then(a.cmp(&b))
        });
        for at in order {
            if deficit == 0 {
                break;
            }
            if let Some(slot) = span.get_mut(at) {
                *slot = slot.saturating_add(1);
                deficit -= 1;
            }
        }
    }

    /// Source samples this axis expects.
    #[must_use]
    pub const fn src(&self) -> u32 {
        self.src
    }

    /// Destination samples this axis produces.
    #[must_use]
    pub const fn dst(&self) -> u32 {
        self.dst
    }

    /// Whether this axis is a pass-through.
    #[must_use]
    pub const fn is_identity(&self) -> bool {
        self.src == self.dst
    }

    /// The source span and weights for one destination sample.
    #[must_use]
    pub fn span(&self, dst_index: u32) -> Option<(u32, &[u32])> {
        let at = dst_index as usize;
        let start = *self.starts.get(at)?;
        let len = *self.lens.get(at)? as usize;
        let offset = *self.offsets.get(at)? as usize;
        let weights = self.weights.get(offset..offset.checked_add(len)?)?;
        Some((start, weights))
    }

    /// The spans for destination samples `from..to`, in order.
    ///
    /// The hot loop walks these rather than calling [`Self::span`] per pixel.
    /// That call is four bounds-checked lookups into four vectors, which at a
    /// million destination pixels a frame is four million checks the compiler
    /// cannot hoist — and it cannot vectorise around them either.
    pub fn spans_in(&self, from: u32, to: u32) -> impl Iterator<Item = (u32, &[u32])> {
        let lo = from as usize;
        let hi = (to as usize).min(self.starts.len());
        let starts = self.starts.get(lo..hi).unwrap_or_default();
        let lens = self.lens.get(lo..hi).unwrap_or_default();
        let base = self.offsets.get(lo).copied().unwrap_or(0) as usize;
        let mut cursor = base;
        starts.iter().zip(lens.iter()).map(move |(&start, &len)| {
            let n = len as usize;
            let w = self
                .weights
                .get(cursor..cursor.saturating_add(n))
                .unwrap_or_default();
            cursor = cursor.saturating_add(n);
            (start, w)
        })
    }

    /// The destination samples that any of `[start, start + len)` feeds.
    ///
    /// Conservative by construction: it returns every destination sample
    /// whose span touches the source range, and never fewer. Returning fewer
    /// would leave a destination sample holding a stale average of source
    /// pixels that have since changed, which is a patch of the previous frame
    /// that never repaints — the exact failure damage tracking exists to
    /// avoid, made invisible by being correct almost everywhere.
    #[must_use]
    pub fn dst_range(&self, start: u32, len: u32) -> Option<(u32, u32)> {
        if len == 0 || start >= self.src {
            return None;
        }
        let end = start.saturating_add(len).min(self.src);

        // The lowest destination sample that reads `start`, exactly, with no
        // search. Writing `lo = floor(start*dst/src)`:
        //
        //   lo <= start*dst/src, so (lo+1)*src/dst > start,
        //   so this sample's span ends after `start` — it overlaps.
        //
        //   lo*src/dst <= start, so the previous sample's span ends at or
        //   before `start` — it does not.
        //
        // An earlier version walked outwards from this guess in both
        // directions to be safe. Neither loop could ever execute, which is
        // only visible once you try to break the guess and nothing fails, so
        // the proof above and `dst_range_never_misses_a_destination_that_
        // reads_the_source` stand in place of the search.
        let lo = u32::try_from(u64::from(start) * u64::from(self.dst) / u64::from(self.src))
            .unwrap_or(0)
            .min(self.dst.saturating_sub(1));

        let mut hi = lo;
        while hi.saturating_add(1) < self.dst {
            let Some((s, _)) = self.span(hi.saturating_add(1)) else {
                break;
            };
            if s < end {
                hi = hi.saturating_add(1);
            } else {
                break;
            }
        }
        Some((lo, hi.saturating_sub(lo).saturating_add(1)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Several of these sweep every size pair in a range, which is the point:
    // a weight table that is right for the ratios someone thought of is not a
    // weight table that is right. They are `#[ignore]`d under MIRI, which
    // interprets one operation at a time and turns a second into minutes.
    // Ignored there rather than shrunk — a bound chosen to keep an interpreter
    // happy stops proving the thing — and every code path they cover is also
    // covered by a small deterministic test that MIRI does run.

    /// Every weight of one destination sample, spread over the whole source
    /// axis, computed in floating point.
    ///
    /// Dense rather than a span so it can be compared against the integer
    /// table without also agreeing about where the span starts — which is
    /// exactly the thing floating point gets wrong at an exact boundary.
    fn reference(src: u32, dst: u32, i: u32) -> Vec<f64> {
        let ratio = f64::from(src) / f64::from(dst);
        let lo = f64::from(i) * ratio;
        let hi = f64::from(i + 1) * ratio;
        (0..src)
            .map(|j| {
                let cover_lo = lo.max(f64::from(j));
                let cover_hi = hi.min(f64::from(j + 1));
                (cover_hi - cover_lo).max(0.0) / (hi - lo)
            })
            .collect()
    }

    /// The integer table's weights for one destination sample, spread the
    /// same way.
    fn dense(axis: &Axis, i: u32) -> Vec<f64> {
        let mut out = vec![0.0; axis.src() as usize];
        let (start, weights) = axis.span(i).expect("span in range");
        for (k, &w) in weights.iter().enumerate() {
            let at = start as usize + k;
            if let Some(slot) = out.get_mut(at) {
                *slot = f64::from(w) / f64::from(ONE);
            }
        }
        out
    }

    #[test]
    #[cfg_attr(miri, ignore = "an exhaustive sweep: minutes under MIRI")]
    fn weights_sum_to_exactly_one() {
        // Not "close to one": a sum of ONE-1 is a constant darkening of every
        // frame, and it would never be noticed.
        for src in 1..=96u32 {
            for dst in 1..=src {
                let axis = Axis::new(src, dst).expect("valid");
                for i in 0..dst {
                    let (_, w) = axis.span(i).expect("span in range");
                    let sum: u32 = w.iter().copied().sum();
                    assert_eq!(sum, ONE, "{src}->{dst} sample {i} sums to {sum}");
                }
            }
        }
    }

    #[test]
    #[cfg_attr(miri, ignore = "an exhaustive sweep: minutes under MIRI")]
    fn weights_match_an_area_reference() {
        for src in 1..=64u32 {
            for dst in 1..=src {
                let axis = Axis::new(src, dst).expect("valid");
                for i in 0..dst {
                    let got = dense(&axis, i);
                    let want = reference(src, dst, i);
                    for (j, (g, w)) in got.iter().zip(want.iter()).enumerate() {
                        assert!(
                            (g - w).abs() <= 1.0 / f64::from(ONE),
                            "{src}->{dst} sample {i} source {j}: {g} vs {w}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    #[cfg_attr(miri, ignore = "an exhaustive sweep: minutes under MIRI")]
    fn every_source_sample_is_read_by_something() {
        // A source sample no destination sample covers is a column or row of
        // the picture that is simply thrown away, which is what nearest
        // neighbour does and why it destroys text.
        for src in 1..=64u32 {
            for dst in 1..=src {
                let axis = Axis::new(src, dst).expect("valid");
                let mut seen = vec![false; src as usize];
                for i in 0..dst {
                    let (start, w) = axis.span(i).expect("span in range");
                    for k in 0..w.len() {
                        if let Some(slot) = seen.get_mut(start as usize + k) {
                            *slot = true;
                        }
                    }
                }
                assert!(
                    seen.iter().all(|&s| s),
                    "{src}->{dst} ignores a source sample"
                );
            }
        }
    }

    #[test]
    #[cfg_attr(miri, ignore = "an exhaustive sweep: minutes under MIRI")]
    fn spans_advance_and_stay_in_range() {
        for src in 1..=64u32 {
            for dst in 1..=src {
                let axis = Axis::new(src, dst).expect("valid");
                let mut last_start = 0;
                for i in 0..dst {
                    let (start, w) = axis.span(i).expect("span in range");
                    assert!(!w.is_empty(), "{src}->{dst} sample {i} covers nothing");
                    assert!(start >= last_start, "{src}->{dst} span went backwards");
                    assert!(
                        start as usize + w.len() <= src as usize,
                        "{src}->{dst} sample {i} reads past the source"
                    );
                    last_start = start;
                }
                assert!(axis.span(dst).is_none(), "a span past the end exists");
            }
        }
    }

    #[test]
    fn a_four_to_three_reduction_has_the_weights_the_arithmetic_says() {
        let axis = Axis::new(4, 3).expect("valid");
        let three_quarters = ONE * 3 / 4;
        let quarter = ONE / 4;
        let half = ONE / 2;
        assert_eq!(axis.span(0), Some((0, &[three_quarters, quarter][..])));
        assert_eq!(axis.span(1), Some((1, &[half, half][..])));
        assert_eq!(axis.span(2), Some((2, &[quarter, three_quarters][..])));
    }

    #[test]
    fn an_identity_axis_copies() {
        let axis = Axis::new(17, 17).expect("valid");
        assert!(axis.is_identity());
        for i in 0..17 {
            assert_eq!(axis.span(i), Some((i, &[ONE][..])));
        }
    }

    #[test]
    fn halving_averages_pairs() {
        let axis = Axis::new(8, 4).expect("valid");
        let half = ONE / 2;
        for i in 0..4 {
            assert_eq!(axis.span(i), Some((i * 2, &[half, half][..])));
        }
    }

    #[test]
    fn upscaling_and_nothing_are_refused() {
        assert_eq!(Axis::new(4, 5), Err(AxisError::Upscale { src: 4, dst: 5 }));
        assert_eq!(Axis::new(0, 0), Err(AxisError::Empty(0)));
        assert_eq!(Axis::new(4, 0), Err(AxisError::Empty(0)));
    }

    #[test]
    #[cfg_attr(miri, ignore = "an exhaustive sweep: minutes under MIRI")]
    fn dst_range_never_misses_a_destination_that_reads_the_source() {
        // The property that stops stale patches. If a destination sample
        // averages a source sample that changed, and this range leaves it out,
        // that sample keeps the previous frame's value and never repaints.
        // Checked exhaustively rather than argued, because a range that is
        // right for almost every rectangle looks like a working system.
        for src in 1..=48u32 {
            for dst in 1..=src {
                let axis = Axis::new(src, dst).expect("valid");
                for start in 0..src {
                    for len in 1..=(src - start) {
                        let end = start + len;
                        let mut wanted = Vec::new();
                        for i in 0..dst {
                            let (s, w) = axis.span(i).expect("span in range");
                            let e = s + u32::try_from(w.len()).unwrap_or(0);
                            if s < end && e > start {
                                wanted.push(i);
                            }
                        }
                        let got = axis.dst_range(start, len);
                        let (lo, count) = got.expect("a non-empty range overlaps something");
                        let hi = lo + count;
                        for i in wanted {
                            assert!(
                                i >= lo && i < hi,
                                "{src}->{dst} source [{start},{end}) misses destination {i}, \
                                 range was [{lo},{hi})"
                            );
                        }
                        assert!(hi <= dst, "{src}->{dst} range runs past the destination");
                    }
                }
            }
        }
    }

    #[test]
    #[cfg_attr(miri, ignore = "an exhaustive sweep: minutes under MIRI")]
    fn dst_range_is_not_wastefully_wide() {
        // Conservative is required; unbounded is not. A range that always
        // returned the whole axis would pass the test above and make damage
        // tracking pointless, so the width is held to the overlap plus a
        // sample at each edge.
        for src in 2..=48u32 {
            for dst in 1..=src {
                let axis = Axis::new(src, dst).expect("valid");
                for start in 0..src {
                    for len in 1..=(src - start) {
                        let (_, count) = axis.dst_range(start, len).expect("overlaps");
                        let ideal = (u64::from(len) * u64::from(dst)).div_ceil(u64::from(src));
                        let allowed = u32::try_from(ideal).unwrap_or(dst) + 2;
                        assert!(
                            count <= allowed.min(dst),
                            "{src}->{dst} source [{start},{}) returned {count} destinations, \
                             more than {allowed}",
                            start + len
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn dst_range_declines_what_is_outside() {
        let axis = Axis::new(10, 5).expect("valid");
        assert_eq!(axis.dst_range(0, 0), None);
        assert_eq!(axis.dst_range(10, 1), None);
        assert_eq!(axis.dst_range(11, 4), None);
        // A range running off the end is clamped, not refused.
        assert!(axis.dst_range(8, 99).is_some());
    }

    #[test]
    #[cfg_attr(miri, ignore = "an exhaustive sweep: minutes under MIRI")]
    fn the_whole_source_maps_to_the_whole_destination() {
        for src in 1..=64u32 {
            for dst in 1..=src {
                let axis = Axis::new(src, dst).expect("valid");
                assert_eq!(
                    axis.dst_range(0, src),
                    Some((0, dst)),
                    "{src}->{dst} does not map its own extent"
                );
            }
        }
    }
}
