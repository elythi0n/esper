//! The cursor, which is not video.
//!
//! # Why it travels on its own
//!
//! **The single biggest perceived-latency decision in the product.** A pointer
//! composited into the frame on the host costs a full round trip plus an
//! encode plus a decode for every movement: the pointer lags the physical
//! mouse and the session feels underwater however good the frame rate is.
//!
//! So the shape travels when it changes and the position travels when it
//! moves, both as tiny control messages, and the controller draws the pointer
//! itself — immediately, over whatever frame it currently has. The pointer
//! moves at the speed of the local mouse and the screen catches up.
//!
//! # Hidden is a state, not an empty picture
//!
//! Games and drawing tools hide the pointer. On X11 that is done by setting a
//! fully transparent cursor, so a host could send 4 KB of zeroes and leave the
//! controller to work out what it meant. [`CursorImage::Hidden`] says it
//! instead: one byte on the wire, and no ambiguity about whether a transparent
//! cursor is a hidden pointer or a mistake.
//!
//! # What is deliberately not here
//!
//! A "the pointer is confined" flag. X11 offers no way to ask whether another
//! client holds a pointer grab — the only way to find out is to try to take
//! one, which would steal it — so a host cannot populate such a field
//! honestly, and a field that is always `false` is worse than no field.
//!
//! The controller can tell anyway, from evidence it already has: it knows
//! where it asked the pointer to go, and [`CursorPos`] says where the pointer
//! actually is. When those disagree, something on the host is holding the
//! pointer and a locally drawn cursor would be lying.

use serde::{Deserialize, Serialize};

use crate::Role;

/// The largest cursor this protocol carries, per side.
///
/// X servers cap cursors at what the driver reports and 256 is the usual
/// answer; nothing in a desktop is bigger. The bound is what stops a peer
/// announcing a 4096-square "cursor" and asking for 64 MB.
pub const MAX_CURSOR_EDGE: u16 = 256;

/// What a cursor looks like, or that there is nothing to look at.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CursorImage {
    /// There is no pointer to draw.
    Hidden,
    /// Pixels, with the point that is *the* point.
    Bitmap {
        width: u16,
        height: u16,
        /// Where in the image the pointer actually points, from the top left.
        hotspot_x: u16,
        hotspot_y: u16,
        /// **RGBA bytes, premultiplied, row-major, no padding.**
        ///
        /// Premultiplied because that is what every compositor and both
        /// platforms' cursor APIs want, so straight alpha would mean
        /// converting twice and losing precision on the way. Byte order is
        /// named rather than native: an ARGB `u32` means different bytes on
        /// different machines, which is exactly the class of bug a protocol
        /// exists to rule out.
        #[serde(with = "serde_bytes")]
        rgba: Vec<u8>,
    },
}

/// A cursor's appearance, sent when it changes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CursorShape {
    /// The host's own identifier for this shape.
    ///
    /// Carried so a controller can recognise a shape it already has without
    /// comparing pixels — a pointer crossing a text field and coming back
    /// alternates between two shapes many times a second, and re-sending
    /// either would be pointless. The host sends a shape when the serial
    /// changes; the controller may cache by serial.
    pub serial: u32,
    pub image: CursorImage,
}

impl CursorShape {
    /// Whether this is within bounds.
    #[must_use]
    pub fn is_well_formed(&self) -> bool {
        match &self.image {
            CursorImage::Hidden => true,
            CursorImage::Bitmap {
                width,
                height,
                hotspot_x,
                hotspot_y,
                rgba,
            } => {
                let sized = *width > 0
                    && *height > 0
                    && *width <= MAX_CURSOR_EDGE
                    && *height <= MAX_CURSOR_EDGE;
                // The hotspot has to be *in* the image: a pointer that points
                // outside itself is not a thing, and a consumer that trusted
                // it would index past the pixels.
                let pointed = hotspot_x < width && hotspot_y < height;
                // And the pixels have to be exactly the size the header
                // claims — not more, not fewer.
                let wanted = usize::from(*width)
                    .saturating_mul(usize::from(*height))
                    .saturating_mul(4);
                sized && pointed && rgba.len() == wanted
            }
        }
    }
}

/// Where the pointer is, as a fraction of one monitor.
///
/// The same fixed point as [`crate::PointerMove`], and for the same reasons: a
/// float rounds differently in two languages, and a fraction survives the
/// picture being a different size from the screen — which it is by default,
/// since the resolution ceiling scales anything above 1920.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CursorPos {
    /// Which monitor the pointer is on.
    ///
    /// **Not necessarily the one being captured.** A pointer that has moved to
    /// another screen is not in the picture at all, and a controller that drew
    /// it anyway would show a pointer that is not there.
    pub monitor: u32,
    pub x: u16,
    pub y: u16,
}

impl CursorPos {
    /// Where this lands on a monitor of a known size.
    #[must_use]
    pub fn to_pixels(&self, width: u32, height: u32) -> (u32, u32) {
        crate::PointerMove {
            monitor: self.monitor,
            x: self.x,
            y: self.y,
        }
        .to_pixels(width, height)
    }

    /// A position from pixels on a monitor of a known size.
    #[must_use]
    pub fn from_pixels(monitor: u32, x: i32, y: i32, width: u32, height: u32) -> Self {
        let p = crate::PointerMove::from_pixels(monitor, x, y, width, height);
        Self {
            monitor: p.monitor,
            x: p.x,
            y: p.y,
        }
    }
}

/// Anything the host says about the pointer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CursorMessage {
    /// What it looks like now.
    Shape(CursorShape),
    /// Where it is now.
    Moved(CursorPos),
}

impl CursorMessage {
    /// Whether this message is within bounds.
    #[must_use]
    pub fn is_well_formed(&self) -> bool {
        match self {
            Self::Shape(s) => s.is_well_formed(),
            // A monitor that does not exist is not malformed: the controller
            // simply has no picture of it, and stops drawing.
            Self::Moved(_) => true,
        }
    }

    /// Whether the given role is allowed to send this.
    ///
    /// Host to controller only. A controller telling the host what its own
    /// pointer looks like is nonsense; a controller *moving* the host's
    /// pointer is [`crate::InputMessage::Pointer`], which is a different
    /// thing travelling the other way.
    #[must_use]
    pub const fn may_be_sent_by(&self, role: Role) -> bool {
        matches!(role, Role::Host)
    }

    /// A name for an error message.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        match self {
            Self::Shape(_) => "a cursor shape",
            Self::Moved(_) => "a cursor position",
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    fn bitmap(width: u16, height: u16, hotspot_x: u16, hotspot_y: u16) -> CursorImage {
        CursorImage::Bitmap {
            width,
            height,
            hotspot_x,
            hotspot_y,
            rgba: vec![0; usize::from(width) * usize::from(height) * 4],
        }
    }

    fn shape(image: CursorImage) -> CursorShape {
        CursorShape { serial: 7, image }
    }

    #[test]
    fn an_ordinary_cursor_is_well_formed() {
        assert!(shape(bitmap(24, 24, 0, 0)).is_well_formed());
        assert!(shape(bitmap(32, 24, 15, 23)).is_well_formed());
        assert!(shape(CursorImage::Hidden).is_well_formed());
    }

    /// **The pixels have to be exactly what the header claims.** A consumer
    /// that trusted the width and height would read past the end of a short
    /// buffer, or leave a long one partly unread and show a corrupt pointer.
    #[test]
    fn pixels_that_do_not_match_the_size_are_refused() {
        for (w, h, len) in [
            (24u16, 24u16, 0usize),
            (24, 24, 24 * 24 * 4 - 1),
            (24, 24, 24 * 24 * 4 + 1),
            (16, 16, 24 * 24 * 4),
        ] {
            let image = CursorImage::Bitmap {
                width: w,
                height: h,
                hotspot_x: 0,
                hotspot_y: 0,
                rgba: vec![0; len],
            };
            assert!(
                !shape(image).is_well_formed(),
                "{w}x{h} with {len} bytes must be refused"
            );
        }
    }

    /// A pointer that points outside its own image is not a thing, and a
    /// consumer using the hotspot as an offset would index past the pixels.
    #[test]
    fn a_hotspot_outside_the_image_is_refused() {
        assert!(!shape(bitmap(16, 16, 16, 0)).is_well_formed());
        assert!(!shape(bitmap(16, 16, 0, 16)).is_well_formed());
        assert!(!shape(bitmap(16, 16, 999, 999)).is_well_formed());
        // The last pixel is a legal hotspot.
        assert!(shape(bitmap(16, 16, 15, 15)).is_well_formed());
    }

    /// **The bound that stops a peer asking for sixty-four megabytes.**
    #[test]
    fn a_cursor_larger_than_any_cursor_is_refused() {
        let over = MAX_CURSOR_EDGE + 1;
        // Built by hand rather than through the helper: allocating the pixels
        // for a refused size is the thing being avoided.
        let image = CursorImage::Bitmap {
            width: over,
            height: 8,
            hotspot_x: 0,
            hotspot_y: 0,
            rgba: vec![0; usize::from(over) * 8 * 4],
        };
        assert!(!shape(image).is_well_formed());
        // And the largest legal one is legal.
        assert!(shape(bitmap(MAX_CURSOR_EDGE, 1, 0, 0)).is_well_formed());
    }

    #[test]
    fn a_cursor_with_no_size_is_refused() {
        assert!(!shape(bitmap(0, 16, 0, 0)).is_well_formed());
        assert!(!shape(bitmap(16, 0, 0, 0)).is_well_formed());
    }

    /// **The pointer goes one way.** A controller describing the host's
    /// pointer would be describing a pointer it does not have; moving it is a
    /// different message travelling the other way.
    #[test]
    fn cursor_messages_are_host_to_controller() {
        let messages = [
            CursorMessage::Shape(shape(CursorImage::Hidden)),
            CursorMessage::Moved(CursorPos {
                monitor: 0,
                x: 0,
                y: 0,
            }),
        ];
        for m in messages {
            assert!(m.may_be_sent_by(Role::Host), "{}", m.name());
            assert!(!m.may_be_sent_by(Role::Controller), "{}", m.name());
        }
    }

    /// A position is the same arithmetic as a pointer *move*, so a cursor
    /// reported at a pixel lands on that pixel — including the last one.
    #[test]
    fn a_position_survives_the_round_trip() {
        for (w, h) in [(640u32, 480u32), (1920, 1080), (2560, 1440)] {
            for (x, y) in [(0u32, 0u32), (1, 1), (w / 2, h / 2), (w - 1, h - 1)] {
                let p = CursorPos::from_pixels(3, x as i32, y as i32, w, h);
                assert_eq!(p.monitor, 3);
                assert_eq!(p.to_pixels(w, h), (x, y), "{x},{y} on {w}x{h}");
            }
        }
    }

    #[test]
    fn every_cursor_message_survives_the_wire() {
        let shapes = [
            CursorMessage::Shape(CursorShape {
                serial: 0,
                image: CursorImage::Hidden,
            }),
            CursorMessage::Shape(CursorShape {
                serial: u32::MAX,
                image: CursorImage::Bitmap {
                    width: 2,
                    height: 2,
                    hotspot_x: 1,
                    hotspot_y: 0,
                    rgba: (0..16u8).collect(),
                },
            }),
            CursorMessage::Moved(CursorPos {
                monitor: 9,
                x: 12345,
                y: u16::MAX,
            }),
        ];
        for m in shapes {
            let bytes = crate::encode(&m).expect("encode");
            let back: CursorMessage = crate::decode(&bytes).expect("decode");
            assert_eq!(back, m);
        }
    }

    /// **The pixels travel as bytes, not as numbers.** A cursor encoded as
    /// `u32`s would be a different picture on a big-endian machine, and CBOR
    /// would spend a header per pixel besides. Checked by size: 4 bytes a
    /// pixel plus a small envelope, not 5 or more.
    #[test]
    fn the_pixels_travel_as_one_byte_string() {
        let m = CursorMessage::Shape(CursorShape {
            serial: 1,
            image: CursorImage::Bitmap {
                width: 32,
                height: 32,
                hotspot_x: 0,
                hotspot_y: 0,
                rgba: vec![0xAB; 32 * 32 * 4],
            },
        });
        let bytes = crate::encode(&m).expect("encode");
        let pixels = 32 * 32 * 4;
        assert!(
            bytes.len() < pixels + 128,
            "{} bytes for {pixels} pixels of cursor — the pixels are not being \
             carried as a byte string",
            bytes.len()
        );
    }
}
