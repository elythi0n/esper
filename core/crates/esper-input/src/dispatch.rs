//! Turning a peer's input messages into injection, or refusing to.
//!
//! # This is where permission is settled
//!
//! A sink injects and asks no questions — every backend would otherwise have
//! to implement the check, and one of them eventually would not. So the check
//! lives here, once, above all of them: **a device without `control` gets
//! nothing injected, however well-formed its messages are.**
//!
//! # And where a monitor becomes a place
//!
//! A pointer message names a monitor and a fraction of it. Only the host knows
//! what monitors it has and where they are, so this holds that table and turns
//! the pair into a point on the desktop. A monitor the host does not have is
//! refused by name rather than clamped to one it does — a pointer that
//! silently landed on the wrong screen would be worse than one that did not
//! move.

use esper_proto::InputMessage;

use crate::{Held, InputError, InputSink, Point};

/// One monitor, as the host knows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Monitor {
    pub id: u32,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

/// Applies a controller's input to a sink, or does not.
#[derive(Debug)]
pub struct Dispatcher {
    sink: Box<dyn InputSink>,
    monitors: Vec<Monitor>,
    /// Whether this peer holds `control`.
    allowed: bool,
    injected: u64,
    refused: u64,
}

impl Dispatcher {
    /// A dispatcher for a peer that may or may not drive this machine.
    #[must_use]
    pub fn new(sink: Box<dyn InputSink>, monitors: Vec<Monitor>, allowed: bool) -> Self {
        Self {
            sink,
            monitors,
            allowed,
            injected: 0,
            refused: 0,
        }
    }

    /// Whether this peer may drive the machine.
    #[must_use]
    pub const fn allowed(&self) -> bool {
        self.allowed
    }

    /// How many messages were injected, and how many refused.
    #[must_use]
    pub const fn counts(&self) -> (u64, u64) {
        (self.injected, self.refused)
    }

    /// What the sink is holding.
    #[must_use]
    pub fn held(&self) -> &Held {
        self.sink.held()
    }

    /// Which backend is injecting, for the log and the session indicator.
    #[must_use]
    pub fn backend(&self) -> &'static str {
        self.sink.backend()
    }

    /// Replace the monitor table, after the host's screens changed.
    pub fn set_monitors(&mut self, monitors: Vec<Monitor>) {
        self.monitors = monitors;
    }

    /// Apply one message.
    ///
    /// **A refusal is not an error of the session's.** A device without
    /// permission, a monitor that does not exist, a key this platform has no
    /// position for: all of them are counted and reported, and the session
    /// carries on. Dropping a session over one bad input message would let any
    /// controller end its own session by sending one — and worse, would end a
    /// legitimate one over a key that happens not to exist here.
    pub fn apply(&mut self, message: &InputMessage) -> Result<(), InputError> {
        if !self.allowed {
            self.refused = self.refused.saturating_add(1);
            return Err(InputError::Backend {
                backend: self.sink.backend(),
                detail: "this device does not hold control".to_owned(),
            });
        }
        if !message.is_well_formed() {
            self.refused = self.refused.saturating_add(1);
            return Err(InputError::Backend {
                backend: self.sink.backend(),
                detail: format!("{} is outside its bounds", message.name()),
            });
        }

        let result = match message {
            InputMessage::Key(k) => self.sink.key(*k),
            InputMessage::Button(b) => self.sink.button(*b),
            InputMessage::Scroll(s) => self.sink.scroll(*s),
            InputMessage::ReleaseAll => self.sink.release_all(),
            // **Nothing to inject, and that is the point.** A heartbeat's
            // whole job is to have arrived: it is counted like anything else
            // so a session can say the controller was still there, and the
            // silence timer that releases held keys is reset by any message,
            // this one included.
            InputMessage::Holding => Ok(()),
            InputMessage::Pointer(p) => match self.monitors.iter().find(|m| m.id == p.monitor) {
                Some(m) => {
                    let (dx, dy) = p.to_pixels(m.width, m.height);
                    self.sink.pointer(Point {
                        x: m.x.saturating_add(i32::try_from(dx).unwrap_or(i32::MAX)),
                        y: m.y.saturating_add(i32::try_from(dy).unwrap_or(i32::MAX)),
                    })
                }
                None => Err(InputError::NoSuchMonitor {
                    monitor: p.monitor,
                    count: self.monitors.len(),
                }),
            },
        };
        match result {
            Ok(()) => {
                self.injected = self.injected.saturating_add(1);
                Ok(())
            }
            Err(e) => {
                self.refused = self.refused.saturating_add(1);
                Err(e)
            }
        }
    }

    /// Let go of everything, whatever happened.
    ///
    /// Called on every way a session can end. Returns whether anything was
    /// still held, so a caller can say so — a session that ended with keys
    /// down is worth a line in the log even when the release worked.
    pub fn release_all(&mut self) -> bool {
        let was_holding = !self.sink.held().is_empty();
        if let Err(e) = self.sink.release_all() {
            tracing::error!("could not release held input: {e}");
        }
        was_holding
    }
}

impl Drop for Dispatcher {
    fn drop(&mut self) {
        if !self.sink.held().is_empty() {
            let _ = self.sink.release_all();
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;
    use crate::{NullSink, Recorded};
    use esper_proto::{Button, KeyEvent, PointerButton, PointerMove, Scroll};

    fn monitors() -> Vec<Monitor> {
        vec![
            Monitor {
                id: 0,
                x: 0,
                y: 0,
                width: 1920,
                height: 1080,
            },
            // Off the origin, which is where the arithmetic goes wrong.
            Monitor {
                id: 1,
                x: 1920,
                y: 300,
                width: 2560,
                height: 1440,
            },
        ]
    }

    fn dispatcher(allowed: bool) -> Dispatcher {
        Dispatcher::new(Box::new(NullSink::new()), monitors(), allowed)
    }

    /// **A device without `control` gets nothing injected.** Not a clamped
    /// pointer, not a harmless key: nothing.
    #[test]
    fn a_device_without_control_injects_nothing() {
        let mut d = dispatcher(false);
        let messages = [
            InputMessage::Key(KeyEvent {
                usage: 0x04,
                pressed: true,
            }),
            InputMessage::Pointer(PointerMove {
                monitor: 0,
                x: 0,
                y: 0,
            }),
            InputMessage::Button(PointerButton {
                button: Button::Left,
                pressed: true,
            }),
            InputMessage::ReleaseAll,
        ];
        for m in &messages {
            assert!(d.apply(m).is_err(), "{} must be refused", m.name());
        }
        assert_eq!(d.counts(), (0, 4));
        assert!(d.held().is_empty());
    }

    /// A pointer on a monitor away from the origin lands on that monitor, not
    /// at the same offset on the first one.
    #[test]
    fn a_pointer_lands_on_the_monitor_it_names() {
        let mut d = dispatcher(true);
        // The middle of the second monitor.
        d.apply(&InputMessage::Pointer(PointerMove {
            monitor: 1,
            x: u16::MAX / 2,
            y: u16::MAX / 2,
        }))
        .expect("injected");
        // Its top-left corner.
        d.apply(&InputMessage::Pointer(PointerMove {
            monitor: 1,
            x: 0,
            y: 0,
        }))
        .expect("injected");
        // And the far corner of the first.
        d.apply(&InputMessage::Pointer(PointerMove {
            monitor: 0,
            x: u16::MAX,
            y: u16::MAX,
        }))
        .expect("injected");

        // The sink is behind a box, so what it recorded is read back through
        // the dispatcher's own counts and the positions it was handed.
        assert_eq!(d.counts(), (3, 0));
    }

    #[test]
    fn a_pointer_on_a_monitor_that_does_not_exist_is_refused() {
        let mut d = dispatcher(true);
        let e = d
            .apply(&InputMessage::Pointer(PointerMove {
                monitor: 9,
                x: 0,
                y: 0,
            }))
            .expect_err("there is no monitor 9");
        assert!(
            matches!(
                e,
                InputError::NoSuchMonitor {
                    monitor: 9,
                    count: 2
                }
            ),
            "{e:?}"
        );
        assert_eq!(d.counts(), (0, 1));
    }

    /// A message outside its bounds is refused before it reaches a platform's
    /// injection API.
    #[test]
    fn a_malformed_message_never_reaches_the_sink() {
        let mut d = dispatcher(true);
        assert!(
            d.apply(&InputMessage::Key(KeyEvent {
                usage: 0x02,
                pressed: true
            }))
            .is_err()
        );
        assert!(
            d.apply(&InputMessage::Scroll(Scroll {
                dx_notches: 0,
                dy_notches: 30_000,
                dx_pixels: None,
                dy_pixels: None,
            }))
            .is_err()
        );
        assert_eq!(d.counts(), (0, 2));
    }

    /// **A refusal does not end the session.** Otherwise a controller could
    /// end its own session with one bad message, and a legitimate session
    /// would die on a key this platform happens not to have.
    #[test]
    fn a_refusal_leaves_the_dispatcher_working() {
        let mut d = dispatcher(true);
        let _ = d.apply(&InputMessage::Pointer(PointerMove {
            monitor: 9,
            x: 0,
            y: 0,
        }));
        d.apply(&InputMessage::Key(KeyEvent {
            usage: 0x04,
            pressed: true,
        }))
        .expect("the next message still works");
        assert_eq!(d.counts(), (1, 1));
    }

    /// A heartbeat injects nothing and is still accepted — its arrival is the
    /// whole message, and the caller's silence timer is what acts on it.
    #[test]
    fn a_heartbeat_is_accepted_and_injects_nothing() {
        let mut d = dispatcher(true);
        d.apply(&InputMessage::Holding).expect("accepted");
        assert_eq!(d.counts(), (1, 0));
        assert!(d.held().is_empty());
    }

    /// And a device without control does not even get to say it is holding
    /// something.
    #[test]
    fn a_heartbeat_from_a_device_without_control_is_refused() {
        let mut d = dispatcher(false);
        assert!(d.apply(&InputMessage::Holding).is_err());
    }

    #[test]
    fn releasing_says_whether_anything_was_held() {
        let mut d = dispatcher(true);
        assert!(!d.release_all(), "nothing was held");
        d.apply(&InputMessage::Key(KeyEvent {
            usage: 0xE0,
            pressed: true,
        }))
        .expect("injected");
        assert_eq!(d.held().count(), 1);
        assert!(d.release_all(), "something was");
        assert!(d.held().is_empty());
    }

    /// The monitor table can be replaced, because a host's screens change and
    /// a stale table would put the pointer on a monitor that has moved.
    #[test]
    fn the_monitor_table_can_be_replaced() {
        let mut d = dispatcher(true);
        d.set_monitors(vec![Monitor {
            id: 7,
            x: 0,
            y: 0,
            width: 800,
            height: 600,
        }]);
        assert!(
            d.apply(&InputMessage::Pointer(PointerMove {
                monitor: 0,
                x: 0,
                y: 0
            }))
            .is_err(),
            "the old monitor is gone"
        );
        d.apply(&InputMessage::Pointer(PointerMove {
            monitor: 7,
            x: 0,
            y: 0,
        }))
        .expect("the new one is there");
    }

    /// What a sink was actually asked to do, checked directly rather than
    /// through the dispatcher's counts.
    #[test]
    fn the_sink_is_asked_for_the_right_point() {
        let mut sink = NullSink::new();
        let m = Monitor {
            id: 1,
            x: 1920,
            y: 300,
            width: 2560,
            height: 1440,
        };
        let position = PointerMove {
            monitor: 1,
            x: u16::MAX,
            y: 0,
        };
        let (dx, dy) = position.to_pixels(m.width, m.height);
        crate::InputSink::pointer(
            &mut sink,
            Point {
                x: m.x + i32::try_from(dx).unwrap(),
                y: m.y + i32::try_from(dy).unwrap(),
            },
        )
        .expect("injected");
        assert_eq!(
            sink.asked.first(),
            Some(&Recorded::Pointer(Point {
                x: 1920 + 2559,
                y: 300
            })),
            "the right edge of the second monitor is 1920 + 2559"
        );
    }
}
