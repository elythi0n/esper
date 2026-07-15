//! Area-average downscaling of a captured frame.
//!
//! # Why a separate pass, and not fused into the colour conversion
//!
//! Sampling the source directly into YUV would be one pass instead of two.
//! It would also mean rewriting the conversion's inner loop, which is checked
//! against a floating-point reference over all sixteen million representable
//! colours. Scaling arrives to serve one rung of a degradation ladder; buying
//! a pass with that verification is the wrong trade.
//!
//! The two-pass cost is smaller than it looks. Scaling runs *before* the
//! conversion, so the conversion and the encode then work on fewer pixels: at
//! half scale the conversion sees a quarter of them. And the whole thing is
//! skipped when the target size is the source size, which is the normal case.
//!
//! # Vertical first, into one accumulator row
//!
//! The alternative — a full horizontally-scaled intermediate image — is
//! twelve megabytes at 1080p and evicts everything from cache. Accumulating
//! the source rows for one destination row into a single row of totals is
//! twenty-three kilobytes, which stays resident, and each source row is read
//! at most twice.
//!
//! # Alpha is dropped here
//!
//! H.264 4:2:0 has no alpha channel, so alpha is discarded at the first stage
//! that resamples rather than carried through two passes to be ignored. The
//! output format says so: it is always the input format's
//! [`PixelFormat::without_alpha`] variant, so nothing downstream can read a
//! resampled fourth byte believing it means opacity.

use ownview_capture::damage::{Damage, Regions};
use ownview_capture::frame::{Frame, FrameError, PixelFormat};
use ownview_capture::geometry::Rect;
use ownview_capture::pool::Pixels;

use crate::axis::{Axis, AxisError, SHIFT};

/// The largest value the resampling arithmetic can produce.
///
/// **The bound that licenses the wrapping arithmetic.** A sample is at most
/// 255 and each pass multiplies by a weight, with the weights of one
/// destination sample summing to exactly `ONE`. So a vertical total is at
/// most `255 * ONE`, and a horizontal total of those is at most
/// `255 * ONE * ONE` — plus `ROUND`, which is half of `ONE * ONE`.
pub const MAX_TOTAL: u64 = 255 * (crate::axis::ONE as u64) * (crate::axis::ONE as u64)
    + (crate::axis::ONE as u64) * (crate::axis::ONE as u64) / 2;

/// Rounding term for a product of two `SHIFT`-bit weights.
///
/// Half of the combined scale, added before the shift. Truncating instead
/// biases every sample down by half a level, which darkens the whole picture
/// for one instruction's worth of savings.
const ROUND: u32 = 1 << (2 * SHIFT - 1);

/// What went wrong scaling.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ScaleError {
    /// The weight tables could not be built.
    #[error(transparent)]
    Axis(#[from] AxisError),
    /// The output frame could not be described.
    #[error(transparent)]
    Frame(#[from] FrameError),
    /// The frame is not the size this scaler was built for.
    ///
    /// A resolution change is the caller's to notice; a scaler that quietly
    /// rebuilt itself would hide a backend that changed size without saying so.
    #[error("this scaler takes {want_w}x{want_h} frames, not {got_w}x{got_h}")]
    WrongSize {
        want_w: u32,
        want_h: u32,
        got_w: u32,
        got_h: u32,
    },
    /// The frame's pixels are laid out differently than expected.
    #[error("this scaler takes {want} frames, not {got}")]
    WrongFormat {
        want: &'static str,
        got: &'static str,
    },
    /// A row the frame claimed to have was not there.
    #[error("row {0} is missing from a frame that claims to have it")]
    ShortFrame(u32),
}

/// A downscaler holding the weights and the destination for one size pair.
///
/// The destination buffer is owned and reused, and is deliberately **not**
/// taken from a [`BufferPool`](ownview_capture::pool::BufferPool). Only the
/// changed regions are rewritten each frame, so the untouched pixels have to
/// be the previous *scaled* frame's — not those of whichever unrelated frame
/// last released a pooled buffer.
#[derive(Debug)]
pub struct Scaler {
    x: Axis,
    y: Axis,
    /// The format frames must arrive in.
    input: PixelFormat,
    out: Frame,
    /// Weighted source-column totals for the destination row being built,
    /// four lanes per pixel. Sized for the full source width; a small damage
    /// region uses a window of it.
    acc: Vec<u32>,
}

impl Scaler {
    /// Build a scaler from one size to another.
    ///
    /// Both target dimensions must be no larger than the source: this is a
    /// downscaler, and an upscale request is a bug in whatever computed the
    /// target rather than something to silently satisfy.
    pub fn new(
        src_w: u32,
        src_h: u32,
        dst_w: u32,
        dst_h: u32,
        input: PixelFormat,
    ) -> Result<Self, ScaleError> {
        let x = Axis::new(src_w, dst_w)?;
        let y = Axis::new(src_h, dst_h)?;
        let format = input.without_alpha();
        let stride = (dst_w as usize).saturating_mul(format.bytes_per_pixel());
        let len = stride.saturating_mul(dst_h as usize);
        let out = Frame::new(
            dst_w,
            dst_h,
            stride,
            format,
            Damage::Nothing,
            Pixels::zeroed(len),
        )?;
        Ok(Self {
            x,
            y,
            input,
            out,
            acc: vec![0; (src_w as usize).saturating_mul(4)],
        })
    }

    /// Whether this scaler would change anything.
    #[must_use]
    pub const fn is_identity(&self) -> bool {
        self.x.is_identity() && self.y.is_identity()
    }

    /// The pixel layout frames must arrive in.
    #[must_use]
    pub const fn input_format(&self) -> PixelFormat {
        self.input
    }

    /// The source size this scaler expects.
    #[must_use]
    pub const fn source(&self) -> (u32, u32) {
        (self.x.src(), self.y.src())
    }

    /// The size this scaler produces.
    #[must_use]
    pub const fn target(&self) -> (u32, u32) {
        (self.x.dst(), self.y.dst())
    }

    /// Scale `frame`, returning the reused destination.
    ///
    /// The returned frame is valid until the next call. Its damage is the
    /// given damage mapped through the ratio, widened to cover every
    /// destination pixel that reads a changed source pixel and aligned to even
    /// coordinates so the 4:2:0 conversion that follows has whole chroma
    /// blocks to work with.
    ///
    /// The damage is a parameter rather than read from the frame, because the
    /// caller usually knows more than the frame does: a gate that skipped
    /// frames has *accumulated* damage across them, and scaling only what the
    /// newest frame reported would drop the changes the skipped ones carried.
    pub fn scale(&mut self, frame: &Frame, damage: &Damage) -> Result<&Frame, ScaleError> {
        if frame.width() != self.x.src() || frame.height() != self.y.src() {
            return Err(ScaleError::WrongSize {
                want_w: self.x.src(),
                want_h: self.y.src(),
                got_w: frame.width(),
                got_h: frame.height(),
            });
        }
        if frame.format() != self.input {
            return Err(ScaleError::WrongFormat {
                want: self.input.name(),
                got: frame.format().name(),
            });
        }

        let damage = self.map_damage(damage, frame.bounds());
        for rect in damage.rects(&self.out.bounds()) {
            self.fill(frame, &rect)?;
        }
        self.out.set_damage(damage);
        self.out.set_captured_at(frame.captured_at());
        Ok(&self.out)
    }

    /// The destination damage for a source damage record.
    fn map_damage(&self, damage: &Damage, _source: Rect) -> Damage {
        match damage {
            Damage::Nothing => Damage::Nothing,
            Damage::Everything => Damage::Everything,
            Damage::Regions(regions) => {
                let bounds = self.out.bounds();
                let mapped: Vec<Rect> = regions
                    .as_slice()
                    .iter()
                    .filter_map(|r| self.map_rect(r))
                    .collect();
                Regions::coalesce(&mapped, &bounds)
            }
        }
    }

    /// One source rectangle's destination footprint, aligned to even edges.
    fn map_rect(&self, rect: &Rect) -> Option<Rect> {
        let sx = u32::try_from(rect.x.max(0)).unwrap_or(0);
        let sy = u32::try_from(rect.y.max(0)).unwrap_or(0);
        let (dx, dw) = self.x.dst_range(sx, rect.width)?;
        let (dy, dh) = self.y.dst_range(sy, rect.height)?;
        let out = Rect {
            x: i32::try_from(dx).unwrap_or(i32::MAX),
            y: i32::try_from(dy).unwrap_or(i32::MAX),
            width: dw,
            height: dh,
        };
        out.align_out(2).clamp_to(&self.out.bounds())
    }

    /// Resample one destination rectangle.
    ///
    /// # Four lanes, not three
    ///
    /// The obvious accumulator holds the three colour channels. It is also
    /// three times slower than it needs to be: a three-element stride cannot
    /// be loaded into a vector register without a shuffle, so the whole loop
    /// stays scalar. Carrying the fourth byte along — alpha, which is
    /// discarded — makes every load and store a power of two wide and lets
    /// the compiler do four channels at once.
    ///
    /// It also removes the channel bookkeeping entirely. Lane *k* is byte *k*
    /// of the pixel, in and out, for every layout this accepts, because all
    /// four put alpha last. Nothing in here needs to know where red is; the
    /// output format is the input format, so the bytes land where they came
    /// from. The one place that would care is the final write, and it writes
    /// all four bytes at once.
    fn fill(&mut self, frame: &Frame, rect: &Rect) -> Result<(), ScaleError> {
        let x0 = u32::try_from(rect.x.max(0)).unwrap_or(0);
        let y0 = u32::try_from(rect.y.max(0)).unwrap_or(0);
        let x1 = x0.saturating_add(rect.width).min(self.x.dst());
        let y1 = y0.saturating_add(rect.height).min(self.y.dst());
        if x0 >= x1 || y0 >= y1 {
            return Ok(());
        }

        // The source columns this rectangle's destination columns read, as one
        // contiguous window, so a narrow region does not drive a full-width
        // vertical pass.
        let (win_start, _) = self.x.span(x0).unwrap_or((0, &[]));
        let win_end = self
            .x
            .span(x1.saturating_sub(1))
            .map_or(self.x.src(), |(s, w)| {
                s.saturating_add(u32::try_from(w.len()).unwrap_or(0))
            });
        let win_len = win_end.saturating_sub(win_start) as usize;

        let out_stride = self.out.stride();
        let from = (win_start as usize).saturating_mul(4);
        let to = from.saturating_add(win_len.saturating_mul(4));

        for dst_y in y0..y1 {
            let Some((src_y, wy)) = self.y.span(dst_y) else {
                continue;
            };

            // Vertical: weighted totals of the covered source rows, over the
            // column window only.
            //
            // Wrapping arithmetic, and it never wraps — see `MAX_TOTAL` and
            // the two tests that hold it. This workspace turns overflow
            // checks on in release on purpose, and a check on every multiply
            // and add here is a branch per lane per pixel that stops the loop
            // vectorising: measured, it is the difference between paying for
            // the resolution rung and being paid by it.
            let acc = self
                .acc
                .get_mut(..win_len.saturating_mul(4))
                .unwrap_or_default();
            let cells = acc.as_chunks_mut::<4>().0;

            for (tap, &wv) in wy.iter().enumerate() {
                let y = src_y.saturating_add(u32::try_from(tap).unwrap_or(0));
                let full = frame.row(y).ok_or(ScaleError::ShortFrame(y))?;
                let Some(window) = full.get(from..to) else {
                    return Err(ScaleError::ShortFrame(y));
                };
                let pixels = window.as_chunks::<4>().0;
                if tap == 0 {
                    // The first tap writes rather than adds, which saves a
                    // separate pass zeroing the accumulator for every
                    // destination row.
                    for (slot, px) in cells.iter_mut().zip(pixels) {
                        let [a, b, c, d] = *px;
                        *slot = [
                            u32::from(a).wrapping_mul(wv),
                            u32::from(b).wrapping_mul(wv),
                            u32::from(c).wrapping_mul(wv),
                            u32::from(d).wrapping_mul(wv),
                        ];
                    }
                } else {
                    for (slot, px) in cells.iter_mut().zip(pixels) {
                        let [a, b, c, d] = *px;
                        let [s0, s1, s2, s3] = *slot;
                        *slot = [
                            s0.wrapping_add(u32::from(a).wrapping_mul(wv)),
                            s1.wrapping_add(u32::from(b).wrapping_mul(wv)),
                            s2.wrapping_add(u32::from(c).wrapping_mul(wv)),
                            s3.wrapping_add(u32::from(d).wrapping_mul(wv)),
                        ];
                    }
                }
            }

            // Horizontal: weighted totals of the accumulator, into the row.
            let row_at = (dst_y as usize).saturating_mul(out_stride);
            let acc_ro = self
                .acc
                .get(..win_len.saturating_mul(4))
                .unwrap_or_default();
            let source = acc_ro.as_chunks::<4>().0;
            let Some(row) = self
                .out
                .bytes_mut()
                .get_mut(row_at..row_at.saturating_add(out_stride))
            else {
                continue;
            };
            let out_pixels = row.as_chunks_mut::<4>().0;

            for ((src_x, wx), slot) in self
                .x
                .spans_in(x0, x1)
                .zip(out_pixels.iter_mut().skip(x0 as usize))
            {
                let offset = src_x.saturating_sub(win_start) as usize;
                let Some(cells) = source.get(offset..offset.saturating_add(wx.len())) else {
                    continue;
                };
                let mut t = [0u32; 4];
                for (cell, &wh) in cells.iter().zip(wx.iter()) {
                    let [c0, c1, c2, c3] = *cell;
                    let [t0, t1, t2, t3] = t;
                    t = [
                        t0.wrapping_add(c0.wrapping_mul(wh)),
                        t1.wrapping_add(c1.wrapping_mul(wh)),
                        t2.wrapping_add(c2.wrapping_mul(wh)),
                        t3.wrapping_add(c3.wrapping_mul(wh)),
                    ];
                }
                let [t0, t1, t2, _] = t;
                let done = |v: u32| {
                    u8::try_from((v.wrapping_add(ROUND)) >> (2 * SHIFT)).unwrap_or(u8::MAX)
                };
                // The fourth byte is alpha in every layout this accepts, and
                // is dropped rather than resampled: H.264 4:2:0 has none.
                *slot = [done(t0), done(t1), done(t2), 0xFF];
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::axis::ONE;

    const FORMATS: [PixelFormat; 4] = [
        PixelFormat::Bgra8,
        PixelFormat::Bgrx8,
        PixelFormat::Rgba8,
        PixelFormat::Rgbx8,
    ];

    /// A deterministic pseudorandom source image, held as RGB triples.
    struct Image {
        w: u32,
        h: u32,
        px: Vec<[u8; 3]>,
    }

    impl Image {
        fn noise(w: u32, h: u32, seed: u64) -> Self {
            let mut state = seed | 1;
            let mut next = || {
                state = state
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                u8::try_from((state >> 33) & 0xFF).unwrap_or(0)
            };
            let px = (0..(w as usize * h as usize))
                .map(|_| [next(), next(), next()])
                .collect();
            Self { w, h, px }
        }

        fn flat(w: u32, h: u32, colour: [u8; 3]) -> Self {
            Self {
                w,
                h,
                px: vec![colour; w as usize * h as usize],
            }
        }

        fn get(&self, x: u32, y: u32) -> [u8; 3] {
            self.px
                .get(y as usize * self.w as usize + x as usize)
                .copied()
                .unwrap_or([0, 0, 0])
        }

        fn set(&mut self, x: u32, y: u32, colour: [u8; 3]) {
            if let Some(slot) = self.px.get_mut(y as usize * self.w as usize + x as usize) {
                *slot = colour;
            }
        }

        /// Pack into a frame, optionally with a stride wider than the row.
        fn frame(&self, format: PixelFormat, damage: Damage, pad: usize) -> Frame {
            let (r, g, b) = format.rgb_offsets();
            let stride = self.w as usize * 4 + pad;
            let mut bytes = vec![0u8; stride * self.h as usize];
            for y in 0..self.h {
                for x in 0..self.w {
                    let c = self.get(x, y);
                    let at = y as usize * stride + x as usize * 4;
                    for (off, v) in [(r, c[0]), (g, c[1]), (b, c[2])] {
                        if let Some(slot) = bytes.get_mut(at + off) {
                            *slot = v;
                        }
                    }
                    if let Some(slot) = bytes.get_mut(at + 3) {
                        *slot = 0x80;
                    }
                }
            }
            Frame::new(self.w, self.h, stride, format, damage, Pixels::owned(bytes))
                .expect("a valid frame")
        }
    }

    /// Read a scaled frame back out as RGB triples.
    fn unpack(frame: &Frame) -> Vec<[u8; 3]> {
        let (r, g, b) = frame.format().rgb_offsets();
        let mut out = Vec::new();
        for y in 0..frame.height() {
            let row = frame.row(y).expect("row present");
            for x in 0..frame.width() {
                let at = x as usize * 4;
                out.push([
                    row.get(at + r).copied().unwrap_or(0),
                    row.get(at + g).copied().unwrap_or(0),
                    row.get(at + b).copied().unwrap_or(0),
                ]);
            }
        }
        out
    }

    /// Dense floating-point weights for one destination sample.
    fn weights(src: u32, dst: u32, i: u32) -> Vec<f64> {
        let ratio = f64::from(src) / f64::from(dst);
        let lo = f64::from(i) * ratio;
        let hi = f64::from(i + 1) * ratio;
        (0..src)
            .map(|j| {
                let a = lo.max(f64::from(j));
                let z = hi.min(f64::from(j + 1));
                (z - a).max(0.0) / (hi - lo)
            })
            .collect()
    }

    /// The exact area average, in floating point, for the whole image.
    fn reference(img: &Image, dst_w: u32, dst_h: u32) -> Vec<[f64; 3]> {
        let wx: Vec<Vec<f64>> = (0..dst_w).map(|i| weights(img.w, dst_w, i)).collect();
        let wy: Vec<Vec<f64>> = (0..dst_h).map(|i| weights(img.h, dst_h, i)).collect();
        let mut out = Vec::new();
        for dy in 0..dst_h as usize {
            for dx in 0..dst_w as usize {
                let mut acc = [0.0f64; 3];
                let cy = wy.get(dy).expect("row weights");
                let cx = wx.get(dx).expect("column weights");
                for (sy, &vy) in cy.iter().enumerate() {
                    if vy == 0.0 {
                        continue;
                    }
                    for (sx, &vx) in cx.iter().enumerate() {
                        if vx == 0.0 {
                            continue;
                        }
                        let c = img.get(
                            u32::try_from(sx).unwrap_or(0),
                            u32::try_from(sy).unwrap_or(0),
                        );
                        for (a, v) in acc.iter_mut().zip(c.iter()) {
                            *a += vy * vx * f64::from(*v);
                        }
                    }
                }
                out.push(acc);
            }
        }
        out
    }

    fn scale_once(img: &Image, dst_w: u32, dst_h: u32, format: PixelFormat) -> Vec<[u8; 3]> {
        let mut scaler = Scaler::new(img.w, img.h, dst_w, dst_h, format).expect("a valid scaler");
        let frame = img.frame(format, Damage::Everything, 0);
        unpack(scaler.scale(&frame, &Damage::Everything).expect("scales"))
    }

    #[test]
    fn identity_is_a_faithful_copy() {
        for format in FORMATS {
            let img = Image::noise(23, 17, 7);
            let got = scale_once(&img, 23, 17, format);
            let want: Vec<[u8; 3]> = (0..17)
                .flat_map(|y| (0..23).map(move |x| (x, y)))
                .map(|(x, y)| img.get(x, y))
                .collect();
            assert_eq!(got, want, "{} identity is not a copy", format.name());
        }
    }

    #[test]
    #[cfg_attr(miri, ignore = "an exhaustive sweep: minutes under MIRI")]
    fn a_flat_colour_scales_to_exactly_that_colour() {
        // The test that catches weights summing to slightly under one. A
        // uniform wall must not come out a shade darker, at any ratio.
        for colour in [[0, 0, 0], [255, 255, 255], [17, 200, 61], [1, 254, 128]] {
            for (sw, sh) in [(64, 64), (63, 31), (100, 40), (7, 13)] {
                for (dw, dh) in [(32, 32), (21, 10), (1, 1), (5, 7)] {
                    if dw > sw || dh > sh {
                        continue;
                    }
                    let img = Image::flat(sw, sh, colour);
                    let got = scale_once(&img, dw, dh, PixelFormat::Bgrx8);
                    assert!(
                        got.iter().all(|p| *p == colour),
                        "{sw}x{sh} -> {dw}x{dh} of {colour:?} drifted to {:?}",
                        got.first()
                    );
                }
            }
        }
    }

    #[test]
    fn halving_averages_two_by_two_blocks() {
        let mut img = Image::flat(4, 2, [0, 0, 0]);
        img.set(0, 0, [10, 20, 30]);
        img.set(1, 0, [14, 24, 34]);
        img.set(0, 1, [18, 28, 38]);
        img.set(1, 1, [22, 32, 42]);
        img.set(2, 0, [255, 0, 0]);
        img.set(3, 0, [255, 0, 0]);
        img.set(2, 1, [255, 0, 0]);
        img.set(3, 1, [255, 0, 0]);
        let got = scale_once(&img, 2, 1, PixelFormat::Bgrx8);
        assert_eq!(got.first(), Some(&[16, 26, 36]));
        assert_eq!(got.get(1), Some(&[255, 0, 0]));
    }

    #[test]
    #[cfg_attr(miri, ignore = "an exhaustive sweep: minutes under MIRI")]
    fn matches_the_area_reference_within_one_level() {
        // Fixed point against floating point, the same discipline the colour
        // conversion uses: the reference decides what the answer is, the
        // integer path has to land on it.
        let cases = [
            (64u32, 64u32, 32u32, 32u32),
            (64, 64, 48, 48),
            (100, 60, 75, 45),
            (63, 31, 21, 7),
            (1920, 8, 1280, 4),
            (37, 41, 11, 13),
            (16, 16, 1, 1),
            (50, 50, 49, 49),
        ];
        for (sw, sh, dw, dh) in cases {
            let img = Image::noise(sw, sh, u64::from(sw) * 31 + u64::from(dh));
            let got = scale_once(&img, dw, dh, PixelFormat::Rgbx8);
            let want = reference(&img, dw, dh);
            assert_eq!(got.len(), want.len());
            for (i, (g, w)) in got.iter().zip(want.iter()).enumerate() {
                for (c, (&gv, &wv)) in g.iter().zip(w.iter()).enumerate() {
                    let diff = (f64::from(gv) - wv).abs();
                    assert!(
                        diff <= 1.0,
                        "{sw}x{sh} -> {dw}x{dh} pixel {i} channel {c}: {gv} vs {wv:.4}"
                    );
                }
            }
        }
    }

    #[test]
    fn every_format_keeps_its_channel_order() {
        // A red source must not come out blue. The offsets are read from the
        // format for both the source and the destination, so a mismatch here
        // would be a silent channel swap on one backend only.
        for format in FORMATS {
            let img = Image::flat(8, 8, [200, 100, 50]);
            let got = scale_once(&img, 4, 4, format);
            assert!(
                got.iter().all(|p| *p == [200, 100, 50]),
                "{} swapped channels: {:?}",
                format.name(),
                got.first()
            );
        }
    }

    #[test]
    fn dropping_alpha_does_not_move_the_colour_channels() {
        // The scaler reads a pixel through the input format's offsets and
        // writes it through the same ones, which is only correct because
        // `without_alpha` changes what the fourth byte means and nothing
        // else. If it ever remapped a channel, every scaled frame would come
        // out with red and blue exchanged and the symmetry would hide it.
        for format in FORMATS {
            assert_eq!(
                format.rgb_offsets(),
                format.without_alpha().rgb_offsets(),
                "{} moves its channels when alpha is dropped",
                format.name()
            );
        }
    }

    #[test]
    fn alpha_is_dropped_and_the_format_says_so() {
        for format in FORMATS {
            let scaler = Scaler::new(8, 8, 4, 4, format).expect("valid");
            assert!(
                !scaler.out.format().has_alpha(),
                "{} claims the output has alpha",
                format.name()
            );
        }
        // And the fourth byte is written, not left as whatever was there.
        let img = Image::flat(8, 8, [1, 2, 3]);
        let mut scaler = Scaler::new(8, 8, 4, 4, PixelFormat::Bgra8).expect("valid");
        let frame = img.frame(PixelFormat::Bgra8, Damage::Everything, 0);
        let out = scaler.scale(&frame, &Damage::Everything).expect("scales");
        let row = out.row(0).expect("row");
        assert_eq!(row.get(3), Some(&0xFF));
    }

    #[test]
    fn a_padded_source_stride_is_honoured() {
        // A capture backend whose rows are padded to a page or a multiple of
        // sixteen. Reading it as tightly packed shears the picture.
        let img = Image::noise(30, 20, 99);
        let tight = scale_once(&img, 15, 10, PixelFormat::Bgrx8);
        let mut scaler = Scaler::new(30, 20, 15, 10, PixelFormat::Bgrx8).expect("valid");
        let padded = img.frame(PixelFormat::Bgrx8, Damage::Everything, 37);
        let got = unpack(scaler.scale(&padded, &Damage::Everything).expect("scales"));
        assert_eq!(got, tight);
    }

    #[test]
    #[cfg_attr(miri, ignore = "an exhaustive sweep: minutes under MIRI")]
    fn a_damaged_region_gives_the_same_result_as_a_full_rescale() {
        // The test that catches an under-covering damage map. A destination
        // pixel that averages a changed source pixel and is left out of the
        // damage keeps the previous frame's value forever, and it shows as a
        // small stale patch that only appears at some ratios.
        let cases = [
            (64u32, 64u32, 32u32, 32u32),
            (64, 64, 48, 48),
            (100, 60, 33, 20),
            (63, 47, 21, 15),
            (40, 40, 39, 39),
        ];
        let rects = [
            (0u32, 0u32, 1u32, 1u32),
            (0, 0, 7, 5),
            (5, 7, 3, 11),
            (13, 13, 1, 20),
            (29, 19, 9, 9),
            (0, 0, 64, 64),
        ];
        for (sw, sh, dw, dh) in cases {
            for (rx, ry, rw, rh) in rects {
                if rx + rw > sw || ry + rh > sh {
                    continue;
                }
                let before = Image::noise(sw, sh, 1234);
                let mut after = Image::noise(sw, sh, 1234);
                for y in ry..ry + rh {
                    for x in rx..rx + rw {
                        after.set(x, y, [255, 0, 255]);
                    }
                }

                // Incremental: scale the old frame, then the new one with only
                // the changed rectangle reported.
                let mut scaler = Scaler::new(sw, sh, dw, dh, PixelFormat::Bgrx8).expect("valid");
                let _ = scaler
                    .scale(
                        &before.frame(PixelFormat::Bgrx8, Damage::Everything, 0),
                        &Damage::Everything,
                    )
                    .expect("scales");
                let rect = Rect {
                    x: i32::try_from(rx).unwrap_or(0),
                    y: i32::try_from(ry).unwrap_or(0),
                    width: rw,
                    height: rh,
                };
                let damage = Regions::coalesce(
                    &[rect],
                    &Rect {
                        x: 0,
                        y: 0,
                        width: sw,
                        height: sh,
                    },
                );
                let incremental = unpack(
                    scaler
                        .scale(&after.frame(PixelFormat::Bgrx8, damage.clone(), 0), &damage)
                        .expect("scales"),
                );

                let full = scale_once(&after, dw, dh, PixelFormat::Bgrx8);
                assert_eq!(
                    incremental, full,
                    "{sw}x{sh} -> {dw}x{dh}, damage {rx},{ry} {rw}x{rh} left a stale patch"
                );
            }
        }
    }

    #[test]
    fn damage_stays_even_aligned_for_the_chroma_that_follows() {
        let mut scaler = Scaler::new(64, 64, 32, 32, PixelFormat::Bgrx8).expect("valid");
        let img = Image::noise(64, 64, 5);
        let rect = Rect {
            x: 7,
            y: 9,
            width: 3,
            height: 5,
        };
        let damage = Regions::coalesce(
            &[rect],
            &Rect {
                x: 0,
                y: 0,
                width: 64,
                height: 64,
            },
        );
        let out = scaler
            .scale(&img.frame(PixelFormat::Bgrx8, damage.clone(), 0), &damage)
            .expect("scales");
        for r in out.damage().rects(&out.bounds()) {
            assert_eq!(r.x % 2, 0, "{r:?} starts on an odd column");
            assert_eq!(r.y % 2, 0, "{r:?} starts on an odd row");
            assert_eq!(r.width % 2, 0, "{r:?} has an odd width");
            assert_eq!(r.height % 2, 0, "{r:?} has an odd height");
        }
    }

    #[test]
    fn nothing_changed_means_nothing_is_touched() {
        let mut scaler = Scaler::new(32, 32, 16, 16, PixelFormat::Bgrx8).expect("valid");
        let first = Image::flat(32, 32, [9, 9, 9]);
        let _ = scaler
            .scale(
                &first.frame(PixelFormat::Bgrx8, Damage::Everything, 0),
                &Damage::Everything,
            )
            .expect("scales");
        let second = Image::flat(32, 32, [200, 200, 200]);
        let out = scaler
            .scale(
                &second.frame(PixelFormat::Bgrx8, Damage::Nothing, 0),
                &Damage::Nothing,
            )
            .expect("scales");
        assert_eq!(*out.damage(), Damage::Nothing);
        // The pixels are the *previous* frame's, because nothing was reported
        // as changed. A consumer that encodes a frame with no damage is the
        // bug, not this.
        assert!(unpack(out).iter().all(|p| *p == [9, 9, 9]));
    }

    #[test]
    fn everything_changed_survives_the_mapping() {
        let mut scaler = Scaler::new(32, 32, 16, 16, PixelFormat::Bgrx8).expect("valid");
        let img = Image::noise(32, 32, 3);
        let out = scaler
            .scale(
                &img.frame(PixelFormat::Bgrx8, Damage::Everything, 0),
                &Damage::Everything,
            )
            .expect("scales");
        assert_eq!(*out.damage(), Damage::Everything);
    }

    #[test]
    fn the_same_input_scales_to_the_same_bytes() {
        let img = Image::noise(97, 61, 42);
        let a = scale_once(&img, 41, 27, PixelFormat::Bgrx8);
        let b = scale_once(&img, 41, 27, PixelFormat::Bgrx8);
        assert_eq!(a, b);
    }

    #[test]
    fn the_capture_time_is_the_sources() {
        let img = Image::noise(16, 16, 1);
        let frame = img.frame(PixelFormat::Bgrx8, Damage::Everything, 0);
        let want = frame.captured_at();
        let mut scaler = Scaler::new(16, 16, 8, 8, PixelFormat::Bgrx8).expect("valid");
        let out = scaler.scale(&frame, &Damage::Everything).expect("scales");
        assert_eq!(out.captured_at(), want);
    }

    #[test]
    fn a_frame_of_the_wrong_shape_is_refused() {
        let mut scaler = Scaler::new(32, 32, 16, 16, PixelFormat::Bgrx8).expect("valid");
        let wrong = Image::noise(31, 32, 1);
        assert_eq!(
            scaler
                .scale(
                    &wrong.frame(PixelFormat::Bgrx8, Damage::Everything, 0),
                    &Damage::Everything
                )
                .err(),
            Some(ScaleError::WrongSize {
                want_w: 32,
                want_h: 32,
                got_w: 31,
                got_h: 32
            })
        );
        let right = Image::noise(32, 32, 1);
        assert_eq!(
            scaler
                .scale(
                    &right.frame(PixelFormat::Rgba8, Damage::Everything, 0),
                    &Damage::Everything
                )
                .err(),
            Some(ScaleError::WrongFormat {
                want: "BGRX8",
                got: "RGBA8"
            })
        );
    }

    #[test]
    fn upscaling_is_refused_rather_than_invented() {
        assert!(matches!(
            Scaler::new(16, 16, 32, 16, PixelFormat::Bgrx8),
            Err(ScaleError::Axis(_))
        ));
        assert!(matches!(
            Scaler::new(16, 16, 16, 32, PixelFormat::Bgrx8),
            Err(ScaleError::Axis(_))
        ));
    }

    #[test]
    #[cfg_attr(miri, ignore = "an exhaustive sweep: minutes under MIRI")]
    fn awkward_sizes_do_not_panic() {
        for sw in 1..=9u32 {
            for sh in 1..=9u32 {
                for dw in 1..=sw {
                    for dh in 1..=sh {
                        let img = Image::noise(sw, sh, u64::from(sw * 100 + sh));
                        let _ = scale_once(&img, dw, dh, PixelFormat::Bgra8);
                    }
                }
            }
        }
    }

    #[test]
    fn the_target_and_source_are_reported() {
        let scaler = Scaler::new(100, 50, 25, 10, PixelFormat::Bgrx8).expect("valid");
        assert_eq!(scaler.source(), (100, 50));
        assert_eq!(scaler.target(), (25, 10));
        assert!(!scaler.is_identity());
        assert!(
            Scaler::new(8, 8, 8, 8, PixelFormat::Bgrx8)
                .expect("valid")
                .is_identity()
        );
    }

    #[test]
    fn the_rounding_term_matches_the_weight_scale() {
        // ROUND is half of the combined scale of two weights. If SHIFT ever
        // changes and this does not, every sample gains or loses half a level.
        assert_eq!(ROUND, ONE * ONE / 2);
    }
}
