//! When to stop waiting for a frame.
//!
//! # A remote desktop is not a video player
//!
//! A player buffers seconds, because it has seconds of material and nothing
//! else to show. A remote desktop has neither: the buffer *is* the latency,
//! and the latency is the product. Somebody moving a window feels every
//! millisecond of it.
//!
//! So this is not a buffer in the player's sense. It holds nothing. It answers
//! one question — **how long to wait before deciding a frame is as complete as
//! it is going to get** — and it exists because that question was being
//! answered by a hardcoded constant in a command-line tool, which is where
//! numbers go to be wrong quietly.
//!
//! # The deadline does two jobs
//!
//! **It releases a frame nothing has bounded.** Reassembly waits for the next
//! frame to bound one whose start is unknown, which is correct and which on a
//! *static screen* waits forever: the host sends one keyframe and then
//! correctly sends nothing. The deadline is what makes a still picture appear.
//!
//! **It gives a late packet time to arrive.** A datagram five milliseconds
//! behind its neighbours should not cost a frame, and abandoning one the
//! instant a hole appears would abandon frames constantly on a healthy link.
//!
//! Both want the same thing — a short, bounded wait — which is why one number
//! serves them.
//!
//! # Where the number comes from
//!
//! Two frame intervals at the stream's own framerate, clamped. Derived rather
//! than fixed, because a fixed value is wrong at one end or the other: 80 ms is
//! a quarter of a second of stutter at 60 fps, and 16 ms is not long enough for
//! a frame split across forty datagrams at 10 fps.
//!
//! Clamped because the framerate is the *ceiling*, not the rate: a screen that
//! changes twice a minute would otherwise derive a deadline from a number that
//! has nothing to do with how often frames actually arrive.
//!
//! This is the **fixed** buffer the plan calls for. The adaptive one belongs
//! with the rest of the quality work, and what it will need is measurements —
//! so the jitter is measured here even though nothing acts on it yet.

use std::time::{Duration, Instant};

/// The shortest deadline worth having.
///
/// Below this a deadline is indistinguishable from not having one: two
/// datagrams of the same frame can easily be twenty milliseconds apart on a
/// link that is working perfectly.
pub const MIN_WAIT: Duration = Duration::from_millis(20);

/// The longest deadline worth having.
///
/// Eighty milliseconds is already at the edge of what feels direct when it is
/// added to everything else in the path. Past it, a frame held for a late
/// packet costs more than the frame was worth.
pub const MAX_WAIT: Duration = Duration::from_millis(80);

/// How many frame intervals to wait.
///
/// Two. One is not enough to distinguish "the next frame is late" from "there
/// is no next frame", which is the distinction the deadline exists to make.
const INTERVALS: u32 = 2;

/// What the arrival pattern looks like, for the adaptive version to use later.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct JitterStats {
    /// Datagrams timed.
    pub samples: u64,
    /// Interarrival jitter, as RFC 3550 defines it, in the stream's clock
    /// units.
    ///
    /// The standard estimator, so the number means the same thing it means in
    /// every other RTP implementation: a smoothed mean deviation of the
    /// difference between a packet's transit time and its predecessor's.
    pub jitter_ticks: f64,
    /// The longest gap seen between two datagrams of the same frame.
    ///
    /// The number that says whether the deadline is long enough: if this
    /// exceeds the deadline, frames are being abandoned for packets that were
    /// merely late.
    pub worst_gap: Duration,
    /// Deadlines that fired, which is how often something had to be decided
    /// without the next frame's help.
    pub deadlines: u64,
}

/// The deadline policy for one stream.
#[derive(Debug)]
pub struct Jitter {
    wait: Duration,
    clock_hz: u32,
    /// When the frame being assembled first showed up.
    opened_at: Option<Instant>,
    /// The previous datagram's arrival and RTP timestamp, for the estimator.
    previous: Option<(Instant, u32)>,
    stats: JitterStats,
}

impl Jitter {
    /// A policy for a stream at this framerate and clock.
    #[must_use]
    pub fn new(max_fps: u16, clock_hz: u32) -> Self {
        Self {
            wait: derive_wait(max_fps),
            clock_hz: clock_hz.max(1),
            opened_at: None,
            previous: None,
            stats: JitterStats::default(),
        }
    }

    /// The deadline in force.
    #[must_use]
    pub const fn wait(&self) -> Duration {
        self.wait
    }

    /// What the arrivals have looked like.
    #[must_use]
    pub const fn stats(&self) -> JitterStats {
        self.stats
    }

    /// Interarrival jitter in milliseconds, which is the readable form.
    #[must_use]
    pub fn jitter_ms(&self) -> f64 {
        self.stats.jitter_ticks * 1000.0 / f64::from(self.clock_hz)
    }

    /// Record a datagram's arrival.
    ///
    /// `assembling` says whether a frame is now part-assembled, which is what
    /// decides whether there is anything for a deadline to apply to.
    pub fn arrived(&mut self, now: Instant, timestamp: u32, assembling: bool) {
        // Whether a frame was *already* in progress before this datagram.
        //
        // The distinction matters and getting it wrong is easy: `assembling`
        // describes the state *after* this packet, so the first packet of a
        // new frame arrives with it set. Counting that gap measured the idle
        // time between frames as a late packet, and a static screen reported a
        // four-second "worst gap" — a number that says the deadline is far too
        // short when in fact nothing was ever late.
        let continuing = self.opened_at.is_some() && assembling;
        if let Some((was, then)) = self.previous {
            // RFC 3550's estimator. The transit difference is the change in
            // (arrival - timestamp) between consecutive packets, so a constant
            // network delay cancels and only the *variation* is measured.
            let arrival_ticks = self.ticks(now.saturating_duration_since(was));
            let sent_ticks = f64::from(timestamp.wrapping_sub(then) as i32);
            let d = arrival_ticks - sent_ticks;
            self.stats.jitter_ticks += (d.abs() - self.stats.jitter_ticks) / 16.0;

            let gap = now.saturating_duration_since(was);
            if continuing && gap > self.stats.worst_gap {
                self.stats.worst_gap = gap;
            }
        }
        self.previous = Some((now, timestamp));
        self.stats.samples = self.stats.samples.saturating_add(1);

        match (assembling, self.opened_at) {
            // A frame started, so the clock starts with it.
            (true, None) => self.opened_at = Some(now),
            // Nothing in hand, so nothing to time.
            (false, _) => self.opened_at = None,
            (true, Some(_)) => {}
        }
    }

    /// How long is left before the deadline, or `None` when nothing is waiting.
    ///
    /// `Some(ZERO)` means it is due now.
    #[must_use]
    pub fn due_in(&self, now: Instant) -> Option<Duration> {
        let opened = self.opened_at?;
        Some(
            self.wait
                .saturating_sub(now.saturating_duration_since(opened)),
        )
    }

    /// Whether the deadline has passed.
    #[must_use]
    pub fn is_due(&self, now: Instant) -> bool {
        self.due_in(now).is_some_and(|left| left.is_zero())
    }

    /// Note that a deadline was acted on, and stop timing.
    pub fn fired(&mut self) {
        self.opened_at = None;
        self.stats.deadlines = self.stats.deadlines.saturating_add(1);
    }

    /// A frame completed on its own, so there is nothing to time.
    pub fn settled(&mut self) {
        self.opened_at = None;
    }

    /// The stream's framerate changed, so the deadline should too.
    pub fn retarget(&mut self, max_fps: u16) {
        self.wait = derive_wait(max_fps);
    }

    fn ticks(&self, d: Duration) -> f64 {
        d.as_secs_f64() * f64::from(self.clock_hz)
    }
}

/// Two frame intervals, clamped.
///
/// Pure, so the reasoning can be checked without a stream.
#[must_use]
pub fn derive_wait(max_fps: u16) -> Duration {
    let fps = u32::from(max_fps).max(1);
    let interval = Duration::from_secs(1) / fps;
    (interval * INTERVALS).clamp(MIN_WAIT, MAX_WAIT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_deadline_follows_the_framerate_and_stays_inside_its_bounds() {
        // Two intervals where that lands inside the clamp. Compared in whole
        // milliseconds, because 1s/60*2 is 33.333ms and asserting the exact
        // rational would be asserting the arithmetic against itself.
        assert_eq!(derive_wait(30).as_millis(), 66);
        assert_eq!(derive_wait(60).as_millis(), 33);
        assert_eq!(derive_wait(15).as_millis(), 80, "clamped at the top");
        // And the clamps bite at both ends. A screen that updates twice a
        // minute must not derive a two-second deadline from its ceiling.
        assert_eq!(derive_wait(1), MAX_WAIT);
        assert_eq!(derive_wait(2), MAX_WAIT);
        assert_eq!(derive_wait(240), MIN_WAIT);
        // Zero is not a framerate, and must not divide by itself.
        assert_eq!(derive_wait(0), MAX_WAIT);
    }

    #[test]
    fn every_framerate_gives_a_usable_deadline() {
        for fps in 0..=240u16 {
            let w = derive_wait(fps);
            assert!(w >= MIN_WAIT && w <= MAX_WAIT, "{fps} fps gave {w:?}");
        }
    }

    #[test]
    fn nothing_is_due_when_no_frame_is_waiting() {
        let j = Jitter::new(30, 90_000);
        assert_eq!(j.due_in(Instant::now()), None);
        assert!(!j.is_due(Instant::now()));
    }

    /// The clock starts when the frame does, not when the last packet arrived.
    /// Timing from the last packet would keep extending the deadline for as
    /// long as datagrams kept trickling in, which is the one case it needs to
    /// terminate.
    #[test]
    fn the_deadline_runs_from_the_frames_first_packet() {
        let mut j = Jitter::new(30, 90_000);
        let start = Instant::now();
        j.arrived(start, 0, true);
        // More packets for the same frame do not push the deadline back.
        j.arrived(start + Duration::from_millis(30), 0, true);
        j.arrived(start + Duration::from_millis(50), 0, true);
        assert!(!j.is_due(start + Duration::from_millis(60)));
        assert!(
            j.is_due(start + Duration::from_millis(70)),
            "the deadline was pushed back by later packets of the same frame"
        );
    }

    #[test]
    fn a_frame_that_settles_stops_the_clock() {
        let mut j = Jitter::new(30, 90_000);
        let start = Instant::now();
        j.arrived(start, 0, true);
        assert!(j.due_in(start).is_some());
        j.settled();
        assert_eq!(j.due_in(start + Duration::from_secs(1)), None);
    }

    #[test]
    fn a_datagram_with_nothing_assembling_stops_the_clock() {
        let mut j = Jitter::new(30, 90_000);
        let start = Instant::now();
        j.arrived(start, 0, true);
        j.arrived(start + Duration::from_millis(5), 3000, false);
        assert_eq!(j.due_in(start + Duration::from_secs(1)), None);
    }

    #[test]
    fn firing_is_counted_and_stops_the_clock() {
        let mut j = Jitter::new(30, 90_000);
        let start = Instant::now();
        j.arrived(start, 0, true);
        assert!(j.is_due(start + Duration::from_millis(100)));
        j.fired();
        assert_eq!(j.stats().deadlines, 1);
        assert!(!j.is_due(start + Duration::from_secs(1)));
    }

    /// **A constant network delay must not read as jitter.** The estimator
    /// measures the *variation* in transit time, so a link that is uniformly
    /// slow has none — and one that alternates fast and slow has plenty.
    #[test]
    fn a_steady_link_has_no_jitter() {
        let mut j = Jitter::new(30, 90_000);
        let start = Instant::now();
        // Frames 33 ms apart in both clocks: 3000 ticks at 90 kHz.
        for i in 0..40u32 {
            j.arrived(
                start + Duration::from_millis(u64::from(i) * 33),
                i * 2970,
                false,
            );
        }
        assert!(
            j.jitter_ms() < 1.0,
            "a steady link measured {} ms of jitter",
            j.jitter_ms()
        );
    }

    #[test]
    fn an_erratic_link_shows_jitter() {
        let mut j = Jitter::new(30, 90_000);
        let start = Instant::now();
        let mut at = 0u64;
        for i in 0..60u32 {
            // Alternating early and late arrivals for evenly-spaced frames.
            at += if i % 2 == 0 { 10 } else { 56 };
            j.arrived(start + Duration::from_millis(at), i * 2970, false);
        }
        assert!(
            j.jitter_ms() > 5.0,
            "an erratic link measured only {} ms of jitter",
            j.jitter_ms()
        );
    }

    /// The worst gap is the number that says whether the deadline is long
    /// enough, so it must only count gaps *within* a frame — a gap between
    /// frames on an idle screen is not a late packet.
    #[test]
    fn the_worst_gap_only_counts_packets_of_a_frame_being_assembled() {
        let mut j = Jitter::new(30, 90_000);
        let start = Instant::now();
        j.arrived(start, 0, false);
        // A long quiet period with nothing assembling.
        j.arrived(start + Duration::from_secs(5), 0, false);
        assert_eq!(
            j.stats().worst_gap,
            Duration::ZERO,
            "an idle gap was counted as a late packet"
        );
        // **The sequence that actually happened.** A frame completes, the
        // screen is idle for seconds, and then a new frame's *first* packet
        // arrives — which is the moment `assembling` becomes true. The gap
        // before it is idle time, not a late packet.
        //
        // The first version of this test set `assembling: false` on the idle
        // arrival, so it never exercised this and passed while a real session
        // reported a four-second worst gap.
        let mut k = Jitter::new(30, 90_000);
        let t = Instant::now();
        k.arrived(t, 0, true);
        k.arrived(t + Duration::from_millis(8), 0, true);
        k.settled();
        // Four seconds of nothing, then a new frame starts.
        k.arrived(t + Duration::from_secs(4), 3000, true);
        assert_eq!(
            k.stats().worst_gap,
            Duration::from_millis(8),
            "the idle gap between frames was counted as a late packet"
        );

        // And a real gap inside a frame is counted.
        k.arrived(
            t + Duration::from_secs(4) + Duration::from_millis(12),
            3000,
            true,
        );
        assert_eq!(k.stats().worst_gap, Duration::from_millis(12));
    }

    #[test]
    fn retargeting_changes_the_deadline() {
        let mut j = Jitter::new(10, 90_000);
        assert_eq!(j.wait(), MAX_WAIT);
        j.retarget(60);
        assert_eq!(j.wait().as_millis(), 33);
    }

    /// The RTP timestamp wraps, and a wrap must not read as an enormous
    /// transit difference.
    #[test]
    fn a_timestamp_wrap_does_not_look_like_a_jitter_spike() {
        let mut j = Jitter::new(30, 90_000);
        let start = Instant::now();
        j.arrived(start, u32::MAX - 1000, false);
        j.arrived(start + Duration::from_millis(33), 1970, false);
        assert!(
            j.jitter_ms() < 50.0,
            "a timestamp wrap measured {} ms of jitter",
            j.jitter_ms()
        );
    }
}
