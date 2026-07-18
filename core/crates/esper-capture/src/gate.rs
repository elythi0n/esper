//! Whether to encode this frame at all.
//!
//! With software encode, not encoding an unchanged screen is not an
//! optimisation — it is the mechanism that makes the product usable. A person
//! reading a document on a 4K screen should cost single-digit kilobits per
//! second, and the only way to get there is to notice that the screen is not
//! changing.
//!
//! The ladder, in order of how much a screen is doing:
//!
//! | Screen state | What happens |
//! |---|---|
//! | Changing | Encode, up to the frame rate ceiling |
//! | Still for over a second | One frame a second, for resilience |
//! | Still for over ten seconds | A keyframe every five seconds, nothing between |
//!
//! # Skipping must not lose an update
//!
//! The rate ceiling means a frame can arrive and not be encoded. If its damage
//! were dropped with it, those pixels would never be sent and the viewer would
//! keep a stale patch of screen for as long as nothing else happened to change
//! there — a bug that shows as "part of the window is out of date" and is
//! nearly impossible to reproduce on purpose.
//!
//! So the gate **accumulates** damage across skipped frames and hands the
//! union to whoever encodes next. This is only sound because every frame is a
//! complete picture rather than a delta: the later frame already contains the
//! earlier frame's changes, so encoding it against the accumulated region is
//! correct.
//!
//! # Why the clock is a parameter
//!
//! Every decision here is about elapsed time, and a state machine that read
//! the clock itself could only be tested by sleeping. Ten seconds of idle
//! behaviour is then ten seconds of test.

use std::time::{Duration, Instant};

use crate::damage::{Damage, Regions};
use crate::geometry::Rect;

/// How the ladder is configured.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Policy {
    /// Frames per second ceiling while the screen is changing.
    pub max_fps: u32,
    /// Still for this long, and the screen counts as idle.
    pub idle_after: Duration,
    /// Frames per second while idle, for resilience against a lost packet.
    pub idle_fps: u32,
    /// Still for this long, and even the idle rate stops.
    pub deep_idle_after: Duration,
    /// A keyframe at least this often, whatever else is happening.
    ///
    /// This is the recovery path for a decoder that has lost sync and for a
    /// viewer that joined late, so it applies in every state.
    pub keyframe_interval: Duration,
}

impl Default for Policy {
    /// The shipping defaults, which are the table in the module documentation.
    fn default() -> Self {
        Self {
            max_fps: 60,
            idle_after: Duration::from_secs(1),
            idle_fps: 1,
            deep_idle_after: Duration::from_secs(10),
            keyframe_interval: Duration::from_secs(5),
        }
    }
}

impl Policy {
    /// Shortest gap between encodes while the screen is changing.
    fn active_interval(&self) -> Duration {
        Duration::from_nanos(1_000_000_000 / u64::from(self.max_fps.max(1)))
    }

    /// Gap between the resilience frames sent while idle.
    fn idle_interval(&self) -> Duration {
        Duration::from_nanos(1_000_000_000 / u64::from(self.idle_fps.max(1)))
    }
}

/// How busy the screen is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rung {
    /// Changing. Encode up to the ceiling.
    Active,
    /// Still for a while. One frame a second.
    Idle,
    /// Still for a long while. Keyframes only.
    DeepIdle,
}

impl Rung {
    /// A word for the session HUD.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Idle => "idle",
            Self::DeepIdle => "deep-idle",
        }
    }
}

/// Why a frame is being encoded.
///
/// Reported so the HUD can say what the pipeline is doing and why, rather than
/// leaving a user to wonder why an apparently static screen is still costing
/// something.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    /// The first frame of the stream. Always a keyframe: a decoder with no
    /// reference has nothing to apply an update to.
    First,
    /// The screen changed.
    Changed,
    /// Nothing changed, but an idle refresh is due.
    IdleRefresh,
    /// The periodic keyframe is due.
    PeriodicKeyframe,
    /// The far end asked for a keyframe, having lost sync.
    Requested,
}

/// What to do with a frame.
#[derive(Debug, Clone, PartialEq)]
pub enum Verdict {
    /// Do not encode it. Its damage, if any, is kept for the next one.
    Skip,
    /// Encode it.
    Encode {
        /// Whether it must be a keyframe.
        keyframe: bool,
        /// Everything that changed since the last encoded frame, not just
        /// since the last captured one.
        damage: Damage,
        /// Why.
        reason: Reason,
    },
}

impl Verdict {
    /// Whether this frame is to be encoded.
    #[must_use]
    pub const fn encodes(&self) -> bool {
        matches!(self, Self::Encode { .. })
    }
}

/// The damage filter and the idle ladder.
#[derive(Debug)]
pub struct EncodeGate {
    policy: Policy,
    frame: Rect,
    /// Damage since the last encoded frame, which is not the same as damage
    /// since the last captured one.
    pending: Vec<Rect>,
    /// Set when a skipped frame reported whole-frame damage, which no list of
    /// rectangles can represent.
    pending_everything: bool,
    /// Whether the far end has asked for a keyframe.
    keyframe_requested: bool,
    last_change: Option<Instant>,
    last_encode: Option<Instant>,
    last_keyframe: Option<Instant>,
    encodes: u64,
    skips: u64,
}

impl EncodeGate {
    /// A gate for a frame of this size.
    #[must_use]
    pub fn new(frame: Rect, policy: Policy) -> Self {
        Self {
            policy,
            frame,
            pending: Vec::new(),
            pending_everything: false,
            keyframe_requested: false,
            last_change: None,
            last_encode: None,
            last_keyframe: None,
            encodes: 0,
            skips: 0,
        }
    }

    /// The policy in force.
    #[must_use]
    pub const fn policy(&self) -> &Policy {
        &self.policy
    }

    /// How many frames have been encoded, and how many skipped.
    ///
    /// The pair the whole design is judged on: a static screen should be
    /// almost all skips.
    #[must_use]
    pub const fn counts(&self) -> (u64, u64) {
        (self.encodes, self.skips)
    }

    /// Change the frame rate ceiling.
    ///
    /// This is where the CPU budget's framerate rung has to land, and without
    /// it the rung reached nothing: the governor would report "15 fps" while
    /// the gate went on admitting frames at 60. The encoder was told, which
    /// changed its rate model and not its workload — so the pipeline looked
    /// degraded and cost the same.
    ///
    /// Changed in place rather than by rebuilding the gate, because a rebuilt
    /// gate loses the damage it has accumulated from skipped frames, and
    /// losing that leaves stale pixels on the viewer's screen.
    pub fn set_max_fps(&mut self, fps: u32) {
        self.policy.max_fps = fps.max(1);
    }

    /// The far end lost sync and needs a keyframe.
    ///
    /// Honoured on the next frame rather than immediately, because there is no
    /// frame to encode until one is captured — and inventing one would send a
    /// picture the host is not currently showing.
    pub fn request_keyframe(&mut self) {
        self.keyframe_requested = true;
    }

    /// The frame size changed, so everything downstream is stale.
    pub fn reconfigure(&mut self, frame: Rect) {
        self.frame = frame;
        self.pending.clear();
        // Whole-frame damage, and a keyframe: every reference the decoder
        // holds is the wrong size.
        self.pending_everything = true;
        self.keyframe_requested = true;
    }

    /// Which rung the screen is on, as of `now`.
    #[must_use]
    pub fn rung(&self, now: Instant) -> Rung {
        let Some(since) = self.last_change.map(|t| now.saturating_duration_since(t)) else {
            // Nothing has ever changed, which for a screen that has just
            // started being watched is not the same as idle: the first frame
            // still has to go out.
            return Rung::Active;
        };
        if since >= self.policy.deep_idle_after {
            Rung::DeepIdle
        } else if since >= self.policy.idle_after {
            Rung::Idle
        } else {
            Rung::Active
        }
    }

    /// Decide what to do with a captured frame.
    pub fn decide(&mut self, damage: &Damage, now: Instant) -> Verdict {
        // A screen that has never changed still has to age down the ladder,
        // or a session opened on a static screen stays on the active rung for
        // ever. The first frame seen is when the clock starts.
        if self.last_change.is_none() {
            self.last_change = Some(now);
        }

        // Accumulate first, so nothing is lost whatever is decided.
        match damage {
            Damage::Nothing => {}
            Damage::Everything => self.pending_everything = true,
            Damage::Regions(r) => self.pending.extend_from_slice(r.as_slice()),
        }
        if !damage.is_empty() {
            self.last_change = Some(now);
        }

        // The first frame always goes, as a keyframe. A decoder holding no
        // reference cannot use anything else.
        if self.last_encode.is_none() {
            return self.emit(true, Reason::First, now);
        }

        if self.keyframe_requested {
            self.keyframe_requested = false;
            return self.emit(true, Reason::Requested, now);
        }

        let keyframe_due = self
            .last_keyframe
            .is_none_or(|t| now.saturating_duration_since(t) >= self.policy.keyframe_interval);

        match self.rung(now) {
            Rung::Active => {
                if !self.has_pending() {
                    // Nothing to send — but the periodic keyframe applies on
                    // every rung, not only the idle ones.
                    //
                    // Reachable and load-bearing: a session that opens on a
                    // screen which then never changes sits on this rung until
                    // the idle threshold, and an earlier version simply
                    // skipped here. A decoder that lost sync in that first
                    // second would have waited for a keyframe that was never
                    // scheduled.
                    if keyframe_due {
                        return self.emit(true, Reason::PeriodicKeyframe, now);
                    }
                    return self.skip();
                }
                if self.too_soon(now, self.policy.active_interval()) {
                    return self.skip();
                }
                let reason = if keyframe_due {
                    Reason::PeriodicKeyframe
                } else {
                    Reason::Changed
                };
                self.emit(keyframe_due, reason, now)
            }
            Rung::Idle => {
                if self.has_pending() {
                    // A change arrived; the rung is stale by one frame.
                    return self.emit(keyframe_due, Reason::Changed, now);
                }
                if keyframe_due {
                    return self.emit(true, Reason::PeriodicKeyframe, now);
                }
                if self.too_soon(now, self.policy.idle_interval()) {
                    return self.skip();
                }
                self.emit(false, Reason::IdleRefresh, now)
            }
            Rung::DeepIdle => {
                if self.has_pending() {
                    return self.emit(keyframe_due, Reason::Changed, now);
                }
                if keyframe_due {
                    return self.emit(true, Reason::PeriodicKeyframe, now);
                }
                self.skip()
            }
        }
    }

    /// Whether anything is waiting to be sent.
    fn has_pending(&self) -> bool {
        self.pending_everything || !self.pending.is_empty()
    }

    /// Whether the last encode was too recent for this interval.
    fn too_soon(&self, now: Instant, interval: Duration) -> bool {
        self.last_encode
            .is_some_and(|t| now.saturating_duration_since(t) < interval)
    }

    fn skip(&mut self) -> Verdict {
        self.skips = self.skips.saturating_add(1);
        Verdict::Skip
    }

    /// Emit an encode verdict, taking the accumulated damage with it.
    fn emit(&mut self, keyframe: bool, reason: Reason, now: Instant) -> Verdict {
        let damage = if keyframe {
            // A keyframe re-sends the whole picture by definition, so
            // reporting anything narrower would be a lie to whoever reads the
            // statistics.
            self.pending.clear();
            self.pending_everything = false;
            Damage::Everything
        } else if self.pending_everything {
            self.pending.clear();
            self.pending_everything = false;
            Damage::Everything
        } else {
            let taken = std::mem::take(&mut self.pending);
            Regions::coalesce(&taken, &self.frame)
        };

        self.last_encode = Some(now);
        if keyframe {
            self.last_keyframe = Some(now);
        }
        self.encodes = self.encodes.saturating_add(1);
        Verdict::Encode {
            keyframe,
            damage,
            reason,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FRAME: Rect = Rect::sized(1920, 1080);

    fn gate() -> EncodeGate {
        EncodeGate::new(FRAME, Policy::default())
    }

    fn small_change() -> Damage {
        Regions::one(Rect::new(100, 100, 40, 20), &FRAME)
    }

    /// A fixed starting point, so every test's arithmetic is exact.
    fn t0() -> Instant {
        Instant::now()
    }

    fn at(base: Instant, ms: u64) -> Instant {
        base.checked_add(Duration::from_millis(ms)).unwrap_or(base)
    }

    /// A decoder with no reference cannot use anything but a keyframe.
    #[test]
    fn the_first_frame_is_always_an_encoded_keyframe() {
        let mut g = gate();
        let v = g.decide(&Damage::Nothing, t0());
        assert_eq!(
            v,
            Verdict::Encode {
                keyframe: true,
                damage: Damage::Everything,
                reason: Reason::First
            },
            "even with no damage reported, the stream has to start"
        );
    }

    /// **The number this whole design exists for.** A screen that is not
    /// changing must cost almost nothing.
    #[test]
    #[cfg_attr(
        miri,
        ignore = "an exhaustive sweep: MIRI interprets rather than executes, so this takes minutes. This crate forbids unsafe code and has no FFI, so what MIRI is here for is the buffer pool's Drop, which the cheaper tests exercise"
    )]
    fn a_static_screen_encodes_almost_nothing() {
        let mut g = gate();
        let start = t0();

        // Sixty seconds at 60 fps with nothing changing at all.
        for i in 0..3600u64 {
            let _ = g.decide(&Damage::Nothing, at(start, i * 16));
        }

        let (encodes, skips) = g.counts();
        // One first frame, then one a second for the first ten seconds, then a
        // keyframe every five. Nothing like 3600.
        assert!(
            encodes <= 25,
            "a minute of a static screen encoded {encodes} frames of 3600 — \
             the whole product depends on this being a small number"
        );
        assert!(encodes >= 2, "but resilience frames must still go out");
        assert_eq!(encodes + skips, 3600);
    }

    /// The ceiling holds however fast frames arrive.
    ///
    /// Frames are offered every millisecond rather than every four, because
    /// the gate can only act when a frame arrives: at four-millisecond
    /// arrivals a 16.6 ms interval quantises to 20 ms and the honest answer is
    /// fifty encodes, not sixty. Testing at one millisecond measures the
    /// ceiling rather than the arrival grid.
    /// **The rung that reached nothing.** The governor's framerate lever has to
    /// change what the gate admits, not just what the encoder is told.
    #[test]
    fn the_frame_rate_ceiling_can_be_lowered_and_takes_effect() {
        let mut g = EncodeGate::new(FRAME, Policy::default());
        let start = t0();
        let _ = g.decide(&small_change(), start);

        // At 60 fps, frames a millisecond apart are mostly skipped but some
        // get through.
        let mut fast = 0;
        for i in 1..=200u64 {
            if g.decide(&small_change(), at(start, i)).encodes() {
                fast += 1;
            }
        }

        // Drop to 15 and the same arrival pattern encodes far fewer.
        g.set_max_fps(15);
        let base = at(start, 1000);
        let mut slow = 0;
        for i in 1..=200u64 {
            if g.decide(&small_change(), at(base, i)).encodes() {
                slow += 1;
            }
        }

        assert!(
            slow * 2 < fast,
            "lowering the ceiling from 60 to 15 changed encodes from {fast} to \
             {slow}, which is not a ceiling taking effect"
        );
        assert_eq!(g.policy().max_fps, 15);
    }

    /// A ceiling of zero would mean a division by zero in the interval, so it
    /// is floored rather than accepted.
    #[test]
    fn a_frame_rate_ceiling_of_zero_is_floored() {
        let mut g = EncodeGate::new(FRAME, Policy::default());
        g.set_max_fps(0);
        assert_eq!(g.policy().max_fps, 1);
        let start = t0();
        for i in 0..50u64 {
            let _ = g.decide(&small_change(), at(start, i * 10));
        }
    }

    /// **The bug the static-screen test found.** The periodic keyframe applies
    /// on every rung, including the active one.
    ///
    /// A session opened on a screen that then never changes sits on the active
    /// rung until the idle threshold. An earlier version skipped there without
    /// checking whether a keyframe was due, so a decoder that lost sync in
    /// that window waited for a keyframe that was never scheduled — and a
    /// viewer joining late saw nothing at all.
    #[test]
    #[cfg_attr(
        miri,
        ignore = "an exhaustive sweep: MIRI interprets rather than executes, so this takes minutes. This crate forbids unsafe code and has no FFI, so what MIRI is here for is the buffer pool's Drop, which the cheaper tests exercise"
    )]
    fn the_periodic_keyframe_applies_on_the_active_rung_too() {
        let mut g = EncodeGate::new(
            FRAME,
            Policy {
                // A long idle threshold, so the whole test stays on the active
                // rung and the idle branches cannot supply the keyframe.
                idle_after: Duration::from_secs(600),
                deep_idle_after: Duration::from_secs(1200),
                keyframe_interval: Duration::from_secs(5),
                ..Policy::default()
            },
        );
        let start = t0();
        let _ = g.decide(&Damage::Nothing, start);
        assert_eq!(g.rung(at(start, 20_000)), Rung::Active);

        let mut keyframes = 0;
        for i in 1..=20_000u64 {
            if let Verdict::Encode {
                reason: Reason::PeriodicKeyframe,
                ..
            } = g.decide(&Damage::Nothing, at(start, i))
            {
                keyframes += 1;
            }
        }
        assert!(
            (3..=4).contains(&keyframes),
            "twenty seconds on the active rung should schedule about four \
             keyframes, got {keyframes} — a decoder that lost sync has no \
             other way back"
        );
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "an exhaustive sweep: MIRI interprets rather than executes, so this takes minutes. This crate forbids unsafe code and has no FFI, so what MIRI is here for is the buffer pool's Drop, which the cheaper tests exercise"
    )]
    fn a_changing_screen_encodes_up_to_the_ceiling_and_no_faster() {
        let mut g = gate();
        let start = t0();
        let _ = g.decide(&small_change(), start);

        let mut encoded = 0;
        for i in 1..=1000u64 {
            if g.decide(&small_change(), at(start, i)).encodes() {
                encoded += 1;
            }
        }
        assert!(
            encoded <= 61,
            "the 60 fps ceiling was exceeded: {encoded} encodes in a second"
        );
        assert!(
            encoded >= 52,
            "expected close to the 60 fps ceiling, got {encoded}"
        );
    }

    /// **The bug that would be nearly impossible to reproduce.** A frame
    /// skipped for the rate ceiling must not take its damage with it, or those
    /// pixels are never sent and the viewer keeps a stale patch.
    #[test]
    fn damage_from_a_skipped_frame_is_still_sent_with_the_next_one() {
        let mut g = gate();
        let start = t0();
        let _ = g.decide(&Damage::Everything, start);

        // Two changes in quick succession, the first of which will be skipped
        // by the rate ceiling.
        let far_left = Regions::one(Rect::new(0, 0, 50, 50), &FRAME);
        let far_right = Regions::one(Rect::new(1800, 1000, 50, 50), &FRAME);

        let first = g.decide(&far_left, at(start, 1));
        assert_eq!(first, Verdict::Skip, "too soon after the first frame");

        let second = g.decide(&far_right, at(start, 500));
        match second {
            Verdict::Encode { damage, .. } => {
                let rects = damage.rects(&FRAME);
                let left = Rect::new(0, 0, 50, 50);
                let right = Rect::new(1800, 1000, 50, 50);
                assert!(
                    rects.iter().any(|r| r.contains(&left)),
                    "the skipped frame's damage was lost: {rects:?}"
                );
                assert!(
                    rects.iter().any(|r| r.contains(&right)),
                    "and the current frame's damage must be there too: {rects:?}"
                );
            }
            Verdict::Skip => panic!("half a second later is not too soon"),
        }
    }

    /// Whole-frame damage cannot be represented as a list of rectangles, so it
    /// must survive accumulation as itself.
    #[test]
    fn whole_frame_damage_from_a_skipped_frame_survives() {
        let mut g = gate();
        let start = t0();
        let _ = g.decide(&small_change(), start);

        assert_eq!(g.decide(&Damage::Everything, at(start, 1)), Verdict::Skip);
        match g.decide(&Damage::Nothing, at(start, 500)) {
            Verdict::Encode { damage, .. } => assert_eq!(damage, Damage::Everything),
            other => panic!("the whole-frame update was lost: {other:?}"),
        }
    }

    #[test]
    fn a_still_screen_drops_to_the_idle_rate() {
        let mut g = gate();
        let start = t0();
        let _ = g.decide(&small_change(), start);

        // Past the idle threshold with nothing happening.
        assert_eq!(g.rung(at(start, 999)), Rung::Active);
        assert_eq!(g.rung(at(start, 1001)), Rung::Idle);
        assert_eq!(g.rung(at(start, 10_001)), Rung::DeepIdle);
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "an exhaustive sweep: MIRI interprets rather than executes, so this takes minutes. This crate forbids unsafe code and has no FFI, so what MIRI is here for is the buffer pool's Drop, which the cheaper tests exercise"
    )]
    fn while_idle_a_resilience_frame_goes_out_once_a_second() {
        let mut g = gate();
        let start = t0();
        let _ = g.decide(&small_change(), start);

        let mut refreshes = 0;
        let mut keyframes = 0;
        // Nine seconds of stillness: idle, not yet deep idle.
        for i in 1..=9000u64 {
            match g.decide(&Damage::Nothing, at(start, i)) {
                Verdict::Encode {
                    reason: Reason::IdleRefresh,
                    keyframe,
                    ..
                } => {
                    refreshes += 1;
                    assert!(!keyframe, "an idle refresh need not be a keyframe");
                }
                Verdict::Encode {
                    reason: Reason::PeriodicKeyframe,
                    ..
                } => keyframes += 1,
                _ => {}
            }
        }
        assert!(
            (6..=9).contains(&refreshes),
            "expected about one refresh a second, got {refreshes}"
        );
        assert!(keyframes >= 1, "and the periodic keyframe still applies");
    }

    /// Deep idle is the state a screen spends most of its life in, and it must
    /// cost a keyframe every five seconds and nothing else.
    #[test]
    #[cfg_attr(
        miri,
        ignore = "an exhaustive sweep: MIRI interprets rather than executes, so this takes minutes. This crate forbids unsafe code and has no FFI, so what MIRI is here for is the buffer pool's Drop, which the cheaper tests exercise"
    )]
    fn deep_idle_sends_only_periodic_keyframes() {
        let mut g = gate();
        let start = t0();
        let _ = g.decide(&small_change(), start);

        // Get past deep idle.
        for i in 1..=11_000u64 {
            let _ = g.decide(&Damage::Nothing, at(start, i * 2));
        }
        let (before, _) = g.counts();

        // Then twenty more seconds of nothing.
        let base = at(start, 22_000);
        for i in 1..=20_000u64 {
            let v = g.decide(&Damage::Nothing, at(base, i));
            if let Verdict::Encode {
                reason, keyframe, ..
            } = v
            {
                assert_eq!(
                    reason,
                    Reason::PeriodicKeyframe,
                    "nothing but keyframes should go out in deep idle"
                );
                assert!(keyframe);
            }
        }
        let (after, _) = g.counts();
        let sent = after - before;
        assert!(
            (3..=5).contains(&sent),
            "twenty seconds of deep idle should be about four keyframes, got {sent}"
        );
    }

    /// A change while idle must be sent at once, not held until the next
    /// scheduled refresh. Otherwise the first keystroke after a pause is
    /// visibly late, which is the moment a user judges the whole session.
    #[test]
    #[cfg_attr(
        miri,
        ignore = "an exhaustive sweep: MIRI interprets rather than executes, so this takes minutes. This crate forbids unsafe code and has no FFI, so what MIRI is here for is the buffer pool's Drop, which the cheaper tests exercise"
    )]
    fn a_change_while_idle_is_sent_immediately() {
        let mut g = gate();
        let start = t0();
        let _ = g.decide(&small_change(), start);
        for i in 1..=2000u64 {
            let _ = g.decide(&Damage::Nothing, at(start, i));
        }
        assert_eq!(g.rung(at(start, 2001)), Rung::Idle);

        // The rate ceiling may hold it for one frame interval — that is the
        // ceiling working, not a latency bug. What must not happen is waiting
        // for the next *idle* refresh, a whole second away.
        let mut sent_at = None;
        for ms in 2001..=2020u64 {
            if g.decide(&small_change(), at(start, ms)).encodes() {
                sent_at = Some(ms);
                break;
            }
        }
        assert!(
            sent_at.is_some(),
            "a change after a pause waited more than one frame interval, which \
             is the moment a user judges the whole session"
        );
    }

    /// And the same from deep idle, which is the harder case because nothing
    /// has been sent for a while.
    #[test]
    #[cfg_attr(
        miri,
        ignore = "an exhaustive sweep: MIRI interprets rather than executes, so this takes minutes. This crate forbids unsafe code and has no FFI, so what MIRI is here for is the buffer pool's Drop, which the cheaper tests exercise"
    )]
    fn a_change_while_deeply_idle_is_sent_immediately() {
        let mut g = gate();
        let start = t0();
        let _ = g.decide(&small_change(), start);
        for i in 1..=15_000u64 {
            let _ = g.decide(&Damage::Nothing, at(start, i));
        }
        assert_eq!(g.rung(at(start, 15_001)), Rung::DeepIdle);

        let mut sent_at = None;
        for ms in 15_001..=15_020u64 {
            if g.decide(&small_change(), at(start, ms)).encodes() {
                sent_at = Some(ms);
                break;
            }
        }
        assert!(
            sent_at.is_some(),
            "a change after a long pause must not wait for the next scheduled \
             keyframe, which could be five seconds away"
        );
    }

    /// A decoder that has lost sync is stuck until it gets a keyframe, so the
    /// request must be honoured on the very next frame.
    #[test]
    fn a_keyframe_request_is_honoured_on_the_next_frame() {
        let mut g = gate();
        let start = t0();
        let _ = g.decide(&Damage::Everything, start);

        g.request_keyframe();
        // Even inside the rate ceiling, which would otherwise skip.
        match g.decide(&Damage::Nothing, at(start, 1)) {
            Verdict::Encode {
                keyframe,
                reason,
                damage,
            } => {
                assert!(keyframe);
                assert_eq!(reason, Reason::Requested);
                assert_eq!(damage, Damage::Everything);
            }
            other => panic!("a stuck decoder cannot wait: {other:?}"),
        }

        // And it is not honoured twice.
        assert_eq!(g.decide(&Damage::Nothing, at(start, 2)), Verdict::Skip);
    }

    /// A keyframe re-sends the whole picture, so reporting narrower damage
    /// would misstate what was spent.
    #[test]
    fn a_keyframe_always_reports_whole_frame_damage() {
        let mut g = gate();
        let start = t0();
        let _ = g.decide(&small_change(), start);
        g.request_keyframe();
        match g.decide(&small_change(), at(start, 1)) {
            Verdict::Encode { damage, .. } => assert_eq!(damage, Damage::Everything),
            other => panic!("{other:?}"),
        }
    }

    /// A resolution change invalidates every reference the decoder holds.
    #[test]
    fn a_reconfigure_forces_a_whole_frame_keyframe() {
        let mut g = gate();
        let start = t0();
        let _ = g.decide(&small_change(), start);

        g.reconfigure(Rect::sized(1280, 720));
        match g.decide(&Damage::Nothing, at(start, 1)) {
            Verdict::Encode {
                keyframe,
                damage,
                reason,
            } => {
                assert!(keyframe, "every reference is the wrong size now");
                assert_eq!(damage, Damage::Everything);
                assert_eq!(reason, Reason::Requested);
            }
            other => panic!("{other:?}"),
        }
    }

    /// Stale damage from before a resolution change must not be applied to the
    /// new geometry — those coordinates mean something different now.
    #[test]
    fn a_reconfigure_discards_damage_from_the_old_geometry() {
        let mut g = gate();
        let start = t0();
        let _ = g.decide(&Damage::Everything, start);
        // Damage that would be out of bounds after the change.
        let _ = g.decide(
            &Regions::one(Rect::new(1900, 1000, 20, 20), &FRAME),
            at(start, 1),
        );

        let smaller = Rect::sized(640, 480);
        g.reconfigure(smaller);
        match g.decide(&Damage::Nothing, at(start, 2)) {
            Verdict::Encode { damage, .. } => {
                assert_eq!(damage, Damage::Everything);
                for r in damage.rects(&smaller) {
                    assert!(
                        smaller.contains(&r),
                        "{r:?} is outside the new {smaller:?} frame"
                    );
                }
            }
            other => panic!("{other:?}"),
        }
    }

    /// A clock that goes backwards must not produce a negative interval and a
    /// storm of encodes. `Instant` is monotonic, but a caller passing stored
    /// timestamps out of order should not be able to break the ladder.
    #[test]
    #[cfg_attr(
        miri,
        ignore = "an exhaustive sweep: MIRI interprets rather than executes, so this takes minutes. This crate forbids unsafe code and has no FFI, so what MIRI is here for is the buffer pool's Drop, which the cheaper tests exercise"
    )]
    fn a_timestamp_going_backwards_does_not_break_the_ladder() {
        let mut g = gate();
        let start = at(t0(), 100_000);
        let _ = g.decide(&small_change(), start);

        for i in 0..1000u64 {
            let earlier = start.checked_sub(Duration::from_millis(i)).unwrap_or(start);
            let _ = g.decide(&small_change(), earlier);
        }
        let (encodes, _) = g.counts();
        assert!(
            encodes < 20,
            "time going backwards produced {encodes} encodes"
        );
    }

    /// A policy of zero must not divide by zero.
    #[test]
    fn a_zero_frame_rate_policy_does_not_panic() {
        let mut g = EncodeGate::new(
            FRAME,
            Policy {
                max_fps: 0,
                idle_fps: 0,
                ..Policy::default()
            },
        );
        let start = t0();
        for i in 0..100u64 {
            let _ = g.decide(&small_change(), at(start, i * 10));
        }
        let (encodes, skips) = g.counts();
        assert_eq!(encodes + skips, 100);
    }

    #[test]
    fn the_rung_has_a_name_for_the_hud() {
        assert_eq!(Rung::Active.name(), "active");
        assert_eq!(Rung::Idle.name(), "idle");
        assert_eq!(Rung::DeepIdle.name(), "deep-idle");
    }

    /// Whatever arrives, in whatever order, the gate must not panic and the
    /// counts must add up.
    #[test]
    #[cfg_attr(
        miri,
        ignore = "an exhaustive sweep: MIRI interprets rather than executes, so this takes minutes. This crate forbids unsafe code and has no FFI, so what MIRI is here for is the buffer pool's Drop, which the cheaper tests exercise"
    )]
    fn arbitrary_sequences_never_panic() {
        let mut x = 0x6C62_272E_07BB_0142u64;
        let mut g = gate();
        let start = t0();
        for i in 0..20_000u64 {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            let damage = match x % 4 {
                0 => Damage::Nothing,
                1 => Damage::Everything,
                _ => Regions::one(
                    Rect::new(
                        (x % 2000) as i32 - 40,
                        ((x >> 16) % 1200) as i32 - 40,
                        ((x >> 32) % 400) as u32,
                        ((x >> 40) % 400) as u32,
                    ),
                    &FRAME,
                ),
            };
            if x.is_multiple_of(97) {
                g.request_keyframe();
            }
            if x.is_multiple_of(601) {
                g.reconfigure(Rect::sized(((x >> 8) % 3000) as u32 + 1, 720));
            }
            let _ = g.decide(&damage, at(start, i * (x % 40)));
        }
        let (encodes, skips) = g.counts();
        assert!(encodes + skips >= 20_000);
    }
}
