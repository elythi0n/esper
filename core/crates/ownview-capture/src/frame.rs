//! A captured frame, and the pixel layout it arrived in.
//!
//! # Stride is not width
//!
//! Capture buffers are almost never tightly packed. XShm pads scanlines to the
//! server's alignment, DXGI reports a pitch that is not the width, and a
//! PipeWire buffer carries its own stride. A consumer that assumes
//! `width * 4` reads the wrong bytes for every row after the first, and the
//! symptom is the classic diagonally-sheared image.
//!
//! So stride is a field, every row is reached through
//! [`Frame::row`], and there is a test that a padded buffer round-trips.
//!
//! # Padding is not alpha
//!
//! `BGRX` and `BGRA` have identical memory layouts and different meanings: in
//! the first, the fourth byte is undefined padding. Treating it as alpha
//! produces frames that are transparent or black in patches, depending on what
//! the driver happened to leave there, and the bug is intermittent because the
//! garbage depends on what used the memory before.
//!
//! The formats are therefore distinct, and [`PixelFormat::has_alpha`] is the
//! only thing allowed to decide.

use std::time::Instant;

use crate::damage::Damage;
use crate::geometry::Rect;
use crate::pool::Pixels;

/// How pixels are laid out in a captured frame.
///
/// Every variant is 32 bits per pixel, because every capture backend worth
/// supporting produces 32-bit packed pixels. The names read in memory order:
/// `Bgra8` is byte 0 blue, byte 1 green, byte 2 red, byte 3 alpha.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PixelFormat {
    /// Blue, green, red, alpha. X11 32-bit TrueColor with a real alpha
    /// channel, DXGI, ScreenCaptureKit.
    Bgra8,
    /// Blue, green, red, **undefined**. The common X11 and PipeWire case.
    Bgrx8,
    /// Red, green, blue, alpha.
    Rgba8,
    /// Red, green, blue, **undefined**.
    Rgbx8,
}

impl PixelFormat {
    /// Bytes per pixel. Four, for all of them.
    #[must_use]
    pub const fn bytes_per_pixel(self) -> usize {
        4
    }

    /// Whether the fourth byte means anything.
    ///
    /// The one question that must never be guessed: reading padding as alpha
    /// produces frames that are transparent in patches, intermittently,
    /// depending on what used the memory before.
    #[must_use]
    pub const fn has_alpha(self) -> bool {
        matches!(self, Self::Bgra8 | Self::Rgba8)
    }

    /// Byte offsets of red, green and blue within a pixel.
    #[must_use]
    pub const fn rgb_offsets(self) -> (usize, usize, usize) {
        match self {
            Self::Bgra8 | Self::Bgrx8 => (2, 1, 0),
            Self::Rgba8 | Self::Rgbx8 => (0, 1, 2),
        }
    }

    /// The same layout with the fourth byte declared meaningless.
    ///
    /// A backend that reports an alpha format but cannot guarantee the channel
    /// is initialised should say so with this rather than hope.
    #[must_use]
    pub const fn without_alpha(self) -> Self {
        match self {
            Self::Bgra8 | Self::Bgrx8 => Self::Bgrx8,
            Self::Rgba8 | Self::Rgbx8 => Self::Rgbx8,
        }
    }

    /// A short name for logs and the session HUD.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Bgra8 => "BGRA8",
            Self::Bgrx8 => "BGRX8",
            Self::Rgba8 => "RGBA8",
            Self::Rgbx8 => "RGBX8",
        }
    }
}

/// What went wrong describing a frame.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum FrameError {
    /// The stride is narrower than a row of pixels needs.
    #[error("stride {stride} is too small for {width} pixels at {bpp} bytes each")]
    StrideTooSmall {
        stride: usize,
        width: u32,
        bpp: usize,
    },
    /// The buffer is shorter than the dimensions require.
    #[error("a {width}x{height} frame with stride {stride} needs {needed} bytes, got {got}")]
    BufferTooSmall {
        width: u32,
        height: u32,
        stride: usize,
        needed: usize,
        got: usize,
    },
    /// The dimensions are unusable.
    #[error("a frame cannot be {width}x{height}")]
    EmptyFrame { width: u32, height: u32 },
}

/// One captured frame.
///
/// Owns its pixels. The buffer comes from a [`crate::pool::BufferPool`] in the
/// capture loop, so a steady stream of frames does not allocate.
pub struct Frame {
    width: u32,
    height: u32,
    stride: usize,
    format: PixelFormat,
    /// When the backend produced it, for the per-stage timings the pipeline
    /// reports. Monotonic, so it cannot go backwards over a clock change.
    captured_at: Instant,
    damage: Damage,
    data: Pixels,
}

impl std::fmt::Debug for Frame {
    /// Deliberately does not print the pixels.
    ///
    /// A `tracing::debug!(?frame)` on a 4K frame would write thirty-three
    /// megabytes of hex into a log, once per frame. What a reader wants is the
    /// shape and how much of it changed.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Frame")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("stride", &self.stride)
            .field("format", &self.format.name())
            .field("damage", &self.damage)
            .finish_non_exhaustive()
    }
}

impl Frame {
    /// Wrap a buffer, checking that it can hold what it claims to.
    ///
    /// The check is not a formality. Every later access indexes by stride and
    /// row, and a buffer one row short is an out-of-bounds read on the last
    /// row of every frame.
    pub fn new(
        width: u32,
        height: u32,
        stride: usize,
        format: PixelFormat,
        damage: Damage,
        data: Pixels,
    ) -> Result<Self, FrameError> {
        let frame = Self {
            width,
            height,
            stride,
            format,
            captured_at: Instant::now(),
            damage,
            data,
        };
        frame.validate()?;
        Ok(frame)
    }

    fn validate(&self) -> Result<(), FrameError> {
        if self.width == 0 || self.height == 0 {
            return Err(FrameError::EmptyFrame {
                width: self.width,
                height: self.height,
            });
        }
        let bpp = self.format.bytes_per_pixel();
        let row_bytes = (self.width as usize).saturating_mul(bpp);
        if self.stride < row_bytes {
            return Err(FrameError::StrideTooSmall {
                stride: self.stride,
                width: self.width,
                bpp,
            });
        }
        // The last row needs `row_bytes`, not a whole stride: a tightly-packed
        // buffer of exactly `height * stride` is normal, but so is one that
        // stops after the last row's pixels. Requiring the padding of the
        // final row would reject valid buffers.
        let needed = self
            .stride
            .saturating_mul((self.height as usize).saturating_sub(1))
            .saturating_add(row_bytes);
        let got = self.data.len();
        if got < needed {
            return Err(FrameError::BufferTooSmall {
                width: self.width,
                height: self.height,
                stride: self.stride,
                needed,
                got,
            });
        }
        Ok(())
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

    /// Bytes per row, including any padding.
    #[must_use]
    pub const fn stride(&self) -> usize {
        self.stride
    }

    /// The pixel layout.
    #[must_use]
    pub const fn format(&self) -> PixelFormat {
        self.format
    }

    /// What changed since the previous frame.
    #[must_use]
    pub const fn damage(&self) -> &Damage {
        &self.damage
    }

    /// When the backend produced it.
    #[must_use]
    pub const fn captured_at(&self) -> Instant {
        self.captured_at
    }

    /// The whole frame as a rectangle, for clamping and damage arithmetic.
    #[must_use]
    pub const fn bounds(&self) -> Rect {
        Rect::sized(self.width, self.height)
    }

    /// Bytes in one row of pixels, excluding padding.
    #[must_use]
    pub const fn row_bytes(&self) -> usize {
        (self.width as usize).saturating_mul(self.format.bytes_per_pixel())
    }

    /// One row's pixels, without its padding.
    ///
    /// The only way to reach a row. A caller doing its own stride arithmetic is
    /// a caller that will eventually get it wrong, and the wrong answer looks
    /// like a sheared image rather than a crash.
    #[must_use]
    pub fn row(&self, y: u32) -> Option<&[u8]> {
        if y >= self.height {
            return None;
        }
        let start = self.stride.checked_mul(y as usize)?;
        let end = start.checked_add(self.row_bytes())?;
        self.data.get(start..end)
    }

    /// One row's pixels, mutably.
    pub fn row_mut(&mut self, y: u32) -> Option<&mut [u8]> {
        if y >= self.height {
            return None;
        }
        let start = self.stride.checked_mul(y as usize)?;
        let end = start.checked_add(self.row_bytes())?;
        self.data.get_mut(start..end)
    }

    /// One pixel, in the frame's own format order.
    #[must_use]
    pub fn pixel(&self, x: u32, y: u32) -> Option<[u8; 4]> {
        if x >= self.width {
            return None;
        }
        let bpp = self.format.bytes_per_pixel();
        let at = (x as usize).checked_mul(bpp)?;
        let row = self.row(y)?;
        let px = row.get(at..at.checked_add(bpp)?)?;
        <[u8; 4]>::try_from(px).ok()
    }

    /// The whole buffer, padding included.
    ///
    /// For a consumer that does its own stride arithmetic — a colour
    /// conversion doing whole rows at a time, which is the one place it is
    /// worth the risk.
    pub fn bytes(&self) -> &[u8] {
        &self.data
    }

    /// The whole buffer, mutably.
    pub fn bytes_mut(&mut self) -> &mut [u8] {
        &mut self.data
    }

    /// Replace the damage record.
    ///
    /// For a stage between capture and encode that knows more than the backend
    /// did — the first frame of a session is `Everything` however little the
    /// compositor reported.
    pub fn set_damage(&mut self, damage: Damage) {
        self.damage = damage;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn buffer(n: usize) -> Pixels {
        Pixels::zeroed(n)
    }

    fn frame(w: u32, h: u32, stride: usize) -> Result<Frame, FrameError> {
        let needed = stride * (h as usize - 1) + (w as usize) * 4;
        Frame::new(
            w,
            h,
            stride,
            PixelFormat::Bgrx8,
            Damage::Everything,
            buffer(needed),
        )
    }

    #[test]
    fn a_tightly_packed_frame_is_accepted() {
        let f = frame(64, 32, 64 * 4).expect("valid");
        assert_eq!(f.width(), 64);
        assert_eq!(f.height(), 32);
        assert_eq!(f.row_bytes(), 256);
        assert_eq!(f.bounds(), Rect::sized(64, 32));
    }

    /// The normal case for a real backend: rows padded to an alignment.
    #[test]
    fn a_padded_frame_is_accepted_and_rows_skip_the_padding() {
        let f = frame(10, 4, 64).expect("valid");
        assert_eq!(f.stride(), 64);
        assert_eq!(
            f.row(0).map(<[u8]>::len),
            Some(40),
            "a row is pixels, not stride"
        );
        assert_eq!(f.row(3).map(<[u8]>::len), Some(40));
        assert_eq!(f.row(4), None, "one past the last row");
    }

    /// **The sheared-image bug.** Rows must be found by stride, not by width,
    /// and this is the test that says so: write a marker at the start of every
    /// row and read it back.
    #[test]
    fn rows_are_addressed_by_stride_not_by_width() {
        let mut f = frame(10, 8, 64).expect("valid");
        for y in 0..8u32 {
            let row = f.row_mut(y).expect("row");
            if let Some(first) = row.first_mut() {
                *first = 100 + y as u8;
            }
        }
        for y in 0..8u32 {
            assert_eq!(
                f.row(y).and_then(|r| r.first().copied()),
                Some(100 + y as u8),
                "row {y} was found at the wrong offset"
            );
        }
        // And the padding between rows was never touched.
        let raw: Vec<u8> = f.bytes().to_vec();
        for y in 0..7usize {
            let pad = raw.get(y * 64 + 40..y * 64 + 64).expect("padding");
            assert!(
                pad.iter().all(|b| *b == 0),
                "row {y}'s padding was written into"
            );
        }
    }

    #[test]
    fn a_stride_narrower_than_a_row_is_rejected() {
        let err = Frame::new(
            100,
            10,
            399,
            PixelFormat::Bgra8,
            Damage::Everything,
            buffer(1_000_000),
        )
        .expect_err("399 < 400");
        assert!(matches!(err, FrameError::StrideTooSmall { .. }), "{err:?}");
    }

    /// A buffer one row short is an out-of-bounds read on the last row of
    /// every frame, so it is refused at construction rather than discovered
    /// later.
    #[test]
    fn a_buffer_too_small_for_its_dimensions_is_rejected() {
        let err = Frame::new(
            64,
            32,
            256,
            PixelFormat::Bgra8,
            Damage::Everything,
            buffer(256 * 31),
        )
        .expect_err("one row short");
        match err {
            FrameError::BufferTooSmall { needed, got, .. } => {
                assert_eq!(needed, 256 * 32);
                assert_eq!(got, 256 * 31);
            }
            other => panic!("{other:?}"),
        }
    }

    /// A buffer that stops after the last row's pixels, with no trailing
    /// padding, is a real and valid shape — requiring the final row's padding
    /// would reject it.
    #[test]
    fn a_buffer_without_padding_after_the_last_row_is_accepted() {
        let exact = 64 * 3 + 40;
        Frame::new(
            10,
            4,
            64,
            PixelFormat::Bgrx8,
            Damage::Everything,
            buffer(exact),
        )
        .expect("the last row needs pixels, not padding");
    }

    #[test]
    fn a_zero_sized_frame_is_rejected() {
        for (w, h) in [(0u32, 10u32), (10, 0), (0, 0)] {
            let err = Frame::new(
                w,
                h,
                64,
                PixelFormat::Bgra8,
                Damage::Everything,
                buffer(1024),
            )
            .expect_err("{w}x{h}");
            assert!(matches!(err, FrameError::EmptyFrame { .. }), "{err:?}");
        }
    }

    #[test]
    fn pixels_are_read_in_the_frames_own_byte_order() {
        let mut f = frame(4, 2, 16).expect("valid");
        if let Some(row) = f.row_mut(1) {
            for (i, b) in row.iter_mut().enumerate().take(8) {
                *b = i as u8;
            }
        }
        assert_eq!(f.pixel(0, 1), Some([0, 1, 2, 3]));
        assert_eq!(f.pixel(1, 1), Some([4, 5, 6, 7]));
        assert_eq!(f.pixel(4, 1), None, "one past the last column");
        assert_eq!(f.pixel(0, 2), None, "one past the last row");
    }

    /// The distinction that produces intermittently transparent frames when it
    /// is ignored.
    #[test]
    fn padding_is_not_alpha() {
        assert!(PixelFormat::Bgra8.has_alpha());
        assert!(PixelFormat::Rgba8.has_alpha());
        assert!(
            !PixelFormat::Bgrx8.has_alpha(),
            "the fourth byte of BGRX is whatever used the memory before"
        );
        assert!(!PixelFormat::Rgbx8.has_alpha());

        // And a backend that cannot vouch for the channel can say so.
        assert_eq!(PixelFormat::Bgra8.without_alpha(), PixelFormat::Bgrx8);
        assert_eq!(PixelFormat::Rgba8.without_alpha(), PixelFormat::Rgbx8);
        assert_eq!(
            PixelFormat::Bgrx8.without_alpha(),
            PixelFormat::Bgrx8,
            "and doing it twice changes nothing"
        );
    }

    /// Colour channel order is the other half of the same problem: getting it
    /// wrong swaps red and blue, which looks like a colour-space bug and is
    /// not one.
    #[test]
    fn channel_offsets_match_the_formats_name() {
        assert_eq!(PixelFormat::Bgra8.rgb_offsets(), (2, 1, 0));
        assert_eq!(PixelFormat::Bgrx8.rgb_offsets(), (2, 1, 0));
        assert_eq!(PixelFormat::Rgba8.rgb_offsets(), (0, 1, 2));
        assert_eq!(PixelFormat::Rgbx8.rgb_offsets(), (0, 1, 2));

        for f in [
            PixelFormat::Bgra8,
            PixelFormat::Bgrx8,
            PixelFormat::Rgba8,
            PixelFormat::Rgbx8,
        ] {
            let (r, g, b) = f.rgb_offsets();
            assert!(r < 4 && g < 4 && b < 4, "{}", f.name());
            assert!(r != g && g != b && r != b, "{} repeats a channel", f.name());
            assert_eq!(f.bytes_per_pixel(), 4);
        }
    }

    /// A frame's `Debug` must not be able to write a screenful of hex into a
    /// log line.
    #[test]
    fn debug_does_not_print_the_pixels() {
        let f = frame(1920, 1080, 1920 * 4).expect("valid");
        let s = format!("{f:?}");
        assert!(s.len() < 300, "Debug produced {} characters", s.len());
        assert!(s.contains("1920") && s.contains("BGRX8"), "{s}");
    }
}
