//! A policy cap on how many pixels are encoded, independent of load.
//!
//! # Why a ceiling at all, when there is already a governor
//!
//! The governor is *reactive*: it degrades once cost has been observed. On a
//! 4K host that means the first seconds of every session are spent
//! discovering that eight million pixels do not fit in one core, and the
//! discovery is visible — a stutter, then a recovery.
//!
//! A ceiling is the prior. It says that above some size the extra pixels were
//! never going to be affordable, so the session starts where it would have
//! ended up. The two compose: the ceiling caps the native size, and the
//! governor's ladder then works in the space that leaves.
//!
//! # 1920 on the long edge
//!
//! Predictable, and it only engages where it should. A 1080p or 1200p host is
//! untouched; 1440p becomes 1920x1080; 4K becomes 1920x1080; an ultrawide
//! 3440x1440 becomes 1920x802. The alternative — a cap on total pixel count —
//! is better in theory and produces sizes nobody can predict or describe,
//! which matters for something a user reads off a session HUD.
//!
//! It is a default, not a law: a host with cores to spare should raise it, and
//! a session over a link that cannot carry 1080p should lower it.

/// The largest edge a frame may have before it is scaled down.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ceiling {
    long_edge: Option<u32>,
}

impl Default for Ceiling {
    fn default() -> Self {
        Self::of(Self::DEFAULT_LONG_EDGE)
    }
}

impl Ceiling {
    /// The default cap on the longer edge.
    pub const DEFAULT_LONG_EDGE: u32 = 1920;

    /// The smallest cap that is worth honouring.
    ///
    /// Below this a remote desktop is not one, and a mistyped value should not
    /// be able to produce a session that shows a thumbnail. The floor mirrors
    /// how the CPU budget refuses to accept a zero.
    pub const MIN_LONG_EDGE: u32 = 160;

    /// A cap on the longer edge, floored at [`Self::MIN_LONG_EDGE`].
    #[must_use]
    pub const fn of(long_edge: u32) -> Self {
        let long_edge = if long_edge < Self::MIN_LONG_EDGE {
            Self::MIN_LONG_EDGE
        } else {
            long_edge
        };
        Self {
            long_edge: Some(long_edge),
        }
    }

    /// No cap: encode whatever the display is.
    #[must_use]
    pub const fn none() -> Self {
        Self { long_edge: None }
    }

    /// The cap, if there is one.
    #[must_use]
    pub const fn long_edge(&self) -> Option<u32> {
        self.long_edge
    }

    /// The size to capture at, for a display of `width` by `height`.
    ///
    /// Aspect ratio is preserved and both edges land on even numbers, because
    /// 4:2:0 chroma is one sample per 2x2 block and an odd edge leaves a half
    /// block that every stage after this would have to special-case.
    #[must_use]
    pub fn apply(&self, width: u32, height: u32) -> (u32, u32) {
        let Some(cap) = self.long_edge else {
            return (even(width), even(height));
        };
        let long = width.max(height);
        if long <= cap {
            return (even(width), even(height));
        }
        // Scale by cap/long in integers, so the result is reproducible and the
        // longer edge lands exactly on the cap rather than a pixel either side.
        let scale = |v: u32| {
            let scaled = u64::from(v) * u64::from(cap) / u64::from(long.max(1));
            even(u32::try_from(scaled).unwrap_or(cap))
        };
        (scale(width), scale(height))
    }

    /// Whether a display of this size would be scaled.
    #[must_use]
    pub fn engages_for(&self, width: u32, height: u32) -> bool {
        self.apply(width, height) != (even(width), even(height))
    }

    /// A phrase for the session HUD and the startup log.
    #[must_use]
    pub fn describe(&self) -> String {
        match self.long_edge {
            None => "no resolution ceiling".to_owned(),
            Some(cap) => format!("resolution ceiling {cap} on the long edge"),
        }
    }
}

/// Round down to an even number, but never below the smallest size the
/// encoder and the chroma planes can both work with.
///
/// Matches the rule the degradation ladder uses, so a size that arrives via
/// the ceiling and a size that arrives via a rung are snapped the same way.
#[must_use]
pub fn even(v: u32) -> u32 {
    (v & !1).max(16)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn common_displays_land_where_the_documentation_says() {
        let c = Ceiling::default();
        assert_eq!(c.apply(1920, 1080), (1920, 1080), "1080p is untouched");
        assert_eq!(c.apply(1920, 1200), (1920, 1200), "1200p is untouched");
        assert_eq!(
            c.apply(1366, 768),
            (1366, 768),
            "a small laptop is untouched"
        );
        assert_eq!(c.apply(2560, 1440), (1920, 1080), "1440p becomes 1080p");
        assert_eq!(c.apply(3840, 2160), (1920, 1080), "4K becomes 1080p");
        assert_eq!(
            c.apply(3440, 1440),
            (1920, 802),
            "an ultrawide keeps its shape"
        );
        assert_eq!(c.apply(5120, 1440), (1920, 540), "so does a wider one");
    }

    #[test]
    fn a_portrait_display_is_capped_on_its_long_edge() {
        // The cap is on the longer edge, not on width, or a rotated monitor
        // would be scaled to a sliver.
        let c = Ceiling::default();
        assert_eq!(c.apply(1440, 2560), (1080, 1920));
        assert_eq!(c.apply(1080, 1920), (1080, 1920));
    }

    #[test]
    fn the_long_edge_lands_exactly_on_the_cap() {
        for cap in [640u32, 1280, 1920, 2560] {
            let c = Ceiling::of(cap);
            for (w, h) in [(3840u32, 2160u32), (2560, 1600), (5120, 1440), (1600, 4000)] {
                let (ow, oh) = c.apply(w, h);
                let long = ow.max(oh);
                assert_eq!(
                    long,
                    even(cap),
                    "{w}x{h} at cap {cap} gave a long edge of {long}"
                );
            }
        }
    }

    #[test]
    fn the_aspect_ratio_survives() {
        let c = Ceiling::default();
        for (w, h) in [(3840u32, 2160u32), (2560, 1440), (3440, 1440), (2880, 1800)] {
            let (ow, oh) = c.apply(w, h);
            let before = f64::from(w) / f64::from(h);
            let after = f64::from(ow) / f64::from(oh);
            // Two pixels of even-snapping on the short edge is the whole
            // budget; anything more is a stretched picture.
            assert!(
                (before - after).abs() / before < 0.004,
                "{w}x{h} -> {ow}x{oh}: {before:.4} became {after:.4}"
            );
        }
    }

    #[test]
    fn every_result_is_even_and_encodable() {
        let caps = [Ceiling::none(), Ceiling::default(), Ceiling::of(700)];
        for c in caps {
            for w in 1..200u32 {
                for h in [1u32, 3, 17, 199, 1080, 2161] {
                    let (ow, oh) = c.apply(w, h);
                    assert_eq!(ow % 2, 0, "{w}x{h} gave an odd width {ow}");
                    assert_eq!(oh % 2, 0, "{w}x{h} gave an odd height {oh}");
                    assert!(ow >= 16 && oh >= 16, "{w}x{h} gave {ow}x{oh}");
                    assert!(ow <= even(w).max(16) && oh <= even(h).max(16));
                }
            }
        }
    }

    #[test]
    fn no_ceiling_only_evens_the_size() {
        let c = Ceiling::none();
        assert_eq!(c.long_edge(), None);
        assert_eq!(c.apply(3840, 2160), (3840, 2160));
        assert_eq!(c.apply(1921, 1081), (1920, 1080));
        assert!(!c.engages_for(7680, 4320));
    }

    #[test]
    fn a_nonsense_cap_is_floored_rather_than_honoured() {
        // A mistyped zero must not produce a session that shows a postage
        // stamp, the same way the CPU budget refuses a zero.
        assert_eq!(Ceiling::of(0).long_edge(), Some(Ceiling::MIN_LONG_EDGE));
        assert_eq!(Ceiling::of(1).long_edge(), Some(Ceiling::MIN_LONG_EDGE));
        assert_eq!(Ceiling::of(159).long_edge(), Some(Ceiling::MIN_LONG_EDGE));
        assert_eq!(Ceiling::of(160).long_edge(), Some(160));
    }

    #[test]
    fn engagement_is_reported_honestly() {
        let c = Ceiling::default();
        assert!(c.engages_for(2560, 1440));
        assert!(c.engages_for(3840, 2160));
        assert!(!c.engages_for(1920, 1080));
        assert!(!c.engages_for(1600, 900));
        // An odd native size is evened, but that is not the ceiling engaging.
        assert!(!c.engages_for(1921, 1081));
    }

    #[test]
    fn the_description_says_what_is_in_force() {
        assert_eq!(Ceiling::none().describe(), "no resolution ceiling");
        assert_eq!(
            Ceiling::default().describe(),
            "resolution ceiling 1920 on the long edge"
        );
    }

    #[test]
    fn a_capped_size_is_never_larger_than_the_display() {
        // Scaling up is the scaler's error to refuse, but the ceiling must
        // never be the thing that asks for it.
        for cap in [160u32, 500, 1920, 4096] {
            let c = Ceiling::of(cap);
            for w in [16u32, 17, 100, 1920, 3840, 7680] {
                for h in [16u32, 31, 900, 2160] {
                    let (ow, oh) = c.apply(w, h);
                    assert!(ow <= w.max(16), "{w}x{h} cap {cap} widened to {ow}");
                    assert!(oh <= h.max(16), "{w}x{h} cap {cap} heightened to {oh}");
                }
            }
        }
    }
}
