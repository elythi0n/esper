//! Which colour space the YUV is in.
//!
//! # Why this is two decisions and not one
//!
//! An encoder writes a description of its own output into the bitstream, and a
//! decoder believes it. If the description and the conversion disagree, the
//! picture is wrong — and wrong in a way that looks like a hardware or driver
//! fault rather than an arithmetic one, because every pixel is plausible and
//! all of them are slightly off.
//!
//! Two things have to agree:
//!
//! **The matrix.** BT.601 for standard definition, BT.709 for high. Using the
//! wrong one shifts hue, most visibly on saturated reds and greens, by about
//! as much as a mis-set monitor — enough to be noticed and not enough to be
//! diagnosed.
//!
//! **The range.** Limited puts luma in 16..=235, full uses 0..=255. Getting
//! this wrong is worse than the matrix, and asymmetrically so:
//!
//! - Encoded full, decoded as limited: everything below 16 clips to black and
//!   everything above 235 clips to white. Dark interface chrome becomes a flat
//!   black shape and text on white blows out. Destructive.
//! - Encoded limited, decoded as full: blacks lift to a dark grey and whites
//!   drop. Washed out — visibly wrong, but nothing is lost.
//!
//! So the default is **BT.709 limited**, which is what an H.264 decoder
//! assumes when it is not told otherwise and what WebRTC signals by default.
//! Full range is offered because it keeps 255 levels instead of 219, but it is
//! not the default: the failure mode of a viewer that ignores the flag is the
//! destructive one, and 36 extra levels do not matter for text, which is high
//! contrast rather than finely graded.

/// The luma coefficients of a YUV matrix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Matrix {
    /// BT.601. Standard definition, and what a decoder assumes for small
    /// pictures when the bitstream does not say.
    Bt601,
    /// BT.709. High definition, and the right choice for a screen.
    Bt709,
}

impl Matrix {
    /// `(Kr, Kg, Kb)`, the luma weights. They sum to one.
    #[must_use]
    pub const fn luma_weights(self) -> (f64, f64, f64) {
        match self {
            // Rec. ITU-R BT.601-7, table 3.
            Self::Bt601 => (0.299, 0.587, 0.114),
            // Rec. ITU-R BT.709-6, item 3.2.
            Self::Bt709 => (0.2126, 0.7152, 0.0722),
        }
    }

    /// The `matrix_coefficients` value for the bitstream's VUI.
    ///
    /// This is the number a decoder reads to learn which matrix to invert. It
    /// must match [`Matrix::luma_weights`], and there is a test that says so
    /// rather than a comment hoping so.
    #[must_use]
    pub const fn vui_matrix_coefficients(self) -> u8 {
        match self {
            // Both from ITU-T H.273 table 4.
            Self::Bt601 => 6,
            Self::Bt709 => 1,
        }
    }

    /// The `colour_primaries` value for the bitstream's VUI.
    #[must_use]
    pub const fn vui_colour_primaries(self) -> u8 {
        match self {
            // H.273 table 2.
            Self::Bt601 => 6,
            Self::Bt709 => 1,
        }
    }

    /// The `transfer_characteristics` value for the bitstream's VUI.
    ///
    /// Both of these use the same transfer function, which is why the value is
    /// the same for both matrices — and why it is a separate method rather
    /// than being assumed to follow from the matrix.
    #[must_use]
    pub const fn vui_transfer_characteristics(self) -> u8 {
        match self {
            // H.273 table 3.
            Self::Bt601 => 6,
            Self::Bt709 => 1,
        }
    }

    /// A short name for the session HUD and the log.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Bt601 => "BT.601",
            Self::Bt709 => "BT.709",
        }
    }
}

/// How much of the 8-bit code space the YUV uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Range {
    /// Luma 16..=235, chroma 16..=240. What a decoder assumes by default.
    Limited,
    /// The whole 0..=255. Keeps 255 levels instead of 219, at the cost of
    /// clipping badly on any viewer that ignores the flag.
    Full,
}

impl Range {
    /// `(offset, scale)` for luma: `Y = offset + scale * Y'`.
    #[must_use]
    pub const fn luma(self) -> (f64, f64) {
        match self {
            Self::Limited => (16.0, 219.0),
            Self::Full => (0.0, 255.0),
        }
    }

    /// `(offset, scale)` for chroma: `C = offset + scale * C'`, where `C'` is
    /// in -0.5..=0.5.
    #[must_use]
    pub const fn chroma(self) -> (f64, f64) {
        match self {
            Self::Limited => (128.0, 224.0),
            Self::Full => (128.0, 255.0),
        }
    }

    /// The `video_full_range_flag` for the bitstream's VUI.
    #[must_use]
    pub const fn vui_full_range_flag(self) -> bool {
        matches!(self, Self::Full)
    }

    /// A short name for the session HUD and the log.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Limited => "limited",
            Self::Full => "full",
        }
    }
}

/// A matrix and a range together — everything a decoder needs to be told.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ColorSpace {
    pub matrix: Matrix,
    pub range: Range,
}

impl Default for ColorSpace {
    /// BT.709 limited. See the module documentation for why not full.
    fn default() -> Self {
        Self {
            matrix: Matrix::Bt709,
            range: Range::Limited,
        }
    }
}

impl ColorSpace {
    /// A space with these two properties.
    #[must_use]
    pub const fn new(matrix: Matrix, range: Range) -> Self {
        Self { matrix, range }
    }

    /// What to write into the bitstream so a decoder inverts what we did.
    ///
    /// Returned as one value rather than four accessors, because the four have
    /// to travel together: three of them right and one of them wrong is the
    /// same broken picture as all four wrong, and harder to spot.
    #[must_use]
    pub const fn vui(self) -> Vui {
        Vui {
            colour_primaries: self.matrix.vui_colour_primaries(),
            transfer_characteristics: self.matrix.vui_transfer_characteristics(),
            matrix_coefficients: self.matrix.vui_matrix_coefficients(),
            full_range: self.range.vui_full_range_flag(),
        }
    }

    /// A description for the HUD, e.g. `BT.709 limited`.
    #[must_use]
    pub fn describe(self) -> String {
        format!("{} {}", self.matrix.name(), self.range.name())
    }
}

/// The colour description an H.264 bitstream carries in its VUI.
///
/// Not a convenience: it is the contract with the decoder. Every value here
/// has to describe what the conversion actually did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Vui {
    pub colour_primaries: u8,
    pub transfer_characteristics: u8,
    pub matrix_coefficients: u8,
    pub full_range: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The weights must sum to one, or luma is not luma and grey is not grey.
    #[test]
    fn the_luma_weights_sum_to_one() {
        for m in [Matrix::Bt601, Matrix::Bt709] {
            let (r, g, b) = m.luma_weights();
            assert!(
                (r + g + b - 1.0).abs() < 1e-9,
                "{}: {r} + {g} + {b} = {}",
                m.name(),
                r + g + b
            );
        }
    }

    /// The values written into the bitstream must be the ones ITU-T H.273
    /// assigns, because a decoder looks them up in that table and not in ours.
    #[test]
    fn the_vui_values_are_the_ones_h273_assigns() {
        let bt709 = ColorSpace::new(Matrix::Bt709, Range::Limited).vui();
        assert_eq!(
            bt709,
            Vui {
                colour_primaries: 1,
                transfer_characteristics: 1,
                matrix_coefficients: 1,
                full_range: false,
            }
        );

        let bt601_full = ColorSpace::new(Matrix::Bt601, Range::Full).vui();
        assert_eq!(
            bt601_full,
            Vui {
                colour_primaries: 6,
                transfer_characteristics: 6,
                matrix_coefficients: 6,
                full_range: true,
            }
        );
    }

    /// The two matrices must be distinguishable in the bitstream, or signalling
    /// them is pointless.
    #[test]
    fn the_two_matrices_are_signalled_differently() {
        assert_ne!(
            Matrix::Bt601.vui_matrix_coefficients(),
            Matrix::Bt709.vui_matrix_coefficients()
        );
        assert_ne!(Matrix::Bt601.luma_weights(), Matrix::Bt709.luma_weights());
    }

    /// The default is the safe one. See the module documentation: encoding full
    /// range for a viewer that assumes limited clips both ends, which is
    /// destructive, while the other way round is merely washed out.
    #[test]
    fn the_default_is_the_conservative_choice() {
        let d = ColorSpace::default();
        assert_eq!(d.matrix, Matrix::Bt709);
        assert_eq!(d.range, Range::Limited);
        assert!(!d.vui().full_range);
        assert_eq!(d.describe(), "BT.709 limited");
    }

    #[test]
    fn limited_range_leaves_headroom_and_footroom() {
        let (offset, scale) = Range::Limited.luma();
        assert_eq!(offset, 16.0);
        assert_eq!(offset + scale, 235.0, "white sits at 235, not 255");

        let (offset, scale) = Range::Full.luma();
        assert_eq!(offset, 0.0);
        assert_eq!(offset + scale, 255.0);
    }

    #[test]
    fn chroma_is_centred_on_128_in_both_ranges() {
        for r in [Range::Limited, Range::Full] {
            let (offset, scale) = r.chroma();
            assert_eq!(offset, 128.0, "{}", r.name());
            // A chroma of -0.5 and +0.5 must stay inside 8 bits.
            assert!(offset - scale / 2.0 >= 0.0, "{}", r.name());
            assert!(offset + scale / 2.0 <= 255.5, "{}", r.name());
        }
        assert_eq!(Range::Limited.chroma().1, 224.0, "16..=240");
    }
}
