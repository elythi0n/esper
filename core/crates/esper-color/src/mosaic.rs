//! Several pictures laid out as one, the way the screens they came from are.
//!
//! # Why the viewer composites rather than the host
//!
//! A host serving three screens sends three streams, and the alternative — one
//! stream of a picture the host has already stitched together — is worse in
//! three ways. It would encode the black gaps between screens that are not
//! aligned; it would make every screen's damage the whole mosaic's damage, so
//! a caret blinking on one would cost a frame of all of them; and it would
//! give the viewer no way to look at one screen at a time without asking the
//! host to reconfigure.
//!
//! So the host sends what it captures, one stream per screen, and this is
//! where they meet again. The layout comes from the geometry the host
//! advertised, which is the same geometry a pointer position is expressed
//! against — so a picture and a cursor end up in the same place by
//! construction rather than by two pieces of arithmetic agreeing.
//!
//! # Why the origins are snapped to even
//!
//! I420 carries one chroma sample per 2×2 block, so a picture placed at an odd
//! offset would have its chroma land half a block out — every colour shifted a
//! pixel from its luma, which looks like a badly tuned television. Desktop
//! monitors sit on even coordinates in practice and nothing guarantees it, so
//! the origin is rounded down to even and the picture moves at most one pixel
//! from where it was said to be. That is invisible; misaligned chroma is not.

use crate::I420;

/// Where one picture goes in the mosaic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct At {
    pub x: i32,
    pub y: i32,
}

/// The size that holds every screen, and the origin to place them against.
///
/// Monitor coordinates can be negative — a screen to the left of the primary
/// one starts at a negative x — so the mosaic's origin is the smallest corner
/// rather than zero, and every placement is relative to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bounds {
    pub left: i32,
    pub top: i32,
    pub width: u32,
    pub height: u32,
}

impl Bounds {
    /// The box that holds all of them.
    ///
    /// `None` when there is nothing to hold, which a caller must treat as "no
    /// mosaic" rather than as a zero-sized one: an I420 of no pixels is not a
    /// picture and every buffer downstream is sized from this.
    #[must_use]
    pub fn around(screens: &[(At, u32, u32)]) -> Option<Self> {
        let mut it = screens.iter();
        let (first, fw, fh) = it.next().copied()?;
        let mut left = first.x;
        let mut top = first.y;
        let mut right = first
            .x
            .saturating_add(i32::try_from(fw).unwrap_or(i32::MAX));
        let mut bottom = first
            .y
            .saturating_add(i32::try_from(fh).unwrap_or(i32::MAX));
        for (at, w, h) in it {
            left = left.min(at.x);
            top = top.min(at.y);
            right = right.max(at.x.saturating_add(i32::try_from(*w).unwrap_or(i32::MAX)));
            bottom = bottom.max(at.y.saturating_add(i32::try_from(*h).unwrap_or(i32::MAX)));
        }
        // Snapped out to even, so every screen's offset from the origin stays
        // even after the origin moves — rounding the origin down and each
        // placement down separately would put a screen half a chroma block
        // out depending on where the origin happened to land.
        let left = left & !1;
        let top = top & !1;
        Some(Self {
            left,
            top,
            width: u32::try_from(right.saturating_sub(left)).unwrap_or(0),
            height: u32::try_from(bottom.saturating_sub(top)).unwrap_or(0),
        })
    }
}

/// Copy `src` into `dst` with its top-left at `at`, relative to `bounds`.
///
/// Anything falling outside `dst` is clipped rather than refused: a host that
/// re-advertised its layout between a picture being sent and being composited
/// would otherwise turn a stale frame into an error, and a stale frame in the
/// wrong place for one refresh is better than a viewer that stops.
pub fn place(dst: &mut I420, bounds: Bounds, at: At, src: &I420) {
    let x = at.x.saturating_sub(bounds.left).max(0) as u32 & !1;
    let y = at.y.saturating_sub(bounds.top).max(0) as u32 & !1;
    if x >= dst.width() || y >= dst.height() {
        return;
    }
    let cols = src.width().min(dst.width().saturating_sub(x));
    let rows = src.height().min(dst.height().saturating_sub(y));
    let (src_y_stride, src_uv_stride) = (src.y_stride(), src.uv_stride());
    let (dst_y_stride, dst_uv_stride) = (dst.y_stride(), dst.uv_stride());
    let (sy, su, sv) = (src.y(), src.u(), src.v());
    let (dy, du, dv) = dst.planes_mut();

    for row in 0..rows as usize {
        let from = row.saturating_mul(src_y_stride);
        let to = (y as usize)
            .saturating_add(row)
            .saturating_mul(dst_y_stride)
            .saturating_add(x as usize);
        copy_row(sy, from, dy, to, cols as usize);
    }
    // Chroma is half the size in both directions, and the offsets are even by
    // construction above, so this is the same arithmetic halved rather than a
    // second rounding decision.
    let (cx, cy) = (x as usize / 2, y as usize / 2);
    let (ccols, crows) = (cols.div_ceil(2) as usize, rows.div_ceil(2) as usize);
    for row in 0..crows {
        let from = row.saturating_mul(src_uv_stride);
        let to = cy
            .saturating_add(row)
            .saturating_mul(dst_uv_stride)
            .saturating_add(cx);
        copy_row(su, from, du, to, ccols);
        copy_row(sv, from, dv, to, ccols);
    }
}

/// One row, as far as both sides can go.
fn copy_row(src: &[u8], from: usize, dst: &mut [u8], to: usize, len: usize) {
    let Some(taken) = src.get(from..from.saturating_add(len)) else {
        return;
    };
    let Some(slot) = dst.get_mut(to..to.saturating_add(len)) else {
        return;
    };
    slot.copy_from_slice(taken);
}

/// Fill a mosaic with the colour of a screen that is not there.
///
/// **Black rather than left alone.** A mosaic is as large as the box round
/// every screen, and screens that are not rectangular together leave gaps
/// nothing writes — so a buffer reused between frames would show whatever was
/// in the gap last time, which is a picture of somewhere else.
pub fn clear(dst: &mut I420) {
    let (y, u, v) = dst.planes_mut();
    y.fill(16);
    u.fill(128);
    v.fill(128);
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    /// A picture of one solid colour, so where it lands is visible in both
    /// planes.
    fn solid(w: u32, h: u32, y: u8, u: u8, v: u8) -> I420 {
        let mut p = I420::new(w, h).expect("a picture");
        let (py, pu, pv) = p.planes_mut();
        py.fill(y);
        pu.fill(u);
        pv.fill(v);
        p
    }

    /// **The box round every screen, including the ones left of the primary.**
    ///
    /// A monitor to the left starts at a negative x, so a mosaic anchored at
    /// zero would put it off the edge of its own buffer. The origin is the
    /// smallest corner.
    #[test]
    fn the_bounds_hold_every_screen_wherever_it_is() {
        let screens = [
            (At { x: 0, y: 0 }, 1920, 1080),
            (At { x: -1280, y: 100 }, 1280, 720),
            (At { x: 1920, y: -200 }, 800, 600),
        ];
        let b = Bounds::around(&screens).expect("three screens");
        assert_eq!(b.left, -1280);
        assert_eq!(b.top, -200);
        assert_eq!(b.width, 1280 + 1920 + 800);
        assert_eq!(b.height, 1080 + 200);
        assert_eq!(Bounds::around(&[]), None, "nothing to hold is not a mosaic");
    }

    /// **Chroma lands with its luma.**
    ///
    /// The reason origins are snapped to even: I420 carries one chroma sample
    /// per 2×2 block, so a picture placed at an odd offset has every colour a
    /// pixel away from its brightness — which looks like a badly tuned
    /// television and is very hard to attribute to a compositor.
    #[test]
    fn a_placed_picture_keeps_its_colour_with_its_brightness() {
        let bounds = Bounds {
            left: 0,
            top: 0,
            width: 64,
            height: 64,
        };
        let mut dst = I420::new(bounds.width, bounds.height).expect("a mosaic");
        clear(&mut dst);
        let src = solid(16, 16, 200, 60, 240);
        place(&mut dst, bounds, At { x: 32, y: 16 }, &src);

        for (x, y) in [(32, 16), (33, 16), (32, 17), (47, 31), (46, 30)] {
            assert_eq!(dst.luma_at(x, y), Some(200), "luma at {x},{y}");
            assert_eq!(dst.chroma_at(x, y), Some((60, 240)), "chroma at {x},{y}");
        }
        // And nothing outside it moved.
        assert_eq!(dst.luma_at(31, 16), Some(16), "the pixel to its left");
        assert_eq!(dst.chroma_at(30, 16), Some((128, 128)));
        assert_eq!(dst.luma_at(48, 16), Some(16), "the pixel to its right");
    }

    /// Two screens side by side, and neither writes into the other.
    #[test]
    fn screens_side_by_side_do_not_overlap() {
        let screens = [(At { x: 0, y: 0 }, 32, 32), (At { x: 32, y: 0 }, 32, 32)];
        let bounds = Bounds::around(&screens).expect("two screens");
        assert_eq!((bounds.width, bounds.height), (64, 32));
        let mut dst = I420::new(bounds.width, bounds.height).expect("a mosaic");
        clear(&mut dst);
        place(&mut dst, bounds, screens[0].0, &solid(32, 32, 80, 10, 20));
        place(
            &mut dst,
            bounds,
            screens[1].0,
            &solid(32, 32, 180, 200, 210),
        );
        assert_eq!(dst.luma_at(0, 0), Some(80));
        assert_eq!(dst.luma_at(31, 31), Some(80));
        assert_eq!(dst.luma_at(32, 0), Some(180));
        assert_eq!(dst.luma_at(63, 31), Some(180));
        assert_eq!(dst.chroma_at(30, 30), Some((10, 20)));
        assert_eq!(dst.chroma_at(34, 30), Some((200, 210)));
    }

    /// **An odd origin moves the picture, it does not shear its colour.**
    ///
    /// A screen advertised at an odd coordinate is placed at the even one
    /// below it — at most one pixel from where it was said to be, which is
    /// invisible, where misaligned chroma is not.
    #[test]
    fn an_odd_origin_is_snapped_rather_than_honoured() {
        let bounds = Bounds {
            left: 0,
            top: 0,
            width: 32,
            height: 32,
        };
        let mut dst = I420::new(32, 32).expect("a mosaic");
        clear(&mut dst);
        place(
            &mut dst,
            bounds,
            At { x: 7, y: 9 },
            &solid(8, 8, 111, 33, 44),
        );
        assert_eq!(dst.luma_at(6, 8), Some(111), "snapped down to 6,8");
        assert_eq!(dst.chroma_at(6, 8), Some((33, 44)));
        assert_eq!(dst.luma_at(5, 8), Some(16), "and not before it");
    }

    /// A picture that hangs off the edge is clipped, not refused: a layout
    /// that changed between a frame being sent and composited must not stop
    /// the viewer.
    #[test]
    fn a_picture_hanging_off_the_edge_is_clipped() {
        let bounds = Bounds {
            left: 0,
            top: 0,
            width: 16,
            height: 16,
        };
        let mut dst = I420::new(16, 16).expect("a mosaic");
        clear(&mut dst);
        place(
            &mut dst,
            bounds,
            At { x: 8, y: 8 },
            &solid(32, 32, 99, 1, 2),
        );
        assert_eq!(dst.luma_at(15, 15), Some(99), "what fits is written");
        assert_eq!(dst.luma_at(7, 7), Some(16), "and nothing before it");
        // Entirely outside is a no-op rather than a panic.
        place(
            &mut dst,
            bounds,
            At { x: 999, y: 999 },
            &solid(4, 4, 1, 1, 1),
        );
        assert_eq!(dst.luma_at(15, 15), Some(99));
    }

    /// **A gap shows black, not the last frame that was there.**
    ///
    /// Screens that are not rectangular together leave parts of the mosaic
    /// nothing writes, and a buffer reused between frames would show whatever
    /// was in the gap before — a picture of somewhere else.
    #[test]
    fn a_gap_is_black_rather_than_whatever_was_there() {
        let mut dst = I420::new(8, 8).expect("a mosaic");
        let (y, u, v) = dst.planes_mut();
        y.fill(240);
        u.fill(9);
        v.fill(9);
        clear(&mut dst);
        assert_eq!(dst.luma_at(0, 0), Some(16), "black, in the limited range");
        assert_eq!(dst.chroma_at(0, 0), Some((128, 128)), "and no colour");
    }
}
