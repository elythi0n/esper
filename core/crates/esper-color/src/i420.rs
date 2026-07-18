//! The planar YUV buffer an H.264 encoder takes.
//!
//! I420: a full-resolution luma plane, then two chroma planes at half
//! resolution in both directions. 4:2:0, which is the only chroma format
//! baseline and main profile H.264 support and therefore the only one that
//! matters for v1.
//!
//! # Why the buffer is reused
//!
//! It is 1.5 bytes per pixel — 3 MB at 1080p, 12 MB at 4K — and it is written
//! every frame. Allocating one per frame is the same page-fault jitter the
//! frame pool exists to avoid, in the stage whose budget is one millisecond.
//!
//! Reuse also makes **damage-driven conversion** possible, which is the larger
//! win: if the buffer still holds the previous frame, only the regions that
//! changed need converting. On a screen where a cursor is blinking, that is a
//! few hundred pixels instead of two million.

/// A planar 4:2:0 image.
///
/// Dimensions are the *encoder's*, which are the capture dimensions rounded up
/// to even. See [`I420::new`].
pub struct I420 {
    width: u32,
    height: u32,
    y: Vec<u8>,
    u: Vec<u8>,
    v: Vec<u8>,
}

impl std::fmt::Debug for I420 {
    /// The shape, never the pixels.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("I420")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("bytes", &self.byte_len())
            .finish()
    }
}

/// What went wrong.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum I420Error {
    /// Zero-sized.
    #[error("an image cannot be {width}x{height}")]
    Empty { width: u32, height: u32 },
    /// Too large for this machine's address space.
    #[error("a {width}x{height} image does not fit in memory")]
    TooLarge { width: u32, height: u32 },
}

impl I420 {
    /// An image of this size, with every plane allocated.
    ///
    /// **Dimensions are rounded up to even.** 4:2:0 has one chroma sample per
    /// 2×2 block of luma, so an odd width or height has no representation —
    /// and screens are odd-sized more often than one would hope, since a
    /// compositor is free to hand out a 1919×1079 window. The extra row or
    /// column is filled by repeating the edge and cropped out by the
    /// bitstream's own cropping fields, which is what every encoder does.
    pub fn new(width: u32, height: u32) -> Result<Self, I420Error> {
        if width == 0 || height == 0 {
            return Err(I420Error::Empty { width, height });
        }
        let width = round_up_even(width);
        let height = round_up_even(height);

        let luma = (width as usize)
            .checked_mul(height as usize)
            .ok_or(I420Error::TooLarge { width, height })?;
        // Each chroma plane is a quarter of the luma plane. Exact, because
        // both dimensions are even.
        let chroma = luma / 4;

        Ok(Self {
            width,
            height,
            y: vec![0u8; luma],
            u: vec![128u8; chroma],
            v: vec![128u8; chroma],
        })
    }

    /// Resize in place, keeping the allocation where possible.
    ///
    /// Returns whether the dimensions changed, so a caller can invalidate
    /// whatever it was caching about the previous shape — including its belief
    /// that the buffer holds a usable previous frame, which after a resize it
    /// does not.
    pub fn resize(&mut self, width: u32, height: u32) -> Result<bool, I420Error> {
        if width == 0 || height == 0 {
            return Err(I420Error::Empty { width, height });
        }
        let width = round_up_even(width);
        let height = round_up_even(height);
        if width == self.width && height == self.height {
            return Ok(false);
        }
        let luma = (width as usize)
            .checked_mul(height as usize)
            .ok_or(I420Error::TooLarge { width, height })?;
        self.width = width;
        self.height = height;
        self.y.clear();
        self.y.resize(luma, 0);
        self.u.clear();
        self.u.resize(luma / 4, 128);
        self.v.clear();
        self.v.resize(luma / 4, 128);
        Ok(true)
    }

    /// Encoder width, always even.
    #[must_use]
    pub const fn width(&self) -> u32 {
        self.width
    }

    /// Encoder height, always even.
    #[must_use]
    pub const fn height(&self) -> u32 {
        self.height
    }

    /// Bytes per luma row.
    #[must_use]
    pub const fn y_stride(&self) -> usize {
        self.width as usize
    }

    /// Bytes per chroma row.
    #[must_use]
    pub const fn uv_stride(&self) -> usize {
        (self.width / 2) as usize
    }

    /// Chroma plane height.
    #[must_use]
    pub const fn uv_height(&self) -> u32 {
        self.height / 2
    }

    /// Total bytes across all three planes.
    #[must_use]
    pub const fn byte_len(&self) -> usize {
        self.y.len() + self.u.len() + self.v.len()
    }

    /// The luma plane.
    #[must_use]
    pub fn y(&self) -> &[u8] {
        &self.y
    }

    /// The Cb plane.
    #[must_use]
    pub fn u(&self) -> &[u8] {
        &self.u
    }

    /// The Cr plane.
    #[must_use]
    pub fn v(&self) -> &[u8] {
        &self.v
    }

    /// All three planes, mutably, for a converter to fill.
    ///
    /// Handed out together because a converter writing one plane and not the
    /// others leaves an image whose luma and chroma are from different frames,
    /// which renders as colour smearing that trails behind motion.
    pub fn planes_mut(&mut self) -> (&mut [u8], &mut [u8], &mut [u8]) {
        (&mut self.y, &mut self.u, &mut self.v)
    }

    /// One luma sample, for tests and for reading the frame counter back.
    #[must_use]
    pub fn luma_at(&self, x: u32, y: u32) -> Option<u8> {
        if x >= self.width || y >= self.height {
            return None;
        }
        self.y
            .get(
                (y as usize)
                    .checked_mul(self.y_stride())?
                    .checked_add(x as usize)?,
            )
            .copied()
    }

    /// The chroma pair covering a luma coordinate.
    #[must_use]
    pub fn chroma_at(&self, x: u32, y: u32) -> Option<(u8, u8)> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let at = ((y / 2) as usize)
            .checked_mul(self.uv_stride())?
            .checked_add((x / 2) as usize)?;
        Some((self.u.get(at).copied()?, self.v.get(at).copied()?))
    }
}

/// Round up to the next even number, saturating.
const fn round_up_even(v: u32) -> u32 {
    if v.is_multiple_of(2) {
        v
    } else {
        v.saturating_add(1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_planes_are_the_right_sizes() {
        let img = I420::new(1920, 1080).expect("valid");
        assert_eq!(img.y().len(), 1920 * 1080);
        assert_eq!(img.u().len(), 1920 * 1080 / 4);
        assert_eq!(img.v().len(), 1920 * 1080 / 4);
        assert_eq!(img.y_stride(), 1920);
        assert_eq!(img.uv_stride(), 960);
        assert_eq!(img.uv_height(), 540);
        // 1.5 bytes per pixel.
        assert_eq!(img.byte_len(), 1920 * 1080 * 3 / 2);
    }

    /// **A compositor will hand out an odd-sized window.** 4:2:0 has no
    /// representation for an odd dimension, so the buffer rounds up rather
    /// than crashing or silently truncating a row.
    #[test]
    fn odd_dimensions_are_rounded_up_to_even() {
        let img = I420::new(1919, 1079).expect("valid");
        assert_eq!((img.width(), img.height()), (1920, 1080));
        assert_eq!(img.y().len(), 1920 * 1080);

        // And the chroma planes stay exactly a quarter, which they only do
        // because both dimensions are even.
        assert_eq!(img.u().len() * 4, img.y().len());
        assert_eq!(img.v().len() * 4, img.y().len());
    }

    #[test]
    fn every_odd_size_gives_planes_that_divide_exactly() {
        for w in [1u32, 2, 3, 17, 63, 64, 65, 1919, 1920, 3839] {
            for h in [1u32, 2, 3, 17, 63, 64, 65, 1079, 1080, 2159] {
                let img = I420::new(w, h).expect("valid");
                assert!(img.width().is_multiple_of(2), "{w}x{h}");
                assert!(img.height().is_multiple_of(2), "{w}x{h}");
                assert_eq!(img.u().len() * 4, img.y().len(), "{w}x{h}");
                assert_eq!(img.u().len(), img.v().len(), "{w}x{h}");
                assert_eq!(
                    img.y().len(),
                    img.y_stride() * img.height() as usize,
                    "{w}x{h}"
                );
            }
        }
    }

    #[test]
    fn a_zero_dimension_is_refused() {
        assert_eq!(
            I420::new(0, 100).expect_err("zero width"),
            I420Error::Empty {
                width: 0,
                height: 100
            }
        );
        assert!(I420::new(100, 0).is_err());
    }

    /// A fresh buffer must be neutral grey, not a random colour: chroma of 128
    /// is colourless, and chroma of 0 is a bright green that would be the
    /// first thing anyone saw if a plane were left unwritten.
    #[test]
    fn a_fresh_buffer_is_neutral_rather_than_green() {
        let img = I420::new(16, 16).expect("valid");
        assert!(img.u().iter().all(|c| *c == 128));
        assert!(img.v().iter().all(|c| *c == 128));
        assert!(img.y().iter().all(|c| *c == 0));
    }

    #[test]
    fn resizing_reports_whether_the_shape_changed() {
        let mut img = I420::new(64, 48).expect("valid");
        assert_eq!(
            img.resize(64, 48),
            Ok(false),
            "the same size is not a change"
        );
        assert_eq!(
            img.resize(63, 47),
            Ok(false),
            "and neither is one that rounds to the same"
        );
        assert_eq!(img.resize(128, 96), Ok(true));
        assert_eq!((img.width(), img.height()), (128, 96));
        assert_eq!(img.y().len(), 128 * 96);
        assert_eq!(img.u().len(), 128 * 96 / 4);
    }

    /// A resize must leave every plane consistent, or the next conversion
    /// writes past the end of one of them.
    #[test]
    fn resizing_keeps_the_planes_consistent() {
        let mut img = I420::new(16, 16).expect("valid");
        for (w, h) in [(1920u32, 1080u32), (17, 17), (2, 2), (640, 480), (3, 1)] {
            let _ = img.resize(w, h).expect("valid");
            assert_eq!(img.y().len(), img.y_stride() * img.height() as usize);
            assert_eq!(
                img.u().len(),
                img.uv_stride() * img.uv_height() as usize,
                "{w}x{h}"
            );
            assert_eq!(img.u().len(), img.v().len());
        }
    }

    #[test]
    fn samples_are_read_from_the_right_place() {
        let mut img = I420::new(4, 4).expect("valid");
        {
            let (y, u, v) = img.planes_mut();
            for (i, p) in y.iter_mut().enumerate() {
                *p = i as u8;
            }
            for (i, p) in u.iter_mut().enumerate() {
                *p = 100 + i as u8;
            }
            for (i, p) in v.iter_mut().enumerate() {
                *p = 200 + i as u8;
            }
        }
        assert_eq!(img.luma_at(0, 0), Some(0));
        assert_eq!(img.luma_at(3, 0), Some(3));
        assert_eq!(
            img.luma_at(0, 1),
            Some(4),
            "the second row starts at stride"
        );
        assert_eq!(img.luma_at(4, 0), None);
        assert_eq!(img.luma_at(0, 4), None);

        // A 2x2 luma block shares one chroma pair.
        assert_eq!(img.chroma_at(0, 0), Some((100, 200)));
        assert_eq!(img.chroma_at(1, 1), Some((100, 200)));
        assert_eq!(img.chroma_at(2, 0), Some((101, 201)));
        assert_eq!(img.chroma_at(0, 2), Some((102, 202)));
    }

    #[test]
    fn debug_does_not_print_the_pixels() {
        let img = I420::new(1920, 1080).expect("valid");
        let s = format!("{img:?}");
        assert!(s.len() < 120, "{} characters", s.len());
        assert!(s.contains("1920"), "{s}");
    }
}
