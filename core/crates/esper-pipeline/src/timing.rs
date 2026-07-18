//! Per-stage timing, in both the units that matter.
//!
//! # Why every stage is timed twice
//!
//! Wall clock answers "does this keep up"; CPU time answers "does this fit the
//! budget". They are the same number only for single-threaded work, and this
//! pipeline is not single-threaded — the encoder may use several threads, so
//! its wall clock can be a quarter of its cost.
//!
//! Measuring only wall clock produced a real error in this project: a
//! four-thread encoder was going to be reported as taking full-motion 1080p
//! from 148% of a core to 44%, when the CPU figure was 158%. Both numbers are
//! true and they answer different questions, so both are kept.

use std::time::{Duration, Instant};

/// Process CPU time, summed across threads.
///
/// `None` where the platform has no cheap way to ask. Reported as absent
/// rather than substituted with wall clock, because a budget silently measured
/// in the wrong unit is worse than one that says it cannot be measured.
#[must_use]
pub fn process_cpu_time() -> Option<Duration> {
    #[cfg(unix)]
    {
        let t = rustix::time::clock_gettime(rustix::time::ClockId::ProcessCPUTime);
        let secs = u64::try_from(t.tv_sec).ok()?;
        let nanos = u32::try_from(t.tv_nsec).ok()?;
        Some(Duration::new(secs, nanos))
    }
    #[cfg(not(unix))]
    {
        None
    }
}

/// What one stage of the pipeline has cost.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Stage {
    /// How many times it ran.
    pub calls: u64,
    /// Elapsed time.
    pub wall: Duration,
    /// CPU time, where the platform can say.
    pub cpu: Option<Duration>,
    /// The slowest single call.
    ///
    /// Kept because a mean hides the frame that stuttered, and the stutter is
    /// what the user noticed.
    pub worst: Duration,
}

impl Stage {
    /// Fold in one call.
    pub fn record(&mut self, wall: Duration, cpu: Option<Duration>) {
        self.calls = self.calls.saturating_add(1);
        self.wall = self.wall.saturating_add(wall);
        self.cpu = match (self.cpu, cpu) {
            (Some(a), Some(b)) => Some(a.saturating_add(b)),
            // Once unmeasurable, always unmeasurable: a partial total would
            // read as a small one.
            (None, Some(b)) if self.calls == 1 => Some(b),
            _ => None,
        };
        if wall > self.worst {
            self.worst = wall;
        }
    }

    /// Mean milliseconds of wall clock per call.
    #[must_use]
    pub fn mean_wall_ms(&self) -> f64 {
        if self.calls == 0 {
            return 0.0;
        }
        self.wall.as_secs_f64() * 1000.0 / self.calls as f64
    }

    /// Mean milliseconds of CPU per call.
    #[must_use]
    pub fn mean_cpu_ms(&self) -> Option<f64> {
        if self.calls == 0 {
            return None;
        }
        Some(self.cpu?.as_secs_f64() * 1000.0 / self.calls as f64)
    }

    /// Share of one core this stage used over a period.
    ///
    /// CPU time over elapsed time — the unit the design's ceiling is written
    /// in. `None` rather than a wall-clock substitute where CPU time is
    /// unavailable.
    #[must_use]
    pub fn core_share(&self, over: Duration) -> Option<f64> {
        let secs = over.as_secs_f64();
        if secs <= 0.0 {
            return None;
        }
        Some(self.cpu?.as_secs_f64() / secs)
    }
}

/// Times a stage, capturing both clocks.
///
/// Both readings are taken as close together as the code allows, so the two
/// measurements describe the same work rather than overlapping windows.
#[derive(Debug)]
pub struct Timer {
    wall: Instant,
    cpu: Option<Duration>,
}

impl Timer {
    /// Start timing.
    #[must_use]
    pub fn start() -> Self {
        Self {
            cpu: process_cpu_time(),
            wall: Instant::now(),
        }
    }

    /// Stop, and fold the result into a stage.
    pub fn stop(self, into: &mut Stage) {
        let wall = self.wall.elapsed();
        let cpu = match (self.cpu, process_cpu_time()) {
            (Some(before), Some(after)) => Some(after.saturating_sub(before)),
            _ => None,
        };
        into.record(wall, cpu);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stage_starts_empty() {
        let s = Stage::default();
        assert_eq!(s.calls, 0);
        assert_eq!(s.mean_wall_ms(), 0.0);
        assert_eq!(s.mean_cpu_ms(), None);
        assert_eq!(s.core_share(Duration::from_secs(1)), None);
    }

    #[test]
    fn calls_accumulate_and_average() {
        let mut s = Stage::default();
        s.record(Duration::from_millis(10), Some(Duration::from_millis(12)));
        s.record(Duration::from_millis(20), Some(Duration::from_millis(28)));
        assert_eq!(s.calls, 2);
        assert_eq!(s.wall, Duration::from_millis(30));
        assert_eq!(s.cpu, Some(Duration::from_millis(40)));
        assert!((s.mean_wall_ms() - 15.0).abs() < 0.001);
        assert!((s.mean_cpu_ms().unwrap_or(0.0) - 20.0).abs() < 0.001);
    }

    /// A mean of 4 ms hides a 40 ms frame, and it is the 40 ms frame the user
    /// noticed.
    #[test]
    fn the_worst_call_is_kept_not_averaged_away() {
        let mut s = Stage::default();
        for _ in 0..99 {
            s.record(Duration::from_millis(4), None);
        }
        s.record(Duration::from_millis(40), None);
        assert_eq!(s.worst, Duration::from_millis(40));
        assert!(s.mean_wall_ms() < 5.0);
    }

    /// **The distinction that produced a real error.** CPU time is what the
    /// budget is written in, and for threaded work it is not the wall clock.
    #[test]
    fn the_core_share_is_cpu_time_not_wall_clock() {
        let mut s = Stage::default();
        // Thirty frames that each took 10 ms of wall clock and 40 ms of CPU —
        // a four-thread encode.
        for _ in 0..30 {
            s.record(Duration::from_millis(10), Some(Duration::from_millis(40)));
        }
        let share = s.core_share(Duration::from_secs(1)).unwrap_or(0.0);
        assert!(
            (share - 1.2).abs() < 0.001,
            "expected 120% of a core from the CPU figure, got {share}"
        );
        // And the wall clock would have said 30%, which is the wrong answer to
        // the budget question.
        assert!((s.mean_wall_ms() - 10.0).abs() < 0.001);
    }

    /// A budget silently measured in the wrong unit is worse than one that
    /// says it cannot be measured.
    #[test]
    fn a_stage_with_no_cpu_reading_reports_absence_not_zero() {
        let mut s = Stage::default();
        s.record(Duration::from_millis(5), None);
        assert_eq!(s.cpu, None);
        assert_eq!(s.mean_cpu_ms(), None);
        assert_eq!(s.core_share(Duration::from_secs(1)), None);
    }

    /// One missing reading among many must not produce a total that reads as
    /// small but complete.
    #[test]
    fn a_partial_cpu_total_is_discarded_rather_than_reported() {
        let mut s = Stage::default();
        s.record(Duration::from_millis(5), Some(Duration::from_millis(5)));
        s.record(Duration::from_millis(5), Some(Duration::from_millis(5)));
        assert_eq!(s.cpu, Some(Duration::from_millis(10)));
        // A gap poisons the total, deliberately.
        s.record(Duration::from_millis(5), None);
        assert_eq!(
            s.cpu, None,
            "a total missing one of three readings would read as a complete \
             small number"
        );
    }

    #[test]
    fn the_timer_records_something_for_real_work() {
        let mut s = Stage::default();
        let t = Timer::start();
        // Enough work that the clock definitely moves.
        let mut acc = 0u64;
        for i in 0..2_000_000u64 {
            acc = acc.wrapping_add(i);
        }
        std::hint::black_box(acc);
        t.stop(&mut s);

        assert_eq!(s.calls, 1);
        assert!(s.wall > Duration::ZERO, "the wall clock did not move");
        #[cfg(unix)]
        assert!(
            s.cpu.is_some(),
            "CPU time should be available on unix, and the budget needs it"
        );
    }

    #[cfg(unix)]
    #[test]
    fn process_cpu_time_is_available_and_advances() {
        let first = process_cpu_time().expect("unix has a CPU clock");
        let mut acc = 0u64;
        for i in 0..5_000_000u64 {
            acc = acc.wrapping_add(i);
        }
        std::hint::black_box(acc);
        let second = process_cpu_time().expect("still available");
        assert!(
            second > first,
            "CPU time did not advance across five million additions"
        );
    }
}
