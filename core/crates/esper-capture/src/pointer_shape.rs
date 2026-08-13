//! Turning a Desktop Duplication pointer shape into pixels.
//!
//! # Three shapes, and two of them are not images
//!
//! Duplication reports the pointer separately from the screen, which is
//! exactly what the protocol wants — a pointer moving must not cost a frame.
//! What it hands over is one of three things, and only one of them is a
//! picture:
//!
//! - **Colour**: 32-bit BGRA. Nearly an image already.
//! - **Monochrome**: two 1-bit masks, AND then XOR, stacked in one buffer.
//! - **Masked colour**: 32-bit BGRA where the alpha byte is a *mask* rather
//!   than an opacity.
//!
//! The two mask forms date from a time when a cursor was drawn by XORing it
//! onto the framebuffer, and both can ask for a pixel to be **inverted**
//! against whatever is behind it. RGBA cannot express that, and neither can
//! the protocol — see [`INVERTED`] for what is done instead and what it costs.
//!
//! # The traps
//!
//! **A monochrome shape's reported height is twice its real height.** The
//! buffer holds the AND mask and then the XOR mask, and
//! `DXGI_OUTDUPL_POINTER_SHAPE_INFO::Height` is the height of *both*. A
//! reader that believes it produces a cursor twice as tall as it should be,
//! made of the AND mask on top of the XOR mask, which looks like a rendering
//! bug rather than a units bug.
//!
//! **The pitch is not the width.** Rows are `Pitch` bytes apart and `Pitch`
//! is at least the row's own size, often more. Walking the buffer by width
//! shears the image diagonally — the classic symptom, and the reason the
//! tests below use a pitch larger than the row.
//!
//! **Within a byte, the leftmost pixel is the high bit.** Reading them the
//! other way mirrors every cursor horizontally.
//!
//! # What this module refuses
//!
//! A shape whose buffer is smaller than its own header says it should be.
//! Duplication is not an attacker, but the buffer size comes from a separate
//! call to the one that gives the header, and the crate forbids indexing —
//! so every read is bounds-checked and a short buffer is an error rather than
//! a panic or a torn image.

use crate::cursor::CursorImage;

/// What a pixel that asks to be inverted is drawn as.
///
/// Both mask forms can ask for a pixel to be XORed with the screen, which is
/// how an I-beam stays visible over black *and* white text. There is no screen
/// here — the pointer is sent separately from the frame precisely so that it
/// need not be composited — so a colour has to be chosen.
///
/// **Opaque black.** Most content is light, so black is the choice that leaves
/// an I-beam or a crosshair visible on the majority of screens. What it costs
/// is that the same pointer over dark content is harder to see than it would
/// be natively, which is a legibility cost on a minority of screens rather
/// than a correctness one.
///
/// The alternative is to send the masks and composite at the far end, which
/// means the viewer having the pixels behind the pointer — it does, it is
/// showing them — and a protocol that carries two masks instead of an image.
/// Worth doing if anybody reports it; not worth doing first.
pub const INVERTED: [u8; 4] = [0, 0, 0, 255];

/// A monochrome pixel that is neither black, white, nor transparent.
const OPAQUE_BLACK: [u8; 4] = [0, 0, 0, 255];
/// White, opaque.
const OPAQUE_WHITE: [u8; 4] = [255, 255, 255, 255];
/// Nothing at all.
const CLEAR: [u8; 4] = [0, 0, 0, 0];

/// The most pixels a pointer may have.
///
/// Windows cursors are 32x32 or 48x48, and a large-cursor accessibility
/// setting reaches 256x256. Bounded well above that and well below the point
/// where a shape is a screenshot by another name: the buffer is allocated from
/// this header, so an absurd one is a large allocation asked for by a
/// structure this code did not fill in.
pub const MOST_PIXELS: usize = 512 * 512;

/// Which of the three shapes Duplication reported.
///
/// The numbers are `DXGI_OUTDUPL_POINTER_SHAPE_TYPE`'s own, so a backend can
/// pass the value through rather than translate it — and an unrecognised one
/// is a value Windows added, not a value to guess at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shape {
    /// `_MONOCHROME`, 1. Two stacked 1-bit masks.
    Monochrome,
    /// `_COLOR`, 2. 32-bit BGRA with real alpha.
    Colour,
    /// `_MASKED_COLOR`, 4. 32-bit BGRA whose alpha byte is a mask.
    MaskedColour,
}

impl Shape {
    /// Read the type Duplication reported.
    #[must_use]
    pub const fn from_kind(kind: u32) -> Option<Self> {
        match kind {
            1 => Some(Self::Monochrome),
            2 => Some(Self::Colour),
            4 => Some(Self::MaskedColour),
            _ => None,
        }
    }
}

/// `DXGI_OUTDUPL_POINTER_SHAPE_INFO`, as this module needs it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShapeInfo {
    /// Which of the three.
    pub shape: Shape,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels — **doubled for [`Shape::Monochrome`]**, because the
    /// buffer holds two masks.
    pub height: u32,
    /// Bytes between the start of one row and the next.
    pub pitch: u32,
    /// Where the pointer actually points.
    pub hotspot_x: u32,
    /// The other half of that.
    pub hotspot_y: u32,
}

/// Why a shape could not be turned into pixels.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ShapeError {
    /// A dimension was zero, or the pointer was absurdly large.
    #[error("a {width}x{height} pointer is not a pointer this can draw")]
    NotAPointer { width: u32, height: u32 },

    /// The pitch cannot hold a row of the claimed width.
    #[error("a {width}-pixel row of {shape:?} needs {needed} bytes and the pitch is {pitch}")]
    PitchTooSmall {
        shape: Shape,
        width: u32,
        pitch: u32,
        needed: u32,
    },

    /// The buffer is shorter than the header says it should be.
    #[error("a {rows}-row pointer at pitch {pitch} needs {needed} bytes and {got} arrived")]
    Short {
        rows: u32,
        pitch: u32,
        needed: usize,
        got: usize,
    },

    /// The hotspot is not inside the pointer.
    #[error("the hotspot ({x}, {y}) is outside a {width}x{height} pointer")]
    HotspotOutside {
        x: u32,
        y: u32,
        width: u32,
        height: u32,
    },
}

/// Turn a reported shape and its bytes into an image.
///
/// # Errors
///
/// If the header describes something that is not a pointer, if the pitch
/// cannot hold a row, if the buffer is shorter than the header implies, or if
/// the hotspot falls outside the image.
pub fn image_of(info: &ShapeInfo, bytes: &[u8]) -> Result<CursorImage, ShapeError> {
    // A monochrome buffer holds two masks and reports their combined height.
    let height = match info.shape {
        Shape::Monochrome => info.height / 2,
        Shape::Colour | Shape::MaskedColour => info.height,
    };

    if info.width == 0
        || height == 0
        || usize::try_from(info.width)
            .ok()
            .and_then(|w| usize::try_from(height).ok().map(|h| w.saturating_mul(h)))
            .is_none_or(|pixels| pixels > MOST_PIXELS)
    {
        return Err(ShapeError::NotAPointer {
            width: info.width,
            height,
        });
    }

    let needed_per_row = match info.shape {
        // One bit per pixel, rounded up to a whole byte.
        Shape::Monochrome => info.width.div_ceil(8),
        Shape::Colour | Shape::MaskedColour => info.width.saturating_mul(4),
    };
    if info.pitch < needed_per_row {
        return Err(ShapeError::PitchTooSmall {
            shape: info.shape,
            width: info.width,
            pitch: info.pitch,
            needed: needed_per_row,
        });
    }

    // Monochrome reads `info.height` rows, both masks; the others read
    // `height`. The last row need only hold its own pixels, not a full pitch —
    // Duplication does supply a full pitch, but requiring it would reject a
    // buffer that is complete.
    let rows = match info.shape {
        Shape::Monochrome => info.height,
        Shape::Colour | Shape::MaskedColour => height,
    };
    let needed = usize::try_from(rows.saturating_sub(1))
        .unwrap_or(usize::MAX)
        .saturating_mul(usize::try_from(info.pitch).unwrap_or(usize::MAX))
        .saturating_add(usize::try_from(needed_per_row).unwrap_or(usize::MAX));
    if bytes.len() < needed {
        return Err(ShapeError::Short {
            rows,
            pitch: info.pitch,
            needed,
            got: bytes.len(),
        });
    }

    if info.hotspot_x >= info.width || info.hotspot_y >= height {
        return Err(ShapeError::HotspotOutside {
            x: info.hotspot_x,
            y: info.hotspot_y,
            width: info.width,
            height,
        });
    }

    let rgba = match info.shape {
        Shape::Monochrome => monochrome(info, height, bytes),
        Shape::Colour => colour(info, height, bytes, false),
        Shape::MaskedColour => colour(info, height, bytes, true),
    };

    Ok(CursorImage {
        width: u16::try_from(info.width).unwrap_or(u16::MAX),
        height: u16::try_from(height).unwrap_or(u16::MAX),
        hotspot_x: u16::try_from(info.hotspot_x).unwrap_or(0),
        hotspot_y: u16::try_from(info.hotspot_y).unwrap_or(0),
        rgba,
    })
}

/// One bit out of a mask, counting the leftmost pixel as the high bit.
///
/// Returns `None` past the end of the buffer, which the caller treats as
/// transparent — reachable only if the length check above is ever loosened,
/// and a hole is better than a panic in either case.
fn bit(bytes: &[u8], row_start: usize, x: u32) -> Option<bool> {
    let offset = usize::try_from(x / 8).ok()?;
    let byte = bytes.get(row_start.checked_add(offset)?)?;
    let shift = 7u32.checked_sub(x % 8)?;
    Some(byte & (1u8 << shift) != 0)
}

/// The two stacked masks, turned into pixels.
///
/// | AND | XOR | |
/// |---|---|---|
/// | 0 | 0 | opaque black |
/// | 0 | 1 | opaque white |
/// | 1 | 0 | transparent |
/// | 1 | 1 | inverted — see [`INVERTED`] |
fn monochrome(info: &ShapeInfo, height: u32, bytes: &[u8]) -> Vec<u8> {
    let pitch = usize::try_from(info.pitch).unwrap_or(0);
    let rows = usize::try_from(height).unwrap_or(0);
    let mut rgba = Vec::with_capacity(
        rows.saturating_mul(usize::try_from(info.width).unwrap_or(0))
            .saturating_mul(4),
    );

    for y in 0..height {
        let row = usize::try_from(y).unwrap_or(0);
        let and_row = row.saturating_mul(pitch);
        // The XOR mask begins where the AND mask ends: `height` rows further
        // down, which is half the reported height.
        let xor_row = row.saturating_add(rows).saturating_mul(pitch);
        for x in 0..info.width {
            let and = bit(bytes, and_row, x).unwrap_or(true);
            let xor = bit(bytes, xor_row, x).unwrap_or(false);
            let pixel = match (and, xor) {
                (false, false) => OPAQUE_BLACK,
                (false, true) => OPAQUE_WHITE,
                (true, false) => CLEAR,
                (true, true) => INVERTED,
            };
            rgba.extend_from_slice(&pixel);
        }
    }
    rgba
}

/// BGRA rows, turned into RGBA.
///
/// With `masked`, the alpha byte is not an opacity: `0` means the colour
/// replaces the screen and anything else means XOR it against the screen.
/// That is [`Shape::MaskedColour`], and it is why this cannot simply swap two
/// bytes and copy.
fn colour(info: &ShapeInfo, height: u32, bytes: &[u8], masked: bool) -> Vec<u8> {
    let pitch = usize::try_from(info.pitch).unwrap_or(0);
    let width = usize::try_from(info.width).unwrap_or(0);
    let mut rgba = Vec::with_capacity(
        usize::try_from(height)
            .unwrap_or(0)
            .saturating_mul(width)
            .saturating_mul(4),
    );

    for y in 0..height {
        let row = usize::try_from(y).unwrap_or(0).saturating_mul(pitch);
        for x in 0..width {
            let at = row.saturating_add(x.saturating_mul(4));
            let b = bytes.get(at).copied().unwrap_or(0);
            let g = bytes.get(at.saturating_add(1)).copied().unwrap_or(0);
            let r = bytes.get(at.saturating_add(2)).copied().unwrap_or(0);
            let a = bytes.get(at.saturating_add(3)).copied().unwrap_or(0);
            let pixel = if masked {
                if a == 0 { [r, g, b, 255] } else { INVERTED }
            } else {
                [r, g, b, a]
            };
            rgba.extend_from_slice(&pixel);
        }
    }
    rgba
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    /// The pixel at `(x, y)` of an image, as four bytes.
    fn pixel(image: &CursorImage, x: u32, y: u32) -> [u8; 4] {
        let at = (usize::try_from(y).unwrap() * usize::from(image.width)
            + usize::try_from(x).unwrap())
            * 4;
        let bytes: [u8; 4] = image
            .rgba
            .get(at..at + 4)
            .expect("inside the image")
            .try_into()
            .expect("four bytes");
        bytes
    }

    /// An 8-wide, 2-tall monochrome pointer at a pitch of four bytes.
    ///
    /// A row needs one byte and the pitch is four, which is the shape that
    /// catches a reader walking the buffer by width instead of by pitch.
    fn mono() -> (ShapeInfo, Vec<u8>) {
        let info = ShapeInfo {
            shape: Shape::Monochrome,
            width: 8,
            // Doubled: the AND mask and the XOR mask are both in here.
            height: 4,
            pitch: 4,
            hotspot_x: 0,
            hotspot_y: 0,
        };
        // Written out rather than indexed into, so the layout is visible: one
        // byte of mask per row and three bytes of padding to the next pitch.
        let bytes = vec![
            // AND mask, row 0, then padding.
            0b1111_0000,
            0,
            0,
            0,
            // AND mask, row 1.
            0b0000_1111,
            0,
            0,
            0,
            // XOR mask, row 0 — two pitches down, where the AND mask ends.
            0b1010_1010,
            0,
            0,
            0,
            // XOR mask, row 1.
            0b0101_0101,
            0,
            0,
            0,
        ];
        (info, bytes)
    }

    #[test]
    fn a_monochrome_pointer_is_half_the_height_it_reports() {
        // The units trap. A reader that believes the header produces a cursor
        // twice as tall, made of the AND mask stacked on the XOR mask — which
        // looks like a rendering fault and is an arithmetic one.
        let (info, bytes) = mono();
        let image = image_of(&info, &bytes).expect("a pointer");
        assert_eq!(image.height, 2, "reported {} for two rows", info.height);
        assert_eq!(image.width, 8);
        assert_eq!(image.rgba.len(), 8 * 2 * 4);
    }

    #[test]
    fn the_four_mask_combinations_are_the_four_pixels() {
        let (info, bytes) = mono();
        let image = image_of(&info, &bytes).expect("a pointer");
        // Row 0: AND 11110000, XOR 10101010.
        for (x, want, why) in [
            (0, INVERTED, "AND 1, XOR 1 asks for the screen inverted"),
            (1, CLEAR, "AND 1, XOR 0 is transparent"),
            (2, INVERTED, "AND 1, XOR 1"),
            (3, CLEAR, "AND 1, XOR 0"),
            (4, OPAQUE_WHITE, "AND 0, XOR 1 is white"),
            (5, OPAQUE_BLACK, "AND 0, XOR 0 is black"),
            (6, OPAQUE_WHITE, "AND 0, XOR 1"),
            (7, OPAQUE_BLACK, "AND 0, XOR 0"),
        ] {
            assert_eq!(pixel(&image, x, 0), want, "x={x}: {why}");
        }
    }

    #[test]
    fn the_leftmost_pixel_is_the_high_bit() {
        // Reading a byte the other way round mirrors every cursor
        // horizontally, which is subtle enough on a symmetrical arrow to
        // survive being looked at.
        let info = ShapeInfo {
            shape: Shape::Monochrome,
            width: 8,
            height: 2,
            pitch: 1,
            hotspot_x: 0,
            hotspot_y: 0,
        };
        // AND 0 everywhere so nothing is transparent; XOR sets only the
        // leftmost pixel, so only x=0 may be white.
        let bytes = vec![0b0000_0000, 0b1000_0000];
        let image = image_of(&info, &bytes).expect("a pointer");
        assert_eq!(pixel(&image, 0, 0), OPAQUE_WHITE, "the leftmost pixel");
        for x in 1..8 {
            assert_eq!(pixel(&image, x, 0), OPAQUE_BLACK, "x={x} should be black");
        }
    }

    #[test]
    fn the_second_row_comes_from_the_second_row() {
        // Which is what fails when the pitch is ignored.
        let (info, bytes) = mono();
        let image = image_of(&info, &bytes).expect("a pointer");
        // Row 1: AND 00001111, XOR 01010101.
        for (x, want) in [
            (0, OPAQUE_BLACK), // AND 0, XOR 0
            (1, OPAQUE_WHITE), // AND 0, XOR 1
            (4, CLEAR),        // AND 1, XOR 0
            (5, INVERTED),     // AND 1, XOR 1
        ] {
            assert_eq!(pixel(&image, x, 1), want, "x={x} of row 1");
        }
    }

    #[test]
    fn a_fully_transparent_monochrome_pointer_reads_as_hidden() {
        // How a hidden pointer looks: `CursorImage::is_invisible` is the state
        // rather than a separate question, and an all-AND mask must reach it.
        let info = ShapeInfo {
            shape: Shape::Monochrome,
            width: 8,
            height: 2,
            pitch: 1,
            hotspot_x: 0,
            hotspot_y: 0,
        };
        let bytes = vec![0xFF, 0x00];
        let image = image_of(&info, &bytes).expect("a pointer");
        assert!(image.is_invisible(), "an all-AND, no-XOR mask is nothing");
    }

    #[test]
    fn a_colour_pointer_is_byte_swapped_and_nothing_else() {
        // BGRA in, RGBA out. Alpha is a real opacity here, so it passes
        // through — including a partly transparent edge, which is most of what
        // a modern cursor's alpha is for.
        let info = ShapeInfo {
            shape: Shape::Colour,
            width: 2,
            height: 1,
            pitch: 16,
            hotspot_x: 0,
            hotspot_y: 0,
        };
        // B, G, R, A for two pixels, then padding to the pitch.
        let mut bytes = vec![10, 20, 30, 255, 40, 50, 60, 128];
        bytes.resize(16, 0);
        let image = image_of(&info, &bytes).expect("a pointer");
        assert_eq!(pixel(&image, 0, 0), [30, 20, 10, 255]);
        assert_eq!(pixel(&image, 1, 0), [60, 50, 40, 128], "alpha survives");
    }

    #[test]
    fn a_masked_colour_alpha_byte_is_a_mask_and_not_an_opacity() {
        // The one that looks like an image and is not. Treating the byte as
        // alpha makes every opaque pixel invisible and every inverted one
        // solid — a cursor that is exactly wrong everywhere.
        let info = ShapeInfo {
            shape: Shape::MaskedColour,
            width: 2,
            height: 1,
            pitch: 8,
            hotspot_x: 0,
            hotspot_y: 0,
        };
        let bytes = vec![
            // Alpha 0 means the colour replaces the screen: opaque.
            10, 20, 30, 0, // Anything else means XOR against the screen.
            40, 50, 60, 0xFF,
        ];
        let image = image_of(&info, &bytes).expect("a pointer");
        assert_eq!(
            pixel(&image, 0, 0),
            [30, 20, 10, 255],
            "a zero mask byte is an opaque pixel, not a transparent one"
        );
        assert_eq!(
            pixel(&image, 1, 0),
            INVERTED,
            "a set mask byte asks for the screen inverted"
        );
    }

    #[test]
    fn every_shape_produces_an_image_the_type_calls_consistent() {
        // The cross-check with the existing type: `is_consistent` is what the
        // rest of the pipeline relies on, so anything built here must satisfy
        // it without the caller checking.
        let (mono_info, mono_bytes) = mono();
        let cases: Vec<(ShapeInfo, Vec<u8>)> = vec![
            (mono_info, mono_bytes),
            (
                ShapeInfo {
                    shape: Shape::Colour,
                    width: 4,
                    height: 3,
                    pitch: 32,
                    hotspot_x: 1,
                    hotspot_y: 2,
                },
                vec![0x80; 32 * 3],
            ),
            (
                ShapeInfo {
                    shape: Shape::MaskedColour,
                    width: 5,
                    height: 5,
                    pitch: 40,
                    hotspot_x: 4,
                    hotspot_y: 0,
                },
                vec![0x00; 40 * 5],
            ),
        ];
        for (info, bytes) in cases {
            let image = image_of(&info, &bytes).expect("a pointer");
            assert!(
                image.is_consistent(),
                "{:?} produced an image the type calls inconsistent: {}x{} hot \
                 ({}, {}) with {} bytes",
                info.shape,
                image.width,
                image.height,
                image.hotspot_x,
                image.hotspot_y,
                image.rgba.len()
            );
        }
    }

    #[test]
    fn a_short_buffer_is_refused_rather_than_read_past() {
        // The buffer's length comes from a different call than the header, so
        // the two can disagree. The crate forbids indexing, so this is an
        // error rather than a panic — and a torn image would be worse than
        // either.
        let (info, bytes) = mono();
        // Four rows at a pitch of four, where the last row needs only its own
        // byte: three full pitches plus one. The buffer `mono` builds is
        // sixteen bytes, so 13, 14 and 15 are complete and must be accepted —
        // this test asserted otherwise at first, which was the test being
        // wrong about the boundary rather than the code.
        let enough = 3 * 4 + 1;
        for short in 0..enough {
            match image_of(&info, bytes.get(..short).expect("a prefix")) {
                Err(ShapeError::Short { needed, got, .. }) => {
                    assert_eq!(needed, enough);
                    assert_eq!(got, short);
                }
                Err(other) => panic!("{short} bytes gave {other:?}"),
                Ok(_) => panic!("{short} bytes of a {enough}-byte pointer was accepted"),
            }
        }
        for complete in enough..=bytes.len() {
            assert!(
                image_of(&info, bytes.get(..complete).expect("a prefix")).is_ok(),
                "{complete} bytes is enough and was refused"
            );
        }
    }

    #[test]
    fn the_last_row_need_not_hold_a_whole_pitch() {
        // Duplication does supply a full pitch, but requiring one would refuse
        // a buffer that is complete — and the failure would be a pointer that
        // never appears, on a driver that is behaving.
        let info = ShapeInfo {
            shape: Shape::Colour,
            width: 2,
            height: 2,
            pitch: 16,
            hotspot_x: 0,
            hotspot_y: 0,
        };
        // Two rows: 16 + 8 rather than 16 + 16.
        let bytes = vec![0xFFu8; 16 + 8];
        assert!(image_of(&info, &bytes).is_ok());
        assert!(
            matches!(
                image_of(&info, bytes.get(..16 + 7).expect("a prefix")),
                Err(ShapeError::Short { .. })
            ),
            "one byte short of the last row is still short"
        );
    }

    #[test]
    fn a_pitch_that_cannot_hold_a_row_is_refused() {
        let info = ShapeInfo {
            shape: Shape::Colour,
            width: 8,
            height: 1,
            // 8 pixels of BGRA is 32 bytes.
            pitch: 16,
            hotspot_x: 0,
            hotspot_y: 0,
        };
        assert!(matches!(
            image_of(&info, &[0u8; 64]),
            Err(ShapeError::PitchTooSmall { needed: 32, .. })
        ));
    }

    #[test]
    fn nothing_and_something_absurd_are_both_refused() {
        for (width, height) in [(0, 8), (8, 0), (0, 0), (4096, 4096)] {
            let info = ShapeInfo {
                shape: Shape::Colour,
                width,
                height,
                pitch: width.saturating_mul(4),
                hotspot_x: 0,
                hotspot_y: 0,
            };
            assert!(
                matches!(
                    image_of(&info, &[0u8; 64]),
                    Err(ShapeError::NotAPointer { .. })
                ),
                "{width}x{height} should be refused"
            );
        }
    }

    #[test]
    fn a_hotspot_outside_the_pointer_is_refused() {
        // `CursorImage::is_consistent` requires the hotspot inside the image,
        // so accepting one here would build a value the rest of the pipeline
        // rejects — and the report would name the wrong thing.
        for (x, y) in [(8, 0), (0, 2), (99, 99)] {
            let info = ShapeInfo {
                shape: Shape::Monochrome,
                width: 8,
                height: 4,
                pitch: 4,
                hotspot_x: x,
                hotspot_y: y,
            };
            assert!(
                matches!(
                    image_of(&info, &[0u8; 16]),
                    Err(ShapeError::HotspotOutside { .. })
                ),
                "hotspot ({x}, {y}) should be refused"
            );
        }
    }

    #[test]
    fn an_odd_reported_height_rounds_down_rather_than_reading_a_half_row() {
        // A monochrome header claiming an odd height is malformed. Halving
        // rounds down, so the masks stay whole and the length check does the
        // refusing if there is not enough for them.
        let info = ShapeInfo {
            shape: Shape::Monochrome,
            width: 8,
            height: 5,
            pitch: 1,
            hotspot_x: 0,
            hotspot_y: 0,
        };
        let image = image_of(&info, &[0u8; 5]).expect("two rows of a five-row claim");
        assert_eq!(image.height, 2);
    }

    #[test]
    fn the_shape_numbers_are_the_ones_windows_uses() {
        assert_eq!(Shape::from_kind(1), Some(Shape::Monochrome));
        assert_eq!(Shape::from_kind(2), Some(Shape::Colour));
        assert_eq!(Shape::from_kind(4), Some(Shape::MaskedColour));
        // 3 is not a gap somebody forgot; the values are flags and there is no
        // 3. A value Windows adds later must not be guessed at.
        assert_eq!(Shape::from_kind(3), None);
        assert_eq!(Shape::from_kind(0), None);
        assert_eq!(Shape::from_kind(8), None);
    }
}
