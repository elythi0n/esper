//! Fuzz the damage region decomposition.
//!
//! Four contracts, and the interesting ones are not "does not crash":
//!
//! 1. **Arbitrary rectangles must return, never panic and never hang.** The
//!    reduction that enforces the region cap is a loop with two different
//!    moves, and a loop like that is exactly where a non-terminating case
//!    hides. Overflow checks are on, and the inputs include values at the
//!    limits of `i32` and `u32`.
//!
//! 2. **The result never loses a pixel**, and covers exactly the input
//!    whenever the region cap did not have to bind.
//!
//!    Losing one leaves a stale patch on a viewer's screen, which is the one
//!    failure a damage system must not have. Covering more is wasted encoding,
//!    and is allowed only as the deliberate price of the cap — enforcing it
//!    collapses bands to their hulls, which grows coverage on purpose.
//!
//!    The cap cannot bind on four rectangles or fewer: `n` rectangles give at
//!    most `2n - 1` bands of at most `n` spans each, and `4 * 7` is under the
//!    cap of 32. So exactness is required there and superset everywhere.
//!
//!    Checked against a bitmap, so the oracle shares no logic with the code.
//!
//!    This distinction is here because the first version of this target
//!    demanded exactness unconditionally and the fuzzer found a seventeen
//!    rectangle input where the cap bound. The code was right; the contract
//!    was written too strongly.
//!
//! 3. **No two regions overlap**, because callers add their areas together to
//!    decide how much of the screen changed.
//!
//! 4. **The region count stays bounded** however many rectangles arrive: the
//!    bookkeeping must not be driven by whatever the compositor happens to do.
//!
//! Run locally:
//!     cargo +nightly fuzz run damage_coalesce

#![no_main]

use libfuzzer_sys::fuzz_target;
use ownview_capture::damage::{Damage, MAX_REGIONS, Regions};
use ownview_capture::geometry::Rect;

/// A small frame, so the bitmap oracle is cheap and the region cap is
/// reachable with a handful of rectangles.
const W: i64 = 48;
const H: i64 = 32;

/// Rasterise rectangles into changed pixels.
fn raster(rects: &[Rect]) -> Vec<bool> {
    let mut grid = vec![false; (W * H) as usize];
    for r in rects {
        for y in i64::from(r.y).max(0)..r.bottom().min(H) {
            for x in i64::from(r.x).max(0)..r.right().min(W) {
                if let Some(cell) = grid.get_mut((y * W + x) as usize) {
                    *cell = true;
                }
            }
        }
    }
    grid
}

/// Six bytes per rectangle: two signed coordinates, two sizes, with the ranges
/// stretched past the frame so out-of-bounds damage is exercised too.
fn rects_from(data: &[u8]) -> Vec<Rect> {
    data.chunks_exact(6)
        .map(|c| {
            let g = |i: usize| c.get(i).copied().unwrap_or(0);
            Rect::new(
                i32::from(g(0) as i8) - 8,
                i32::from(g(1) as i8) - 8,
                u32::from(g(2)) % 80,
                u32::from(g(3)) % 60,
            )
        })
        .collect()
}

fuzz_target!(|data: &[u8]| {
    let frame = Rect::sized(W as u32, H as u32);
    let raw = rects_from(data);
    if raw.is_empty() {
        return;
    }

    let want = raster(&raw);
    let got = Regions::coalesce(&raw, &frame);

    match &got {
        Damage::Nothing => {
            assert!(
                want.iter().all(|c| !*c),
                "reported no change while pixels changed"
            );
        }
        // The deliberate "a lot of it changed" answer, which is allowed to
        // cover more than changed but never less.
        Damage::Everything => {
            let covered = raster(&got.rects(&frame));
            for (i, w) in want.iter().enumerate() {
                assert!(
                    !*w || covered.get(i).copied().unwrap_or(false),
                    "a whole frame failed to cover pixel {i}"
                );
            }
        }
        Damage::Regions(r) => {
            let covered = raster(r.as_slice());
            for (i, w) in want.iter().enumerate() {
                assert!(
                    !*w || covered.get(i).copied().unwrap_or(false),
                    "pixel {i} changed but is not covered — a stale patch \
                     would be left on the viewer's screen"
                );
            }
            // Below the cap, nothing may be covered that did not change.
            if raw.len() <= 4 {
                assert_eq!(
                    covered, want,
                    "with the cap unable to bind, the decomposition must be exact"
                );
            }
            assert!(
                r.len() <= MAX_REGIONS,
                "{} regions survived a cap of {MAX_REGIONS}",
                r.len()
            );
            assert_eq!(
                r.area(),
                r.as_slice().iter().map(Rect::area).sum::<u64>(),
                "the cached area disagrees with the regions"
            );

            let rs = r.as_slice();
            for (i, a) in rs.iter().enumerate() {
                assert!(!a.is_empty(), "an empty region survived");
                assert!(frame.contains(a), "{a:?} is outside the frame");
                for b in rs.iter().skip(i.saturating_add(1)) {
                    assert!(
                        !a.overlaps(b),
                        "{a:?} overlaps {b:?}, so areas cannot be added"
                    );
                }
            }
        }
    }
});
