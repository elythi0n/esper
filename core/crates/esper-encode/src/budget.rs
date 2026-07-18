//! The ceiling, and what gives way to stay under it.
//!
//! One constraint shapes the whole media pipeline: **the host is usually not
//! your machine.** Pegging a colleague's CPU to show you their screen is a
//! product failure even if the picture is perfect, so host cost is a
//! correctness target rather than a benchmark.
//!
//! Without an enforced ceiling a software encoder under load simply takes what
//! it can get. So it is told a budget and degrades to stay inside it. It does
//! not get to decide it deserves more.
//!
//! # What gives way, in order
//!
//! **Framerate first.** Halving it halves cost linearly, and 30 fps with a
//! locally drawn cursor feels better than 60 with a laggy one. Resolution does
//! not scale cost linearly and it is what makes text legible.
//!
//! **Then quality.** A coarser quantiser costs less and blurs detail, which is
//! worse for text than a lower framerate is.
//!
//! **Then resolution, last.** It is the one that makes text unreadable, and
//! reading text is what remote desktop is disproportionately used for.
//!
//! The design's ladder also lists chroma between quality and resolution.
//! **There is no chroma lever in v1**: 4:2:0 is the only chroma format
//! baseline and main profile H.264 support, and 4:4:4 needs High 4:4:4
//! Predictive — a profile many decoders, including several webviews, will not
//! touch. So v1 degrades framerate, then quality, then resolution, and the
//! chroma step arrives if and when the profile does.
//!
//! # Why there is a floor
//!
//! Degrading without a bottom produces a session that is technically running
//! and useless — four frames a second at quarter resolution is not a remote
//! desktop, it is a slideshow of a blurred screen. Below the floor the honest
//! answer is to tell the user the host cannot do this, which the HUD does by
//! reporting the degradation and why.
//!
//! # Why the clock and the measurements are parameters
//!
//! Every decision here is about measured cost over elapsed time. A governor
//! that read the clock and timed the encoder itself could only be tested by
//! running an encoder, which makes the test slow, machine-dependent, and
//! unable to reach the interesting cases at all — a host at 300% of budget is
//! not something you can arrange on demand.

use std::time::Duration;

/// How much of the machine the pipeline may use.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Budget {
    /// Share of a single core, so `1.0` is one core fully occupied.
    ///
    /// A share rather than a percentage of the machine, because that is the
    /// unit the cost is measured in and the unit a host's owner recognises:
    /// "one core" means something to a person in a way that "5% of a
    /// twenty-thread laptop" does not.
    pub cores: f64,
}

impl Budget {
    /// The shipping default: one core, or a quarter of the machine, whichever
    /// is smaller.
    ///
    /// The `min` is what makes it safe on small hosts. A quarter of a
    /// twenty-thread laptop is five cores, which is far more than this needs;
    /// a quarter of a two-core virtual machine is half a core, which is less
    /// than one and is the right answer there. Taking the smaller of the two
    /// means the rule is generous on big machines and careful on small ones,
    /// rather than the other way round.
    #[must_use]
    pub fn default_for(total_cores: usize) -> Self {
        let quarter = total_cores as f64 * 0.25;
        Self {
            cores: quarter.clamp(MINIMUM_CORES, 1.0),
        }
    }

    /// A budget of exactly this many cores, for `session.cpu_budget`.
    #[must_use]
    pub fn of(cores: f64) -> Self {
        Self {
            cores: cores.max(MINIMUM_CORES),
        }
    }

    /// Whether a measured share is inside this budget.
    #[must_use]
    pub fn allows(&self, share: f64) -> bool {
        share <= self.cores
    }

    /// How many encoder threads this budget can actually pay for.
    ///
    /// # Why the thread count follows from the budget
    ///
    /// Threading and the CPU budget are in direct conflict, and it took a
    /// measurement to see it. Slice-parallel encoding of full-motion 1080p:
    ///
    /// | Threads | Wall ms/frame | **CPU ms/frame** | Share of a core at 30 fps |
    /// |---:|---:|---:|---:|
    /// | 1 | 49.7 | 49.4 | 148% |
    /// | 2 | 29.4 | 56.0 | 168% |
    /// | 4 | 15.5 | 52.6 | 158% |
    /// | 8 | 20.5 | 61.8 | 186% |
    ///
    /// Threading fixes **latency** — one thread cannot finish a full-motion
    /// 1080p frame inside a 33 ms interval and four can. It does not fix
    /// **cost**: the CPU total goes *up* by 6% to 25%, because slices are
    /// coded independently and the parallelism has overhead.
    ///
    /// So four threads inside a one-core budget is incoherent: it means using
    /// four cores' worth of CPU for a quarter of the wall clock, which
    /// averages to the same violation. A budget of one core buys one thread.
    ///
    /// Deriving the count from the budget makes that explicit, rather than
    /// leaving two knobs that quietly fight — which is what a separate
    /// `threads` setting was.
    ///
    /// At least one, because zero threads is not a thing an encoder can be.
    #[must_use]
    pub fn encoder_threads(&self) -> u16 {
        // Truncating rather than rounding: a budget of 1.9 cores buys one
        // thread, because the second would spend CPU the budget has not got.
        let whole = self.cores.floor().max(1.0);
        // Capped, because the machine's own core count is a hard limit and a
        // raised budget on a small host should not ask for threads that do not
        // exist.
        let cores = std::thread::available_parallelism().map_or(1, std::num::NonZero::get);
        let wanted = whole.min(cores as f64) as u16;
        wanted.max(1)
    }
}

/// The least budget anything can be given.
///
/// A budget of zero, or a negative one from a mistyped configuration, would
/// mean an encoder that may never run — a session that connects and shows
/// nothing, with no error to explain it. A twentieth of a core is enough for a
/// terminal-sized picture at a low framerate.
pub const MINIMUM_CORES: f64 = 0.05;

impl Default for Budget {
    fn default() -> Self {
        Self::default_for(std::thread::available_parallelism().map_or(1, std::num::NonZero::get))
    }
}

/// How coarsely the encoder may quantise.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Fidelity {
    /// Sharp. What text needs.
    High,
    /// The default.
    Balanced,
    /// Visibly softer. Cheaper.
    Low,
}

impl Fidelity {
    /// The quantiser range to hand the encoder.
    ///
    /// Lower is sharper and more expensive. The high end is left well below
    /// H.264's maximum of 51: past about 40 the picture is blocky enough that
    /// a user would rather have fewer frames, which is why framerate is
    /// spent first.
    #[must_use]
    pub const fn qp_range(self) -> (u8, u8) {
        match self {
            Self::High => (14, 32),
            Self::Balanced => (20, 38),
            Self::Low => (26, 42),
        }
    }

    /// A name for the HUD.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::High => "high",
            Self::Balanced => "balanced",
            Self::Low => "low",
        }
    }
}

/// One rung of the degradation ladder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Level {
    /// Frames per second ceiling.
    pub fps: u32,
    /// Resolution as a fraction of native, as `(numerator, denominator)`.
    ///
    /// A fraction rather than a float so the arithmetic is exact and the
    /// resulting sizes are reproducible — a resolution that differs by a pixel
    /// between two runs would reconfigure the encoder for no reason.
    pub scale: (u32, u32),
    /// How coarsely to quantise.
    pub fidelity: Fidelity,
}

impl Level {
    /// The picture size this level asks for, given the native size.
    ///
    /// Rounded down to even, because 4:2:0 has no representation for an odd
    /// dimension, and floored at 16 so the encoder is never handed something
    /// it refuses.
    #[must_use]
    pub fn size(&self, native_w: u32, native_h: u32) -> (u32, u32) {
        let (num, den) = (self.scale.0.max(1), self.scale.1.max(1));
        let scale = |v: u32| {
            let scaled = (u64::from(v) * u64::from(num) / u64::from(den)) as u32;
            (scaled & !1).max(16)
        };
        (scale(native_w), scale(native_h))
    }

    /// Roughly what this level costs relative to the top of the ladder.
    ///
    /// Framerate scales cost linearly and pixel count roughly does too.
    /// Fidelity is the loosest of the three — a coarser quantiser saves in the
    /// entropy coder and the motion search, not in proportion to anything
    /// tidy — so it is given a modest, deliberately conservative factor.
    ///
    /// This is an *estimate*, used only to decide which rung to try next. The
    /// governor then measures what actually happened, which is why a rough
    /// model is enough and a precise one would be false confidence.
    #[must_use]
    pub fn relative_cost(&self, top: &Self) -> f64 {
        let fps = f64::from(self.fps) / f64::from(top.fps.max(1));
        let pixels = (f64::from(self.scale.0) / f64::from(self.scale.1.max(1))).powi(2)
            / (f64::from(top.scale.0) / f64::from(top.scale.1.max(1))).powi(2);
        // **Relative to the top's fidelity, not absolute.** An earlier version
        // used the factor directly, so a top rung at high fidelity reported
        // its own relative cost as 1.15 rather than 1 — which quietly shifted
        // every projection by 15% and made a test of the governor's
        // anti-oscillation logic pass for the wrong reason.
        let fidelity = fidelity_factor(self.fidelity) / fidelity_factor(top.fidelity);
        fps * pixels * fidelity
    }

    /// A description for the HUD, e.g. `30 fps, full, balanced`.
    #[must_use]
    pub fn describe(&self) -> String {
        let scale = if self.scale.0 == self.scale.1 {
            "full".to_owned()
        } else {
            format!("{}/{}", self.scale.0, self.scale.1)
        };
        format!("{} fps, {}, {}", self.fps, scale, self.fidelity.name())
    }
}

/// How much a fidelity level costs, on an arbitrary scale.
///
/// Only ratios of these are used, so the absolute values mean nothing. The
/// spread is deliberately narrow: a coarser quantiser saves in the entropy
/// coder and the motion search, not in proportion to anything tidy, so
/// claiming a large factor would be false precision.
const fn fidelity_factor(f: Fidelity) -> f64 {
    match f {
        Fidelity::High => 1.15,
        Fidelity::Balanced => 1.0,
        Fidelity::Low => 0.85,
    }
}

/// The ladder, best first.
///
/// Written out rather than generated, because the order is the design decision
/// and a generated cross-product would put resolution cuts above framerate
/// cuts by accident. Read down the list: framerate falls three times before
/// fidelity moves, and fidelity twice before resolution does.
pub const LADDER: &[Level] = &[
    Level {
        fps: 60,
        scale: (1, 1),
        fidelity: Fidelity::High,
    },
    Level {
        fps: 30,
        scale: (1, 1),
        fidelity: Fidelity::High,
    },
    Level {
        fps: 20,
        scale: (1, 1),
        fidelity: Fidelity::High,
    },
    Level {
        fps: 15,
        scale: (1, 1),
        fidelity: Fidelity::High,
    },
    // Framerate has fallen as far as it usefully can; text is still sharp.
    Level {
        fps: 15,
        scale: (1, 1),
        fidelity: Fidelity::Balanced,
    },
    Level {
        fps: 15,
        scale: (1, 1),
        fidelity: Fidelity::Low,
    },
    // Only now does the picture get smaller.
    Level {
        fps: 15,
        scale: (3, 4),
        fidelity: Fidelity::Balanced,
    },
    Level {
        fps: 10,
        scale: (3, 4),
        fidelity: Fidelity::Low,
    },
    // The floor. Below this it is a slideshow of a blurred screen, and the
    // honest answer is to say the host cannot do it.
    Level {
        fps: 10,
        scale: (1, 2),
        fidelity: Fidelity::Low,
    },
];

/// How eagerly the governor moves.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Reflexes {
    /// Consecutive over-budget samples before stepping down.
    ///
    /// Small, because being over budget is the thing that must not persist.
    pub patience_down: u32,
    /// Consecutive comfortable samples before stepping up.
    ///
    /// Much larger than `patience_down`. The asymmetry is the point: a
    /// governor equally quick in both directions oscillates, and oscillation
    /// is visible as the picture pulsing between sharp and soft, which is more
    /// annoying than simply being one step lower.
    pub patience_up: u32,
    /// How far under budget it must be before stepping up.
    ///
    /// Stepping up when merely *inside* budget guarantees going back over,
    /// because the higher rung costs more by definition. The headroom has to
    /// be enough to absorb the step.
    pub headroom: f64,
}

impl Default for Reflexes {
    fn default() -> Self {
        Self {
            patience_down: 3,
            patience_up: 60,
            headroom: 0.7,
        }
    }
}

/// Why the level last moved, for the HUD.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Moved {
    /// Nothing changed.
    Held,
    /// Degraded to get back inside the budget.
    Down,
    /// Recovered, having been comfortably inside it.
    Up,
    /// Over budget and already at the floor: nothing left to give.
    AtFloor,
}

/// Keeps the pipeline inside its CPU budget.
#[derive(Debug)]
pub struct Governor {
    budget: Budget,
    reflexes: Reflexes,
    /// Index into [`LADDER`]. Zero is best.
    rung: usize,
    over: u32,
    comfortable: u32,
    /// The most recent measured share, for the HUD.
    last_share: f64,
    steps_down: u64,
    steps_up: u64,
}

impl Governor {
    /// A governor starting at the top of the ladder.
    #[must_use]
    pub fn new(budget: Budget, reflexes: Reflexes) -> Self {
        Self {
            budget,
            reflexes,
            rung: 0,
            over: 0,
            comfortable: 0,
            last_share: 0.0,
            steps_down: 0,
            steps_up: 0,
        }
    }

    /// A governor with the shipping defaults for this machine.
    #[must_use]
    pub fn detected() -> Self {
        Self::new(Budget::default(), Reflexes::default())
    }

    /// The level in force.
    #[must_use]
    pub fn level(&self) -> Level {
        LADDER.get(self.rung).copied().unwrap_or(Level {
            fps: 15,
            scale: (1, 1),
            fidelity: Fidelity::Balanced,
        })
    }

    /// The budget in force.
    #[must_use]
    pub const fn budget(&self) -> Budget {
        self.budget
    }

    /// Whether the pipeline is currently degraded, for the HUD's honest
    /// signal — which reports cost rather than apologising for the codec.
    #[must_use]
    pub const fn is_degraded(&self) -> bool {
        self.rung > 0
    }

    /// Whether there is nothing left to give.
    #[must_use]
    pub fn is_at_floor(&self) -> bool {
        self.rung.saturating_add(1) >= LADDER.len()
    }

    /// The most recent measured share of a core.
    #[must_use]
    pub const fn last_share(&self) -> f64 {
        self.last_share
    }

    /// How many times it has stepped down and up.
    #[must_use]
    pub const fn steps(&self) -> (u64, u64) {
        (self.steps_down, self.steps_up)
    }

    /// Report what the pipeline actually cost over a period, and get back
    /// whether the level moved.
    ///
    /// `spent` is CPU time in the pipeline — conversion plus encode — and
    /// `elapsed` is wall clock. Their ratio is the share of one core, which is
    /// the unit the budget is written in.
    pub fn observe(&mut self, spent: Duration, elapsed: Duration) -> Moved {
        let secs = elapsed.as_secs_f64();
        if secs <= 0.0 {
            // No time passed, so there is no share to compute. Reporting a
            // division by zero as "infinitely over budget" would degrade the
            // session on the strength of a caller's rounding.
            return Moved::Held;
        }
        let share = spent.as_secs_f64() / secs;
        self.last_share = share;

        if !self.budget.allows(share) {
            self.comfortable = 0;
            self.over = self.over.saturating_add(1);
            if self.over < self.reflexes.patience_down {
                return Moved::Held;
            }
            self.over = 0;
            if self.is_at_floor() {
                // Nothing left. Said plainly rather than by silently holding,
                // so the HUD can tell the user the host cannot do this.
                return Moved::AtFloor;
            }
            self.rung = self.rung.saturating_add(1);
            self.steps_down = self.steps_down.saturating_add(1);
            return Moved::Down;
        }

        // Inside the budget. Stepping up needs two things, and the second is
        // the one that stops a slow oscillation.
        self.over = 0;
        if self.rung == 0 {
            self.comfortable = 0;
            return Moved::Held;
        }

        // First: headroom, not merely being inside. The higher rung costs
        // more by definition, so recovering on a bare pass guarantees going
        // straight back over.
        if share > self.budget.cores * self.reflexes.headroom {
            self.comfortable = 0;
            return Moved::Held;
        }

        // Second: **the projected cost of the rung above must fit.**
        //
        // Headroom alone is a fixed fraction, and the gap between two rungs is
        // not. A host whose cost at 30 fps is 65% of budget has headroom by
        // any reasonable measure, and doubling to 60 fps puts it at 130% — so
        // it steps up, goes over, steps back down, and does it again for as
        // long as the session lasts. Slowly, which is worse than quickly:
        // quickly is a bug someone notices and fixes, slowly is a session that
        // changes quality every minute for no visible reason.
        //
        // The ladder's own cost model answers this. It is rough — see
        // `relative_cost` — and rough is enough, because the alternative is
        // trying the rung and finding out, which is exactly the oscillation.
        if !self.projected_step_up_fits(share) {
            self.comfortable = 0;
            return Moved::Held;
        }

        self.comfortable = self.comfortable.saturating_add(1);
        if self.comfortable < self.reflexes.patience_up {
            return Moved::Held;
        }
        self.comfortable = 0;
        self.rung = self.rung.saturating_sub(1);
        self.steps_up = self.steps_up.saturating_add(1);
        Moved::Up
    }

    /// Whether the rung above would fit, given what this one measured.
    ///
    /// Scales the observed share by the ratio of the two rungs' modelled
    /// costs. A little pessimism is deliberate: being wrong towards "do not
    /// step up" costs a level the host might have managed, and being wrong the
    /// other way costs an oscillation the user watches.
    fn projected_step_up_fits(&self, share: f64) -> bool {
        let Some(current) = LADDER.get(self.rung) else {
            return false;
        };
        let Some(above) = LADDER.get(self.rung.saturating_sub(1)) else {
            return false;
        };
        let top = match LADDER.first() {
            Some(t) => t,
            None => return false,
        };
        let here = current.relative_cost(top);
        if here <= 0.0 {
            return false;
        }
        let projected = share * above.relative_cost(top) / here;
        // The same headroom applied to the projection, so a rung that would
        // land exactly on the budget is not attempted.
        projected <= self.budget.cores * self.reflexes.headroom
    }

    /// A line for the session HUD.
    ///
    /// Reports cost and, when degraded, what gave way — rather than
    /// apologising for the codec, which is not the useful information.
    #[must_use]
    pub fn describe(&self) -> String {
        let pct = self.last_share * 100.0;
        let cap = self.budget.cores * 100.0;
        if self.is_at_floor() && self.is_degraded() {
            format!(
                "{pct:.0}% of a core (cap {cap:.0}%) — {}, and this host cannot go lower",
                self.level().describe()
            )
        } else if self.is_degraded() {
            format!(
                "{pct:.0}% of a core (cap {cap:.0}%) — reduced to {}",
                self.level().describe()
            )
        } else {
            format!(
                "{pct:.0}% of a core (cap {cap:.0}%) — {}",
                self.level().describe()
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    /// Feed the same measurement repeatedly.
    fn feed(g: &mut Governor, spent_ms: u64, times: u32) -> Vec<Moved> {
        (0..times)
            .map(|_| g.observe(ms(spent_ms), ms(1000)))
            .collect()
    }

    #[test]
    fn the_default_budget_is_a_core_or_a_quarter_of_the_machine() {
        // A big machine: a quarter is more than a core, so a core wins.
        assert_eq!(Budget::default_for(20).cores, 1.0);
        assert_eq!(Budget::default_for(8).cores, 1.0);
        assert_eq!(Budget::default_for(4).cores, 1.0);
        // A small one: a quarter is less than a core, and a quarter wins.
        assert_eq!(Budget::default_for(2).cores, 0.5);
        assert_eq!(Budget::default_for(3).cores, 0.75);
    }

    /// The `min` is what makes the rule careful on small hosts and generous on
    /// large ones rather than the other way round.
    #[test]
    fn a_small_host_gets_a_smaller_share_not_a_larger_one() {
        let small = Budget::default_for(2).cores;
        let large = Budget::default_for(32).cores;
        assert!(
            small < large,
            "a two-core host got {small} and a 32-core host {large}"
        );
    }

    /// A budget of zero would be a session that connects and shows nothing,
    /// with no error to explain it.
    #[test]
    fn a_budget_can_never_be_zero_or_negative() {
        assert_eq!(Budget::of(0.0).cores, MINIMUM_CORES);
        assert_eq!(Budget::of(-5.0).cores, MINIMUM_CORES);
        assert_eq!(Budget::default_for(0).cores, MINIMUM_CORES);
        assert!(
            Budget::of(4.0).cores > 1.0,
            "and a raised budget is honoured"
        );
    }

    /// **The conflict that a separate `threads` setting hid.** Threading
    /// fixes latency and costs CPU, so a one-core budget cannot pay for four
    /// threads however many cores the machine has.
    #[test]
    fn a_budget_of_one_core_buys_one_thread() {
        assert_eq!(Budget::of(1.0).encoder_threads(), 1);
        assert_eq!(
            Budget::of(1.9).encoder_threads(),
            1,
            "1.9 cores buys one thread, not two: the second would spend CPU \
             the budget has not got"
        );
        assert_eq!(Budget::of(2.0).encoder_threads(), 2);
        assert_eq!(Budget::of(4.0).encoder_threads(), 4);
        // A fraction of a core still buys a thread, because zero threads is
        // not a thing an encoder can be.
        assert_eq!(Budget::of(0.1).encoder_threads(), 1);
        assert_eq!(Budget::default_for(2).encoder_threads(), 1);
    }

    /// A raised budget on a small machine must not ask for threads that do
    /// not exist.
    #[test]
    fn the_thread_count_never_exceeds_the_machine() {
        let cores = std::thread::available_parallelism().map_or(1, std::num::NonZero::get);
        let absurd = Budget::of(1024.0);
        assert!(
            usize::from(absurd.encoder_threads()) <= cores,
            "asked for {} threads on a {cores} core machine",
            absurd.encoder_threads()
        );
    }

    #[test]
    fn a_host_inside_its_budget_stays_at_the_top() {
        let mut g = Governor::new(Budget::of(1.0), Reflexes::default());
        // 200 ms of work per second: 20% of a core.
        let moves = feed(&mut g, 200, 200);
        assert!(
            moves.iter().all(|m| *m == Moved::Held),
            "an idle host should never be degraded"
        );
        assert!(!g.is_degraded());
        assert_eq!(g.level().fps, 60);
    }

    #[test]
    fn a_host_over_budget_is_degraded() {
        let mut g = Governor::new(Budget::of(1.0), Reflexes::default());
        // 1.5 s of work per second: 150% of a core.
        let moves = feed(&mut g, 1500, 3);
        assert_eq!(moves.last().copied(), Some(Moved::Down));
        assert!(g.is_degraded());
        assert_eq!(g.steps().0, 1);
    }

    /// **Framerate first.** Halving it halves cost, and 30 fps with a locally
    /// drawn cursor feels better than 60 with a laggy one.
    #[test]
    fn framerate_falls_before_anything_else() {
        let mut g = Governor::new(Budget::of(1.0), Reflexes::default());
        let top = g.level();
        let _ = feed(&mut g, 1500, 3);
        let after = g.level();

        assert!(after.fps < top.fps, "framerate should have fallen first");
        assert_eq!(
            after.scale, top.scale,
            "resolution must not fall while framerate can"
        );
        assert_eq!(
            after.fidelity, top.fidelity,
            "fidelity must not fall while framerate can"
        );
    }

    /// **Resolution last.** It is what makes text unreadable, and reading text
    /// is what remote desktop is disproportionately used for.
    #[test]
    fn resolution_is_the_last_thing_to_go() {
        let mut order = Vec::new();
        for level in LADDER {
            order.push((level.fps, level.scale, level.fidelity));
        }
        // The first rung where resolution drops...
        let first_scaled = LADDER
            .iter()
            .position(|l| l.scale != (1, 1))
            .expect("the ladder must eventually scale down");
        // ...must come after the last rung where framerate or fidelity moved.
        let first_fps_drop = LADDER
            .iter()
            .position(|l| l.fps < 60)
            .expect("the ladder must reduce framerate");
        let first_fidelity_drop = LADDER
            .iter()
            .position(|l| l.fidelity != Fidelity::High)
            .expect("the ladder must reduce fidelity");

        assert!(
            first_fps_drop < first_scaled,
            "resolution dropped at rung {first_scaled} before framerate at \
             {first_fps_drop}: {order:?}"
        );
        assert!(
            first_fidelity_drop < first_scaled,
            "resolution dropped before fidelity: {order:?}"
        );
    }

    /// The top of the ladder costs exactly one by definition, which is what
    /// "relative to the top" means. An earlier version returned 1.15 for it,
    /// because the fidelity factor was absolute rather than relative — and
    /// that 15% skew was invisible in every test until one of them needed the
    /// projections to be right.
    #[test]
    fn the_top_of_the_ladder_costs_exactly_one() {
        let top = LADDER.first().copied().expect("a top");
        assert!(
            (top.relative_cost(&top) - 1.0).abs() < 1e-12,
            "the top rung reports its own cost as {}",
            top.relative_cost(&top)
        );
        // And halving the framerate halves the cost, with nothing else moving.
        let half_rate = Level { fps: 30, ..top };
        assert!((half_rate.relative_cost(&top) - 0.5).abs() < 1e-12);
        // Halving both dimensions quarters the pixels.
        let half_size = Level {
            scale: (1, 2),
            ..top
        };
        assert!((half_size.relative_cost(&top) - 0.25).abs() < 1e-12);
    }

    /// The ladder must actually descend in cost, or degrading makes things
    /// worse.
    #[test]
    fn every_rung_costs_less_than_the_one_above_it() {
        let top = LADDER.first().copied().expect("a ladder needs a top");
        let mut previous = f64::MAX;
        for (i, level) in LADDER.iter().enumerate() {
            let cost = level.relative_cost(&top);
            assert!(
                cost < previous,
                "rung {i} ({}) costs {cost:.3}, which is not less than the \
                 rung above at {previous:.3}",
                level.describe()
            );
            previous = cost;
        }
        // And the floor must be a substantial saving, or the ladder is
        // decoration.
        let floor = LADDER.last().copied().expect("a ladder needs a floor");
        assert!(
            floor.relative_cost(&top) < 0.1,
            "the floor is only {:.0}% cheaper than the top",
            (1.0 - floor.relative_cost(&top)) * 100.0
        );
    }

    /// **The oscillation the asymmetric patience exists to prevent.** A
    /// governor equally quick in both directions pulses the picture between
    /// sharp and soft, which is more annoying than simply being one step
    /// lower.
    #[test]
    fn a_host_hovering_at_its_budget_does_not_oscillate() {
        let mut g = Governor::new(Budget::of(1.0), Reflexes::default());
        // Alternate just over and just under the budget for a long time.
        let mut moves = 0;
        for i in 0..2000 {
            let spent = if i % 2 == 0 { 1010 } else { 990 };
            match g.observe(ms(spent), ms(1000)) {
                Moved::Held => {}
                _ => moves += 1,
            }
        }
        assert!(
            moves <= 4,
            "the level moved {moves} times while hovering at the budget, \
             which the user sees as the picture pulsing"
        );
    }

    /// **The oscillation that a fixed headroom does not prevent, and that the
    /// first version of this test could not see.**
    ///
    /// Simulates a host whose cost actually depends on the level, which is the
    /// only way the scenario can arise: at 30 fps it sits at 65% of budget —
    /// comfortable by any measure — and doubling to 60 fps puts it at 130%. A
    /// governor that steps up on headroom alone goes over, steps back down,
    /// waits, and does it again for the whole session.
    ///
    /// Slowly, which is worse than quickly: quickly is a bug someone notices,
    /// slowly is a session that changes quality every minute for no visible
    /// reason. The fix is that the ladder's cost model gets consulted before
    /// stepping up.
    ///
    /// The earlier version of this test fed a fixed measurement, so the
    /// headroom check alone satisfied it and removing the patience asymmetry
    /// changed nothing. A test that cannot fail is not a test.
    #[test]
    fn a_host_that_fits_one_rung_but_not_the_one_above_settles() {
        let budget = Budget::of(1.0);
        let mut g = Governor::new(budget, Reflexes::default());
        let top = LADDER.first().copied().expect("a top");

        // This host would need 1.3 cores at the top of the ladder.
        let cost_at_top = 1.3;
        let mut moves = 0u32;
        let mut rungs = Vec::new();

        for _ in 0..5000 {
            let share = cost_at_top * g.level().relative_cost(&top);
            let spent = Duration::from_secs_f64(share);
            if g.observe(spent, Duration::from_secs(1)) != Moved::Held {
                moves += 1;
            }
            rungs.push(g.rung);
        }

        assert!(
            moves <= 3,
            "the level moved {moves} times on a host whose cost is stable —              the user sees that as the picture changing quality for no reason"
        );
        // And it must settle somewhere inside the budget rather than at the
        // floor: this host can manage a real level.
        let settled = g.level().relative_cost(&top) * cost_at_top;
        assert!(
            budget.allows(settled),
            "settled at {} which costs {settled:.2} cores against a budget of              {}",
            g.level().describe(),
            budget.cores
        );
        assert!(
            !g.is_at_floor(),
            "a host needing 1.3 cores at the top should not end at the floor,              it ended at {}",
            g.level().describe()
        );
        // The last thousand samples must all be the same rung.
        let tail: std::collections::BTreeSet<usize> =
            rungs.iter().rev().take(1000).copied().collect();
        assert_eq!(tail.len(), 1, "still moving at the end: {tail:?}");
    }

    /// Recovery happens, but only with real headroom and only slowly.
    #[test]
    fn a_host_that_becomes_idle_recovers() {
        let mut g = Governor::new(Budget::of(1.0), Reflexes::default());
        let _ = feed(&mut g, 1500, 9);
        let degraded_to = g.rung;
        assert!(degraded_to > 0, "should have degraded");

        // Now comfortably idle: 10% of a core.
        let moves = feed(&mut g, 100, 500);
        assert!(moves.contains(&Moved::Up), "an idle host should recover");
        assert!(g.rung < degraded_to);
        assert!(g.steps().1 > 0);
    }

    /// Recovering on a bare pass guarantees going straight back over, because
    /// the higher rung costs more by definition.
    #[test]
    fn recovery_needs_headroom_not_merely_being_inside_the_budget() {
        let mut g = Governor::new(Budget::of(1.0), Reflexes::default());
        let _ = feed(&mut g, 1500, 3);
        let rung = g.rung;

        // Just inside the budget: 95% of a core. Not enough to step up into.
        let moves = feed(&mut g, 950, 1000);
        assert!(
            !moves.contains(&Moved::Up),
            "stepped up on a bare pass, which guarantees going back over"
        );
        assert_eq!(g.rung, rung);
    }

    /// At the floor, over budget is said plainly rather than hidden — so the
    /// HUD can tell the user the host cannot do this.
    #[test]
    fn at_the_floor_being_over_budget_is_reported_rather_than_hidden() {
        let mut g = Governor::new(Budget::of(0.1), Reflexes::default());
        // Hammer it until it bottoms out.
        let mut saw_floor = false;
        for _ in 0..200 {
            if g.observe(ms(5000), ms(1000)) == Moved::AtFloor {
                saw_floor = true;
            }
        }
        assert!(saw_floor, "should have reported reaching the floor");
        assert!(g.is_at_floor());
        assert_eq!(
            g.rung,
            LADDER.len() - 1,
            "and it must not step past the end of the ladder"
        );
        assert!(g.describe().contains("cannot go lower"), "{}", g.describe());
    }

    /// A brief spike must not degrade the session. A garbage collection
    /// somewhere else on the machine should not cost the user their framerate.
    #[test]
    fn a_single_spike_does_not_degrade_anything() {
        let mut g = Governor::new(Budget::of(1.0), Reflexes::default());
        let _ = feed(&mut g, 200, 50);
        assert_eq!(g.observe(ms(3000), ms(1000)), Moved::Held, "one spike");
        let _ = feed(&mut g, 200, 50);
        assert!(!g.is_degraded(), "a single spike cost the user their level");
    }

    /// Sizes have to be even, because 4:2:0 has no representation for an odd
    /// dimension, and never below what the encoder accepts.
    #[test]
    fn every_rung_produces_a_size_the_encoder_will_accept() {
        for native in [(1920u32, 1080u32), (1366, 768), (3840, 2160), (17, 17)] {
            for level in LADDER {
                let (w, h) = level.size(native.0, native.1);
                assert!(w.is_multiple_of(2) && h.is_multiple_of(2), "{w}x{h}");
                assert!(w >= 16 && h >= 16, "{w}x{h} from {native:?}");
            }
        }
    }

    #[test]
    fn scaling_is_exact_and_reproducible() {
        let half = Level {
            fps: 30,
            scale: (1, 2),
            fidelity: Fidelity::Balanced,
        };
        assert_eq!(half.size(1920, 1080), (960, 540));
        let three_quarters = Level {
            fps: 30,
            scale: (3, 4),
            fidelity: Fidelity::Balanced,
        };
        assert_eq!(three_quarters.size(1920, 1080), (1440, 810));
        // An odd result rounds down to even rather than up.
        assert_eq!(three_quarters.size(1366, 768), (1024, 576));
    }

    /// Lower fidelity must mean a coarser quantiser, or the lever does
    /// nothing.
    #[test]
    fn lower_fidelity_means_a_coarser_quantiser() {
        let (hi_min, hi_max) = Fidelity::High.qp_range();
        let (mid_min, mid_max) = Fidelity::Balanced.qp_range();
        let (lo_min, lo_max) = Fidelity::Low.qp_range();
        assert!(hi_min < mid_min && mid_min < lo_min);
        assert!(hi_max < mid_max && mid_max < lo_max);
        // And nothing exceeds H.264's range.
        for f in [Fidelity::High, Fidelity::Balanced, Fidelity::Low] {
            let (min, max) = f.qp_range();
            assert!(min < max && max <= 51, "{}: {min}..{max}", f.name());
        }
    }

    /// A caller reporting no elapsed time must not degrade the session on the
    /// strength of its own rounding.
    #[test]
    fn no_elapsed_time_is_not_infinite_cost() {
        let mut g = Governor::new(Budget::of(1.0), Reflexes::default());
        for _ in 0..100 {
            assert_eq!(g.observe(ms(50), Duration::ZERO), Moved::Held);
        }
        assert!(!g.is_degraded());
    }

    /// The HUD reports cost, not an apology for the codec.
    #[test]
    fn the_description_reports_cost_and_what_gave_way() {
        let mut g = Governor::new(Budget::of(1.0), Reflexes::default());
        let _ = feed(&mut g, 200, 5);
        let idle = g.describe();
        assert!(idle.contains("20% of a core"), "{idle}");
        assert!(idle.contains("cap 100%"), "{idle}");
        assert!(!idle.contains("reduced"), "{idle}");

        let _ = feed(&mut g, 1500, 3);
        let degraded = g.describe();
        assert!(degraded.contains("reduced to"), "{degraded}");
        assert!(degraded.contains("fps"), "{degraded}");
    }

    /// Whatever is reported, in whatever order, the governor must stay on the
    /// ladder and never panic.
    #[test]
    fn arbitrary_measurements_never_panic_or_leave_the_ladder() {
        let mut x = 0x7F4A_7C15_9E37_79B9u64;
        let mut g = Governor::new(Budget::of(0.4), Reflexes::default());
        for _ in 0..50_000 {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            let spent = ms(x % 10_000);
            let elapsed = ms(x % 3);
            let _ = g.observe(spent, elapsed);
            assert!(g.rung < LADDER.len(), "left the ladder at {}", g.rung);
            let (w, h) = g.level().size(1920, 1080);
            assert!(w >= 16 && h >= 16);
        }
    }
}
