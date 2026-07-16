//! Input: keys, pointer, buttons and scroll, on their way to a host's screen.
//!
//! # A key is a position, not a character
//!
//! Every key event here carries a **USB HID usage from page 0x07** — where the
//! key *is*, not what it would type. The host applies its own layout.
//!
//! Sending characters is the classic remote desktop bug: a Finnish keyboard
//! driving a US-layout host produces the wrong symbols, and no amount of care
//! on either side can fix it, because a character is the *result* of a layout
//! and the layout that matters belongs to the machine being controlled. The
//! numbering is settled in a decision record; nothing else in the product
//! knows another one exists.
//!
//! # Its own stream, for a reason
//!
//! Input travels on a control stream of its own rather than sharing the media
//! session's. A stream is ordered, so a key event queued behind a
//! [`crate::StreamConfig`] carrying a monitor list would wait for it — and
//! input is the most latency-sensitive traffic in the protocol. Sharing the
//! stream would mean a keystroke occasionally arriving a round trip late for
//! no reason a person could understand.
//!
//! # Every field has a range
//!
//! A peer with `control` can move a pointer and press keys; that is what the
//! permission means. It cannot send a *nonsensical* one — a scroll of thirty
//! thousand notches, a pointer position outside the screen, a key usage that
//! names nothing. Those are refused by name, because a message no hand could
//! produce is either a broken implementation or somebody trying something, and
//! both are worth telling apart from ordinary use.

use serde::{Deserialize, Serialize};

use crate::Role;

/// The largest usage this protocol carries, from HID page 0x07.
///
/// 0xE7 is Right GUI, the last key the page defines. Above that is unassigned,
/// and a usage nothing names is refused rather than passed to a platform table
/// that would have to decide what to do with it.
pub const MAX_KEY_USAGE: u16 = 0xE7;

/// The smallest usage that names a key. 0x00 to 0x03 are error and reserved
/// codes a keyboard reports about *itself*, never a key that was pressed.
pub const MIN_KEY_USAGE: u16 = 0x04;

/// A pointer position is a fraction of a monitor, in this many steps.
///
/// **Not a float.** A float on the wire rounds differently in two languages,
/// which for a pointer means a cursor that lands a pixel away depending on
/// which implementation sent it. 65 536 steps is finer than a pixel on any
/// display that exists: across a 7 680-pixel edge it is a ninth of a pixel.
pub const POINTER_SCALE: u32 = u16::MAX as u32;

/// The most notches one scroll message may carry.
///
/// A wheel detent is a physical click and a human hand produces a handful at a
/// time. The bound is generous — a fast flick on a free-spinning wheel — and
/// its purpose is that *some* number is out of range: without one, a single
/// message could ask for a scroll no input device could generate.
pub const MAX_NOTCHES: i16 = 64;

/// The most pixels of scroll one message may carry.
///
/// A high-resolution wheel or a touchpad reports pixels, and a fast flick is a
/// few hundred. Bounded at a large display's height, since a single event
/// asking for more than a screenful is not a gesture.
pub const MAX_SCROLL_PIXELS: i32 = 4320;

/// A key going down or coming up.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyEvent {
    /// The USB HID usage from page 0x07 — **where the key is**.
    pub usage: u16,
    /// Whether it is now down.
    pub pressed: bool,
}

impl KeyEvent {
    /// Whether this names a key at all.
    #[must_use]
    pub const fn is_well_formed(&self) -> bool {
        self.usage >= MIN_KEY_USAGE && self.usage <= MAX_KEY_USAGE
    }
}

/// Where the pointer is, as a fraction of one monitor.
///
/// **Absolute and per-monitor, not relative.** A relative protocol makes the
/// host's pointer drift away from the controller's over a session — every
/// dropped or clamped movement is permanent — and there is nothing to
/// resynchronise it with. Per-monitor rather than desktop-wide because a
/// controller is looking at one monitor's pixels and knows nothing about how
/// the host arranges the rest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PointerMove {
    /// Which monitor the position is on.
    pub monitor: u32,
    /// Across, in [`POINTER_SCALE`] steps.
    pub x: u16,
    /// Down, in [`POINTER_SCALE`] steps.
    pub y: u16,
}

impl PointerMove {
    /// A position from pixels on a monitor of a known size.
    ///
    /// Saturating rather than wrapping: a coordinate outside the monitor is
    /// clamped to its edge, which is what a pointer at the edge of a window
    /// means, rather than appearing on the opposite side.
    #[must_use]
    pub fn from_pixels(monitor: u32, x: i32, y: i32, width: u32, height: u32) -> Self {
        Self {
            monitor,
            x: fraction(x, width),
            y: fraction(y, height),
        }
    }

    /// Back to pixels on a monitor of a known size.
    ///
    /// The last pixel is reachable: a fraction of 65 535 lands on `width - 1`
    /// rather than on `width`, which would be off the screen.
    #[must_use]
    pub fn to_pixels(&self, width: u32, height: u32) -> (u32, u32) {
        (pixel(self.x, width), pixel(self.y, height))
    }
}

/// Which button.
///
/// Named rather than numbered: X11 button 2 is the middle button and Windows
/// button 2 is the right one, and a protocol carrying a platform's numbering
/// is a protocol that swaps buttons between platforms.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Button {
    Left,
    Middle,
    Right,
    /// The side button that usually means "back".
    Back,
    /// The side button that usually means "forward".
    Forward,
}

/// A button going down or coming up.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PointerButton {
    pub button: Button,
    pub pressed: bool,
}

/// A scroll, in both units.
///
/// **Notches are mandatory and pixels are not.** A host that can only click a
/// wheel — X11's XTEST can do nothing else — must never be handed a message it
/// cannot act on, so the coarse unit is always present and a controller with a
/// pixel-precise device accumulates its own notches. A host that *can* scroll
/// by pixels uses them and gets the smooth version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Scroll {
    /// Horizontal notches: positive is right.
    pub dx_notches: i16,
    /// Vertical notches: positive is down, matching every platform's
    /// convention for content moving up.
    pub dy_notches: i16,
    /// Horizontal pixels, when the source knows them.
    pub dx_pixels: Option<i32>,
    /// Vertical pixels, when the source knows them.
    pub dy_pixels: Option<i32>,
}

impl Scroll {
    /// Whether this scroll asks for something a device could produce.
    #[must_use]
    pub fn is_well_formed(&self) -> bool {
        let notches = self.dx_notches.abs() <= MAX_NOTCHES && self.dy_notches.abs() <= MAX_NOTCHES;
        let pixels = |p: Option<i32>| p.is_none_or(|v| v.abs() <= MAX_SCROLL_PIXELS);
        notches && pixels(self.dx_pixels) && pixels(self.dy_pixels)
    }
}

/// Anything a controller sends to drive a host's screen.
///
/// One tagged enum on one ordered stream, so a key release cannot overtake the
/// press it releases — which would leave a key held down on somebody else's
/// machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputMessage {
    Key(KeyEvent),
    Pointer(PointerMove),
    Button(PointerButton),
    Scroll(Scroll),
    /// Let go of everything held.
    ///
    /// Sent when the controller's window loses focus, and by a host to itself
    /// when a session ends however it ends. **A key left held on a machine
    /// nobody is driving any more is the worst failure this path has**: a
    /// stuck Ctrl or Alt makes the host unusable for its own owner, who has no
    /// way to know why.
    ReleaseAll,
}

impl InputMessage {
    /// Whether this message is within bounds.
    #[must_use]
    pub fn is_well_formed(&self) -> bool {
        match self {
            Self::Key(k) => k.is_well_formed(),
            Self::Scroll(s) => s.is_well_formed(),
            // A monitor that does not exist is refused by the host, which is
            // the only side that knows what monitors there are.
            Self::Pointer(_) | Self::Button(_) | Self::ReleaseAll => true,
        }
    }

    /// Whether the given role is allowed to send this.
    ///
    /// All of it is controller to host. A host sending input to a controller
    /// would be typing into the machine of the person watching it, which is
    /// not a feature — it is the product backwards.
    #[must_use]
    pub const fn may_be_sent_by(&self, role: Role) -> bool {
        matches!(role, Role::Controller)
    }

    /// A name for an error message.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        match self {
            Self::Key(_) => "a key event",
            Self::Pointer(_) => "a pointer move",
            Self::Button(_) => "a button event",
            Self::Scroll(_) => "a scroll",
            Self::ReleaseAll => "a release-all",
        }
    }
}

/// A pixel offset as a fraction of an edge.
fn fraction(value: i32, edge: u32) -> u16 {
    if edge <= 1 {
        return 0;
    }
    let clamped = value.clamp(0, i32::try_from(edge.saturating_sub(1)).unwrap_or(i32::MAX));
    // Across `edge` pixels there are `edge - 1` steps between first and last,
    // so the last pixel maps to the top of the range rather than just short of
    // it — otherwise a pointer at the right edge never reaches it.
    let steps = u64::from(edge.saturating_sub(1)).max(1);
    // **Rounded, not truncated, and so is the way back.** Truncating in both
    // directions loses up to a whole pixel: on a 640-wide monitor each pixel
    // is about 102 steps, and pixel 1 became step 102, which became pixel 0.
    // A pointer that cannot express "one pixel from the edge" is a pointer
    // that cannot grab a window border.
    //
    // Rounding *here* is the smaller half of that. The way back carries the
    // round trip on its own; this improves the worst error across a size
    // change from 0.61 pixels to 0.51 — measured, not assumed, and no test
    // fails without it.
    let scaled = (u64::from(clamped.unsigned_abs()) * u64::from(POINTER_SCALE))
        .saturating_add(steps / 2)
        / steps;
    u16::try_from(scaled.min(u64::from(POINTER_SCALE))).unwrap_or(u16::MAX)
}

/// A fraction of an edge back to a pixel on it.
fn pixel(value: u16, edge: u32) -> u32 {
    if edge == 0 {
        return 0;
    }
    let last = u64::from(edge.saturating_sub(1));
    let scale = u64::from(POINTER_SCALE);
    let scaled = (u64::from(value) * last).saturating_add(scale / 2) / scale;
    u32::try_from(scaled.min(last)).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    #[test]
    fn a_key_usage_outside_the_page_is_refused() {
        // 0x04 is 'a' in the US layout's position; 0xE7 is Right GUI.
        assert!(
            KeyEvent {
                usage: 0x04,
                pressed: true
            }
            .is_well_formed()
        );
        assert!(
            KeyEvent {
                usage: 0xE7,
                pressed: false
            }
            .is_well_formed()
        );
        // 0x00..0x03 are a keyboard reporting about itself, not keys.
        for usage in [0x00, 0x01, 0x02, 0x03, 0xE8, 0xFFFF] {
            assert!(
                !KeyEvent {
                    usage,
                    pressed: true
                }
                .is_well_formed(),
                "{usage:#06x} names no key"
            );
        }
    }

    /// **A pointer must be able to reach every pixel, including the last
    /// one.** An off-by-one here is a cursor that cannot touch the right or
    /// bottom edge of the screen — where the scrollbars and the close button
    /// are.
    #[test]
    fn a_position_survives_the_round_trip_to_pixels_and_back() {
        for (w, h) in [
            (1u32, 1u32),
            (2, 2),
            (640, 480),
            (1280, 720),
            (1920, 1200),
            (2560, 1440),
            (3840, 2160),
            (7680, 4320),
        ] {
            // Only positions *on* the monitor: one off the edge is clamped
            // by design, which the next test is about.
            let last_x = i32::try_from(w.saturating_sub(1)).unwrap();
            let last_y = i32::try_from(h.saturating_sub(1)).unwrap();
            for (x, y) in [
                (0i32, 0i32),
                (1.min(last_x), 1.min(last_y)),
                (last_x / 2, last_y / 2),
                (last_x, last_y),
            ] {
                let p = PointerMove::from_pixels(0, x, y, w, h);
                let (bx, by) = p.to_pixels(w, h);
                assert_eq!(
                    (bx, by),
                    (x.unsigned_abs(), y.unsigned_abs()),
                    "{x},{y} on {w}x{h} came back as {bx},{by}"
                );
            }
        }
    }

    /// A position outside the monitor is clamped to its edge, not wrapped to
    /// the other side — which is what a pointer leaving a window means.
    /// **Every pixel of a real monitor, not a handful of samples.** The
    /// failure this catches is a rounding loss of one pixel, which a test
    /// checking the corners and the middle would miss entirely — and one pixel
    /// is the difference between grabbing a window border and clicking through
    /// it.
    #[test]
    fn every_pixel_of_a_real_monitor_survives_the_round_trip() {
        for (w, h) in [(640u32, 480u32), (1280, 720), (1920, 1200), (3840, 2160)] {
            for x in 0..w {
                let p = PointerMove::from_pixels(0, i32::try_from(x).unwrap(), 0, w, h);
                assert_eq!(p.to_pixels(w, h).0, x, "column {x} of {w}");
            }
            for y in 0..h {
                let p = PointerMove::from_pixels(0, 0, i32::try_from(y).unwrap(), w, h);
                assert_eq!(p.to_pixels(w, h).1, y, "row {y} of {h}");
            }
        }
    }

    /// **The case the whole design is for: the two sides are different
    /// sizes.**
    ///
    /// A session is scaled by default — the resolution ceiling sends a 1440p
    /// screen as 1080p — so a controller clicks a pixel of a 1080p picture and
    /// the host has to land on a pixel of a 1440p monitor. The fraction is
    /// what makes that possible, and it has to stay accurate across the
    /// change.
    #[test]
    fn a_position_taken_on_one_size_lands_where_it_should_on_another() {
        let pairs = [
            ((1920u32, 1080u32), (2560u32, 1440u32)),
            ((1920, 1080), (3840, 2160)),
            ((1280, 720), (1920, 1200)),
            ((3840, 2160), (1280, 720)),
            ((640, 480), (7680, 4320)),
        ];
        for ((from_w, from_h), (to_w, to_h)) in pairs {
            let mut worst = 0f64;
            for x in 0..from_w {
                let p = PointerMove::from_pixels(0, i32::try_from(x).unwrap(), 0, from_w, from_h);
                let landed = p.to_pixels(to_w, to_h).0;
                let ideal = f64::from(x) * f64::from(to_w.saturating_sub(1))
                    / f64::from(from_w.saturating_sub(1));
                worst = worst.max((f64::from(landed) - ideal).abs());
            }
            assert!(
                worst <= 1.0,
                "{from_w}x{from_h} to {to_w}x{to_h} was off by {worst:.2} pixels at worst"
            );
            println!("{from_w}x{from_h} → {to_w}x{to_h}: within {worst:.2} px");
        }
    }

    #[test]
    fn a_position_off_the_monitor_lands_on_the_edge() {
        let over = PointerMove::from_pixels(0, 5000, 5000, 1920, 1080);
        assert_eq!(over.to_pixels(1920, 1080), (1919, 1079));
        let under = PointerMove::from_pixels(0, -50, -1, 1920, 1080);
        assert_eq!(under.to_pixels(1920, 1080), (0, 0));
    }

    /// A monitor a pixel wide has one position, and asking for it must not
    /// divide by zero.
    #[test]
    fn a_monitor_with_no_room_has_one_position() {
        let p = PointerMove::from_pixels(0, 0, 0, 1, 1);
        assert_eq!(p.to_pixels(1, 1), (0, 0));
        let none = PointerMove::from_pixels(0, 3, 3, 0, 0);
        assert_eq!(none.to_pixels(0, 0), (0, 0));
    }

    /// The two ends of the range are the two ends of the screen. Stated
    /// separately from the round trip because it is the property a host
    /// depends on when it converts to its own pixels.
    #[test]
    fn the_ends_of_the_range_are_the_ends_of_the_screen() {
        assert_eq!(
            PointerMove {
                monitor: 0,
                x: 0,
                y: 0
            }
            .to_pixels(1920, 1080),
            (0, 0)
        );
        assert_eq!(
            PointerMove {
                monitor: 0,
                x: u16::MAX,
                y: u16::MAX
            }
            .to_pixels(1920, 1080),
            (1919, 1079)
        );
    }

    #[test]
    fn a_scroll_no_device_could_produce_is_refused() {
        let ok = Scroll {
            dx_notches: 1,
            dy_notches: -3,
            dx_pixels: Some(0),
            dy_pixels: Some(-120),
        };
        assert!(ok.is_well_formed());

        let too_many = Scroll {
            dx_notches: 0,
            dy_notches: 30_000,
            dx_pixels: None,
            dy_pixels: None,
        };
        assert!(!too_many.is_well_formed());

        let too_far = Scroll {
            dx_notches: 0,
            dy_notches: 1,
            dx_pixels: None,
            dy_pixels: Some(1_000_000),
        };
        assert!(!too_far.is_well_formed());
    }

    /// A scroll with no pixels is legal, because notches are the unit every
    /// host can act on.
    #[test]
    fn a_scroll_without_pixels_is_legal() {
        assert!(
            Scroll {
                dx_notches: 0,
                dy_notches: 1,
                dx_pixels: None,
                dy_pixels: None,
            }
            .is_well_formed()
        );
    }

    /// **Input goes one way.** A host sending input to a controller would be
    /// typing into the machine of the person watching it.
    #[test]
    fn every_input_message_is_controller_to_host() {
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
            InputMessage::Scroll(Scroll {
                dx_notches: 0,
                dy_notches: 1,
                dx_pixels: None,
                dy_pixels: None,
            }),
            InputMessage::ReleaseAll,
        ];
        for m in messages {
            assert!(m.may_be_sent_by(Role::Controller), "{}", m.name());
            assert!(!m.may_be_sent_by(Role::Host), "{}", m.name());
        }
    }

    #[test]
    fn every_input_message_survives_the_wire() {
        let messages = [
            InputMessage::Key(KeyEvent {
                usage: 0x1D,
                pressed: false,
            }),
            InputMessage::Pointer(PointerMove {
                monitor: 3,
                x: 12345,
                y: 65535,
            }),
            InputMessage::Button(PointerButton {
                button: Button::Forward,
                pressed: true,
            }),
            InputMessage::Scroll(Scroll {
                dx_notches: -2,
                dy_notches: 5,
                dx_pixels: None,
                dy_pixels: Some(-40),
            }),
            InputMessage::ReleaseAll,
        ];
        for m in messages {
            let bytes = crate::encode(&m).expect("encode");
            let back: InputMessage = crate::decode(&bytes).expect("decode");
            assert_eq!(back, m);
        }
    }
}
