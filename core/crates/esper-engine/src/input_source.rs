//! Turning what the viewer's window saw into input messages.
//!
//! # Where the translation belongs
//!
//! The presenter reports what its window was told, in the window's own
//! coordinates and the platform's own key codes. This turns that into the
//! protocol's physical positions and monitor fractions, because it is the only
//! place that knows both: the presenter does not know which monitor is on
//! screen, and the protocol does not know what an X11 keycode is.
//!
//! # What it refuses to send
//!
//! A key this platform has that HID does not name, and a pointer position
//! outside the picture. Both are dropped rather than approximated: a keystroke
//! the far end would interpret as a different key is worse than one that does
//! not arrive, and a click a pixel outside a window is not a click on the
//! screen's edge.

use std::time::{Duration, Instant};

use esper_input::{Held, hid};
use esper_present::PresentEvent;
use esper_proto::{Button, CursorPos, InputMessage, KeyEvent, PointerButton, PointerMove, Scroll};

/// What Xorg's input drivers add to an evdev code to get a keycode, undone.
///
/// The same convention the injecting side probes, in the other direction. A
/// keycode below the offset is not an evdev key at all.
const KEYCODE_OFFSET: u32 = 8;

/// How long the pointer must be still before the host's position is compared
/// against it.
///
/// Two hundred milliseconds — comfortably longer than any round trip a session
/// is usable over, so the host's report has caught up before it is judged.
pub const SETTLED: Duration = Duration::from_millis(200);

/// How far apart the two positions may be and still be the same place.
///
/// In monitor fractions, where the whole edge is 65 535. Five hundred is about
/// fifteen pixels on a 1920-wide screen: far more than the rounding between two
/// sizes can produce, and far less than a grab moves a pointer.
pub const TOLERANCE: i32 = 500;

/// How often a controller says it is still holding something.
///
/// A second. The host lets go after a few seconds of silence, so this has to
/// be well inside that with room for a lost packet — and it costs a handful of
/// bytes, only while something is actually held.
pub const HEARTBEAT: Duration = Duration::from_secs(1);

/// Translates a window's events for one session.
#[derive(Debug)]
pub struct InputSource {
    /// The monitor the picture is of, which every pointer position is a
    /// fraction of.
    monitor: u32,
    /// The picture's size, which is the window's size too.
    size: (u32, u32),
    /// Dropped events, counted so a session can say it happened rather than
    /// logging one line per keystroke.
    dropped: u64,
    /// **What this side believes the host is holding.**
    ///
    /// Tracked here because this is where the presses are turned into
    /// messages, so nothing else has to be told. It answers one question: is
    /// there anything the host would be left holding if this process
    /// disappeared right now.
    holding: Held,
    /// When the last heartbeat went out.
    said: Option<Instant>,
    /// The last position asked for, and when.
    ///
    /// Kept to answer one question: **is the host's pointer following mine?**
    /// If it is not, a pointer drawn under the person's hand is a lie, and the
    /// host's own position has to be drawn instead.
    asked: Option<(PointerMove, Instant)>,
}

impl InputSource {
    /// A source for a picture of this monitor at this size.
    #[must_use]
    pub const fn new(monitor: u32, size: (u32, u32)) -> Self {
        Self {
            monitor,
            size,
            dropped: 0,
            holding: Held::new(),
            said: None,
            asked: None,
        }
    }

    /// Whether the host is holding anything on this session's behalf.
    #[must_use]
    pub fn is_holding(&self) -> bool {
        !self.holding.is_empty()
    }

    /// A heartbeat, if one is due.
    ///
    /// `None` when nothing is held — a session where nobody is touching the
    /// keyboard says nothing at all — and `None` again until the interval has
    /// passed.
    pub fn heartbeat(&mut self, now: Instant) -> Option<InputMessage> {
        if self.holding.is_empty() {
            self.said = None;
            return None;
        }
        let due = self
            .said
            .is_none_or(|then| now.saturating_duration_since(then) >= HEARTBEAT);
        if !due {
            return None;
        }
        self.said = Some(now);
        Some(InputMessage::Holding)
    }

    /// The picture changed shape or monitor, so positions mean something else.
    pub const fn showing(&mut self, monitor: u32, size: (u32, u32)) {
        self.monitor = monitor;
        self.size = size;
    }

    /// How many events could not be translated.
    #[must_use]
    pub const fn dropped(&self) -> u64 {
        self.dropped
    }

    /// One window event as a protocol message, if it is one.
    pub fn translate(&mut self, event: &PresentEvent) -> Option<InputMessage> {
        match event {
            PresentEvent::Key { code, pressed } => {
                let Some(usage) = self.usage_of(*code) else {
                    self.dropped = self.dropped.saturating_add(1);
                    return None;
                };
                self.holding.key(usage, *pressed);
                Some(InputMessage::Key(KeyEvent {
                    usage,
                    pressed: *pressed,
                }))
            }
            PresentEvent::Pointer { x, y } => {
                // Outside the picture is dropped, not clamped: a click a pixel
                // past the edge is not a click on the edge, and a session that
                // clamped would put presses on a scrollbar the person was
                // moving away from.
                let (w, h) = self.size;
                if *x < 0
                    || *y < 0
                    || *x >= i32::try_from(w).unwrap_or(i32::MAX)
                    || *y >= i32::try_from(h).unwrap_or(i32::MAX)
                {
                    self.dropped = self.dropped.saturating_add(1);
                    return None;
                }
                let asked = PointerMove::from_pixels(self.monitor, *x, *y, w, h);
                self.asked = Some((asked, Instant::now()));
                Some(InputMessage::Pointer(asked))
            }
            PresentEvent::Button { code, pressed } => {
                let Some(button) = button_of(*code) else {
                    self.dropped = self.dropped.saturating_add(1);
                    return None;
                };
                self.holding.button(button, *pressed);
                Some(InputMessage::Button(PointerButton {
                    button,
                    pressed: *pressed,
                }))
            }
            PresentEvent::Scroll { dx, dy } => Some(InputMessage::Scroll(Scroll {
                dx_notches: *dx,
                dy_notches: *dy,
                // **This source has no pixels to offer.** X11 reports the
                // wheel as a button, so the notch *is* the resolution, and
                // inventing a pixel count from it would be inventing
                // precision the person's hardware did not provide.
                dx_pixels: None,
                dy_pixels: None,
            })),
            // **The whole reason focus is reported.** Somebody who alt-tabs
            // away mid-keystroke has released nothing, and without this the
            // host holds the key for ever.
            PresentEvent::FocusLost => {
                self.holding.clear();
                Some(InputMessage::ReleaseAll)
            }
            PresentEvent::CloseRequested | PresentEvent::Resized { .. } | PresentEvent::Exposed => {
                None
            }
        }
    }

    /// Whether the host's pointer has stopped following this one.
    ///
    /// # Why this waits for the pointer to settle
    ///
    /// The host's report of where its pointer is arrives a round trip after
    /// the position that put it there. **During movement the two always
    /// disagree** — that is the latency this whole design exists to hide — so
    /// comparing them while the mouse is moving would call every gesture a
    /// divergence and switch to a drawn pointer for the duration.
    ///
    /// So the question is only asked once nothing has been sent for
    /// [`SETTLED`]. A difference that survives the pointer standing still is
    /// not latency: something on the host is holding the pointer — a game, a
    /// drawing tool, anything with a grab — and the local pointer is no longer
    /// where the host's is.
    ///
    /// `false` when nothing has been asked for yet: a session that has not
    /// moved the pointer has nothing to disagree with.
    #[must_use]
    pub fn host_is_not_following(&self, reported: CursorPos, now: Instant) -> bool {
        let Some((asked, when)) = self.asked else {
            return false;
        };
        if now.saturating_duration_since(when) < SETTLED {
            return false;
        }
        if reported.monitor != asked.monitor {
            return true;
        }
        let dx = i32::from(reported.x) - i32::from(asked.x);
        let dy = i32::from(reported.y) - i32::from(asked.y);
        dx.abs() > TOLERANCE || dy.abs() > TOLERANCE
    }

    /// The HID usage for one of this platform's key codes.
    fn usage_of(&self, code: u32) -> Option<u16> {
        let evdev = code.checked_sub(KEYCODE_OFFSET)?;
        hid::usage_for_evdev(hid::Evdev(u16::try_from(evdev).ok()?))
    }
}

/// The protocol's button for one of X11's numbers.
///
/// 4 to 7 are the wheel and never reach here — the presenter turns them into
/// scrolls, because a wheel forwarded as a button would be a middle click on
/// the host.
fn button_of(code: u32) -> Option<Button> {
    match code {
        1 => Some(Button::Left),
        2 => Some(Button::Middle),
        3 => Some(Button::Right),
        8 => Some(Button::Back),
        9 => Some(Button::Forward),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    fn source() -> InputSource {
        InputSource::new(0, (1920, 1080))
    }

    /// **The two tables meet here**, and a mistake in either direction is a
    /// key that types the wrong thing. `a` is keycode 38 on an X server with
    /// the usual offset, and usage 0x04.
    #[test]
    fn a_platform_keycode_becomes_the_usage_for_that_position() {
        let mut s = source();
        let m = s
            .translate(&PresentEvent::Key {
                code: 38,
                pressed: true,
            })
            .expect("a key message");
        assert_eq!(
            m,
            InputMessage::Key(KeyEvent {
                usage: 0x04,
                pressed: true
            })
        );
    }

    #[test]
    fn a_keycode_this_platform_has_and_hid_does_not_name_is_dropped() {
        let mut s = source();
        // Keycode 8 is evdev 0, which is not a key.
        assert!(
            s.translate(&PresentEvent::Key {
                code: 8,
                pressed: true
            })
            .is_none()
        );
        // And below the offset is not an evdev key at all.
        assert!(
            s.translate(&PresentEvent::Key {
                code: 3,
                pressed: true
            })
            .is_none()
        );
        assert_eq!(s.dropped(), 2);
    }

    #[test]
    fn a_pointer_in_the_window_becomes_a_fraction_of_the_monitor() {
        let mut s = source();
        let m = s
            .translate(&PresentEvent::Pointer { x: 0, y: 0 })
            .expect("a pointer message");
        assert_eq!(
            m,
            InputMessage::Pointer(PointerMove {
                monitor: 0,
                x: 0,
                y: 0
            })
        );
        let far = s
            .translate(&PresentEvent::Pointer { x: 1919, y: 1079 })
            .expect("a pointer message");
        assert_eq!(
            far,
            InputMessage::Pointer(PointerMove {
                monitor: 0,
                x: u16::MAX,
                y: u16::MAX
            })
        );
    }

    /// Outside the picture is dropped, not clamped: a click a pixel past the
    /// edge is not a click on the edge.
    #[test]
    fn a_pointer_outside_the_picture_is_dropped() {
        let mut s = source();
        for (x, y) in [(-1, 0), (0, -1), (1920, 0), (0, 1080), (9999, 9999)] {
            assert!(
                s.translate(&PresentEvent::Pointer { x, y }).is_none(),
                "{x},{y} is outside a 1920x1080 picture"
            );
        }
        assert_eq!(s.dropped(), 5);
    }

    /// After a monitor switch the same window position means a fraction of a
    /// different screen — which is the whole point of carrying the monitor id.
    #[test]
    fn a_switch_changes_what_a_position_means() {
        let mut s = source();
        s.showing(2, (1280, 720));
        let m = s
            .translate(&PresentEvent::Pointer { x: 1279, y: 719 })
            .expect("a pointer message");
        assert_eq!(
            m,
            InputMessage::Pointer(PointerMove {
                monitor: 2,
                x: u16::MAX,
                y: u16::MAX
            })
        );
    }

    #[test]
    fn the_buttons_map_to_their_names() {
        let mut s = source();
        for (code, want) in [
            (1u32, Button::Left),
            (2, Button::Middle),
            (3, Button::Right),
            (8, Button::Back),
            (9, Button::Forward),
        ] {
            let m = s
                .translate(&PresentEvent::Button {
                    code,
                    pressed: true,
                })
                .expect("a button message");
            assert_eq!(
                m,
                InputMessage::Button(PointerButton {
                    button: want,
                    pressed: true
                }),
                "X11 button {code}"
            );
        }
        // A button nothing names is dropped rather than guessed at.
        assert!(
            s.translate(&PresentEvent::Button {
                code: 12,
                pressed: true
            })
            .is_none()
        );
    }

    #[test]
    fn a_scroll_carries_notches_and_no_invented_pixels() {
        let mut s = source();
        let m = s
            .translate(&PresentEvent::Scroll { dx: 0, dy: -1 })
            .expect("a scroll message");
        assert_eq!(
            m,
            InputMessage::Scroll(Scroll {
                dx_notches: 0,
                dy_notches: -1,
                dx_pixels: None,
                dy_pixels: None,
            })
        );
    }

    /// **Losing the focus releases everything.** Without it, a person who
    /// alt-tabs away mid-keystroke leaves the key held on somebody else's
    /// machine.
    #[test]
    fn losing_the_focus_asks_for_everything_to_be_released() {
        let mut s = source();
        assert_eq!(
            s.translate(&PresentEvent::FocusLost),
            Some(InputMessage::ReleaseAll)
        );
    }

    /// **A session with nobody touching the keyboard says nothing.**
    #[test]
    fn nothing_held_means_no_heartbeat() {
        let mut s = source();
        let now = Instant::now();
        assert!(!s.is_holding());
        assert_eq!(s.heartbeat(now), None);
        assert_eq!(s.heartbeat(now + Duration::from_secs(10)), None);
    }

    /// **Something held is said about once a second**, which is what lets the
    /// host let go if this process disappears.
    #[test]
    fn something_held_is_said_about_once_a_second() {
        let mut s = source();
        s.translate(&PresentEvent::Key {
            code: 37,
            pressed: true,
        })
        .expect("ctrl down");
        assert!(s.is_holding());

        let start = Instant::now();
        assert_eq!(s.heartbeat(start), Some(InputMessage::Holding), "at once");
        assert_eq!(
            s.heartbeat(start + Duration::from_millis(300)),
            None,
            "and not again immediately"
        );
        assert_eq!(
            s.heartbeat(start + HEARTBEAT),
            Some(InputMessage::Holding),
            "and again after the interval"
        );
    }

    /// Releasing the last thing held stops the heartbeat.
    #[test]
    fn releasing_everything_stops_the_heartbeat() {
        let mut s = source();
        s.translate(&PresentEvent::Key {
            code: 37,
            pressed: true,
        })
        .expect("down");
        assert!(s.is_holding());
        s.translate(&PresentEvent::Key {
            code: 37,
            pressed: false,
        })
        .expect("up");
        assert!(!s.is_holding());
        assert_eq!(s.heartbeat(Instant::now()), None);
    }

    /// Losing the focus is the other way it stops: the host is told to release
    /// everything, so this side is holding nothing.
    #[test]
    fn losing_the_focus_stops_the_heartbeat_too() {
        let mut s = source();
        s.translate(&PresentEvent::Button {
            code: 1,
            pressed: true,
        })
        .expect("button down");
        assert!(s.is_holding());
        assert_eq!(
            s.translate(&PresentEvent::FocusLost),
            Some(InputMessage::ReleaseAll)
        );
        assert!(!s.is_holding(), "nothing is held after a release-all");
    }

    /// A repeat while a key is held must not make the release insufficient —
    /// the same rule the injecting side keeps, checked from this end too.
    #[test]
    fn a_repeat_does_not_need_two_releases_to_stop_holding() {
        let mut s = source();
        for _ in 0..5 {
            s.translate(&PresentEvent::Key {
                code: 38,
                pressed: true,
            })
            .expect("repeat");
        }
        s.translate(&PresentEvent::Key {
            code: 38,
            pressed: false,
        })
        .expect("up");
        assert!(!s.is_holding());
    }

    /// **A moving pointer is not a diverging one.** The host's report arrives
    /// a round trip late, so comparing during movement would call every
    /// gesture a divergence — which is why the comparison waits for the
    /// pointer to settle.
    #[test]
    fn a_pointer_still_in_motion_is_never_called_divergent() {
        let mut s = source();
        s.translate(&PresentEvent::Pointer { x: 100, y: 100 })
            .expect("a move");
        let now = Instant::now();
        // The host reports somewhere completely different, as it would while
        // a fast gesture is in flight.
        let stale = CursorPos {
            monitor: 0,
            x: 0,
            y: 0,
        };
        assert!(
            !s.host_is_not_following(stale, now),
            "a report arriving mid-gesture is latency, not a grab"
        );
        assert!(
            !s.host_is_not_following(stale, now + SETTLED / 2),
            "and still is, halfway to settled"
        );
    }

    /// Once the pointer has settled, a difference that survives is a host that
    /// is not following.
    #[test]
    fn a_settled_pointer_the_host_ignores_is_divergent() {
        let mut s = source();
        s.translate(&PresentEvent::Pointer { x: 960, y: 540 })
            .expect("a move");
        let later = Instant::now() + SETTLED + Duration::from_millis(50);
        let elsewhere = CursorPos {
            monitor: 0,
            x: 0,
            y: 0,
        };
        assert!(s.host_is_not_following(elsewhere, later));
    }

    /// And a host that *is* following is not divergent, even with the
    /// rounding between two different sizes in the way.
    #[test]
    fn a_host_that_followed_is_not_divergent() {
        let mut s = source();
        let Some(InputMessage::Pointer(asked)) =
            s.translate(&PresentEvent::Pointer { x: 960, y: 540 })
        else {
            panic!("a pointer message");
        };
        let later = Instant::now() + SETTLED + Duration::from_millis(50);
        // Exactly where it was asked to be.
        let echo = CursorPos {
            monitor: asked.monitor,
            x: asked.x,
            y: asked.y,
        };
        assert!(!s.host_is_not_following(echo, later));

        // And a few units off, which is what converting through two different
        // monitor sizes produces.
        let rounded = CursorPos {
            monitor: asked.monitor,
            x: asked.x.saturating_add(40),
            y: asked.y.saturating_sub(40),
        };
        assert!(
            !s.host_is_not_following(rounded, later),
            "rounding between sizes must not look like a grab"
        );
    }

    /// A pointer that has moved to another monitor is not following this one,
    /// whatever the numbers say.
    #[test]
    fn a_pointer_on_another_monitor_is_not_following() {
        let mut s = source();
        s.translate(&PresentEvent::Pointer { x: 10, y: 10 })
            .expect("a move");
        let later = Instant::now() + SETTLED * 2;
        assert!(s.host_is_not_following(
            CursorPos {
                monitor: 3,
                x: 0,
                y: 0
            },
            later
        ));
    }

    /// **Nothing asked for, nothing to disagree with.** A session that has
    /// never moved the pointer must not decide the host is holding it.
    #[test]
    fn a_session_that_has_not_moved_the_pointer_is_not_divergent() {
        let s = source();
        assert!(!s.host_is_not_following(
            CursorPos {
                monitor: 0,
                x: 12345,
                y: 54321
            },
            Instant::now() + SETTLED * 10
        ));
    }

    #[test]
    fn window_events_that_are_not_input_produce_nothing() {
        let mut s = source();
        for event in [
            PresentEvent::CloseRequested,
            PresentEvent::Exposed,
            PresentEvent::Resized {
                width: 10,
                height: 10,
            },
        ] {
            assert!(s.translate(&event).is_none());
        }
        assert_eq!(s.dropped(), 0, "these are not dropped input");
    }

    /// Every message this source produces has to be one the protocol accepts,
    /// whatever the window said — otherwise the far end refuses it and the
    /// keystroke is lost with a complaint at the wrong end.
    #[test]
    fn everything_it_produces_is_within_the_protocols_bounds() {
        let mut s = source();
        let mut produced = 0;
        for code in 0u32..256 {
            for pressed in [true, false] {
                if let Some(m) = s.translate(&PresentEvent::Key { code, pressed }) {
                    assert!(m.is_well_formed(), "keycode {code} produced {m:?}");
                    produced += 1;
                }
            }
        }
        assert!(produced > 100, "only {produced} keycodes translated");
        for x in [0, 1, 959, 1919] {
            for y in [0, 1, 539, 1079] {
                let m = s
                    .translate(&PresentEvent::Pointer { x, y })
                    .expect("in the picture");
                assert!(m.is_well_formed());
            }
        }
    }
}
