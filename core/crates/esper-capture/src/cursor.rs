//! What the pointer looks like and where it is.
//!
//! # Not part of a frame, on purpose
//!
//! The pointer is deliberately absent from every captured frame — on X11 it is
//! a sprite the server composites on the way to the screen, so it was never in
//! the drawable — and it travels as its own tiny messages instead. A pointer
//! composited on the host costs a round trip, an encode and a decode for every
//! movement, and the session feels underwater however good the frame rate is.
//!
//! # Its own cadence
//!
//! A mouse moves faster than a screen changes. A cursor read once per frame
//! would be a pointer sampled at fifteen hertz on a quiet desktop, so a
//! [`CursorSource`] is polled on its own schedule rather than alongside
//! capture.
//!
//! # Capture types, not wire types
//!
//! These are what a backend can see. Turning them into protocol messages is
//! somebody else's job, the same way a [`crate::frame::Frame`] becomes
//! datagrams elsewhere: a capture backend has no business knowing the byte
//! order on the wire.

/// A pointer's pixels.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CursorImage {
    pub width: u16,
    pub height: u16,
    /// Where in the image the pointer actually points.
    pub hotspot_x: u16,
    pub hotspot_y: u16,
    /// **RGBA bytes, premultiplied, row-major, no padding.**
    ///
    /// Premultiplied because that is what the platform gave and what every
    /// consumer wants; converting to straight alpha and back would lose a bit
    /// of precision each way for nothing.
    pub rgba: Vec<u8>,
}

impl CursorImage {
    /// Whether every pixel is fully transparent.
    ///
    /// **This is how a hidden pointer looks on X11.** There is no "the cursor
    /// is hidden" question to ask a server: an application hides the pointer
    /// by setting a fully transparent cursor, so an invisible image *is* the
    /// hidden state, and saying so beats sending four kilobytes of zeroes and
    /// leaving the far end to work it out.
    #[must_use]
    pub fn is_invisible(&self) -> bool {
        self.rgba
            .as_chunks::<4>()
            .0
            .iter()
            .all(|px| px.get(3) == Some(&0))
    }

    /// Whether the pixels are the size the header claims.
    #[must_use]
    pub fn is_consistent(&self) -> bool {
        let wanted = usize::from(self.width)
            .saturating_mul(usize::from(self.height))
            .saturating_mul(4);
        self.width > 0
            && self.height > 0
            && self.hotspot_x < self.width
            && self.hotspot_y < self.height
            && self.rgba.len() == wanted
    }
}

/// The pointer as a backend last saw it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CursorState {
    /// The platform's own identifier for the shape.
    ///
    /// What makes "on change only" a comparison rather than a guess: a pointer
    /// crossing a text field and coming back alternates between two shapes
    /// many times a second, and comparing pixels to notice would cost more
    /// than sending them.
    pub serial: u32,
    /// `None` when there is no pointer to draw.
    pub image: Option<CursorImage>,
    /// Where it is, in desktop coordinates.
    pub x: i32,
    pub y: i32,
}

/// Somewhere to read the pointer from.
pub trait CursorSource: std::fmt::Debug + Send {
    /// Which backend this is, for the log.
    fn backend(&self) -> &'static str;

    /// The pointer now.
    ///
    /// `None` when this backend cannot see one at all — a synthetic screen has
    /// no pointer, and a session over it should say so rather than invent one.
    fn read(&mut self) -> Result<Option<CursorState>, crate::capturer::CaptureError>;
}

/// A source that reports whatever it is told to.
///
/// How the change detection and the rate limiting are tested without a
/// display: the interesting behaviour is *which* reads turn into messages, and
/// that is a function of what came back, not of where it came from.
#[derive(Debug, Default)]
pub struct ScriptedCursor {
    /// Reads to hand out, in order. The last one repeats once exhausted.
    pub reads: Vec<Option<CursorState>>,
    /// How many times it was read.
    pub reads_taken: usize,
}

impl ScriptedCursor {
    /// A source that hands out these, in order.
    #[must_use]
    pub fn new(reads: Vec<Option<CursorState>>) -> Self {
        Self {
            reads,
            reads_taken: 0,
        }
    }
}

impl CursorSource for ScriptedCursor {
    fn backend(&self) -> &'static str {
        "scripted"
    }

    fn read(&mut self) -> Result<Option<CursorState>, crate::capturer::CaptureError> {
        let at = self.reads_taken.min(self.reads.len().saturating_sub(1));
        self.reads_taken = self.reads_taken.saturating_add(1);
        Ok(self.reads.get(at).cloned().flatten())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    fn image(alpha: u8) -> CursorImage {
        CursorImage {
            width: 2,
            height: 2,
            hotspot_x: 0,
            hotspot_y: 0,
            rgba: (0..4).flat_map(|_| [1, 2, 3, alpha]).collect(),
        }
    }

    /// **An invisible cursor is a hidden pointer**, which is how every
    /// application on X11 hides one.
    #[test]
    fn a_fully_transparent_cursor_is_invisible() {
        assert!(image(0).is_invisible());
        assert!(!image(1).is_invisible());
        assert!(!image(255).is_invisible());
    }

    /// One opaque pixel in a transparent image is a pointer somebody can see,
    /// however faintly — a cursor that is nearly invisible is not hidden.
    #[test]
    fn one_visible_pixel_is_not_hidden() {
        let mut c = image(0);
        if let Some(a) = c.rgba.get_mut(3) {
            *a = 8;
        }
        assert!(!c.is_invisible());
    }

    #[test]
    fn a_cursor_whose_pixels_do_not_match_its_size_is_inconsistent() {
        let mut c = image(255);
        assert!(c.is_consistent());
        c.rgba.pop();
        assert!(!c.is_consistent());
        c.width = 0;
        assert!(!c.is_consistent());
    }

    #[test]
    fn a_hotspot_outside_the_image_is_inconsistent() {
        let mut c = image(255);
        c.hotspot_x = 2;
        assert!(!c.is_consistent());
    }

    #[test]
    fn a_scripted_source_hands_out_its_reads_and_then_repeats_the_last() {
        let a = CursorState {
            serial: 1,
            image: Some(image(255)),
            x: 10,
            y: 20,
        };
        let b = CursorState {
            serial: 2,
            image: None,
            x: 30,
            y: 40,
        };
        let mut s = ScriptedCursor::new(vec![Some(a.clone()), Some(b.clone())]);
        assert_eq!(s.read().expect("read"), Some(a));
        assert_eq!(s.read().expect("read"), Some(b.clone()));
        assert_eq!(s.read().expect("read"), Some(b), "the last one repeats");
        assert_eq!(s.reads_taken, 3);
    }
}
