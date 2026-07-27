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
//! # And where the rest of it comes from
//!
//! The framerate is only half the answer, and for a while it was the whole of
//! it. It answers "how long until the next frame would have started", which is
//! right on a link fast enough that a frame arrives all at once. It is the
//! wrong question when a single frame takes longer to come down the wire than
//! the gap between frames — a five-kilobyte keyframe on a two-hundred-kilobit
//! uplink occupies it for two hundred milliseconds — and then a sixty-six
//! millisecond deadline cuts that frame into three pieces and discards each as
//! incomplete. Every keyframe, for ever, because keyframes are always the
//! largest frames. Measured: a session showing nothing at all while the link
//! delivered every byte of it.
//!
//! So the deadline also follows what the arrivals show, and the quantity that
//! matters turned out **not to be jitter**. Jitter is variation *between*
//! frames; this module measured it faithfully, reported it in every session
//! summary, and waited for an adaptive version to act on it. What abandons a
//! frame is how long that frame's own datagrams take. See [`Jitter::wait`] for
//! the rule and [`Jitter::fired`] for why the evidence cannot come only from
//! frames that completed.
//!
//! The jitter estimate stays, because it is the honest RFC 3550 number and it
//! is what a person reads to tell a link that varies from one that is slow.

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

/// The most a link whose frames genuinely take that long may be given.
///
/// **A different quantity from [`MAX_WAIT`], which is why it is a different
/// number.** `MAX_WAIT` bounds what a *healthy* link should ever wait for a
/// late packet, and eighty milliseconds is already at the edge of feeling
/// direct. This bounds something else: how long a frame's own datagrams take
/// to come down a slow wire. A five-kilobyte keyframe on a two-hundred
/// kilobit link occupies it for two hundred milliseconds, and a deadline
/// shorter than that abandons the frame while it is still arriving — every
/// time, for ever, because keyframes are always the largest frames.
///
/// Measured: on such a link the receiver abandoned each keyframe in three
/// pieces — four datagrams, then two, then one — and showed **nothing at
/// all** for the whole session while the link delivered every byte.
///
/// Four hundred milliseconds is past interactive, and that is the point: the
/// choice on such a link is a late picture or no picture.
pub const MAX_SLOW_WAIT: Duration = Duration::from_millis(400);

/// How many frames' worth of evidence the deadline is sized from.
///
/// Sixteen: long enough that one unusual frame does not move it, short enough
/// that a link which improves is noticed within a second or two.
const EVIDENCE: usize = 16;

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
    /// Two frame intervals at the announced framerate: the healthy-link
    /// number, and the floor under everything below.
    from_fps: Duration,
    clock_hz: u32,
    /// When the frame being assembled first showed up.
    opened_at: Option<Instant>,
    /// The last datagram that belonged to it, which is how a frame still
    /// arriving is told from one that has stopped.
    last_for_frame: Option<Instant>,
    /// The previous datagram's arrival and RTP timestamp, for the estimator.
    previous: Option<(Instant, u32)>,
    /// How long recent frames took to arrive, first datagram to last. See
    /// [`Jitter::wait`].
    evidence: [Duration; EVIDENCE],
    evidence_at: usize,
    stats: JitterStats,
}

impl Jitter {
    /// A policy for a stream at this framerate and clock.
    #[must_use]
    pub fn new(max_fps: u16, clock_hz: u32) -> Self {
        Self {
            from_fps: derive_wait(max_fps),
            clock_hz: clock_hz.max(1),
            opened_at: None,
            last_for_frame: None,
            previous: None,
            evidence: [Duration::ZERO; EVIDENCE],
            evidence_at: 0,
            stats: JitterStats::default(),
        }
    }

    /// The deadline in force.
    ///
    /// **The larger of what the framerate says and what the link has shown.**
    /// The framerate answers "how long until the next frame would have
    /// started", which is the right question on a link fast enough that a
    /// frame arrives all at once. It is the wrong question when a single frame
    /// takes longer to come down the wire than the gap between frames, and
    /// then the only number that works is the one the arrivals themselves
    /// give.
    ///
    /// This is the adaptive buffer `capture-and-codec.md` §3 asks for, sized
    /// from measurement rather than from a constant — and the measurement it
    /// needed turned out not to be jitter. Jitter is variation *between*
    /// frames; what abandons a keyframe is how long its own datagrams take.
    #[must_use]
    pub fn wait(&self) -> Duration {
        let shown = self
            .evidence
            .iter()
            .copied()
            .max()
            .unwrap_or(Duration::ZERO);
        self.from_fps.max(shown.min(MAX_SLOW_WAIT))
    }

    /// The widest recent evidence, for a session that wants to say why it is
    /// waiting longer than its framerate would suggest.
    #[must_use]
    pub fn widest_arrival(&self) -> Duration {
        self.evidence
            .iter()
            .copied()
            .max()
            .unwrap_or(Duration::ZERO)
    }

    fn remember(&mut self, spread: Duration) {
        if let Some(slot) = self.evidence.get_mut(self.evidence_at) {
            *slot = spread;
        }
        self.evidence_at = self.evidence_at.saturating_add(1) % EVIDENCE;
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
        if assembling {
            self.last_for_frame = Some(now);
        }
    }

    /// How long is left before the deadline, or `None` when nothing is waiting.
    ///
    /// `Some(ZERO)` means it is due now.
    #[must_use]
    pub fn due_in(&self, now: Instant) -> Option<Duration> {
        let opened = self.opened_at?;
        Some(
            self.wait()
                .saturating_sub(now.saturating_duration_since(opened)),
        )
    }

    /// Whether the deadline has passed.
    #[must_use]
    pub fn is_due(&self, now: Instant) -> bool {
        self.due_in(now).is_some_and(|left| left.is_zero())
    }

    /// Note that a deadline was acted on, and stop timing.
    ///
    /// **This is where the evidence has to come from, or it never comes.** A
    /// deadline that abandons every large frame leaves only small ones to
    /// measure, so an estimate built from frames that *completed* can never
    /// grow past the deadline that is cutting them short. So a frame abandoned
    /// while its datagrams were still coming records more than the deadline it
    /// hit, and the estimate climbs until frames start completing.
    ///
    /// "Still coming" means a datagram for it arrived in the second half of
    /// the wait. That distinguishes the two cases that matter: a frame on a
    /// slow wire has a datagram arriving every few tens of milliseconds right
    /// up to the deadline, while a frame that lost a packet on a fast link
    /// stopped arriving near the beginning of it — and lengthening the
    /// deadline for the second one would only add latency to a link that is
    /// merely lossy.
    pub fn fired(&mut self, now: Instant) {
        let wait = self.wait();
        let opened = self.opened_at;
        let still_coming = self
            .last_for_frame
            .is_some_and(|t| now.saturating_duration_since(t) < wait / 2);
        let evidence = if still_coming {
            wait.saturating_mul(3) / 2
        } else {
            match (opened, self.last_for_frame) {
                (Some(opened), Some(last)) => last.saturating_duration_since(opened),
                _ => Duration::ZERO,
            }
        };
        self.remember(evidence);
        self.opened_at = None;
        self.last_for_frame = None;
        self.stats.deadlines = self.stats.deadlines.saturating_add(1);
    }

    /// A frame completed on its own, so there is nothing to time.
    ///
    /// And it is the honest evidence: the time from its first datagram to its
    /// last is exactly what the deadline has to be longer than.
    /// Returns how long the frame took to arrive, first datagram to last,
    /// when there were two to measure between.
    ///
    /// **That spread is how fast the link carried a burst**, and a burst is
    /// the only thing that measures a link's *capacity* rather than a
    /// sender's demand — the sender asked for everything at once, so what
    /// came out the other end was the link going as fast as it can. See
    /// [`crate::ControllerStream::link_capacity`].
    pub fn settled(&mut self, now: Instant) -> Option<Duration> {
        let _ = now;
        let spread = match (self.opened_at, self.last_for_frame) {
            (Some(opened), Some(last)) => {
                let spread = last.saturating_duration_since(opened);
                self.remember(spread);
                Some(spread)
            }
            _ => None,
        };
        self.opened_at = None;
        self.last_for_frame = None;
        spread
    }

    /// The stream's framerate changed, so the deadline should too.
    pub fn retarget(&mut self, max_fps: u16) {
        self.from_fps = derive_wait(max_fps);
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

    /// **A frame that takes longer to arrive than the deadline must lengthen
    /// it, or it can never arrive at all.**
    ///
    /// The failure this prevents was measured end to end: on a 200 kbit/s
    /// link a five-kilobyte keyframe occupies the wire for two hundred
    /// milliseconds, the deadline was sixty-six, and the receiver abandoned
    /// every keyframe in three pieces — four datagrams, then two, then one —
    /// for the whole of a ten-second session, showing nothing while the link
    /// delivered every byte.
    #[test]
    fn a_deadline_grows_for_a_frame_still_arriving() {
        let mut j = Jitter::new(30, 90_000);
        let start = Instant::now();
        let from_fps = j.wait();
        assert_eq!(
            from_fps,
            derive_wait(30),
            "it starts where the framerate says"
        );

        // A frame whose datagrams keep coming right up to the deadline. Each
        // arrival is well inside the wait, so nothing here looks like loss.
        let mut now = start;
        for _ in 0..6 {
            j.arrived(now, 0, true);
            now += from_fps / 3;
        }
        assert!(j.is_due(now), "the deadline should have passed by now");
        j.fired(now);

        assert!(
            j.wait() > from_fps,
            "a frame cut short while still arriving left the deadline at {:?}",
            j.wait()
        );
    }

    /// And it keeps growing until frames start completing — because a
    /// deadline that abandons every large frame leaves only small ones to
    /// measure, so evidence taken from completed frames alone can never
    /// exceed the deadline cutting them short.
    #[test]
    fn it_climbs_until_the_frames_fit_and_stops_at_a_ceiling() {
        let mut j = Jitter::new(30, 90_000);
        let mut now = Instant::now();
        let mut seen = Vec::new();
        for _ in 0..12 {
            let wait = j.wait();
            // Datagrams arriving steadily, always still coming when the
            // deadline lands.
            let mut at = now;
            while at < now + wait {
                j.arrived(at, 0, true);
                at += wait / 4;
            }
            now = at;
            j.fired(now);
            seen.push(j.wait());
        }
        assert!(
            seen.windows(2).any(|w| matches!(w, [a, b] if b > a)),
            "the deadline never grew: {seen:?}"
        );
        assert_eq!(
            j.wait(),
            MAX_SLOW_WAIT,
            "and it should settle at the ceiling rather than climbing for ever"
        );
    }

    /// **But a merely lossy link must not lengthen it.**
    ///
    /// A frame that loses a packet on a fast link stops arriving near the
    /// start of the wait and then nothing comes. Growing the deadline for
    /// that would add latency to every session on a link that is not slow at
    /// all — so the evidence is the spread of what actually arrived, which is
    /// small.
    #[test]
    fn a_lost_packet_on_a_fast_link_does_not_lengthen_it() {
        let mut j = Jitter::new(30, 90_000);
        let from_fps = j.wait();
        let start = Instant::now();
        for _ in 0..8 {
            // Two datagrams close together, then silence until the deadline.
            j.arrived(start, 0, true);
            j.arrived(start + Duration::from_millis(2), 0, true);
            j.fired(start + from_fps + Duration::from_millis(1));
        }
        assert_eq!(
            j.wait(),
            from_fps,
            "a link that merely loses packets must not pay for a slow one"
        );
    }

    /// A frame that completes contributes the honest number: first datagram
    /// to last.
    #[test]
    fn a_frame_that_completes_is_measured_as_it_arrived() {
        let mut j = Jitter::new(60, 90_000);
        let start = Instant::now();
        let spread = Duration::from_millis(120);
        j.arrived(start, 0, true);
        j.arrived(start + spread, 0, true);
        let _ = j.settled(start + spread);
        assert_eq!(j.widest_arrival(), spread);
        assert!(
            j.wait() >= spread,
            "the deadline must be at least as long as a frame takes to arrive"
        );
    }

    #[test]
    fn a_frame_that_settles_stops_the_clock() {
        let mut j = Jitter::new(30, 90_000);
        let start = Instant::now();
        j.arrived(start, 0, true);
        assert!(j.due_in(start).is_some());
        let _ = j.settled(start);
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
        j.fired(start + Duration::from_millis(100));
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
        let _ = k.settled(t + Duration::from_millis(8));
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
