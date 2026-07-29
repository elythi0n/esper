//! Every number this project has promised, in one place.
//!
//! # Why the targets are data
//!
//! They are stated in `capture-and-codec.md` as a table, and until now they
//! lived only there — which meant every one of them was checked by hand, once,
//! by somebody who then wrote the number into a document. A target nothing
//! runs against is a wish, and this project's own findings say so.
//!
//! # A target and a baseline are different things
//!
//! A **target** is what the product should do. A **baseline** is what this
//! machine last did. They are kept apart because they fail differently:
//!
//! - A target that is not met is *reported*, and does not fail a build. Some
//!   of them are not met today — full-motion encode is the headline one — and
//!   a suite that went red for a known gap is a suite people stop reading.
//! - A baseline that is *regressed against* fails, because that is a change
//!   somebody just made.
//!
//! So the build goes red when something got worse, not when something is not
//! yet good enough. The two are said separately in the report.

use serde::{Deserialize, Serialize};

/// Which way is better.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Better {
    /// Milliseconds, bytes, per cent of a core.
    Lower,
    /// Frames, pictures.
    Higher,
}

/// One thing that is measured.
#[derive(Debug, Clone, Copy)]
pub struct Target {
    /// What it is called, in the report and in a baseline file.
    pub name: &'static str,
    /// A sentence saying what it means and why it is worth a number.
    pub about: &'static str,
    /// The unit, for the report.
    pub unit: &'static str,
    /// Which direction is an improvement.
    pub better: Better,
    /// What the product should manage. `None` where the design document
    /// states no figure and this exists to establish one.
    pub want: Option<f64>,
    /// Whether this figure means the same thing on a different machine.
    ///
    /// # Why most of them do not
    ///
    /// An encode time is a property of the processor. A CPU share is too. Even
    /// a *bitrate* from a whole-path run is not portable: the gate's frame
    /// rate ceiling is reached on a fast machine and not on a slow one, so
    /// fewer frames go out and fewer bits with them.
    ///
    /// What is portable is a **ratio between two measurements taken on the
    /// same machine**, where the machine cancels — and a figure the *policy*
    /// decides rather than the hardware, like how often an idle screen sends
    /// a keyframe.
    ///
    /// This matters because CI runs on a machine nobody has a baseline for.
    /// A suite that gated everything there would fail on every run; one that
    /// gated nothing would catch nothing. So the portable ones are gated
    /// everywhere and the rest only where the baseline was recorded. One
    /// thing gated correctly beats four gated wrongly.
    pub portable: bool,
}

/// How far a measurement may drift from a baseline before it is a regression.
///
/// **Twenty-five per cent, and that is not generous — it is honest.** These
/// are wall-clock and CPU figures on a machine that is also running a browser
/// and a build; measured over ten repeated runs of the same code, the spread
/// on the encode figures was well inside this and the spread on the whole-path
/// ones was not far off it. A tighter band would fail on somebody's laptop for
/// reasons that have nothing to do with the change they made, and a suite that
/// cries wolf is a suite that gets an `|| true` next to it.
pub const DRIFT: f64 = 0.25;

/// Every target, in report order.
pub const TARGETS: &[Target] = &[
    Target {
        name: "encode.screen.ms",
        about: "One 1080p frame of screen-like content, encoded. What a session \
                costs per frame when somebody is reading a document.",
        unit: "ms/frame",
        better: Better::Lower,
        want: Some(15.0),
        portable: false,
    },
    Target {
        name: "encode.screen.core",
        about: "The same, as a share of one core at 30 frames a second.",
        unit: "cores",
        better: Better::Lower,
        want: Some(1.0),
        portable: false,
    },
    Target {
        name: "encode.motion.ms",
        about: "One 1080p frame with every pixel different from the last. The \
                worst an encoder is asked to do, and the target this project \
                currently misses.",
        unit: "ms/frame",
        better: Better::Lower,
        want: Some(15.0),
        portable: false,
    },
    Target {
        name: "encode.motion.core",
        about: "The same, as a share of one core at 30 frames a second. A hard \
                ceiling: a host that spends more than a core on encoding is a \
                host somebody else cannot use.",
        unit: "cores",
        better: Better::Lower,
        want: Some(1.0),
        portable: false,
    },
    Target {
        name: "convert.whole.ms",
        about: "A whole 1080p frame from BGRA to I420. What a keyframe costs, \
                and what every frame costs when the whole screen changed.",
        unit: "ms",
        better: Better::Lower,
        want: Some(1.0),
        portable: false,
    },
    Target {
        name: "convert.line.ms",
        about: "One line of text's worth of a 1080p frame. The entire case for \
                converting damage rather than frames.",
        unit: "ms",
        better: Better::Lower,
        want: None,
        portable: false,
    },
    Target {
        name: "path.screen.core",
        about: "Capture, gate, convert and encode, 1080p30 with a small moving \
                region — a session where somebody is reading.",
        unit: "cores",
        better: Better::Lower,
        want: Some(0.10),
        portable: false,
    },
    Target {
        name: "path.screen.kbit",
        about: "What that session costs on the wire.",
        unit: "kbit/s",
        better: Better::Lower,
        want: None,
        portable: false,
    },
    Target {
        name: "path.motion.kbit",
        about: "Full-motion 1080p30 at the balanced profile.",
        unit: "kbit/s",
        better: Better::Lower,
        want: Some(5000.0),
        portable: false,
    },
    Target {
        name: "path.idle.kbit",
        about: "A 4K screen with nothing happening on it. **An upper bound \
                rather than the case the target describes**: the synthetic \
                pattern is one-pixel stripes, colour bars and grid lines, \
                designed to be the hardest thing there is to compress, and a \
                real idle desktop is mostly flat. What is left in this figure \
                is almost entirely the periodic keyframe, which is a policy \
                number rather than an encoder one.",
        unit: "kbit/s",
        better: Better::Lower,
        want: Some(50.0),
        portable: false,
    },
    Target {
        name: "path.idle.fps",
        about: "How many frames a second an idle screen actually sends. **The \
                machine-independent version of the claim above**: if \
                damage-driven capture works, the answer is only the periodic \
                keyframe — one every five seconds by policy, so a fifth of a \
                frame a second — whatever the picture happens to compress to.",
        unit: "fps",
        better: Better::Lower,
        want: Some(0.25),
        // The policy decides this, not the processor: any machine that can
        // manage a fifth of a frame a second gives the same answer.
        portable: true,
    },
    Target {
        name: "convert.ratio",
        about: "How many times cheaper it is to convert one line of text than \
                a whole frame. **The entire case for damage-driven \
                conversion**, and portable because it is a ratio of two \
                timings on the same machine — whatever the processor is, it \
                cancels.",
        unit: "x",
        better: Better::Higher,
        want: Some(20.0),
        portable: true,
    },
];

/// The target by that name, if there is one.
///
/// Used by the tests below and by anything that has a name and wants the
/// sentence that goes with it.
#[must_use]
#[cfg_attr(not(test), allow(dead_code))]
pub fn named(name: &str) -> Option<&'static Target> {
    TARGETS.iter().find(|t| t.name == name)
}

impl Target {
    /// Whether a measurement meets what the product promised.
    ///
    /// `None` when nothing was promised — which is not a pass and not a
    /// failure, and is said as neither.
    #[must_use]
    pub fn meets(&self, measured: f64) -> Option<bool> {
        let want = self.want?;
        Some(match self.better {
            Better::Lower => measured <= want,
            Better::Higher => measured >= want,
        })
    }

    /// Whether a measurement is worse than a baseline by more than the drift.
    #[must_use]
    pub fn regressed(&self, measured: f64, baseline: f64) -> bool {
        match self.better {
            Better::Lower => measured > baseline * (1.0 + DRIFT),
            Better::Higher => measured < baseline * (1.0 - DRIFT),
        }
    }

    /// Whether a measurement is better than a baseline by more than the drift.
    ///
    /// Reported so an improvement is visible and the baseline can be moved
    /// **deliberately**. Never moved automatically: a baseline that ratchets
    /// on a noisy measurement records the luckiest run anybody ever had, and
    /// then fails for everybody else.
    #[must_use]
    pub fn improved(&self, measured: f64, baseline: f64) -> bool {
        match self.better {
            Better::Lower => measured < baseline * (1.0 - DRIFT),
            Better::Higher => measured > baseline * (1.0 + DRIFT),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **A portable figure means the same thing on another machine.** An
    /// encode time does not; a ratio of two timings on one machine does, and
    /// so does a figure the policy decides. Getting this wrong in either
    /// direction is how a suite either fails on every CI run or catches
    /// nothing on any of them.
    #[test]
    fn only_the_figures_that_survive_a_change_of_machine_are_portable() {
        let portable: Vec<&str> = TARGETS
            .iter()
            .filter(|t| t.portable)
            .map(|t| t.name)
            .collect();
        assert_eq!(portable, vec!["path.idle.fps", "convert.ratio"]);

        // Nothing measured in milliseconds or cores can be portable: both are
        // properties of the processor.
        for t in TARGETS {
            if t.unit.contains("ms") || t.unit == "cores" {
                assert!(!t.portable, "{} is measured in {}", t.name, t.unit);
            }
        }
        // And a whole-path bitrate is not either: the frame rate ceiling is
        // reached on a fast machine and not on a slow one.
        for t in TARGETS.iter().filter(|t| t.name.starts_with("path.")) {
            if t.unit == "kbit/s" {
                assert!(!t.portable, "{} is a bitrate from a timed run", t.name);
            }
        }
    }

    #[test]
    fn every_target_says_what_it_is_and_is_named_once() {
        let mut seen = std::collections::BTreeSet::new();
        for t in TARGETS {
            assert!(!t.about.is_empty(), "{}", t.name);
            assert!(!t.unit.is_empty(), "{}", t.name);
            assert!(
                t.name.contains('.'),
                "{} is not in the group.thing form the report groups by",
                t.name
            );
            assert!(seen.insert(t.name), "{} is listed twice", t.name);
            assert_eq!(named(t.name).map(|f| f.name), Some(t.name));
        }
        assert!(named("nothing.like.this").is_none());
    }

    /// **A target that is not met and a baseline that moved are different
    /// things.** Conflating them is how a suite goes red for a known gap and
    /// stops being read.
    #[test]
    fn a_missed_target_is_not_a_regression() {
        let t = named("encode.motion.ms").expect("a known target");
        // Far worse than the target, and exactly the baseline.
        assert_eq!(t.meets(51.0), Some(false));
        assert!(
            !t.regressed(51.0, 51.0),
            "matching a baseline is not a change"
        );
        assert!(!t.improved(51.0, 51.0));
    }

    #[test]
    fn a_drift_within_the_band_is_not_a_regression_either_way() {
        let lower = named("encode.screen.ms").expect("a known target");
        assert!(!lower.regressed(11.0, 10.0), "10% worse is inside the band");
        assert!(lower.regressed(13.0, 10.0), "30% worse is not");
        assert!(!lower.improved(9.0, 10.0), "10% better is inside the band");
        assert!(lower.improved(7.0, 10.0));

        // And the other direction is not simply the same comparison.
        let higher = Target {
            better: Better::Higher,
            ..*lower
        };
        assert!(higher.regressed(7.0, 10.0));
        assert!(!higher.regressed(13.0, 10.0));
        assert!(higher.improved(13.0, 10.0));
    }

    #[test]
    fn a_target_with_nothing_promised_is_neither_met_nor_missed() {
        let t = named("convert.line.ms").expect("a known target");
        assert_eq!(t.want, None);
        assert_eq!(t.meets(0.0), None);
        assert_eq!(t.meets(1_000_000.0), None);
        // It can still regress, which is the point of measuring it at all.
        assert!(t.regressed(2.0, 1.0));
    }
}
