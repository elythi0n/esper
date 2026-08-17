//! What is on the screen, as the damage says rather than as anybody declared.
//!
//! # What this is not
//!
//! It is **not** a guess at what the person is doing. `Profile` is the
//! controller's own statement — reading, or watching something — and that
//! stays theirs, because a slideshow and a film look identical to a damage
//! tracker until it is too late to have chosen differently. Nothing here
//! overrides a viewer who has said what they want.
//!
//! What it answers is the narrower question the host is already answering
//! badly: a controller that says `Balanced` states no preference and leaves
//! the host's own judgement alone, and the host's judgement was the CPU budget
//! and nothing else. A screen showing a still page of text and a screen
//! showing a film were treated identically, and they should not be — the first
//! wants every glyph edge and can have them almost free, and the second wants
//! frames and can spare the detail.
//!
//! So this is a claim about *pixels*, which damage can support, standing in
//! for a claim about *intent*, which it cannot.
//!
//! # The statistic
//!
//! One number: the share of the screen that changes per frame, averaged over a
//! window — **counting frames where nothing changed as zero**, which is the
//! whole point. A terminal with somebody typing in it produces one tiny
//! rectangle every few hundred milliseconds; averaged over only the frames
//! that had damage it looks like a small change, and averaged over all of them
//! it looks like almost nothing at all, which is what it is.
//!
//! # Hysteresis, because the alternative oscillates
//!
//! A single threshold on a noisy signal switches back and forth across it, and
//! every switch here costs an encoder reconfiguration and a keyframe. So each
//! band has a **wider door in than out**, and a verdict has to hold for a
//! minimum dwell before it is acted on. Scrolling a page of text is exactly
//! the case that would otherwise flap: it is motion for a moment inside
//! something that is plainly text.

use std::time::{Duration, Instant};

use crate::damage::Damage;
use crate::geometry::Rect;

/// What the screen looks like it is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Content {
    /// Very little changes, and what does is small. A document, a terminal, a
    /// code editor.
    Text,
    /// Neither one thing nor the other, and the honest answer for most
    /// screens most of the time.
    #[default]
    Mixed,
    /// Most of the screen, most frames. A film, a game, a scrolling map.
    Motion,
}

impl Content {
    /// A phrase for a person reading a session's own output.
    #[must_use]
    pub const fn describe(self) -> &'static str {
        match self {
            Self::Text => "text: little changes, and sharply",
            Self::Mixed => "mixed",
            Self::Motion => "motion: most of the screen, most frames",
        }
    }
}

/// The share of the screen changing per frame above which a screen is motion.
///
/// **Measured against the synthetic patterns rather than chosen.** Full-motion
/// content moves essentially the whole frame every frame and lands near 1.0;
/// the screen-like pattern — bands that never change, a counter, and a box
/// moving across — lands two orders of magnitude below that. There is a lot of
/// room between them, and a threshold in the middle of a gap that wide is not
/// a number anybody has to tune.
const MOTION_ENTER: f64 = 0.30;

/// And below which it stops being motion. See the module note on hysteresis.
const MOTION_LEAVE: f64 = 0.15;

/// Below this share, a screen is text.
///
/// One 1080p frame is about two million pixels, so this is twenty thousand of
/// them: a couple of lines of a terminal, or a cursor and a word. A whole
/// window redrawing is ten times it.
const TEXT_ENTER: f64 = 0.01;

/// And above which it stops being text.
const TEXT_LEAVE: f64 = 0.05;

/// How long a verdict has to hold before it is acted on.
///
/// Scrolling a page is motion for a moment inside something that is plainly
/// text, and a classifier that believed it would spend a keyframe saying so
/// and another one taking it back.
const DWELL: Duration = Duration::from_secs(2);

/// How long a window of frames the average is taken over.
///
/// Long enough that a single still frame in a film does not read as text, and
/// short enough that starting a video is noticed while it is still playing.
const WINDOW: Duration = Duration::from_secs(2);

/// Watches the damage go by and says what the screen is showing.
#[derive(Debug)]
pub struct Watching {
    /// Damaged share per frame, with the instant it was seen.
    seen: std::collections::VecDeque<(Instant, f64)>,
    /// What is being reported now.
    settled: Content,
    /// What the statistic currently says, and since when — which is not the
    /// same thing, and the difference is the dwell.
    proposed: Content,
    since: Instant,
}

impl Watching {
    /// Start watching, believing nothing in particular.
    #[must_use]
    pub fn new(now: Instant) -> Self {
        Self {
            seen: std::collections::VecDeque::new(),
            settled: Content::Mixed,
            proposed: Content::Mixed,
            since: now,
        }
    }

    /// Take one frame's damage into account.
    pub fn saw(&mut self, damage: &Damage, frame: &Rect, now: Instant) {
        let area = frame.area();
        let share = if area == 0 {
            0.0
        } else {
            // Clamped: a backend that reports overlapping regions could add up
            // to more than the screen, and a share above one would drag the
            // average somewhere the thresholds do not mean anything.
            (damage.changed_pixels(frame) as f64 / area as f64).clamp(0.0, 1.0)
        };
        self.seen.push_back((now, share));
        while self
            .seen
            .front()
            .is_some_and(|(t, _)| now.saturating_duration_since(*t) > WINDOW)
        {
            self.seen.pop_front();
        }

        let proposal = self.proposal();
        if proposal == self.proposed {
            // Held long enough, and it is not what is being reported.
            if self.settled != proposal && now.saturating_duration_since(self.since) >= DWELL {
                self.settled = proposal;
            }
        } else {
            self.proposed = proposal;
            self.since = now;
        }
    }

    /// The share of the screen changing per frame, over the window.
    #[must_use]
    pub fn load(&self) -> f64 {
        if self.seen.is_empty() {
            return 0.0;
        }
        self.seen.iter().map(|(_, s)| *s).sum::<f64>() / self.seen.len() as f64
    }

    /// What the screen is showing, once it has held that for long enough.
    #[must_use]
    pub const fn content(&self) -> Content {
        self.settled
    }

    /// What the statistic says right now, before the dwell.
    ///
    /// The bands are asymmetric: which one applies depends on where the
    /// verdict already is, which is what stops a signal sitting on a threshold
    /// from switching every frame.
    fn proposal(&self) -> Content {
        // Nothing seen yet is not a still screen; it is nothing seen yet.
        if self.seen.is_empty() {
            return self.settled;
        }
        let load = self.load();
        match self.settled {
            Content::Motion if load >= MOTION_LEAVE => Content::Motion,
            Content::Text if load <= TEXT_LEAVE => Content::Text,
            _ if load >= MOTION_ENTER => Content::Motion,
            _ if load <= TEXT_ENTER => Content::Text,
            _ => Content::Mixed,
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    const FRAME: Rect = Rect {
        x: 0,
        y: 0,
        width: 1920,
        height: 1080,
    };

    /// Feed a constant share for a length of time, thirty frames a second.
    fn feed(w: &mut Watching, share: f64, at: &mut Instant, how_long: Duration) {
        let step = Duration::from_millis(33);
        let until = *at + how_long;
        while *at < until {
            let px = (share * FRAME.area() as f64) as u64;
            let damage = if px == 0 {
                Damage::Nothing
            } else {
                let rows = (px / u64::from(FRAME.width)).max(1);
                crate::damage::Regions::one(
                    Rect {
                        x: 0,
                        y: 0,
                        width: FRAME.width,
                        height: u32::try_from(rows)
                            .unwrap_or(FRAME.height)
                            .min(FRAME.height),
                    },
                    &FRAME,
                )
            };
            w.saw(&damage, &FRAME, *at);
            *at += step;
        }
    }

    /// **The thresholds against the patterns they were set from.**
    ///
    /// The two constants sit in a gap, and this is the measurement that says
    /// the gap is there: the synthetic capturer's three patterns are driven
    /// through the classifier and the share each produces is asserted to fall
    /// on the side it should. A threshold justified by a comment is a
    /// threshold nobody has checked.
    #[test]
    fn the_patterns_land_where_the_thresholds_say_they_should() {
        use crate::Capturer as _;
        use crate::pattern::Motion;
        use crate::synthetic::SyntheticCapturer;
        use std::time::Duration;

        // Each pattern, and where its share of the screen has to fall.
        for (motion, lo, hi, want) in [
            (Motion::Still, 0.0, TEXT_ENTER, Content::Text),
            (Motion::Screen, 0.0, TEXT_LEAVE, Content::Text),
            (Motion::Everything, MOTION_ENTER, 1.01, Content::Motion),
        ] {
            // A thousand frames a second and a one-millisecond wait: the
            // capturer paces itself in real time, and at thirty this test
            // spent three minutes waiting for a clock. What is being measured
            // is the *share of the screen* each pattern changes, which does
            // not depend on how fast the frames are asked for.
            let mut cap = SyntheticCapturer::new(1920, 1080, 1000)
                .expect("a synthetic capturer")
                .with_motion(motion);
            let mut at = Instant::now();
            let mut w = Watching::new(at);
            let frame = Rect {
                x: 0,
                y: 0,
                width: 1920,
                height: 1080,
            };
            // **Few frames, spaced widely in virtual time.** Generating a
            // 1080p pattern is expensive in a debug build and the share each
            // one changes is steady within a few frames — so what this needs
            // is enough *time* to clear the dwell, not enough frames. The
            // timestamps are the test's, and the pattern does not know or
            // care how fast it is being asked.
            // Twenty-four, not sixteen: the first frame of any session is
            // `Everything`, and the average has to age that out of its window
            // before the dwell can even start counting. Sixteen left `Still`
            // still reading as mixed.
            for _ in 0..24 {
                match cap.next_frame(Duration::from_millis(1)) {
                    Ok(crate::Capture::Frame(f)) => w.saw(f.damage(), &frame, at),
                    Ok(_) => w.saw(&Damage::Nothing, &frame, at),
                    Err(e) => panic!("the synthetic capturer stopped: {e}"),
                }
                at += Duration::from_millis(250);
            }
            let load = w.load();
            assert!(
                load >= lo && load < hi,
                "{motion:?} changed {:.4} of the screen per frame, which is outside \
                 the [{lo}, {hi}) this threshold assumes",
                load
            );
            assert_eq!(w.content(), want, "{motion:?} was classified wrongly");
        }
    }

    #[test]
    fn a_screen_where_nothing_moves_reads_as_text() {
        let mut at = Instant::now();
        let mut w = Watching::new(at);
        feed(&mut w, 0.0, &mut at, Duration::from_secs(5));
        assert_eq!(w.content(), Content::Text);
    }

    #[test]
    fn a_screen_that_moves_entirely_reads_as_motion() {
        let mut at = Instant::now();
        let mut w = Watching::new(at);
        feed(&mut w, 1.0, &mut at, Duration::from_secs(5));
        assert_eq!(w.content(), Content::Motion);
    }

    /// **The default is the honest answer, not a lean towards either.**
    #[test]
    fn a_screen_between_the_two_reads_as_mixed() {
        let mut at = Instant::now();
        let mut w = Watching::new(at);
        feed(&mut w, 0.2, &mut at, Duration::from_secs(5));
        assert_eq!(w.content(), Content::Mixed);
    }

    /// **Nothing is decided before there is anything to decide from.**
    #[test]
    fn a_verdict_is_not_reached_before_the_dwell() {
        let mut at = Instant::now();
        let mut w = Watching::new(at);
        feed(&mut w, 1.0, &mut at, Duration::from_millis(500));
        assert_eq!(
            w.content(),
            Content::Mixed,
            "half a second of motion decided the whole session"
        );
    }

    /// **Scrolling a page must not turn a document into a film.** This is the
    /// case the hysteresis exists for: a burst of whole-screen damage inside
    /// something that is plainly text.
    #[test]
    fn a_scroll_does_not_turn_text_into_motion() {
        let mut at = Instant::now();
        let mut w = Watching::new(at);
        feed(&mut w, 0.0, &mut at, Duration::from_secs(5));
        assert_eq!(w.content(), Content::Text);

        // A page-down: everything changes, for a third of a second.
        feed(&mut w, 1.0, &mut at, Duration::from_millis(330));
        assert_eq!(
            w.content(),
            Content::Text,
            "a scroll spent a keyframe calling a document a film"
        );
    }

    /// **And a real film does change the verdict**, or the hysteresis above is
    /// just a way of never deciding anything.
    #[test]
    fn sustained_motion_does_change_the_verdict() {
        let mut at = Instant::now();
        let mut w = Watching::new(at);
        feed(&mut w, 0.0, &mut at, Duration::from_secs(5));
        assert_eq!(w.content(), Content::Text);

        feed(&mut w, 1.0, &mut at, Duration::from_secs(6));
        assert_eq!(w.content(), Content::Motion);
    }

    /// **It comes back**, which is the half a one-way classifier would fail.
    #[test]
    fn a_film_that_ends_lets_the_screen_be_text_again() {
        let mut at = Instant::now();
        let mut w = Watching::new(at);
        feed(&mut w, 1.0, &mut at, Duration::from_secs(6));
        assert_eq!(w.content(), Content::Motion);

        feed(&mut w, 0.0, &mut at, Duration::from_secs(6));
        assert_eq!(w.content(), Content::Text);
    }

    /// **A signal sitting on the threshold must not switch every frame.**
    /// Without the two-sided band this is what oscillates, and every switch
    /// costs an encoder rebuild and a keyframe.
    #[test]
    fn a_signal_on_the_boundary_does_not_flap() {
        let mut at = Instant::now();
        let mut w = Watching::new(at);
        feed(&mut w, 1.0, &mut at, Duration::from_secs(6));
        assert_eq!(w.content(), Content::Motion);

        // Wobbling either side of MOTION_ENTER, which without hysteresis is a
        // switch every time it crosses.
        let mut switches = 0;
        let mut last = w.content();
        for i in 0..200 {
            let share = if i % 2 == 0 {
                MOTION_ENTER + 0.02
            } else {
                MOTION_ENTER - 0.02
            };
            feed(&mut w, share, &mut at, Duration::from_millis(33));
            if w.content() != last {
                switches += 1;
                last = w.content();
            }
        }
        assert_eq!(
            switches, 0,
            "the verdict changed {switches} times on a signal sitting on the threshold"
        );
    }
}
