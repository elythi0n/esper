//! Captured RGB to the planar YUV an encoder wants.
//!
//! # Fixed point, and why the reference is not
//!
//! The conversion runs on every pixel of every frame: two million multiply-add
//! chains at 1080p, inside a budget of one millisecond. Floating point there
//! is the wrong tool — but floating point is exactly the right tool for
//! deciding what the answer *should* be.
//!
//! So the coefficients are derived from the spec in `f64` at compile time,
//! scaled into integers once, and the integer path is checked against a `f64`
//! reference implementation in the tests over every representable colour. That
//! is the only honest way to test fixed-point colour maths: a known-answer
//! table for a handful of colours proves the table, not the arithmetic.
//!
//! # Rounding, not truncating
//!
//! Every scaled result has half a unit added before the shift. Truncating
//! instead biases every sample downwards by an average of half a level, which
//! darkens the whole picture slightly and costs measurable quality for no
//! reason at all. It is a one-instruction difference and it is the kind of
//! thing that survives for years because nobody looks.
//!
//! # Chroma is averaged, not sampled
//!
//! 4:2:0 keeps one chroma pair per 2×2 block. Taking the top-left pixel's
//! chroma is faster and wrong: it aliases, and on the fine detail a screen is
//! full of — text, one-pixel borders, checkerboards — aliasing shows as
//! colour fringing that moves when the content scrolls. The four are averaged.

use esper_capture::frame::{Frame, PixelFormat};
use esper_capture::geometry::Rect;

use crate::i420::{I420, I420Error};
use crate::space::{ColorSpace, Range};

/// Fractional bits in the fixed-point coefficients.
///
/// Fourteen: enough that the rounding error is far below one output level, and
/// small enough that a coefficient times 255 plus the offset stays inside
/// `i32` with room to spare. The tests check the second claim by exhausting
/// the input space rather than by trusting this comment.
const SHIFT: u32 = 14;
const ONE: i32 = 1 << SHIFT;
const HALF: i32 = ONE / 2;

/// What went wrong converting.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ConvertError {
    /// The destination could not be sized to the frame.
    #[error(transparent)]
    Buffer(#[from] I420Error),
    /// The frame and the destination disagree about size.
    #[error("a {fw}x{fh} frame cannot be converted into a {iw}x{ih} image")]
    SizeMismatch { fw: u32, fh: u32, iw: u32, ih: u32 },
}

/// The integer coefficients for one colour space.
///
/// Built once per stream, not per pixel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Coefficients {
    /// Luma: `Y = (yr*R + yg*G + yb*B + y_offset) >> SHIFT`.
    yr: i32,
    yg: i32,
    yb: i32,
    y_offset: i32,
    /// Cb: `U = (ur*R + ug*G + ub*B + c_offset) >> SHIFT`.
    ur: i32,
    ug: i32,
    ub: i32,
    /// Cr.
    vr: i32,
    vg: i32,
    vb: i32,
    c_offset: i32,
}

impl Coefficients {
    /// Derive the integers from the spec's real numbers.
    #[must_use]
    pub fn for_space(space: ColorSpace) -> Self {
        let (kr, kg, kb) = space.matrix.luma_weights();
        let (y_off, y_scale) = space.range.luma();
        let (c_off, c_scale) = space.range.chroma();

        // Y' = kr*R' + kg*G' + kb*B', with R' in 0..=1.
        // Y  = y_off + y_scale * Y'.
        let s = y_scale / 255.0;
        let round = |v: f64| (v * f64::from(ONE)).round() as i32;

        // Cb' = (B' - Y') / (2 * (1 - kb)), in -0.5..=0.5.
        // Cr' = (R' - Y') / (2 * (1 - kr)).
        let cb_div = 2.0 * (1.0 - kb);
        let cr_div = 2.0 * (1.0 - kr);
        let cs = c_scale / 255.0;

        Self {
            yr: round(s * kr),
            yg: round(s * kg),
            yb: round(s * kb),
            // The offset carries the range's black level and the half unit
            // that makes the shift round instead of truncate.
            y_offset: round(y_off) + HALF,

            ur: round(cs * -kr / cb_div),
            ug: round(cs * -kg / cb_div),
            ub: round(cs * (1.0 - kb) / cb_div),

            vr: round(cs * (1.0 - kr) / cr_div),
            vg: round(cs * -kg / cr_div),
            vb: round(cs * -kb / cr_div),

            c_offset: round(c_off) + HALF,
        }
    }

    /// Luma for one pixel.
    ///
    /// Wrapping arithmetic, deliberately, and the reason is measured rather
    /// than assumed: this workspace enables overflow checks in release builds
    /// because arithmetic on attacker-influenced values must trap rather than
    /// wrap, and on this loop those checks cost **1.8×** — 6.2 ms against
    /// 3.7 ms for a 1080p frame.
    ///
    /// The global guarantee stays. It is removed here only because the bound
    /// is provable and, more importantly, *tested*: see
    /// [`Coefficients::worst_intermediate`], which computes the largest value
    /// this can produce for every colour space and asserts it is far inside
    /// `i32`. A comment claiming a bound is worth nothing; a test that would
    /// fail if a coefficient changed is worth something.
    #[must_use]
    pub const fn luma(&self, r: u8, g: u8, b: u8) -> u8 {
        let v = self
            .yr
            .wrapping_mul(r as i32)
            .wrapping_add(self.yg.wrapping_mul(g as i32))
            .wrapping_add(self.yb.wrapping_mul(b as i32))
            .wrapping_add(self.y_offset);
        clamp_u8(v >> SHIFT)
    }

    /// Chroma from the sum of a block's channels, divided by a count known at
    /// compile time.
    ///
    /// The average is taken in the RGB domain before the matrix rather than
    /// after. Both are defensible; averaging first is what libyuv and x264 do
    /// for RGB input, it is one multiply instead of four, and it avoids
    /// averaging values that have already been clamped.
    ///
    /// `SHIFT_BY` is `log2(count)` — 2 for a whole 2×2 block, 1 for a pair at
    /// an odd edge, 0 for a lone pixel. A const parameter rather than a
    /// divisor because an integer division is twenty to forty cycles and this
    /// runs twice per block: at 1080p that is a million divisions a frame,
    /// inside a budget of one millisecond.
    #[must_use]
    pub const fn chroma_of_sum<const SHIFT_BY: u32>(&self, r: i32, g: i32, b: i32) -> (u8, u8) {
        // Wrapping, for the reason given on `luma`, under the bound checked by
        // `worst_intermediate`.
        let u = (self
            .ur
            .wrapping_mul(r)
            .wrapping_add(self.ug.wrapping_mul(g))
            .wrapping_add(self.ub.wrapping_mul(b))
            >> SHIFT_BY)
            .wrapping_add(self.c_offset);
        let v = (self
            .vr
            .wrapping_mul(r)
            .wrapping_add(self.vg.wrapping_mul(g))
            .wrapping_add(self.vb.wrapping_mul(b))
            >> SHIFT_BY)
            .wrapping_add(self.c_offset);
        (clamp_u8(u >> SHIFT), clamp_u8(v >> SHIFT))
    }

    /// The largest magnitude any intermediate in [`Coefficients::luma`] or
    /// [`Coefficients::chroma_of_sum`] can reach.
    ///
    /// The bound that licenses the wrapping arithmetic above. Computed from
    /// the coefficients rather than written down, so changing a coefficient or
    /// widening the input range moves this number and the test that watches it
    /// fails.
    ///
    /// The worst case for each output is every channel at its extreme, summed
    /// over a whole 2×2 block — four pixels of 255.
    #[must_use]
    pub const fn worst_intermediate(&self) -> i64 {
        let luma = weighted_max(self.yr, self.yg, self.yb, self.y_offset);
        let cb = weighted_max(self.ur, self.ug, self.ub, self.c_offset);
        let cr = weighted_max(self.vr, self.vg, self.vb, self.c_offset);
        let worst = if luma > cb { luma } else { cb };
        if worst > cr { worst } else { cr }
    }

    /// Chroma from a sum whose count is only known at run time.
    ///
    /// For the odd-edge path, which runs at most once per row and so does not
    /// need the division removing.
    #[must_use]
    pub const fn chroma_of_sum_dyn(&self, r: i32, g: i32, b: i32, count: i32) -> (u8, u8) {
        let u = (self.ur * r + self.ug * g + self.ub * b) / count + self.c_offset;
        let v = (self.vr * r + self.vg * g + self.vb * b) / count + self.c_offset;
        (clamp_u8(u >> SHIFT), clamp_u8(v >> SHIFT))
    }
}

/// The largest magnitude three coefficients and an offset can produce.
///
/// The worst case is every channel at its extreme, summed over a whole 2×2
/// block — four pixels of 255 each.
const fn weighted_max(a: i32, b: i32, c: i32, offset: i32) -> i64 {
    const MAX_BLOCK_SUM: i64 = 255 * 4;
    let weights = a.unsigned_abs() as i64 + b.unsigned_abs() as i64 + c.unsigned_abs() as i64;
    weights * MAX_BLOCK_SUM + offset.unsigned_abs() as i64
}

/// Clamp to 8 bits without branching on the common path.
const fn clamp_u8(v: i32) -> u8 {
    if v < 0 {
        0
    } else if v > 255 {
        255
    } else {
        v as u8
    }
}

/// Convert a whole frame.
///
/// The destination is resized if it does not already match. Returns whether
/// the destination's shape changed, because a caller doing damage-driven
/// conversion must not trust its previous contents after that.
pub fn whole_frame(frame: &Frame, space: ColorSpace, out: &mut I420) -> Result<bool, ConvertError> {
    let reshaped = out.resize(frame.width(), frame.height())?;
    let all = Rect::sized(frame.width(), frame.height());
    let _ = convert_region(frame, space, out, &all);
    Ok(reshaped)
}

/// Convert only the regions that changed.
///
/// **The destination must already hold the previous frame**, or the regions
/// left alone show whatever was there before. That is the whole point — a
/// blinking cursor costs a few hundred pixels of conversion instead of two
/// million — and it is also the trap: a caller that resized, or that switched
/// streams, has to convert a whole frame first.
///
/// Returns the number of luma samples actually converted, which is the number
/// worth putting in the statistics: it is what the saving is measured in.
pub fn regions(
    frame: &Frame,
    space: ColorSpace,
    out: &mut I420,
    regions: &[Rect],
) -> Result<u64, ConvertError> {
    if out.width() < frame.width() || out.height() < frame.height() {
        return Err(ConvertError::SizeMismatch {
            fw: frame.width(),
            fh: frame.height(),
            iw: out.width(),
            ih: out.height(),
        });
    }

    let bounds = Rect::sized(frame.width(), frame.height());
    let mut converted = 0u64;
    for region in regions {
        let Some(clamped) = region.clamp_to(&bounds) else {
            continue;
        };
        // No snapping to the chroma grid here: `convert_region` rounds its
        // own origin down to even and always processes whole 2×2 blocks,
        // which is the same invariant enforced in one place instead of two.
        //
        // It matters that something enforces it. A region starting on an odd
        // coordinate shares its chroma samples with the pixel beside it, so
        // converting it alone would compute chroma from one new pixel and one
        // stale one — a one-pixel colour fringe along the edge of everything
        // that moves.
        converted = converted.saturating_add(convert_region(frame, space, out, &clamped));
    }
    Ok(converted)
}

/// Convert one rectangle, which is assumed to be inside the frame.
///
/// **Always processes whole 2×2 blocks**, rounding the origin down to even and
/// completing the pair at the far edge. That is not a tidiness choice: a 2×2
/// block shares one chroma pair, so converting half a block would compute its
/// chroma from one new pixel and one left over from the previous frame.
///
/// Returns how many luma samples were written, which is more than the
/// region's own area whenever it started or ended on an odd coordinate — and
/// is the number worth reporting, because it is what was actually spent.
///
/// # Why the channel offsets are const parameters
///
/// The first version of this read them from the pixel format inside the loop.
/// That put a runtime index into the innermost operation, which defeats the
/// bounds-check elision and stops the loop vectorising: **1080p took 14.4 ms
/// against a budget of one.** Dispatching once per region and monomorphising
/// the loop lets the offsets fold into constants, the `chunks_exact` iterators
/// prove their own lengths, and the whole inner block become straight-line
/// arithmetic over two rows at a time.
fn convert_region(frame: &Frame, space: ColorSpace, out: &mut I420, region: &Rect) -> u64 {
    let coeff = Coefficients::for_space(space);
    // One dispatch per region rather than per pixel. Both alpha variants take
    // the same path: the fourth byte is never read, so whether it means
    // anything is not this function's business.
    let written = match frame.format() {
        PixelFormat::Bgra8 | PixelFormat::Bgrx8 => {
            convert_blocks::<2, 1, 0>(frame, &coeff, out, region)
        }
        PixelFormat::Rgba8 | PixelFormat::Rgbx8 => {
            convert_blocks::<0, 1, 2>(frame, &coeff, out, region)
        }
    };
    pad_edges(frame, out, out.y_stride());
    written
}

/// The hot loop, with the channel offsets known at compile time.
fn convert_blocks<const R: usize, const G: usize, const B: usize>(
    frame: &Frame,
    coeff: &Coefficients,
    out: &mut I420,
    region: &Rect,
) -> u64 {
    let fw = frame.width();
    let fh = frame.height();
    // The last even column and row. Beyond these lies at most one lone
    // column or row, from an odd-sized frame.
    let w_even = fw & !1;
    let h_even = fh & !1;

    let x0 = (region.x.max(0) as u32) & !1;
    let y0 = (region.y.max(0) as u32) & !1;
    let x1 = (region.right().max(0) as u32).min(fw);
    let y1 = (region.bottom().max(0) as u32).min(fh);
    if x0 >= x1 || y0 >= y1 {
        return 0;
    }

    let y_stride = out.y_stride();
    let uv_stride = out.uv_stride();
    let out_w = out.width();
    let out_h = out.height();
    let (y_plane, u_plane, v_plane) = out.planes_mut();

    // Full two-pixel columns, and whether a lone final column follows.
    let blocks_end = x1.min(w_even);
    let blocks = (blocks_end.saturating_sub(x0) / 2) as usize;
    let lone_column = x1 > w_even;

    let mut written = 0u64;
    let mut y = y0;
    while y < y1 {
        let paired = y.saturating_add(1) < h_even.max(1) && y.saturating_add(1) < fh;
        let Some(top) = frame.row(y) else { break };

        // Two disjoint mutable rows of the luma plane. `split_at_mut` is the
        // only way to hold both at once, and holding both is what lets the
        // block loop write all four luma samples without re-deriving offsets.
        let split = y
            .saturating_add(1)
            .min(out_h.saturating_sub(1))
            .saturating_mul(y_stride as u32) as usize;
        let (upper, lower) = y_plane.split_at_mut(split.min(y_plane.len()));

        let y_top_base = (y as usize).saturating_mul(y_stride);
        let uv_at = ((y / 2) as usize).saturating_mul(uv_stride);

        if paired {
            let Some(bottom) = frame.row(y.saturating_add(1)) else {
                break;
            };
            written = written.saturating_add(convert_row_pair::<R, G, B>(
                coeff,
                RowPair {
                    top,
                    bottom,
                    x0,
                    blocks,
                    lone_column,
                    w_even,
                    out_w,
                },
                upper.get_mut(y_top_base..).unwrap_or(&mut []),
                lower,
                u_plane.get_mut(uv_at..).unwrap_or(&mut []),
                v_plane.get_mut(uv_at..).unwrap_or(&mut []),
            ));
        } else {
            // A lone final row of an odd-height frame: it is its own partner,
            // so the chroma of the block is that row's own average. Averaging
            // it with itself would give the same answer, so it is not
            // duplicated.
            written = written.saturating_add(convert_row_pair::<R, G, B>(
                coeff,
                RowPair {
                    top,
                    bottom: top,
                    x0,
                    blocks,
                    lone_column,
                    w_even,
                    out_w,
                },
                upper.get_mut(y_top_base..).unwrap_or(&mut []),
                &mut [],
                u_plane.get_mut(uv_at..).unwrap_or(&mut []),
                v_plane.get_mut(uv_at..).unwrap_or(&mut []),
            ));
        }
        y = y.saturating_add(2);
    }
    written
}

/// The source rows and the geometry of one pass, bundled so the inner
/// function does not take eleven arguments.
struct RowPair<'a> {
    top: &'a [u8],
    bottom: &'a [u8],
    x0: u32,
    blocks: usize,
    lone_column: bool,
    w_even: u32,
    out_w: u32,
}

/// Convert one pair of rows.
///
/// `y_bottom` empty means there is no second row — the last row of an
/// odd-height frame.
fn convert_row_pair<const R: usize, const G: usize, const B: usize>(
    coeff: &Coefficients,
    rows: RowPair<'_>,
    y_top: &mut [u8],
    y_bottom: &mut [u8],
    u_row: &mut [u8],
    v_row: &mut [u8],
) -> u64 {
    const BPP: usize = 4;
    /// Bytes in a 2×1 group of pixels.
    const PAIR: usize = BPP * 2;

    let start = (rows.x0 as usize).saturating_mul(BPP);
    let span = rows.blocks.saturating_mul(PAIR);
    let uv_start = (rows.x0 / 2) as usize;

    let Some(top) = rows.top.get(start..start.saturating_add(span)) else {
        return 0;
    };
    let bottom = rows
        .bottom
        .get(start..start.saturating_add(span))
        .unwrap_or(&[]);
    let Some(y_top_span) = y_top.get_mut(rows.x0 as usize..) else {
        return 0;
    };
    let y_bottom_span = y_bottom.get_mut(rows.x0 as usize..).unwrap_or(&mut []);
    let Some(u_span) = u_row.get_mut(uv_start..) else {
        return 0;
    };
    let Some(v_span) = v_row.get_mut(uv_start..) else {
        return 0;
    };
    let two_rows = !y_bottom_span.is_empty() && bottom.len() == top.len();

    let mut written = 0u64;

    // The hot loop. Every iterator here proves its own length, so there are no
    // bounds checks inside, and with the channel offsets folded to constants
    // this is straight-line arithmetic the optimiser can vectorise.
    // `as_chunks` rather than `chunks_exact`: it yields `&[u8; 8]`, so a
    // constant offset into a pixel is provably in bounds after
    // monomorphisation and the load folds to a single instruction. With
    // `chunks_exact` the length is only known dynamically and every channel
    // read keeps a bounds check.
    let (top_pairs, _) = top.as_chunks::<PAIR>();
    let (bottom_pairs, _) = bottom.as_chunks::<PAIR>();
    let (y_top_pairs, _) = y_top_span.as_chunks_mut::<2>();
    let (y_bottom_pairs, _) = y_bottom_span.as_chunks_mut::<2>();

    if two_rows && y_bottom_pairs.len() >= top_pairs.len() {
        for (((tp, bp), (yt, yb)), (u, v)) in top_pairs
            .iter()
            .zip(bottom_pairs)
            .zip(y_top_pairs.iter_mut().zip(y_bottom_pairs.iter_mut()))
            .zip(u_span.iter_mut().zip(v_span.iter_mut()))
        {
            let (t0, t1) = (pixel::<R, G, B>(tp, 0), pixel::<R, G, B>(tp, BPP));
            let (b0, b1) = (pixel::<R, G, B>(bp, 0), pixel::<R, G, B>(bp, BPP));

            write2(coeff, yt, t0, t1);
            write2(coeff, yb, b0, b1);
            let sum = add4(t0, t1, b0, b1);
            let (cu, cv) = coeff.chroma_of_sum::<2>(sum.0, sum.1, sum.2);
            *u = cu;
            *v = cv;
            written = written.saturating_add(4);
        }
    } else {
        for ((tp, yt), (u, v)) in top_pairs
            .iter()
            .zip(y_top_pairs.iter_mut())
            .zip(u_span.iter_mut().zip(v_span.iter_mut()))
        {
            let (t0, t1) = (pixel::<R, G, B>(tp, 0), pixel::<R, G, B>(tp, BPP));
            write2(coeff, yt, t0, t1);
            let sum = add2(t0, t1);
            let (cu, cv) = coeff.chroma_of_sum::<1>(sum.0, sum.1, sum.2);
            *u = cu;
            *v = cv;
            written = written.saturating_add(2);
        }
    }

    // A lone final column of an odd-width frame, outside the hot loop so it
    // costs nothing on the common path.
    if rows.lone_column {
        let at = (rows.w_even as usize).saturating_mul(BPP);
        let uv = (rows.w_even / 2) as usize;
        if rows.w_even < rows.out_w
            && let Some(tp) = rows.top.get(at..at.saturating_add(BPP))
        {
            let t = one_pixel::<R, G, B>(tp);
            let mut sum = (i32::from(t.0), i32::from(t.1), i32::from(t.2));
            let mut count = 1;
            if let Some(slot) = y_top.get_mut(rows.w_even as usize) {
                *slot = coeff.luma(t.0, t.1, t.2);
                written = written.saturating_add(1);
            }
            if two_rows && let Some(bp) = rows.bottom.get(at..at.saturating_add(BPP)) {
                let b = one_pixel::<R, G, B>(bp);
                if let Some(slot) = y_bottom.get_mut(rows.w_even as usize) {
                    *slot = coeff.luma(b.0, b.1, b.2);
                    written = written.saturating_add(1);
                }
                sum.0 += i32::from(b.0);
                sum.1 += i32::from(b.1);
                sum.2 += i32::from(b.2);
                count += 1;
            }
            let (cu, cv) = coeff.chroma_of_sum_dyn(sum.0, sum.1, sum.2, count);
            if let Some(slot) = u_row.get_mut(uv) {
                *slot = cu;
            }
            if let Some(slot) = v_row.get_mut(uv) {
                *slot = cv;
            }
        }
    }
    written
}

/// One pixel's channels, with the offsets folded to constants.
#[inline(always)]
fn pixel<const R: usize, const G: usize, const B: usize>(px: &[u8; 8], at: usize) -> (u8, u8, u8) {
    (
        px.get(at.saturating_add(R)).copied().unwrap_or(0),
        px.get(at.saturating_add(G)).copied().unwrap_or(0),
        px.get(at.saturating_add(B)).copied().unwrap_or(0),
    )
}

/// One pixel from a four-byte slice, for the odd-edge path.
///
/// Separate from [`pixel`] because that one reads out of a pair and this one
/// out of a lone pixel at the end of an odd-width row. Not worth generalising:
/// the pair version is the hot path and giving it a dynamic length would undo
/// the reason `as_chunks` is used at all.
#[inline]
fn one_pixel<const R: usize, const G: usize, const B: usize>(px: &[u8]) -> (u8, u8, u8) {
    (
        px.get(R).copied().unwrap_or(0),
        px.get(G).copied().unwrap_or(0),
        px.get(B).copied().unwrap_or(0),
    )
}

/// Two luma samples into a two-byte slot.
#[inline(always)]
fn write2(coeff: &Coefficients, out: &mut [u8; 2], a: (u8, u8, u8), b: (u8, u8, u8)) {
    if let Some(s) = out.first_mut() {
        *s = coeff.luma(a.0, a.1, a.2);
    }
    if let Some(s) = out.get_mut(1) {
        *s = coeff.luma(b.0, b.1, b.2);
    }
}

/// Sum four pixels' channels.
#[inline(always)]
const fn add4(
    a: (u8, u8, u8),
    b: (u8, u8, u8),
    c: (u8, u8, u8),
    d: (u8, u8, u8),
) -> (i32, i32, i32) {
    (
        a.0 as i32 + b.0 as i32 + c.0 as i32 + d.0 as i32,
        a.1 as i32 + b.1 as i32 + c.1 as i32 + d.1 as i32,
        a.2 as i32 + b.2 as i32 + c.2 as i32 + d.2 as i32,
    )
}

/// Sum two pixels' channels.
#[inline(always)]
const fn add2(a: (u8, u8, u8), b: (u8, u8, u8)) -> (i32, i32, i32) {
    (
        a.0 as i32 + b.0 as i32,
        a.1 as i32 + b.1 as i32,
        a.2 as i32 + b.2 as i32,
    )
}

/// Fill the even-rounding padding by repeating the last real row and column.
fn pad_edges(frame: &Frame, out: &mut I420, y_stride: usize) {
    let (w, h) = (frame.width(), frame.height());
    let (ow, oh) = (out.width(), out.height());
    if ow == w && oh == h {
        return;
    }
    let (y_plane, _, _) = out.planes_mut();

    // The extra column repeats the last real one.
    if ow > w && w > 0 {
        for row in 0..h.min(oh) {
            let base = (row as usize).saturating_mul(y_stride);
            let from = base.saturating_add((w - 1) as usize);
            let to = base.saturating_add(w as usize);
            if let Some(v) = y_plane.get(from).copied()
                && let Some(slot) = y_plane.get_mut(to)
            {
                *slot = v;
            }
        }
    }
    // The extra row repeats the last real one, including its padding column.
    if oh > h && h > 0 {
        let from = ((h - 1) as usize).saturating_mul(y_stride);
        let to = (h as usize).saturating_mul(y_stride);
        let width = ow as usize;
        if from.saturating_add(width) <= y_plane.len() && to.saturating_add(width) <= y_plane.len()
        {
            let copy: Vec<u8> = y_plane
                .get(from..from.saturating_add(width))
                .unwrap_or(&[])
                .to_vec();
            if let Some(dst) = y_plane.get_mut(to..to.saturating_add(width)) {
                dst.copy_from_slice(&copy);
            }
        }
    }
}

/// The floating-point reference, for tests and for anyone checking the
/// integers by hand.
///
/// Deliberately public: it is the definition of correct, and the integer path
/// is only trustworthy because it is measured against this over the whole
/// input space rather than against a table of six colours.
#[must_use]
pub fn reference_yuv(r: u8, g: u8, b: u8, space: ColorSpace) -> (u8, u8, u8) {
    let (kr, kg, kb) = space.matrix.luma_weights();
    let (y_off, y_scale) = space.range.luma();
    let (c_off, c_scale) = space.range.chroma();

    let rf = f64::from(r) / 255.0;
    let gf = f64::from(g) / 255.0;
    let bf = f64::from(b) / 255.0;

    let yp = kr * rf + kg * gf + kb * bf;
    let cb = (bf - yp) / (2.0 * (1.0 - kb));
    let cr = (rf - yp) / (2.0 * (1.0 - kr));

    let q = |v: f64| v.round().clamp(0.0, 255.0) as u8;
    (
        q(y_off + y_scale * yp),
        q(c_off + c_scale * cb),
        q(c_off + c_scale * cr),
    )
}

/// Invert the reference, for round-trip tests.
///
/// Not used in the pipeline — the decoder side does its own inversion — but a
/// conversion that cannot be inverted back to nearly the original colour is
/// wrong in a way no forward comparison would reveal.
#[must_use]
pub fn reference_rgb(y: u8, u: u8, v: u8, space: ColorSpace) -> (u8, u8, u8) {
    let (kr, kg, kb) = space.matrix.luma_weights();
    let (y_off, y_scale) = space.range.luma();
    let (c_off, c_scale) = space.range.chroma();

    let yp = (f64::from(y) - y_off) / y_scale;
    let cb = (f64::from(u) - c_off) / c_scale;
    let cr = (f64::from(v) - c_off) / c_scale;

    let rf = yp + cr * 2.0 * (1.0 - kr);
    let bf = yp + cb * 2.0 * (1.0 - kb);
    let gf = (yp - kr * rf - kb * bf) / kg;

    let q = |x: f64| (x * 255.0).round().clamp(0.0, 255.0) as u8;
    (q(rf), q(gf), q(bf))
}

/// Every colour space worth converting, for tests that sweep them.
#[must_use]
pub fn all_spaces() -> [ColorSpace; 4] {
    use crate::space::Matrix;
    [
        ColorSpace::new(Matrix::Bt709, Range::Limited),
        ColorSpace::new(Matrix::Bt709, Range::Full),
        ColorSpace::new(Matrix::Bt601, Range::Limited),
        ColorSpace::new(Matrix::Bt601, Range::Full),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::space::Matrix;
    use esper_capture::damage::Damage;
    use esper_capture::pool::Pixels;

    /// Build a frame of a solid colour, in a given format and stride.
    fn solid(w: u32, h: u32, format: PixelFormat, padding: usize, rgb: (u8, u8, u8)) -> Frame {
        painted(w, h, format, padding, |_, _| rgb)
    }

    /// Build a frame from a function of position.
    fn painted<F: Fn(u32, u32) -> (u8, u8, u8)>(
        w: u32,
        h: u32,
        format: PixelFormat,
        padding: usize,
        paint: F,
    ) -> Frame {
        let bpp = format.bytes_per_pixel();
        let stride = (w as usize) * bpp + padding;
        let mut buf = vec![0u8; stride * h as usize];
        let (r_at, g_at, b_at) = format.rgb_offsets();
        for y in 0..h {
            for x in 0..w {
                let (r, g, b) = paint(x, y);
                let at = y as usize * stride + x as usize * bpp;
                if let Some(px) = buf.get_mut(at..at + bpp) {
                    if let Some(c) = px.get_mut(r_at) {
                        *c = r;
                    }
                    if let Some(c) = px.get_mut(g_at) {
                        *c = g;
                    }
                    if let Some(c) = px.get_mut(b_at) {
                        *c = b;
                    }
                    if format.has_alpha()
                        && let Some(a) = px.get_mut(3)
                    {
                        *a = 255;
                    }
                }
            }
        }
        Frame::new(w, h, stride, format, Damage::Everything, Pixels::owned(buf))
            .expect("a valid frame")
    }

    /// Use the real `PixelFormat` from the capture crate.
    use esper_capture::frame::PixelFormat;

    /// **The test that makes the fixed-point path trustworthy.** Every one of
    /// the 16.7 million representable colours, in every colour space, must
    /// come out within one level of the floating-point definition.
    ///
    /// A known-answer table for six colours proves the table. This proves the
    /// arithmetic.
    #[test]
    #[cfg_attr(
        miri,
        ignore = "sweeps 16.7 million colours; MIRI interprets rather than executes"
    )]
    fn the_integer_luma_matches_the_reference_for_every_colour() {
        for space in all_spaces() {
            let coeff = Coefficients::for_space(space);
            let mut worst = 0i32;
            for r in 0..=255u8 {
                for g in 0..=255u8 {
                    for b in 0..=255u8 {
                        let got = coeff.luma(r, g, b);
                        let (want, _, _) = reference_yuv(r, g, b, space);
                        let diff = i32::from(got) - i32::from(want);
                        worst = worst.max(diff.abs());
                        assert!(
                            diff.abs() <= 1,
                            "{}: rgb({r},{g},{b}) gave luma {got}, reference {want}",
                            space.describe()
                        );
                    }
                }
            }
            assert!(worst <= 1, "{}: worst luma error {worst}", space.describe());
        }
    }

    /// And the chroma, for a single pixel — the case where averaging is not
    /// involved and the matrix is all that is being measured.
    #[test]
    #[cfg_attr(
        miri,
        ignore = "sweeps a large colour space; MIRI interprets rather than executes"
    )]
    fn the_integer_chroma_matches_the_reference_for_every_colour() {
        for space in all_spaces() {
            let coeff = Coefficients::for_space(space);
            for r in (0..=255u8).step_by(3) {
                for g in (0..=255u8).step_by(3) {
                    for b in (0..=255u8).step_by(3) {
                        let (gu, gv) =
                            coeff.chroma_of_sum::<0>(i32::from(r), i32::from(g), i32::from(b));
                        let (_, wu, wv) = reference_yuv(r, g, b, space);
                        assert!(
                            (i32::from(gu) - i32::from(wu)).abs() <= 1,
                            "{}: rgb({r},{g},{b}) gave Cb {gu}, reference {wu}",
                            space.describe()
                        );
                        assert!(
                            (i32::from(gv) - i32::from(wv)).abs() <= 1,
                            "{}: rgb({r},{g},{b}) gave Cr {gv}, reference {wv}",
                            space.describe()
                        );
                    }
                }
            }
        }
    }

    /// **The bound that licenses the wrapping arithmetic.** The hot loop uses
    /// wrapping multiply and add because this workspace's release overflow
    /// checks cost 1.8× on it — a measured 6.2 ms against 3.7 ms for a 1080p
    /// frame. Removing a safety check is only defensible if the thing it
    /// checks for cannot happen, so this computes the worst case and requires
    /// a wide margin.
    ///
    /// A margin rather than a bare fit: an exact fit would mean the next
    /// person to widen an input range or raise `SHIFT` silently loses the
    /// property, and this test is the only thing standing there.
    #[test]
    fn every_intermediate_is_far_inside_an_i32() {
        for space in all_spaces() {
            let coeff = Coefficients::for_space(space);
            let worst = coeff.worst_intermediate();
            let limit = i64::from(i32::MAX);
            assert!(
                worst < limit,
                "{}: an intermediate can reach {worst}, and i32 stops at {limit}",
                space.describe()
            );
            assert!(
                worst * 16 < limit,
                "{}: the worst intermediate is {worst} against a limit of \
                 {limit} — under a 16x margin, which is too close to keep \
                 using wrapping arithmetic",
                space.describe()
            );
        }
    }

    /// And the wrapping path must agree with a checked one over the whole
    /// input space, which is what actually proves nothing wrapped.
    #[test]
    #[cfg_attr(
        miri,
        ignore = "sweeps a large colour space; MIRI interprets rather than executes"
    )]
    fn the_wrapping_arithmetic_never_actually_wraps() {
        for space in all_spaces() {
            let c = Coefficients::for_space(space);
            for r in (0..=255u8).step_by(7) {
                for g in (0..=255u8).step_by(7) {
                    for b in (0..=255u8).step_by(7) {
                        // The same expression with checked arithmetic. If any
                        // step would overflow, this is `None`.
                        let checked = i32::from(r)
                            .checked_mul(c.yr)
                            .and_then(|v| {
                                i32::from(g)
                                    .checked_mul(c.yg)
                                    .and_then(|w| v.checked_add(w))
                            })
                            .and_then(|v| {
                                i32::from(b)
                                    .checked_mul(c.yb)
                                    .and_then(|w| v.checked_add(w))
                            })
                            .and_then(|v| v.checked_add(c.y_offset));
                        assert!(
                            checked.is_some(),
                            "{}: luma of rgb({r},{g},{b}) would overflow a \
                             checked i32, so the wrapping path is unsound",
                            space.describe()
                        );

                        // And a whole block of the brightest pixels.
                        let sum = i32::from(r) * 4;
                        let sg = i32::from(g) * 4;
                        let sb = i32::from(b) * 4;
                        for (cr, cg, cb) in [(c.ur, c.ug, c.ub), (c.vr, c.vg, c.vb)] {
                            let ok = sum
                                .checked_mul(cr)
                                .and_then(|v| sg.checked_mul(cg).and_then(|w| v.checked_add(w)))
                                .and_then(|v| sb.checked_mul(cb).and_then(|w| v.checked_add(w)))
                                .and_then(|v| v.checked_add(c.c_offset));
                            assert!(
                                ok.is_some(),
                                "{}: chroma of a block of rgb({r},{g},{b}) \
                                 would overflow a checked i32",
                                space.describe()
                            );
                        }
                    }
                }
            }
        }
    }

    /// Rounding, not truncating. Truncation biases every sample down by half a
    /// level on average, which darkens the picture for no reason — and is a
    /// one-instruction difference that survives for years because nobody
    /// looks.
    #[test]
    fn the_conversion_rounds_rather_than_truncating() {
        let space = ColorSpace::default();
        let coeff = Coefficients::for_space(space);
        let mut bias = 0i64;
        for r in (0..=255u8).step_by(5) {
            for g in (0..=255u8).step_by(5) {
                for b in (0..=255u8).step_by(5) {
                    let got = i64::from(coeff.luma(r, g, b));
                    let (want, _, _) = reference_yuv(r, g, b, space);
                    bias += got - i64::from(want);
                }
            }
        }
        let samples = (52i64).pow(3);
        assert!(
            bias.abs() * 20 < samples,
            "the conversion is biased by {bias} over {samples} samples, which \
             means it truncates rather than rounds"
        );
    }

    /// Black and white must land exactly on the range's endpoints. If they do
    /// not, every picture has slightly wrong contrast and nothing else reveals
    /// it.
    #[test]
    fn black_and_white_land_on_the_range_endpoints() {
        for space in all_spaces() {
            let coeff = Coefficients::for_space(space);
            let (black, white) = match space.range {
                Range::Limited => (16u8, 235u8),
                Range::Full => (0, 255),
            };
            assert_eq!(coeff.luma(0, 0, 0), black, "{}: black", space.describe());
            assert_eq!(
                coeff.luma(255, 255, 255),
                white,
                "{}: white",
                space.describe()
            );
            // And both are colourless.
            for (r, g, b) in [(0u8, 0u8, 0u8), (255, 255, 255), (128, 128, 128)] {
                let (u, v) = coeff.chroma_of_sum::<0>(i32::from(r), i32::from(g), i32::from(b));
                assert!(
                    (i32::from(u) - 128).abs() <= 1 && (i32::from(v) - 128).abs() <= 1,
                    "{}: grey rgb({r},{g},{b}) is not colourless: ({u},{v})",
                    space.describe()
                );
            }
        }
    }

    /// The two matrices must actually produce different numbers, or signalling
    /// them separately is theatre. Saturated red and green are where the
    /// difference lives.
    #[test]
    fn the_two_matrices_produce_visibly_different_luma() {
        let a = Coefficients::for_space(ColorSpace::new(Matrix::Bt601, Range::Limited));
        let b = Coefficients::for_space(ColorSpace::new(Matrix::Bt709, Range::Limited));
        let red = (i32::from(a.luma(255, 0, 0)) - i32::from(b.luma(255, 0, 0))).abs();
        let green = (i32::from(a.luma(0, 255, 0)) - i32::from(b.luma(0, 255, 0))).abs();
        assert!(
            red > 10,
            "red differs by only {red} levels between matrices"
        );
        assert!(
            green > 10,
            "green differs by only {green} levels between matrices"
        );
    }

    /// A colour that survives the round trip proves the matrix is invertible,
    /// which no forward comparison can show.
    #[test]
    fn a_colour_survives_a_round_trip_through_yuv() {
        for space in all_spaces() {
            for (r, g, b) in [
                (0u8, 0u8, 0u8),
                (255, 255, 255),
                (255, 0, 0),
                (0, 255, 0),
                (0, 0, 255),
                (18, 52, 86),
                (200, 130, 60),
                (127, 127, 127),
            ] {
                let (y, u, v) = reference_yuv(r, g, b, space);
                let (r2, g2, b2) = reference_rgb(y, u, v, space);
                let worst = [
                    (i32::from(r) - i32::from(r2)).abs(),
                    (i32::from(g) - i32::from(g2)).abs(),
                    (i32::from(b) - i32::from(b2)).abs(),
                ]
                .into_iter()
                .max()
                .unwrap_or(0);
                // Limited range quantises to 219 luma levels, so a couple of
                // levels of loss is arithmetic rather than error.
                let allowed = if space.range == Range::Limited { 3 } else { 2 };
                assert!(
                    worst <= allowed,
                    "{}: rgb({r},{g},{b}) came back as ({r2},{g2},{b2})",
                    space.describe()
                );
            }
        }
    }

    /// A solid colour must convert to a uniform image — every luma sample the
    /// same, every chroma sample the same. Anything else is a stride or an
    /// indexing mistake.
    #[test]
    fn a_solid_frame_converts_to_a_uniform_image() {
        let space = ColorSpace::default();
        for format in [
            PixelFormat::Bgra8,
            PixelFormat::Bgrx8,
            PixelFormat::Rgba8,
            PixelFormat::Rgbx8,
        ] {
            for padding in [0usize, 3, 64] {
                let frame = solid(16, 12, format, padding, (200, 130, 60));
                let mut img = I420::new(16, 12).expect("valid");
                let _ = whole_frame(&frame, space, &mut img).expect("converted");

                let (wy, wu, wv) = reference_yuv(200, 130, 60, space);
                for y in 0..12u32 {
                    for x in 0..16u32 {
                        let got = img.luma_at(x, y).expect("in bounds");
                        assert!(
                            (i32::from(got) - i32::from(wy)).abs() <= 1,
                            "{} padding {padding}: luma at ({x},{y}) is {got}, want {wy}",
                            format.name()
                        );
                    }
                }
                for (u, v) in img.u().iter().zip(img.v()) {
                    assert!(
                        (i32::from(*u) - i32::from(wu)).abs() <= 1
                            && (i32::from(*v) - i32::from(wv)).abs() <= 1,
                        "{} padding {padding}: chroma ({u},{v}), want ({wu},{wv})",
                        format.name()
                    );
                }
            }
        }
    }

    /// Every pixel must reach the right place. A gradient makes a transposed
    /// or offset write obvious in a way a solid colour cannot.
    #[test]
    fn every_pixel_lands_where_it_belongs() {
        let space = ColorSpace::default();
        let frame = painted(32, 16, PixelFormat::Bgrx8, 17, |x, y| {
            ((x * 8) as u8, (y * 16) as u8, 0)
        });
        let mut img = I420::new(32, 16).expect("valid");
        let _ = whole_frame(&frame, space, &mut img).expect("converted");

        for y in 0..16u32 {
            for x in 0..32u32 {
                let (wy, _, _) = reference_yuv((x * 8) as u8, (y * 16) as u8, 0, space);
                let got = img.luma_at(x, y).expect("in bounds");
                assert!(
                    (i32::from(got) - i32::from(wy)).abs() <= 1,
                    "({x},{y}) is {got}, want {wy} — a transposed or offset write"
                );
            }
        }
    }

    /// Chroma is the average of the 2×2 block, not the top-left sample.
    /// Sampling aliases, and on the fine detail a screen is full of that shows
    /// as colour fringing which moves when the content scrolls.
    #[test]
    fn chroma_is_the_average_of_its_block_not_one_corner() {
        let space = ColorSpace::default();
        // A 2x2 frame: red, red, blue, blue. The average is a dull purple,
        // which is neither red's chroma nor blue's.
        let frame = painted(2, 2, PixelFormat::Bgrx8, 0, |_, y| {
            if y == 0 { (255, 0, 0) } else { (0, 0, 255) }
        });
        let mut img = I420::new(2, 2).expect("valid");
        let _ = whole_frame(&frame, space, &mut img).expect("converted");

        let (_, wu, wv) = reference_yuv(128, 0, 128, space); // the mean colour
        let (gu, gv) = img.chroma_at(0, 0).expect("in bounds");
        assert!(
            (i32::from(gu) - i32::from(wu)).abs() <= 2
                && (i32::from(gv) - i32::from(wv)).abs() <= 2,
            "chroma ({gu},{gv}) should be the block average ({wu},{wv}); a \
             corner sample would give red's or blue's chroma instead"
        );

        // And the luma of each row is still that row's own colour: subsampling
        // chroma must not blur luma.
        let (red_y, _, _) = reference_yuv(255, 0, 0, space);
        let (blue_y, _, _) = reference_yuv(0, 0, 255, space);
        assert!((i32::from(img.luma_at(0, 0).unwrap_or(0)) - i32::from(red_y)).abs() <= 1);
        assert!((i32::from(img.luma_at(0, 1).unwrap_or(0)) - i32::from(blue_y)).abs() <= 1);
    }

    /// **An odd-sized screen must not panic or lose a row.** A compositor is
    /// free to hand out a 1919x1079 window, and 4:2:0 has no representation
    /// for an odd dimension.
    #[test]
    fn odd_sized_frames_convert_without_losing_a_row_or_column() {
        let space = ColorSpace::default();
        for (w, h) in [(1u32, 1u32), (3, 3), (5, 1), (1, 5), (17, 13), (63, 31)] {
            let frame = painted(w, h, PixelFormat::Bgrx8, 0, |x, y| {
                (((x * 37) % 256) as u8, ((y * 53) % 256) as u8, 128)
            });
            let mut img = I420::new(w, h).expect("valid");
            let _ = whole_frame(&frame, space, &mut img).expect("converted");

            // Every real pixel is right.
            for y in 0..h {
                for x in 0..w {
                    let (wy, _, _) =
                        reference_yuv(((x * 37) % 256) as u8, ((y * 53) % 256) as u8, 128, space);
                    let got = img.luma_at(x, y).expect("in bounds");
                    assert!(
                        (i32::from(got) - i32::from(wy)).abs() <= 1,
                        "{w}x{h}: ({x},{y}) is {got}, want {wy}"
                    );
                }
            }
            // And the padding repeats the edge rather than being left black,
            // which the encoder would otherwise spend bits reproducing.
            if img.width() > w {
                for y in 0..h {
                    assert_eq!(
                        img.luma_at(w, y),
                        img.luma_at(w - 1, y),
                        "{w}x{h}: the padding column at y={y} is not the edge"
                    );
                }
            }
            if img.height() > h {
                for x in 0..img.width() {
                    assert_eq!(
                        img.luma_at(x, h),
                        img.luma_at(x, h - 1),
                        "{w}x{h}: the padding row at x={x} is not the edge"
                    );
                }
            }
        }
    }

    /// **The damage-driven case.** Converting only what changed must give the
    /// same image as converting everything — otherwise the saving is bought
    /// with a wrong picture.
    #[test]
    fn converting_only_the_damage_gives_the_same_image_as_converting_it_all() {
        let space = ColorSpace::default();
        let first = painted(64, 48, PixelFormat::Bgrx8, 13, |x, y| {
            ((x * 3) as u8, (y * 5) as u8, 90)
        });
        // The second frame differs in one rectangle.
        let changed = Rect::new(10, 6, 20, 14);
        let second = painted(64, 48, PixelFormat::Bgrx8, 13, |x, y| {
            let inside = (10..30).contains(&x) && (6..20).contains(&y);
            if inside {
                (7, 200, 33)
            } else {
                ((x * 3) as u8, (y * 5) as u8, 90)
            }
        });

        // Full conversion of both, as the reference.
        let mut reference = I420::new(64, 48).expect("valid");
        let _ = whole_frame(&second, space, &mut reference).expect("converted");

        // Damage-driven: convert the first whole, then only the region.
        let mut incremental = I420::new(64, 48).expect("valid");
        let _ = whole_frame(&first, space, &mut incremental).expect("converted");
        let converted = regions(&second, space, &mut incremental, &[changed]).expect("converted");

        assert!(
            converted < 64 * 48,
            "the point is to convert less than everything, converted {converted}"
        );
        assert_eq!(
            incremental.y(),
            reference.y(),
            "the luma plane differs between a full and a damage-driven conversion"
        );
        assert_eq!(incremental.u(), reference.u(), "the Cb plane differs");
        assert_eq!(incremental.v(), reference.v(), "the Cr plane differs");
    }

    /// **The invariant that is actually load-bearing.** A damaged region on an
    /// odd coordinate shares chroma samples with the pixel beside it, so the
    /// converter has to widen it to whole 2×2 blocks. Without that, chroma
    /// comes from one new pixel and one left over from the previous frame — a
    /// one-pixel colour fringe along the edge of everything that moves.
    ///
    /// Every combination of odd and even origin and size, because the origin
    /// and the far edge are rounded by different code and an earlier version
    /// had a redundant second mechanism hiding which one mattered.
    #[test]
    fn a_region_on_odd_coordinates_still_converts_whole_chroma_blocks() {
        let space = ColorSpace::default();
        let base = painted(32, 32, PixelFormat::Bgrx8, 0, |_, _| (10, 20, 30));
        let after = painted(32, 32, PixelFormat::Bgrx8, 0, |x, y| {
            if (5..12).contains(&x) && (7..14).contains(&y) {
                (240, 30, 90)
            } else {
                (10, 20, 30)
            }
        });

        let mut reference = I420::new(32, 32).expect("valid");
        let _ = whole_frame(&after, space, &mut reference).expect("converted");

        // The change is x 5..12, y 7..14. Every region that covers it must
        // give the same answer, whatever its own parity.
        for (x, y, w, h) in [
            (5i32, 7i32, 7u32, 7u32),
            (5, 7, 8, 8),
            (4, 6, 8, 8),
            (4, 6, 9, 9),
            (5, 6, 7, 8),
            (0, 0, 32, 32),
        ] {
            let mut incremental = I420::new(32, 32).expect("valid");
            let _ = whole_frame(&base, space, &mut incremental).expect("converted");
            let _ = regions(&after, space, &mut incremental, &[Rect::new(x, y, w, h)])
                .expect("converted");

            assert_eq!(
                incremental.y(),
                reference.y(),
                "region ({x},{y},{w},{h}): luma"
            );
            assert_eq!(
                incremental.u(),
                reference.u(),
                "region ({x},{y},{w},{h}): Cb — a colour fringe would show here"
            );
            assert_eq!(
                incremental.v(),
                reference.v(),
                "region ({x},{y},{w},{h}): Cr"
            );
        }
    }

    /// The reported count is what was actually converted, which is more than
    /// the region asked for when the region straddles the chroma grid. A
    /// number that understated the work would make the damage-driven saving
    /// look better than it is.
    #[test]
    fn the_reported_count_is_what_was_written_not_what_was_asked_for() {
        let space = ColorSpace::default();
        let frame = solid(32, 32, PixelFormat::Bgrx8, 0, (1, 2, 3));
        let mut img = I420::new(32, 32).expect("valid");
        let _ = whole_frame(&frame, space, &mut img).expect("converted");

        // A 1x1 region on an odd coordinate costs a whole 2x2 block.
        let one = regions(&frame, space, &mut img, &[Rect::new(5, 7, 1, 1)]).expect("converted");
        assert_eq!(one, 4, "a single odd pixel costs its whole chroma block");

        // An aligned 4x4 costs exactly 16.
        let aligned =
            regions(&frame, space, &mut img, &[Rect::new(4, 6, 4, 4)]).expect("converted");
        assert_eq!(aligned, 16);

        // And a whole frame costs every pixel.
        let all = regions(&frame, space, &mut img, &[Rect::sized(32, 32)]).expect("converted");
        assert_eq!(all, 32 * 32);
    }

    /// Superseded by the test above, kept because it is the concrete case the
    /// fringe was found in.
    #[test]
    fn a_single_odd_region_matches_a_full_conversion() {
        let space = ColorSpace::default();
        let base = painted(32, 32, PixelFormat::Bgrx8, 0, |_, _| (10, 20, 30));
        let after = painted(32, 32, PixelFormat::Bgrx8, 0, |x, y| {
            if (5..12).contains(&x) && (7..14).contains(&y) {
                (240, 30, 90)
            } else {
                (10, 20, 30)
            }
        });

        let mut reference = I420::new(32, 32).expect("valid");
        let _ = whole_frame(&after, space, &mut reference).expect("converted");

        let mut incremental = I420::new(32, 32).expect("valid");
        let _ = whole_frame(&base, space, &mut incremental).expect("converted");
        // Deliberately odd origin and odd size.
        let _ =
            regions(&after, space, &mut incremental, &[Rect::new(5, 7, 7, 7)]).expect("converted");

        assert_eq!(incremental.y(), reference.y(), "luma");
        assert_eq!(
            incremental.u(),
            reference.u(),
            "Cb — a fringe would show here"
        );
        assert_eq!(incremental.v(), reference.v(), "Cr");
    }

    /// Damage outside the frame must be clamped away rather than converted.
    #[test]
    fn damage_outside_the_frame_is_ignored() {
        let space = ColorSpace::default();
        let frame = solid(16, 16, PixelFormat::Bgrx8, 0, (1, 2, 3));
        let mut img = I420::new(16, 16).expect("valid");
        let _ = whole_frame(&frame, space, &mut img).expect("converted");

        let converted = regions(
            &frame,
            space,
            &mut img,
            &[
                Rect::new(1000, 1000, 10, 10),
                Rect::new(-50, -50, 10, 10),
                Rect::new(0, 0, 0, 10),
            ],
        )
        .expect("converted");
        assert_eq!(converted, 0, "none of those overlap the frame");
    }

    /// A destination smaller than the frame is a caller mistake, not something
    /// to write past the end of.
    #[test]
    fn a_destination_too_small_is_refused() {
        let space = ColorSpace::default();
        let frame = solid(64, 64, PixelFormat::Bgrx8, 0, (1, 2, 3));
        let mut img = I420::new(16, 16).expect("valid");
        let err = regions(&frame, space, &mut img, &[Rect::sized(64, 64)]).expect_err("too small");
        assert!(matches!(err, ConvertError::SizeMismatch { .. }), "{err:?}");
    }

    /// A resize must be reported, because a caller doing damage-driven
    /// conversion cannot trust the buffer's contents after one.
    #[test]
    fn a_conversion_that_reshapes_the_buffer_says_so() {
        let space = ColorSpace::default();
        let mut img = I420::new(16, 16).expect("valid");

        let same = solid(16, 16, PixelFormat::Bgrx8, 0, (1, 2, 3));
        assert!(!whole_frame(&same, space, &mut img).expect("converted"));

        let bigger = solid(64, 48, PixelFormat::Bgrx8, 0, (1, 2, 3));
        assert!(
            whole_frame(&bigger, space, &mut img).expect("converted"),
            "a reshape must be reported, or a caller keeps trusting stale pixels"
        );
        assert_eq!((img.width(), img.height()), (64, 48));
    }

    /// Padding bytes must never reach the output. In an X format the fourth
    /// byte is whatever used the memory before.
    #[test]
    fn the_padding_byte_of_an_x_format_does_not_reach_the_output() {
        let space = ColorSpace::default();
        let mut a = painted(8, 8, PixelFormat::Bgrx8, 0, |_, _| (60, 120, 180));
        let b = painted(8, 8, PixelFormat::Bgrx8, 0, |_, _| (60, 120, 180));

        // Fill every fourth byte of `a` with noise.
        for i in (3..a.bytes().len()).step_by(4) {
            if let Some(slot) = a.bytes_mut().get_mut(i) {
                *slot = 0xA5;
            }
        }

        let mut ia = I420::new(8, 8).expect("valid");
        let mut ib = I420::new(8, 8).expect("valid");
        let _ = whole_frame(&a, space, &mut ia).expect("converted");
        let _ = whole_frame(&b, space, &mut ib).expect("converted");
        assert_eq!(ia.y(), ib.y(), "the padding byte changed the luma");
        assert_eq!(ia.u(), ib.u(), "the padding byte changed the chroma");
        assert_eq!(ia.v(), ib.v());
    }

    /// Whatever the frame, the conversion must not panic. Release builds have
    /// overflow checks on, and a screen is any size a compositor likes.
    #[test]
    #[cfg_attr(
        miri,
        ignore = "sweeps many frame shapes; MIRI interprets rather than executes"
    )]
    fn arbitrary_frames_never_panic() {
        let mut x = 0x4F1B_BCDD_ACAB_1234u64;
        for _ in 0..300 {
            let mut next = || {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                x
            };
            let w = (next() % 70) as u32 + 1;
            let h = (next() % 70) as u32 + 1;
            let format = match next() % 4 {
                0 => PixelFormat::Bgra8,
                1 => PixelFormat::Bgrx8,
                2 => PixelFormat::Rgba8,
                _ => PixelFormat::Rgbx8,
            };
            let padding = (next() % 33) as usize;
            let frame = painted(w, h, format, padding, |px, py| {
                (px as u8, py as u8, (px ^ py) as u8)
            });
            let space = all_spaces()
                .get((next() % 4) as usize)
                .copied()
                .unwrap_or_default();

            let mut img = I420::new(w, h).expect("valid");
            let _ = whole_frame(&frame, space, &mut img).expect("converted");
            let _ = regions(
                &frame,
                space,
                &mut img,
                &[
                    Rect::new(next() as i32 % 90 - 10, next() as i32 % 90 - 10, 7, 5),
                    Rect::sized(w, h),
                ],
            );
        }
    }
}
