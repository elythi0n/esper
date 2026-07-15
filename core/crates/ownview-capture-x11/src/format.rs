//! Working out which byte holds red.
//!
//! # Why this is derived and not assumed
//!
//! Nearly every X server in use is little-endian, depth 24, 32 bits per pixel,
//! with red at `0x00ff0000` — which is BGRX in memory. Assuming that works
//! everywhere it is true and swaps red and blue everywhere it is not, and the
//! places it is not are real: a big-endian server, or a display running at
//! sixteen bits.
//!
//! A swapped channel is also the *worst kind* of wrong, because the picture
//! still looks like a picture. Nobody reviewing a screenshot of a blue-tinted
//! desktop concludes the byte order is wrong; they conclude the theme changed.
//! So the server is asked, and a layout this cannot express is an error rather
//! than a guess.
//!
//! # Alpha is never claimed
//!
//! A depth-32 root window has an alpha channel whose contents are whatever the
//! compositor left there, and it is not opacity of anything — the screen is
//! opaque by definition. `PixelFormat` distinguishes "has alpha" from "has a
//! fourth byte that means nothing" precisely so this can be answered honestly,
//! so capture always reports the second.

use ownview_capture::frame::PixelFormat;
use x11rb::protocol::xproto::{ImageOrder, Setup, Visualid};

/// Why a server's pixel layout cannot be used.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum FormatError {
    /// The depth has no pixmap format, which should not happen.
    #[error("the X server lists no pixmap format for depth {0}")]
    NoPixmapFormat(u8),
    /// Not 32 bits per pixel.
    #[error(
        "this backend needs 32 bits per pixel; the X server offers {bpp} at depth {depth}. \
         A 16-bit display would need a different colour conversion, not a different capturer"
    )]
    NotThirtyTwoBit { depth: u8, bpp: u8 },
    /// The root visual was not found or is not direct colour.
    #[error("the root visual {0} is not a TrueColor visual with 8 bits per channel")]
    NotTrueColor(Visualid),
    /// The channel masks are not three bytes in some order.
    #[error(
        "red {red:#010x}, green {green:#010x}, blue {blue:#010x} is not three whole bytes \
         in an order this can name"
    )]
    UnnamableLayout { red: u32, green: u32, blue: u32 },
}

/// The pixel layout frames from this server arrive in.
pub fn of(
    setup: &Setup,
    root_depth: u8,
    root_visual: Visualid,
) -> Result<PixelFormat, FormatError> {
    let bpp = setup
        .pixmap_formats
        .iter()
        .find(|f| f.depth == root_depth)
        .map(|f| f.bits_per_pixel)
        .ok_or(FormatError::NoPixmapFormat(root_depth))?;
    if bpp != 32 {
        return Err(FormatError::NotThirtyTwoBit {
            depth: root_depth,
            bpp,
        });
    }

    let visual = setup
        .roots
        .iter()
        .flat_map(|s| s.allowed_depths.iter())
        .flat_map(|d| d.visuals.iter())
        .find(|v| v.visual_id == root_visual)
        .ok_or(FormatError::NotTrueColor(root_visual))?;
    if visual.bits_per_rgb_value < 8 {
        return Err(FormatError::NotTrueColor(root_visual));
    }

    layout(
        visual.red_mask,
        visual.green_mask,
        visual.blue_mask,
        setup.image_byte_order,
    )
}

/// Which `PixelFormat` a set of channel masks and a byte order describe.
///
/// Split out from [`of`] because it is the part with the reasoning in it, and
/// it can be tested without an X server.
pub fn layout(
    red: u32,
    green: u32,
    blue: u32,
    order: ImageOrder,
) -> Result<PixelFormat, FormatError> {
    let unnamable = FormatError::UnnamableLayout { red, green, blue };
    let byte_of = |mask: u32| -> Option<usize> {
        // A whole byte, aligned to a byte, and nothing else.
        if mask.count_ones() != 8 || !mask.trailing_zeros().is_multiple_of(8) {
            return None;
        }
        let within = (mask.trailing_zeros() / 8) as usize;
        // The mask describes bits of a 32-bit value. Which *byte* that is
        // depends on how the server serialises it: least significant first
        // puts bits 0-7 in byte 0, most significant first puts them in byte 3.
        Some(match order {
            ImageOrder::MSB_FIRST => 3usize.saturating_sub(within),
            _ => within,
        })
    };
    let (Some(r), Some(g), Some(b)) = (byte_of(red), byte_of(green), byte_of(blue)) else {
        return Err(unnamable);
    };
    if r == g || g == b || r == b {
        return Err(unnamable);
    }
    // The fourth byte is the one no channel claimed, and it is left alone.
    // Only the two orders `PixelFormat` can name are accepted; a server
    // putting green first is not something to paper over.
    match (r, g, b) {
        (2, 1, 0) => Ok(PixelFormat::Bgrx8),
        (0, 1, 2) => Ok(PixelFormat::Rgbx8),
        _ => Err(unnamable),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The layout essentially every X server in use actually has.
    #[test]
    fn a_little_endian_depth_24_server_is_bgrx() {
        assert_eq!(
            layout(0x00ff_0000, 0x0000_ff00, 0x0000_00ff, ImageOrder::LSB_FIRST),
            Ok(PixelFormat::Bgrx8)
        );
    }

    /// **The case that would silently swap red and blue.** The same masks on a
    /// big-endian server serialise the other way round: red lands in byte 1
    /// and the padding in byte 0, which is XRGB. `PixelFormat` has no name for
    /// a layout with the spare byte first — every variant puts it last — so
    /// this is refused.
    ///
    /// Refusing is the point. Assuming BGRX here would produce a desktop with
    /// red and blue exchanged, which looks like a picture and reviews like a
    /// theme change.
    #[test]
    fn the_common_masks_on_a_big_endian_server_are_refused_not_guessed() {
        assert_eq!(
            layout(0x00ff_0000, 0x0000_ff00, 0x0000_00ff, ImageOrder::MSB_FIRST),
            Err(FormatError::UnnamableLayout {
                red: 0x00ff_0000,
                green: 0x0000_ff00,
                blue: 0x0000_00ff
            })
        );
    }

    /// A big-endian server that really is RGBX has its masks in the high
    /// bytes, because those are the ones it writes first.
    #[test]
    fn a_big_endian_rgbx_server_is_recognised() {
        assert_eq!(
            layout(0xff00_0000, 0x00ff_0000, 0x0000_ff00, ImageOrder::MSB_FIRST),
            Ok(PixelFormat::Rgbx8)
        );
        assert_eq!(
            layout(0x0000_ff00, 0x00ff_0000, 0xff00_0000, ImageOrder::MSB_FIRST),
            Ok(PixelFormat::Bgrx8)
        );
    }

    #[test]
    fn a_server_with_blue_in_the_high_byte_is_rgbx_little_endian() {
        assert_eq!(
            layout(0x0000_00ff, 0x0000_ff00, 0x00ff_0000, ImageOrder::LSB_FIRST),
            Ok(PixelFormat::Rgbx8)
        );
    }

    /// Capture never claims alpha, whatever the depth. The screen is opaque;
    /// a fourth byte on a root window holds whatever the compositor left.
    #[test]
    fn no_layout_ever_claims_alpha() {
        for order in [ImageOrder::LSB_FIRST, ImageOrder::MSB_FIRST] {
            for (r, g, b) in [
                (0x00ff_0000u32, 0x0000_ff00u32, 0x0000_00ffu32),
                (0x0000_00ff, 0x0000_ff00, 0x00ff_0000),
                (0xff00_0000, 0x00ff_0000, 0x0000_ff00),
            ] {
                if let Ok(f) = layout(r, g, b, order) {
                    assert!(!f.has_alpha(), "{f:?} claims alpha");
                }
            }
        }
    }

    /// Masks that are not three whole aligned bytes are refused rather than
    /// rounded to something plausible.
    #[test]
    fn a_layout_that_cannot_be_named_is_refused() {
        // 16-bit 5-6-5.
        assert!(layout(0xf800, 0x07e0, 0x001f, ImageOrder::LSB_FIRST).is_err());
        // Ten bits per channel, as a deep-colour visual would have.
        assert!(layout(0x3ff0_0000, 0x000f_fc00, 0x0000_03ff, ImageOrder::LSB_FIRST).is_err());
        // A whole byte, but not byte-aligned.
        assert!(layout(0x0ff0_0000, 0x0000_ff00, 0x0000_00ff, ImageOrder::LSB_FIRST).is_err());
        // Two channels claiming the same byte.
        assert!(layout(0x0000_00ff, 0x0000_00ff, 0x0000_ff00, ImageOrder::LSB_FIRST).is_err());
        // Green first, which PixelFormat has no name for.
        assert!(layout(0x0000_00ff, 0x00ff_0000, 0x0000_ff00, ImageOrder::LSB_FIRST).is_err());
    }

    /// Every accepted layout must round-trip: the offsets the format reports
    /// have to be the offsets the masks described, or the whole derivation is
    /// decorative.
    #[test]
    fn an_accepted_layout_reports_the_offsets_it_was_given() {
        for order in [ImageOrder::LSB_FIRST, ImageOrder::MSB_FIRST] {
            for (r, g, b) in [
                (0x00ff_0000u32, 0x0000_ff00u32, 0x0000_00ffu32),
                (0x0000_00ff, 0x0000_ff00, 0x00ff_0000),
            ] {
                let Ok(f) = layout(r, g, b, order) else {
                    continue;
                };
                let shift = |m: u32| (m.trailing_zeros() / 8) as usize;
                let flip = |w: usize| match order {
                    ImageOrder::MSB_FIRST => 3 - w,
                    _ => w,
                };
                assert_eq!(
                    f.rgb_offsets(),
                    (flip(shift(r)), flip(shift(g)), flip(shift(b))),
                    "{f:?} for {r:#x}/{g:#x}/{b:#x} {order:?}"
                );
            }
        }
    }
}
