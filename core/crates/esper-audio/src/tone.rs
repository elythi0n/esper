//! A source that needs no sound card.
//!
//! The audio counterpart of the synthetic screen, and it exists for the same
//! reason: everything above capture — encode, packetize, send, receive,
//! decode, play — has to be buildable and testable on a machine with no sound
//! server, which includes every CI runner this project will ever use.
//!
//! It is not a stub. It produces a tone of a known frequency at a known
//! amplitude, paced in real time, so a test can assert what came out the far
//! end rather than merely that *something* did. A silent generator would pass
//! a pipeline that dropped every sample.

use std::time::{Duration, Instant};

use crate::{AudioError, Captured, Format, Frame, Source};

/// The backend name reported in logs and the session summary.
pub const BACKEND: &str = "synthetic";

/// A tone, generated on demand.
#[derive(Debug)]
pub struct SyntheticTone {
    format: Format,
    /// Cycles per second of the tone.
    hz: f32,
    /// Peak amplitude, as a fraction of full scale.
    level: f32,
    /// Samples produced so far, per channel — the phase, and the timestamp.
    produced: u64,
    /// When the next frame is due, so a caller polling faster than real time
    /// is told nothing has happened yet rather than handed sound from the
    /// future.
    next_due: Option<Instant>,
    /// Frames to produce before the source ends, if limited.
    limit: Option<u64>,
    /// Whether to pace at all.
    paced: bool,
}

impl SyntheticTone {
    /// A 440 Hz tone at half scale, in the shape this product sends.
    #[must_use]
    pub fn new() -> Self {
        Self {
            format: crate::SENT,
            hz: 440.0,
            level: 0.5,
            produced: 0,
            next_due: None,
            limit: None,
            paced: true,
        }
    }

    /// A tone of this frequency.
    #[must_use]
    pub const fn at_hz(mut self, hz: f32) -> Self {
        self.hz = hz;
        self
    }

    /// A tone at this peak amplitude, as a fraction of full scale.
    #[must_use]
    pub const fn at_level(mut self, level: f32) -> Self {
        self.level = level;
        self
    }

    /// This shape rather than the default.
    #[must_use]
    pub const fn in_format(mut self, format: Format) -> Self {
        self.format = format;
        self
    }

    /// Stop after this many frames, so a test can let a stream end.
    #[must_use]
    pub const fn with_frame_limit(mut self, frames: u64) -> Self {
        self.limit = Some(frames);
        self
    }

    /// Produce frames as fast as they are asked for.
    ///
    /// **For tests that measure something other than time.** A round trip
    /// through an encoder and a decoder is microseconds of work; pacing it at
    /// real time would make a hundred frames take two seconds of a suite that
    /// runs on every commit.
    #[must_use]
    pub const fn unpaced(mut self) -> Self {
        self.paced = false;
        self
    }

    /// How many frames have been produced.
    #[must_use]
    pub const fn frames(&self) -> u64 {
        match self.format.samples_per_frame() {
            0 => 0,
            n => self.produced / n as u64,
        }
    }

    /// The sample this generator would produce at a given position.
    ///
    /// Exposed so a test can compute what it expects instead of comparing a
    /// pipeline's output against a previous run of the same pipeline.
    #[must_use]
    pub fn sample_at(&self, sample: u64) -> i16 {
        // `as` on a small integer part is exact, and the phase is reduced
        // before it is converted so a long-running stream does not lose
        // precision.
        let period = f64::from(self.format.sample_rate_hz) / f64::from(self.hz.max(1.0));
        let phase = if period > 0.0 {
            (sample as f64 % period) / period
        } else {
            0.0
        };
        let value = (phase * std::f64::consts::TAU).sin() * f64::from(self.level) * 32_767.0;
        // Clamped rather than wrapped: a level above one is a caller's mistake
        // and should sound like clipping, not like the opposite sign.
        value.clamp(-32_768.0, 32_767.0) as i16
    }
}

impl Default for SyntheticTone {
    fn default() -> Self {
        Self::new()
    }
}

impl Source for SyntheticTone {
    fn backend(&self) -> &'static str {
        BACKEND
    }

    fn format(&self) -> Format {
        self.format
    }

    fn next_frame(&mut self, timeout: Duration) -> Result<Captured, AudioError> {
        if self.limit.is_some_and(|n| self.frames() >= n) {
            return Ok(Captured::Finished);
        }
        if self.paced {
            let now = Instant::now();
            match self.next_due {
                Some(due) if due > now => {
                    let wait = due.saturating_duration_since(now);
                    // Asked for sound sooner than sound exists. If the caller
                    // is willing to wait, wait; otherwise nothing has
                    // happened, which is the truth.
                    if wait > timeout {
                        return Ok(Captured::Nothing);
                    }
                    std::thread::sleep(wait);
                }
                _ => {}
            }
            let interval = Duration::from_millis(u64::from(self.format.frame_ms));
            self.next_due = Instant::now().checked_add(interval);
        }

        let per_channel = self.format.samples_per_frame();
        let channels = usize::from(self.format.channels.max(1));
        let mut samples = Vec::with_capacity(per_channel.saturating_mul(channels));
        for i in 0..per_channel {
            let value = self.sample_at(self.produced.saturating_add(i as u64));
            // The same value in both channels: a test that swapped left and
            // right would pass either way, and this generator is not the
            // place to check channel order — `the_channels_are_interleaved`
            // in the codec's tests is.
            for _ in 0..channels {
                samples.push(value);
            }
        }
        let at = self.produced;
        self.produced = self.produced.saturating_add(per_channel as u64);
        Ok(Captured::Frame(Frame { samples, at }))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    fn grab(source: &mut SyntheticTone) -> Frame {
        match source.next_frame(Duration::from_secs(1)).expect("a frame") {
            Captured::Frame(f) => f,
            other => panic!("expected a frame, got {other:?}"),
        }
    }

    /// It produces frames of the right shape, with sound in them.
    #[test]
    fn it_produces_frames_with_no_sound_card_anywhere() {
        let mut tone = SyntheticTone::new().unpaced();
        let f = grab(&mut tone);
        assert!(f.fits(&crate::SENT));
        assert_eq!(f.at, 0);
        assert!(
            f.peak() > 10_000,
            "a tone at half scale should be loud, got a peak of {}",
            f.peak()
        );
    }

    /// **The timestamp is a sample count and it advances by exactly one
    /// frame.** Anything else and the far end's idea of when a frame belongs
    /// drifts against the sound, which is heard as a click every few seconds.
    #[test]
    fn timestamps_advance_by_exactly_one_frame() {
        let mut tone = SyntheticTone::new().unpaced();
        let mut want = 0u64;
        for _ in 0..10 {
            let f = grab(&mut tone);
            assert_eq!(f.at, want);
            want += crate::SENT.samples_per_frame() as u64;
        }
        assert_eq!(tone.frames(), 10);
    }

    /// The tone is the tone that was asked for: one period of 480 Hz at 48 kHz
    /// is 100 samples, so the sign pattern repeats every 100.
    #[test]
    fn the_tone_has_the_frequency_it_was_asked_for() {
        let tone = SyntheticTone::new().at_hz(480.0).unpaced();
        for i in 0..300u64 {
            assert_eq!(
                tone.sample_at(i).signum(),
                tone.sample_at(i + 100).signum(),
                "sample {i} and {} should be at the same phase",
                i + 100
            );
        }
        // And it is not a constant, which the check above would also accept.
        let positive = (0..100).filter(|i| tone.sample_at(*i) > 0).count();
        assert!(
            (40..=60).contains(&positive),
            "half of one period should be positive, got {positive} of 100"
        );
    }

    /// A limited source ends, so a test can watch a stream finish rather than
    /// stopping it from outside.
    #[test]
    fn a_limited_source_finishes() {
        let mut tone = SyntheticTone::new().unpaced().with_frame_limit(3);
        for _ in 0..3 {
            let _ = grab(&mut tone);
        }
        assert_eq!(
            tone.next_frame(Duration::from_millis(1)).expect("no error"),
            Captured::Finished
        );
    }

    /// **Paced, when it is paced.** A source that produced a thousand frames
    /// instantly would let a test "send twenty seconds of audio" in a
    /// millisecond and prove nothing about a pipeline that has to keep up.
    #[test]
    fn a_paced_source_will_not_hand_over_sound_from_the_future() {
        let mut tone = SyntheticTone::new();
        let _ = grab(&mut tone);
        // The next frame is 20 ms away and this asks for it with no patience.
        assert_eq!(
            tone.next_frame(Duration::ZERO).expect("no error"),
            Captured::Nothing
        );
    }

    /// Mono is a frame of half the values, not a frame of half the sound.
    #[test]
    fn mono_is_one_value_per_sample() {
        let mono = Format {
            channels: 1,
            ..crate::SENT
        };
        let mut tone = SyntheticTone::new().in_format(mono).unpaced();
        let f = grab(&mut tone);
        assert!(f.fits(&mono));
        assert_eq!(f.samples.len(), 960);
    }
}
