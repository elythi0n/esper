//! Deciding what to say about the pointer.
//!
//! # The whole job is not sending things
//!
//! A pointer read sixty times a second is sixty positions and sixty shapes,
//! and almost all of them are the same as the one before. What travels is:
//!
//! - a **shape** when the serial changes, and never otherwise — a pointer
//!   crossing a text field and coming back alternates between two shapes many
//!   times a second, and re-sending either is bytes for nothing;
//! - a **position** when it actually moves, and no more often than a person
//!   can perceive;
//! - **nothing at all** while the pointer sits still, which on a desktop is
//!   most of the time.
//!
//! # Why the position is cheap to be generous with
//!
//! It is four bytes. The reason to rate-limit it is not bandwidth but the
//! control stream: it is ordered, and a thousand positions a second would put
//! a thousand small writes in front of anything else that needed to go.
//!
//! # What a controller does with it
//!
//! In a session it is *driving*, the controller already knows where the
//! pointer is — it put it there — and draws it locally at the speed of its own
//! mouse. The host's positions are then a correction: when they disagree with
//! what was asked for, something on the host is holding the pointer, and a
//! locally drawn cursor would be lying.
//!
//! In a session it is only *watching*, the host's positions are the only
//! source there is, which is why they are sent whether or not anybody is
//! driving.

use std::time::{Duration, Instant};

use esper_capture::cursor::CursorState;
use esper_proto::{CursorImage, CursorMessage, CursorPos, CursorShape};

/// The most often a position is sent.
///
/// Eight milliseconds — a hundred and twenty-five a second, which is faster
/// than any display refreshes and far faster than a person can see. The point
/// of the limit is the ordered stream behind it, not the bytes.
pub const POSITION_INTERVAL: Duration = Duration::from_millis(8);

/// Turns pointer readings into the messages worth sending.
#[derive(Debug, Default)]
pub struct CursorTracker {
    /// The serial of the shape the controller has been sent.
    sent_serial: Option<u32>,
    /// Whether the controller was last told the pointer is hidden.
    sent_hidden: bool,
    /// The position the controller was last sent.
    sent_at: Option<(u32, u16, u16)>,
    /// When that was.
    sent_when: Option<Instant>,
    shapes: u64,
    positions: u64,
}

impl CursorTracker {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// How many shapes and positions have been sent.
    #[must_use]
    pub const fn counts(&self) -> (u64, u64) {
        (self.shapes, self.positions)
    }

    /// Start again, for a session that has been resumed on a new transport.
    ///
    /// **The far end is new and knows nothing**, so the next reading sends
    /// both a shape and a position however little has changed. Forgetting to
    /// do this would leave a resumed session with no pointer until the shape
    /// happened to change.
    pub fn forget(&mut self) {
        self.sent_serial = None;
        self.sent_hidden = false;
        self.sent_at = None;
        self.sent_when = None;
    }

    /// What to send about this reading, if anything.
    ///
    /// `monitor` is the one being captured, with its geometry, because a
    /// position is a fraction of that monitor. A pointer somewhere else on the
    /// desktop is **not sent**: it is not in the picture, and a controller
    /// given a clamped position would draw a pointer sitting against an edge
    /// that nothing is near.
    pub fn update(
        &mut self,
        state: &CursorState,
        monitor: (u32, i32, i32, u32, u32),
        now: Instant,
    ) -> Vec<CursorMessage> {
        let mut out = Vec::new();
        let (id, mx, my, width, height) = monitor;

        // Whether the pointer is on the screen being sent.
        let on_this_monitor = state.x >= mx
            && state.y >= my
            && state.x < mx.saturating_add(i32::try_from(width).unwrap_or(i32::MAX))
            && state.y < my.saturating_add(i32::try_from(height).unwrap_or(i32::MAX));

        // **A pointer on another screen is a pointer that is not there**, and
        // the far end has to be told so rather than left with the last
        // position it was given.
        //
        // Before this, moving the mouse to a second monitor left the viewer
        // drawing a pointer frozen at the edge of the picture — a pointer
        // somewhere the person's hand is not, which is worse than no pointer
        // at all because it looks live.
        //
        // Said with `Hidden`, which already means "there is no pointer to
        // draw". That does conflate "an application hid it" with "it is on
        // another screen" — and deliberately: the two are the same
        // *instruction* to a viewer, and a second message for a distinction
        // nothing could act on differently would be a protocol carrying a
        // difference to nowhere.
        let hidden = state.image.is_none() || !on_this_monitor;
        let shape_is_new = self.sent_serial != Some(state.serial) || self.sent_hidden != hidden;
        if shape_is_new {
            let image = match &state.image {
                // Nothing to draw: either the application hid the pointer, or
                // it is on a screen this session is not sending.
                _ if !on_this_monitor => Some(CursorImage::Hidden),
                None => Some(CursorImage::Hidden),
                Some(i) if i.is_consistent() => Some(CursorImage::Bitmap {
                    width: i.width,
                    height: i.height,
                    hotspot_x: i.hotspot_x,
                    hotspot_y: i.hotspot_y,
                    rgba: i.rgba.clone(),
                }),
                // A backend that handed over a cursor whose pixels do not
                // match its header is a backend with a bug, and passing it on
                // would make it the controller's problem. Dropped, and the
                // serial is not recorded, so a later good reading of the same
                // shape is still sent.
                Some(_) => None,
            };
            if let Some(image) = image {
                out.push(CursorMessage::Shape(CursorShape {
                    serial: state.serial,
                    image,
                }));
                self.sent_serial = Some(state.serial);
                self.sent_hidden = hidden;
                self.shapes = self.shapes.saturating_add(1);
            }
        }

        // And the position only means anything while it is on this screen.
        if !on_this_monitor {
            // Forgotten, so that coming back sends a position rather than
            // being suppressed as "the same place it already was" — which,
            // after a trip to the other screen, is a pointer that reappears
            // only once it moves.
            self.sent_at = None;
            return out;
        }
        let here = CursorPos::from_pixels(
            id,
            state.x.saturating_sub(mx),
            state.y.saturating_sub(my),
            width,
            height,
        );
        let moved = self.sent_at != Some((here.monitor, here.x, here.y));
        let due = self
            .sent_when
            .is_none_or(|then| now.saturating_duration_since(then) >= POSITION_INTERVAL);
        if moved && due {
            out.push(CursorMessage::Moved(here));
            self.sent_at = Some((here.monitor, here.x, here.y));
            self.sent_when = Some(now);
            self.positions = self.positions.saturating_add(1);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;
    use esper_capture::cursor::CursorImage as Captured;

    /// The monitor being captured: the second one, away from the origin,
    /// because an offset is where this arithmetic goes wrong.
    const MONITOR: (u32, i32, i32, u32, u32) = (1, 1920, 300, 2560, 1440);

    fn shape_of(serial: u32) -> Option<Captured> {
        Some(Captured {
            width: 2,
            height: 2,
            hotspot_x: 0,
            hotspot_y: 0,
            // Opaque, and different per serial so a test can tell them apart.
            rgba: (0..4)
                .flat_map(|_| [u8::try_from(serial & 0xFF).unwrap_or(0), 2, 3, 255])
                .collect(),
        })
    }

    fn state(serial: u32, x: i32, y: i32) -> CursorState {
        CursorState {
            serial,
            image: shape_of(serial),
            x,
            y,
        }
    }

    fn shapes(out: &[CursorMessage]) -> usize {
        out.iter()
            .filter(|m| matches!(m, CursorMessage::Shape(_)))
            .count()
    }

    fn positions(out: &[CursorMessage]) -> Vec<CursorPos> {
        out.iter()
            .filter_map(|m| match m {
                CursorMessage::Moved(p) => Some(*p),
                _ => None,
            })
            .collect()
    }

    /// The first reading says everything, because the far end knows nothing.
    #[test]
    fn the_first_reading_sends_a_shape_and_a_position() {
        let mut t = CursorTracker::new();
        let out = t.update(&state(1, 2000, 400), MONITOR, Instant::now());
        assert_eq!(shapes(&out), 1);
        assert_eq!(positions(&out).len(), 1);
        assert_eq!(t.counts(), (1, 1));
    }

    /// **A shape is sent once.** A pointer that does not change shape must not
    /// keep re-sending its pixels; that is the whole reason the serial is
    /// carried.
    #[test]
    fn an_unchanged_shape_is_never_sent_twice() {
        let mut t = CursorTracker::new();
        let now = Instant::now();
        t.update(&state(1, 2000, 400), MONITOR, now);
        for i in 1..20u32 {
            let later = now + POSITION_INTERVAL * i;
            // Moving each time, so positions flow and only the shape is at
            // issue.
            let out = t.update(&state(1, 2000 + i as i32, 400), MONITOR, later);
            assert_eq!(shapes(&out), 0, "reading {i} re-sent the shape");
        }
        assert_eq!(t.counts().0, 1, "one shape in all");
    }

    /// A new serial is a new shape, and it goes.
    #[test]
    fn a_changed_serial_sends_the_new_shape() {
        let mut t = CursorTracker::new();
        let now = Instant::now();
        t.update(&state(1, 2000, 400), MONITOR, now);
        let out = t.update(&state(2, 2000, 400), MONITOR, now + POSITION_INTERVAL);
        assert_eq!(shapes(&out), 1);
        assert_eq!(t.counts().0, 2);
    }

    /// **Hiding and showing are shape changes even at the same serial.**
    ///
    /// A game hides the pointer and shows it again without the shape's
    /// identity changing, and a controller that was never told would keep
    /// drawing a pointer over a game that has hidden it.
    #[test]
    fn hiding_and_showing_are_sent_even_when_the_serial_does_not_move() {
        let mut t = CursorTracker::new();
        let now = Instant::now();
        t.update(&state(5, 2000, 400), MONITOR, now);
        assert_eq!(t.counts().0, 1);

        let hidden = CursorState {
            serial: 5,
            image: None,
            x: 2000,
            y: 400,
        };
        let out = t.update(&hidden, MONITOR, now + POSITION_INTERVAL);
        assert_eq!(shapes(&out), 1, "the hiding must be told");
        assert!(matches!(
            out.first(),
            Some(CursorMessage::Shape(CursorShape {
                image: CursorImage::Hidden,
                ..
            }))
        ));

        // Still hidden: nothing more to say.
        let out = t.update(&hidden, MONITOR, now + POSITION_INTERVAL * 2);
        assert_eq!(shapes(&out), 0);

        // And back again.
        let out = t.update(&state(5, 2000, 400), MONITOR, now + POSITION_INTERVAL * 3);
        assert_eq!(shapes(&out), 1, "the showing must be told too");
        assert_eq!(t.counts().0, 3);
    }

    /// A pointer sitting still says nothing at all, which on a desktop is most
    /// of the time.
    #[test]
    fn a_still_pointer_says_nothing() {
        let mut t = CursorTracker::new();
        let now = Instant::now();
        t.update(&state(1, 2000, 400), MONITOR, now);
        for i in 1..50u32 {
            let out = t.update(&state(1, 2000, 400), MONITOR, now + POSITION_INTERVAL * i);
            assert!(out.is_empty(), "reading {i} said something about nothing");
        }
        assert_eq!(t.counts(), (1, 1));
    }

    /// Positions are rate-limited: a mouse read faster than the limit does not
    /// put a write on the stream per reading.
    #[test]
    fn positions_are_not_sent_faster_than_the_limit() {
        let mut t = CursorTracker::new();
        let start = Instant::now();
        t.update(&state(1, 2000, 400), MONITOR, start);
        // Twenty readings inside one interval, each at a new place.
        for i in 1..21i32 {
            let out = t.update(
                &state(1, 2000 + i, 400),
                MONITOR,
                start + Duration::from_micros(100) * i as u32,
            );
            assert!(positions(&out).is_empty(), "reading {i} beat the limit");
        }
        // And once the interval has passed, the latest place goes.
        let out = t.update(&state(1, 2100, 400), MONITOR, start + POSITION_INTERVAL);
        assert_eq!(positions(&out).len(), 1);
        assert_eq!(t.counts().1, 2);
    }

    /// **A pointer on another monitor is not in the picture.**
    ///
    /// Clamping it to an edge would draw a pointer pressed against the side of
    /// the screen while the real one is somewhere else entirely.
    #[test]
    fn a_pointer_on_another_monitor_is_not_reported() {
        let mut t = CursorTracker::new();
        let now = Instant::now();
        // Left of this monitor, on the one at the origin.
        let out = t.update(&state(1, 100, 400), MONITOR, now);
        assert!(positions(&out).is_empty(), "not on this monitor");

        // **And the far end is told there is no pointer to draw.**
        //
        // An earlier version sent the real shape here, reasoning that what the
        // pointer looks like is true wherever it is. It is — and it is also
        // useless: the viewer has no other way to know the pointer left, so it
        // went on drawing one at the last position it was given. Moving the
        // mouse to a second screen left a pointer frozen at the edge of the
        // picture, somewhere the person's hand was not, which is worse than no
        // pointer because it looks live.
        assert_eq!(shapes(&out), 1);
        assert!(
            matches!(
                out.first(),
                Some(CursorMessage::Shape(CursorShape {
                    image: CursorImage::Hidden,
                    ..
                }))
            ),
            "a pointer on another screen was announced as visible: {out:?}"
        );

        // Below it, too.
        let out = t.update(
            &state(1, 2000, 300 + 1440),
            MONITOR,
            now + POSITION_INTERVAL,
        );
        assert!(positions(&out).is_empty());
        assert_eq!(
            shapes(&out),
            0,
            "it was already told; saying it again is noise"
        );
    }

    /// **And it comes back.** A pointer that returned and was never announced
    /// again would be a session where the pointer disappears the first time
    /// somebody glances at their other screen and never returns.
    #[test]
    fn a_pointer_that_comes_back_is_drawn_again_without_having_to_move() {
        let mut t = CursorTracker::new();
        let now = Instant::now();

        // On this screen: a shape and a position.
        let out = t.update(&state(1, 2000, 400), MONITOR, now);
        assert_eq!(shapes(&out), 1);
        assert_eq!(positions(&out).len(), 1);

        // Away, and told so.
        let out = t.update(&state(1, 100, 400), MONITOR, now + POSITION_INTERVAL);
        assert!(matches!(
            out.first(),
            Some(CursorMessage::Shape(CursorShape {
                image: CursorImage::Hidden,
                ..
            }))
        ));

        // Back to **exactly where it was**, with the same serial. The shape has
        // to be sent again, because the far end was told there was none — and
        // so does the position, because the far end stopped drawing it.
        let out = t.update(&state(1, 2000, 400), MONITOR, now + POSITION_INTERVAL * 2);
        assert_eq!(shapes(&out), 1, "the pointer never came back: {out:?}");
        assert!(matches!(
            out.first(),
            Some(CursorMessage::Shape(CursorShape {
                image: CursorImage::Bitmap { .. },
                ..
            }))
        ));
        assert_eq!(
            positions(&out).len(),
            1,
            "it reappeared with no position, so nothing would be drawn until it moved"
        );
    }

    /// The corners of the captured monitor are inside it, and the pixel past
    /// each edge is not.
    #[test]
    fn the_edges_of_the_monitor_are_where_they_should_be() {
        let mut t = CursorTracker::new();
        let now = Instant::now();
        let (_, mx, my, w, h) = MONITOR;
        let inside = [
            (mx, my),
            (mx + w as i32 - 1, my),
            (mx, my + h as i32 - 1),
            (mx + w as i32 - 1, my + h as i32 - 1),
        ];
        for (i, (x, y)) in inside.into_iter().enumerate() {
            let out = t.update(
                &state(1, x, y),
                MONITOR,
                now + POSITION_INTERVAL * (i as u32 + 1),
            );
            assert_eq!(positions(&out).len(), 1, "{x},{y} should be on the monitor");
        }
        let outside = [
            (mx - 1, my),
            (mx, my - 1),
            (mx + w as i32, my),
            (mx, my + h as i32),
        ];
        for (i, (x, y)) in outside.into_iter().enumerate() {
            let out = t.update(
                &state(1, x, y),
                MONITOR,
                now + POSITION_INTERVAL * (i as u32 + 10),
            );
            assert!(positions(&out).is_empty(), "{x},{y} should be off it");
        }
    }

    /// A position is a fraction of the monitor it is on, so the far end lands
    /// on the same pixel whatever size the picture is.
    #[test]
    fn a_position_is_relative_to_the_monitor_not_the_desktop() {
        let mut t = CursorTracker::new();
        let (_, mx, my, w, h) = MONITOR;
        // The monitor's own top-left corner.
        let out = t.update(&state(1, mx, my), MONITOR, Instant::now());
        let p = positions(&out).first().copied().expect("a position");
        assert_eq!((p.monitor, p.x, p.y), (1, 0, 0));
        assert_eq!(p.to_pixels(w, h), (0, 0));

        // And its far corner, which must be the far corner of the picture too.
        let mut t = CursorTracker::new();
        let out = t.update(
            &state(1, mx + w as i32 - 1, my + h as i32 - 1),
            MONITOR,
            Instant::now(),
        );
        let p = positions(&out).first().copied().expect("a position");
        assert_eq!((p.x, p.y), (u16::MAX, u16::MAX));
        assert_eq!(p.to_pixels(w, h), (w - 1, h - 1));
        // Including when the picture is scaled, which is the default.
        assert_eq!(p.to_pixels(1920, 1080), (1919, 1079));
    }

    /// **A resumed session starts again.** The far end is new and knows
    /// nothing, so it needs a shape and a position however little has changed
    /// — otherwise it has no pointer until the shape happens to.
    #[test]
    fn forgetting_makes_the_next_reading_say_everything() {
        let mut t = CursorTracker::new();
        let now = Instant::now();
        t.update(&state(1, 2000, 400), MONITOR, now);
        let out = t.update(&state(1, 2000, 400), MONITOR, now + POSITION_INTERVAL);
        assert!(out.is_empty(), "nothing changed");

        t.forget();
        let out = t.update(&state(1, 2000, 400), MONITOR, now + POSITION_INTERVAL * 2);
        assert_eq!(shapes(&out), 1, "the new peer needs the shape");
        assert_eq!(positions(&out).len(), 1, "and the position");
    }

    /// A backend handing over a cursor whose pixels do not match its header is
    /// a backend with a bug, and passing it on would make it the controller's
    /// problem. Dropped — and **not remembered**, so the next good reading of
    /// the same shape is still sent.
    #[test]
    fn a_malformed_shape_is_dropped_and_not_remembered() {
        let mut t = CursorTracker::new();
        let now = Instant::now();
        let broken = CursorState {
            serial: 3,
            image: Some(Captured {
                width: 4,
                height: 4,
                hotspot_x: 0,
                hotspot_y: 0,
                rgba: vec![0; 7],
            }),
            x: 2000,
            y: 400,
        };
        let out = t.update(&broken, MONITOR, now);
        assert_eq!(shapes(&out), 0, "a broken shape must not be passed on");
        assert_eq!(t.counts().0, 0);

        let out = t.update(&state(3, 2000, 400), MONITOR, now + POSITION_INTERVAL);
        assert_eq!(shapes(&out), 1, "the same serial, now well formed, must go");
    }

    /// Everything this produces has to be a message the protocol accepts —
    /// otherwise the far end refuses it and the pointer stops for a reason
    /// reported at the wrong end.
    #[test]
    fn everything_it_produces_is_within_the_protocols_bounds() {
        let mut t = CursorTracker::new();
        let start = Instant::now();
        let mut produced = 0;
        for i in 0..40u32 {
            let s = CursorState {
                serial: i / 3,
                image: if i % 7 == 0 { None } else { shape_of(i) },
                x: 1920 + (i as i32 * 37) % 2560,
                y: 300 + (i as i32 * 53) % 1440,
            };
            for m in t.update(&s, MONITOR, start + POSITION_INTERVAL * i) {
                assert!(m.is_well_formed(), "{m:?}");
                assert!(m.may_be_sent_by(esper_proto::Role::Host));
                produced += 1;
            }
        }
        assert!(
            produced > 20,
            "only {produced} messages — the test is not trying"
        );
    }
}
