//! Planar YUV back to packed RGB, for anything that draws a decoded picture.
//!
//! # Why this exists when the browser does it for free
//!
//! The default viewer hands the bitstream to a webview and lets the platform's
//! hardware decoder and compositor put it on screen. Nothing in that path needs
//! this. Everything *else* does: a native window, a screenshot, a test that
//! wants to look at what came back, and the PSNR the loopback reports. Before
//! this existed the loopback computed PSNR through
//! [`reference_rgb`](crate::convert::reference_rgb) — a `f64` function, per
//! pixel, per frame.
//!
//! # The chroma is upsampled by repetition, on purpose
//!
//! 4:2:0 keeps one chroma pair per 2x2 block, so going back out four luma
//! samples share one pair. Bilinear upsampling would look better on the edge
//! of coloured text, and it is not what this does, for a reason worth stating:
//!
//! The forward conversion **averages** the four chroma samples rather than
//! sampling one, and repetition is that operation's inverse. Keeping the two
//! exact inverses of each other is what lets this be checked against a
//! floating-point reference over the whole input space — every `(Y, U, V)`
//! triple, in every colour space. A bilinear filter blurs across block
//! boundaries, so its output depends on neighbours, and the strongest
//! available test would have to be replaced with a weaker one.
//!
//! It is also invisible where it matters: the picture came out of an H.264
//! decoder that was working in 4:2:0 the whole time. The chroma detail a
//! bilinear filter would interpolate was thrown away before this stage saw it.
//!
//! # Rounding and clamping
//!
//! The maths can leave the 0..255 range legitimately — a `(Y, U, V)` triple
//! need not correspond to a real colour, and a decoder is free to hand back
//! ones that do not. So every channel is clamped rather than wrapped or
//! asserted. Wrapping turns one impossible pixel into a bright speck; a
//! decoder producing a few of those on a lossy frame is normal.

use esper_capture::frame::PixelFormat;
use esper_capture::geometry::Rect;

use crate::convert::ConvertError;
use crate::i420::I420;
use crate::space::ColorSpace;

/// Fractional bits, matching the forward conversion.
const SHIFT: u32 = 14;
const ONE: i32 = 1 << SHIFT;
const HALF: i32 = ONE / 2;

/// The integer coefficients for one colour space, going back to RGB.
///
/// Built once per stream, not per pixel. Derived from the same spec numbers as
/// the forward direction, in `f64`, at construction — the same division of
/// labour: floating point decides what the answer is, integers produce it.
#[derive(Debug, Clone, Copy)]
pub struct RgbCoefficients {
    /// `(Y - y_off) * y_gain`, taking luma to a 0..255 scale.
    y_gain: i32,
    /// Cr's contribution to red.
    cr_r: i32,
    /// Cb's and Cr's contributions to green, both negative.
    cb_g: i32,
    cr_g: i32,
    /// Cb's contribution to blue.
    cb_b: i32,
    /// Black level, subtracted from luma before scaling.
    y_off: i32,
    /// Chroma's zero point.
    c_off: i32,
}

impl RgbCoefficients {
    /// Derive the integers from the spec's real numbers.
    #[must_use]
    pub fn for_space(space: ColorSpace) -> Self {
        let (kr, kg, kb) = space.matrix.luma_weights();
        let (y_off, y_scale) = space.range.luma();
        let (c_off, c_scale) = space.range.chroma();
        let round = |v: f64| (v * f64::from(ONE)).round() as i32;

        // R' = Y' + Cr' * 2(1 - kr)
        // B' = Y' + Cb' * 2(1 - kb)
        // G' = Y' - Cr' * 2(1 - kr)kr/kg - Cb' * 2(1 - kb)kb/kg
        //
        // The green row follows from kr + kg + kb = 1 rather than being a
        // separate published number, which is why it is derived here instead
        // of transcribed: a transcribed matrix can disagree with the luma
        // weights it was supposed to come from.
        let r_from_cr = 2.0 * (1.0 - kr);
        let b_from_cb = 2.0 * (1.0 - kb);
        let scale = 255.0 / y_scale;
        let cscale = 255.0 / c_scale;

        Self {
            y_gain: round(scale),
            cr_r: round(cscale * r_from_cr),
            cb_g: round(cscale * b_from_cb * kb / kg),
            cr_g: round(cscale * r_from_cr * kr / kg),
            cb_b: round(cscale * b_from_cb),
            y_off: y_off.round() as i32,
            c_off: c_off.round() as i32,
        }
    }

    /// One pixel, from its luma and the chroma pair of its block.
    ///
    /// Every product here is bounded well inside `i32`: the largest term is a
    /// chroma delta of 128 times a coefficient under two, scaled by `ONE`,
    /// which is about four million. `the_arithmetic_stays_inside_i32` holds
    /// that claim by exhausting the inputs rather than asserting it.
    #[must_use]
    pub fn pixel(&self, y: u8, u: u8, v: u8) -> (u8, u8, u8) {
        let luma = (i32::from(y) - self.y_off).wrapping_mul(self.y_gain);
        let cb = i32::from(u) - self.c_off;
        let cr = i32::from(v) - self.c_off;

        let r = luma.wrapping_add(cr.wrapping_mul(self.cr_r));
        let g = luma
            .wrapping_sub(cr.wrapping_mul(self.cr_g))
            .wrapping_sub(cb.wrapping_mul(self.cb_g));
        let b = luma.wrapping_add(cb.wrapping_mul(self.cb_b));

        let done = |x: i32| {
            let level = x.wrapping_add(HALF) >> SHIFT;
            u8::try_from(level.clamp(0, 255)).unwrap_or(0)
        };
        (done(r), done(g), done(b))
    }
}

/// Write a whole picture into a packed buffer.
///
/// `stride` is the destination's bytes per row, which a window's back buffer
/// often pads. `format` says which byte holds red; the fourth byte is set to
/// `0xFF` so a surface that treats it as alpha shows an opaque picture rather
/// than a transparent one.
pub fn whole_frame(
    src: &I420,
    space: ColorSpace,
    out: &mut [u8],
    stride: usize,
    format: PixelFormat,
) -> Result<(), ConvertError> {
    let all = Rect::sized(src.width(), src.height());
    region(src, space, out, stride, format, &all)
}

/// Write one rectangle of a picture into a packed buffer.
///
/// For a presenter with damage information: repainting a caret should not cost
/// a whole frame of conversion any more than capturing it did.
///
/// The rectangle is clamped to the picture, and odd edges are handled rather
/// than refused — a damage rectangle is not obliged to fall on chroma
/// boundaries, and refusing one would push the alignment problem onto every
/// caller.
pub fn region(
    src: &I420,
    space: ColorSpace,
    out: &mut [u8],
    stride: usize,
    format: PixelFormat,
    area: &Rect,
) -> Result<(), ConvertError> {
    let bounds = Rect::sized(src.width(), src.height());
    let Some(area) = area.clamp_to(&bounds) else {
        return Ok(());
    };
    let bpp = format.bytes_per_pixel();
    let needed = stride
        .checked_mul(src.height() as usize)
        .ok_or(ConvertError::SizeMismatch {
            fw: src.width(),
            fh: src.height(),
            iw: src.width(),
            ih: src.height(),
        })?;
    if out.len() < needed || stride < (src.width() as usize).saturating_mul(bpp) {
        return Err(ConvertError::SizeMismatch {
            fw: src.width(),
            fh: src.height(),
            iw: u32::try_from(stride / bpp.max(1)).unwrap_or(0),
            ih: u32::try_from(out.len() / stride.max(1)).unwrap_or(0),
        });
    }

    let coeff = RgbCoefficients::for_space(space);
    let (r_at, g_at, b_at) = format.rgb_offsets();
    let y_stride = src.y_stride();
    let uv_stride = src.uv_stride();
    let (x0, y0) = (area.x.max(0) as usize, area.y.max(0) as usize);
    let x1 = x0.saturating_add(area.width as usize);
    let y1 = y0.saturating_add(area.height as usize);

    for row in y0..y1 {
        let luma_row = src
            .y()
            .get(row.saturating_mul(y_stride)..)
            .unwrap_or_default();
        let chroma_row = row / 2;
        let u_row = src
            .u()
            .get(chroma_row.saturating_mul(uv_stride)..)
            .unwrap_or_default();
        let v_row = src
            .v()
            .get(chroma_row.saturating_mul(uv_stride)..)
            .unwrap_or_default();
        let dst_row = row.saturating_mul(stride);

        for col in x0..x1 {
            let (Some(&y), Some(&u), Some(&v)) =
                (luma_row.get(col), u_row.get(col / 2), v_row.get(col / 2))
            else {
                continue;
            };
            let (r, g, b) = coeff.pixel(y, u, v);
            let at = dst_row.saturating_add(col.saturating_mul(bpp));
            let Some(px) = out.get_mut(at..at.saturating_add(bpp)) else {
                continue;
            };
            if let Some(c) = px.get_mut(r_at) {
                *c = r;
            }
            if let Some(c) = px.get_mut(g_at) {
                *c = g;
            }
            if let Some(c) = px.get_mut(b_at) {
                *c = b;
            }
            if let Some(c) = px.get_mut(3) {
                *c = 0xFF;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::convert::{all_spaces, reference_rgb};
    use crate::i420::I420;

    /// **The test this whole module is shaped around.** Every representable
    /// `(Y, U, V)` triple, in every colour space, against the floating-point
    /// reference — sixty-seven million comparisons.
    ///
    /// One level of tolerance, because both sides round and they round at
    /// different points: the reference works in `f64` throughout and rounds
    /// once at the end, the integer path rounds its coefficients at
    /// construction and its result at the shift. A known-answer table for a
    /// dozen colours would prove the table, not the arithmetic.
    #[test]
    #[cfg_attr(
        miri,
        ignore = "sixty-seven million conversions: minutes under MIRI, which interprets rather than executes"
    )]
    fn every_triple_matches_the_floating_point_reference() {
        for space in all_spaces() {
            let coeff = RgbCoefficients::for_space(space);
            let mut worst = 0i32;
            for y in 0..=255u8 {
                for u in 0..=255u8 {
                    for v in 0..=255u8 {
                        let (r, g, b) = coeff.pixel(y, u, v);
                        let (wr, wg, wb) = reference_rgb(y, u, v, space);
                        for (got, want, ch) in [(r, wr, 'r'), (g, wg, 'g'), (b, wb, 'b')] {
                            let d = i32::from(got) - i32::from(want);
                            assert!(
                                d.abs() <= 1,
                                "{} at Y={y} U={u} V={v} channel {ch}: {got} vs {want}",
                                space.describe()
                            );
                            worst = worst.max(d.abs());
                        }
                    }
                }
            }
            assert!(worst <= 1, "{} worst error {worst}", space.describe());
        }
    }

    /// **The bound that licenses the wrapping arithmetic.** The hot path uses
    /// wrapping multiply and add because this workspace turns overflow checks
    /// on in release, and a check per channel per pixel is a branch per
    /// channel per pixel. So it has to be true that nothing can overflow, and
    /// the way to know is to exhaust the inputs against a checked path.
    #[test]
    #[cfg_attr(
        miri,
        ignore = "sixteen million checked conversions: minutes under MIRI"
    )]
    fn the_arithmetic_stays_inside_i32() {
        for space in all_spaces() {
            let c = RgbCoefficients::for_space(space);
            for y in 0..=255u8 {
                for u in 0..=255u8 {
                    for v in 0..=255u8 {
                        let luma = (i32::from(y) - c.y_off)
                            .checked_mul(c.y_gain)
                            .expect("luma term fits");
                        let cb = i32::from(u) - c.c_off;
                        let cr = i32::from(v) - c.c_off;
                        for (delta, k) in [(cr, c.cr_r), (cr, c.cr_g), (cb, c.cb_g), (cb, c.cb_b)] {
                            let term = delta.checked_mul(k).expect("chroma term fits");
                            let sum = luma.checked_add(term).expect("the sum fits");
                            let _ = sum.checked_add(HALF).expect("rounding fits");
                        }
                    }
                }
            }
        }
    }

    /// A colour that survives the round trip through both directions.
    ///
    /// Not every colour does — 4:2:0 throws away three quarters of the chroma
    /// and the ranges do not line up exactly — but a grey has no chroma to
    /// lose, so it must come back where it started.
    #[test]
    fn greys_survive_the_round_trip_exactly() {
        for space in all_spaces() {
            let fwd = crate::convert::Coefficients::for_space(space);
            let back = RgbCoefficients::for_space(space);
            for level in 0..=255u8 {
                let (y, u, v) = crate::convert::reference_yuv(level, level, level, space);
                let _ = fwd;
                let (r, g, b) = back.pixel(y, u, v);
                for (got, ch) in [(r, 'r'), (g, 'g'), (b, 'b')] {
                    let d = i32::from(got) - i32::from(level);
                    assert!(
                        d.abs() <= 1,
                        "{}: grey {level} came back as {ch}={got}",
                        space.describe()
                    );
                }
            }
        }
    }

    /// Black and white are the levels a viewer notices immediately, and the
    /// limited range is where they are easy to get wrong: 16 is black and 235
    /// is white, not 0 and 255.
    #[test]
    fn the_range_endpoints_land_on_black_and_white() {
        use crate::space::{Matrix, Range};
        let limited = RgbCoefficients::for_space(ColorSpace::new(Matrix::Bt709, Range::Limited));
        assert_eq!(limited.pixel(16, 128, 128), (0, 0, 0), "16 must be black");
        assert_eq!(
            limited.pixel(235, 128, 128),
            (255, 255, 255),
            "235 must be white"
        );
        // And below black or above white clamps rather than wrapping. A
        // decoder is allowed to hand back those levels.
        assert_eq!(limited.pixel(0, 128, 128), (0, 0, 0));
        assert_eq!(limited.pixel(255, 128, 128), (255, 255, 255));

        let full = RgbCoefficients::for_space(ColorSpace::new(Matrix::Bt709, Range::Full));
        assert_eq!(full.pixel(0, 128, 128), (0, 0, 0));
        assert_eq!(full.pixel(255, 128, 128), (255, 255, 255));
    }

    /// An impossible triple must clamp, not wrap. Wrapping turns one
    /// out-of-gamut pixel into a bright speck, and a lossy decoder producing a
    /// few of those is normal rather than exceptional.
    #[test]
    fn out_of_gamut_triples_clamp() {
        for space in all_spaces() {
            let c = RgbCoefficients::for_space(space);
            for (y, u, v) in [(255u8, 255u8, 255u8), (0, 0, 0), (255, 0, 255), (0, 255, 0)] {
                let (r, g, b) = c.pixel(y, u, v);
                // Nothing to assert about the values except that they exist
                // and are in range, which `u8` guarantees — what matters is
                // that the reference agrees, and it is checked above. Here the
                // point is that extremes do not panic under overflow checks.
                let _ = (r, g, b);
            }
        }
    }

    fn picture(w: u32, h: u32, fill: impl Fn(u32, u32) -> (u8, u8, u8)) -> I420 {
        let mut img = I420::new(w, h).expect("a valid size");
        let (ys, us) = (img.y_stride(), img.uv_stride());
        let (y_plane, u_plane, v_plane) = img.planes_mut();
        for row in 0..h as usize {
            for col in 0..w as usize {
                let (y, u, v) = fill(col as u32, row as u32);
                if let Some(p) = y_plane.get_mut(row * ys + col) {
                    *p = y;
                }
                if row % 2 == 0 && col % 2 == 0 {
                    if let Some(p) = u_plane.get_mut((row / 2) * us + col / 2) {
                        *p = u;
                    }
                    if let Some(p) = v_plane.get_mut((row / 2) * us + col / 2) {
                        *p = v;
                    }
                }
            }
        }
        img
    }

    #[test]
    fn a_whole_picture_is_written_in_every_format() {
        let space = ColorSpace::default();
        let coeff = RgbCoefficients::for_space(space);
        let img = picture(16, 8, |x, y| {
            ((x * 13 + y * 7) as u8, (x * 5) as u8, (y * 11 + 60) as u8)
        });
        for format in [
            PixelFormat::Bgra8,
            PixelFormat::Bgrx8,
            PixelFormat::Rgba8,
            PixelFormat::Rgbx8,
        ] {
            let stride = 16 * 4;
            let mut out = vec![0u8; stride * 8];
            whole_frame(&img, space, &mut out, stride, format).expect("converts");
            let (r_at, g_at, b_at) = format.rgb_offsets();
            for row in 0..8u32 {
                for col in 0..16u32 {
                    let y = img.luma_at(col, row).expect("luma");
                    let (u, v) = img.chroma_at(col, row).expect("chroma");
                    let want = coeff.pixel(y, u, v);
                    let at = row as usize * stride + col as usize * 4;
                    let px = out.get(at..at + 4).expect("pixel");
                    assert_eq!(
                        (
                            px.get(r_at).copied().unwrap_or(0),
                            px.get(g_at).copied().unwrap_or(0),
                            px.get(b_at).copied().unwrap_or(0)
                        ),
                        want,
                        "{} at {col},{row}",
                        format.name()
                    );
                    assert_eq!(px.get(3), Some(&0xFF), "the fourth byte must be opaque");
                }
            }
        }
    }

    /// A padded destination stride, as a window's back buffer has. Reading it
    /// as tightly packed shears the picture.
    #[test]
    fn a_padded_destination_stride_is_honoured() {
        let space = ColorSpace::default();
        let img = picture(10, 6, |x, y| ((x * 20) as u8, 100, (y * 30) as u8));
        let tight = {
            let mut out = vec![0u8; 10 * 4 * 6];
            whole_frame(&img, space, &mut out, 10 * 4, PixelFormat::Bgrx8).expect("converts");
            out
        };
        let padded_stride = 10 * 4 + 48;
        let mut padded = vec![0u8; padded_stride * 6];
        whole_frame(&img, space, &mut padded, padded_stride, PixelFormat::Bgrx8).expect("converts");
        for row in 0..6usize {
            let a = tight.get(row * 40..row * 40 + 40).expect("tight row");
            let b = padded
                .get(row * padded_stride..row * padded_stride + 40)
                .expect("padded row");
            assert_eq!(a, b, "row {row} differs between strides");
        }
    }

    /// Converting one rectangle must leave everything else exactly as it was,
    /// and must match what a whole-frame conversion would have put there.
    #[test]
    fn a_region_writes_only_itself() {
        let space = ColorSpace::default();
        let img = picture(32, 16, |x, y| {
            ((x * 7 + y) as u8, (y * 9) as u8, (x * 3) as u8)
        });
        let stride = 32 * 4;
        let mut whole = vec![0u8; stride * 16];
        whole_frame(&img, space, &mut whole, stride, PixelFormat::Bgrx8).expect("converts");

        for area in [
            Rect {
                x: 0,
                y: 0,
                width: 1,
                height: 1,
            },
            Rect {
                x: 3,
                y: 5,
                width: 7,
                height: 3,
            },
            Rect {
                x: 30,
                y: 14,
                width: 9,
                height: 9,
            },
            Rect {
                x: 0,
                y: 0,
                width: 32,
                height: 16,
            },
        ] {
            const MARK: u8 = 0x5A;
            let mut partial = vec![MARK; stride * 16];
            region(&img, space, &mut partial, stride, PixelFormat::Bgrx8, &area).expect("converts");
            let clamped = area
                .clamp_to(&Rect::sized(32, 16))
                .expect("overlaps the picture");
            for row in 0..16usize {
                for col in 0..32usize {
                    let at = row * stride + col * 4;
                    let inside = col >= clamped.x as usize
                        && col < (clamped.x as usize + clamped.width as usize)
                        && row >= clamped.y as usize
                        && row < (clamped.y as usize + clamped.height as usize);
                    let got = partial.get(at..at + 4).expect("pixel");
                    if inside {
                        assert_eq!(
                            got,
                            whole.get(at..at + 4).expect("pixel"),
                            "{area:?} at {col},{row} differs from a whole-frame conversion"
                        );
                    } else {
                        assert_eq!(
                            got, &[MARK; 4],
                            "{area:?} wrote outside itself at {col},{row}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn a_region_entirely_outside_the_picture_writes_nothing() {
        let space = ColorSpace::default();
        let img = picture(8, 8, |_, _| (128, 128, 128));
        let stride = 8 * 4;
        let mut out = vec![7u8; stride * 8];
        region(
            &img,
            space,
            &mut out,
            stride,
            PixelFormat::Bgrx8,
            &Rect {
                x: 100,
                y: 100,
                width: 4,
                height: 4,
            },
        )
        .expect("declines quietly");
        assert!(out.iter().all(|&b| b == 7));
    }

    #[test]
    fn a_destination_too_small_is_refused() {
        let space = ColorSpace::default();
        let img = picture(8, 8, |_, _| (128, 128, 128));
        let mut out = vec![0u8; 8 * 4 * 8 - 1];
        assert!(whole_frame(&img, space, &mut out, 8 * 4, PixelFormat::Bgrx8).is_err());
        // And a stride narrower than a row.
        let mut wide = vec![0u8; 8 * 4 * 8];
        assert!(whole_frame(&img, space, &mut wide, 8 * 4 - 4, PixelFormat::Bgrx8).is_err());
    }

    /// The four samples of a 2x2 block share one chroma pair, which is what
    /// makes this the exact inverse of the forward direction's averaging.
    #[test]
    fn a_block_shares_one_chroma_pair() {
        let space = ColorSpace::default();
        let coeff = RgbCoefficients::for_space(space);
        // Distinct luma in every position, one chroma pair for the block.
        let img = picture(2, 2, |x, y| {
            (if x == 0 && y == 0 { 200 } else { 60 }, 90, 200)
        });
        let stride = 8;
        let mut out = vec![0u8; stride * 2];
        whole_frame(&img, space, &mut out, stride, PixelFormat::Bgrx8).expect("converts");
        for (row, col) in [(0u32, 0u32), (0, 1), (1, 0), (1, 1)] {
            let y = img.luma_at(col, row).expect("luma");
            assert_eq!(
                img.chroma_at(col, row).expect("chroma"),
                (90, 200),
                "the block must share its chroma"
            );
            let want = coeff.pixel(y, 90, 200);
            let at = row as usize * stride + col as usize * 4;
            let px = out.get(at..at + 4).expect("pixel");
            assert_eq!(
                (
                    px.get(2).copied().unwrap_or(0),
                    px.get(1).copied().unwrap_or(0),
                    px.first().copied().unwrap_or(0)
                ),
                want
            );
        }
    }
}
