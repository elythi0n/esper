//! Physical keys: HID usages on the wire, platform codes underneath.
//!
//! # One table, and it has to be right
//!
//! A wrong entry here is a key that types the wrong character — the exact bug
//! that carrying physical positions instead of characters exists to prevent.
//! So this is not reviewed, it is **tested against the platform**: the X11
//! backend injects each usage into a window it owns and asserts that the
//! keysym which arrives is the one a US layout would produce.
//!
//! # Why HID and not the platform's own numbering
//!
//! Because there are three other platforms. The choice is recorded in a
//! decision record; the short version is that HID page 0x07 is what every
//! platform already maps *from*, numeric, and small enough to send on the most
//! latency-sensitive path in the protocol.
//!
//! # The Linux chain
//!
//! HID usage → evdev code → whatever the display server wants. The second step
//! is the platform's, and on X11 it is `keycode = evdev + 8`, a convention of
//! Xorg's input drivers rather than a guarantee — which is why the backend
//! probes it instead of trusting it.

/// A Linux evdev key code, from `input-event-codes.h`.
///
/// A newtype rather than a bare `u16`, because a HID usage and an evdev code
/// are both small integers and mixing them up produces a key that works and is
/// the wrong one. `0x04` is `KEY_A` in one and 'a' in the other.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Evdev(pub u16);

/// The table: HID usage (page 0x07) to evdev code.
///
/// Every entry is a key that exists on hardware somebody has. Usages the page
/// defines and no platform has a code for are simply absent, and the caller
/// gets [`crate::InputError::NoSuchKey`] — a Japanese conversion key on a US
/// keyboard is the ordinary case, not a fault.
const TABLE: &[(u16, u16)] = &[
    // Letters, in HID order — which is alphabetical, and deliberately not the
    // order they appear on any keyboard.
    (0x04, 30), // a
    (0x05, 48), // b
    (0x06, 46), // c
    (0x07, 32), // d
    (0x08, 18), // e
    (0x09, 33), // f
    (0x0A, 34), // g
    (0x0B, 35), // h
    (0x0C, 23), // i
    (0x0D, 36), // j
    (0x0E, 37), // k
    (0x0F, 38), // l
    (0x10, 50), // m
    (0x11, 49), // n
    (0x12, 24), // o
    (0x13, 25), // p
    (0x14, 16), // q
    (0x15, 19), // r
    (0x16, 31), // s
    (0x17, 20), // t
    (0x18, 22), // u
    (0x19, 47), // v
    (0x1A, 17), // w
    (0x1B, 45), // x
    (0x1C, 21), // y
    (0x1D, 44), // z
    // Digits along the top row, 1 to 9 then 0.
    (0x1E, 2),
    (0x1F, 3),
    (0x20, 4),
    (0x21, 5),
    (0x22, 6),
    (0x23, 7),
    (0x24, 8),
    (0x25, 9),
    (0x26, 10),
    (0x27, 11),
    (0x28, 28), // enter
    (0x29, 1),  // escape
    (0x2A, 14), // backspace
    (0x2B, 15), // tab
    (0x2C, 57), // space
    (0x2D, 12), // minus
    (0x2E, 13), // equal
    (0x2F, 26), // left bracket
    (0x30, 27), // right bracket
    (0x31, 43), // backslash
    (0x32, 43), // the non-US key in the same place, which Linux does not
    // distinguish
    (0x33, 39), // semicolon
    (0x34, 40), // apostrophe
    (0x35, 41), // grave
    (0x36, 51), // comma
    (0x37, 52), // full stop
    (0x38, 53), // slash
    (0x39, 58), // caps lock
    // Function keys.
    (0x3A, 59),
    (0x3B, 60),
    (0x3C, 61),
    (0x3D, 62),
    (0x3E, 63),
    (0x3F, 64),
    (0x40, 65),
    (0x41, 66),
    (0x42, 67),
    (0x43, 68),
    (0x44, 87),
    (0x45, 88),
    (0x46, 99),  // print screen
    (0x47, 70),  // scroll lock
    (0x48, 119), // pause
    (0x49, 110), // insert
    (0x4A, 102), // home
    (0x4B, 104), // page up
    (0x4C, 111), // delete
    (0x4D, 107), // end
    (0x4E, 109), // page down
    (0x4F, 106), // right
    (0x50, 105), // left
    (0x51, 108), // down
    (0x52, 103), // up
    (0x53, 69),  // num lock
    // The keypad.
    (0x54, 98),
    (0x55, 55),
    (0x56, 74),
    (0x57, 78),
    (0x58, 96),
    (0x59, 79),
    (0x5A, 80),
    (0x5B, 81),
    (0x5C, 75),
    (0x5D, 76),
    (0x5E, 77),
    (0x5F, 71),
    (0x60, 72),
    (0x61, 73),
    (0x62, 82),
    (0x63, 83),
    (0x64, 86),  // the extra key on a 102-key keyboard
    (0x65, 127), // application / menu
    (0x66, 116), // power
    (0x67, 117), // keypad equals
    // F13 to F24, which exist on more keyboards than people expect.
    (0x68, 183),
    (0x69, 184),
    (0x6A, 185),
    (0x6B, 186),
    (0x6C, 187),
    (0x6D, 188),
    (0x6E, 189),
    (0x6F, 190),
    (0x70, 191),
    (0x71, 192),
    (0x72, 193),
    (0x73, 194),
    // The editing and Sun-keyboard block.
    (0x74, 134), // open
    (0x75, 138), // help
    (0x76, 130), // props
    (0x77, 132), // front
    (0x78, 128), // stop
    (0x79, 129), // again
    (0x7A, 131), // undo
    (0x7B, 137), // cut
    (0x7C, 133), // copy
    (0x7D, 135), // paste
    (0x7E, 136), // find
    (0x7F, 113), // mute
    (0x80, 115), // volume up
    (0x81, 114), // volume down
    (0x85, 121), // keypad comma
    // Keys for layouts that need them: Japanese, Korean, and the
    // international keys.
    (0x87, 89),  // ro
    (0x88, 93),  // katakana/hiragana
    (0x89, 124), // yen
    (0x8A, 92),  // henkan
    (0x8B, 94),  // muhenkan
    (0x8C, 95),  // keypad jp comma
    (0x90, 122), // hangeul
    (0x91, 123), // hanja
    (0x92, 90),  // katakana
    (0x93, 91),  // hiragana
    (0x94, 85),  // zenkaku/hankaku
    (0xB6, 179), // keypad left bracket
    (0xB7, 180), // keypad right bracket
    // The modifiers, which are the last thing the page defines and the most
    // important thing to get right: a wrong modifier is a key that sticks.
    (0xE0, 29),  // left control
    (0xE1, 42),  // left shift
    (0xE2, 56),  // left alt
    (0xE3, 125), // left meta
    (0xE4, 97),  // right control
    (0xE5, 54),  // right shift
    (0xE6, 100), // right alt
    (0xE7, 126), // right meta
];

/// The evdev code for a HID usage, if this platform has that key.
#[must_use]
pub fn evdev_for_usage(usage: u16) -> Option<Evdev> {
    TABLE
        .iter()
        .find(|(hid, _)| *hid == usage)
        .map(|(_, code)| Evdev(*code))
}

/// The HID usage for an evdev code, for a controller turning its own
/// keyboard's events into protocol messages.
///
/// The first match wins, which matters for exactly one code: evdev 43 is both
/// the backslash key and the non-US key in the same place, and a controller
/// should send the one every keyboard has.
#[must_use]
pub fn usage_for_evdev(code: Evdev) -> Option<u16> {
    TABLE
        .iter()
        .find(|(_, evdev)| *evdev == code.0)
        .map(|(hid, _)| *hid)
}

/// Every usage this platform can inject, in order.
#[must_use]
pub fn usages() -> Vec<u16> {
    TABLE.iter().map(|(hid, _)| *hid).collect()
}

/// The modifier usages, which a caller may want to treat differently — a
/// panic key is a combination of them, and a stuck one is the worst kind.
pub const MODIFIERS: [u16; 8] = [0xE0, 0xE1, 0xE2, 0xE3, 0xE4, 0xE5, 0xE6, 0xE7];

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    /// The table has to be sorted by usage, because the next reader will
    /// assume it is and a duplicate is invisible in an unsorted list.
    #[test]
    fn the_table_is_sorted_and_has_no_duplicate_usage() {
        let mut last = 0u16;
        for (usage, _) in TABLE {
            assert!(
                *usage > last,
                "{usage:#06x} follows {last:#06x}: the table must be sorted and unique"
            );
            last = *usage;
        }
    }

    /// **Every usage in the table is one the protocol will carry.** An entry
    /// outside the protocol's range could never be reached, and an entry the
    /// protocol allows and the table cannot map is a key that does nothing —
    /// the second is expected, the first is dead code.
    #[test]
    fn every_entry_is_a_usage_the_protocol_accepts() {
        for (usage, _) in TABLE {
            let event = ownview_proto::KeyEvent {
                usage: *usage,
                pressed: true,
            };
            assert!(
                event.is_well_formed(),
                "{usage:#06x} is in the table and outside the protocol's range"
            );
        }
    }

    /// One evdev code appears twice on purpose, and nothing else does: two
    /// usages mapping to one code means one of them cannot be recovered, which
    /// is fine for the pair that *are* the same key and a bug for any other.
    #[test]
    fn only_the_key_in_two_places_maps_twice() {
        let mut seen: Vec<(u16, u16)> = Vec::new();
        let mut duplicates: Vec<u16> = Vec::new();
        for (usage, code) in TABLE {
            if let Some((other, _)) = seen.iter().find(|(_, c)| c == code) {
                duplicates.push(*code);
                assert_eq!(
                    (*other, *usage),
                    (0x31, 0x32),
                    "evdev {code} is claimed by {other:#06x} and {usage:#06x}"
                );
            }
            seen.push((*usage, *code));
        }
        assert_eq!(duplicates, vec![43], "exactly one code is shared");
    }

    #[test]
    fn a_usage_and_its_code_are_not_the_same_number() {
        // The reason `Evdev` is a newtype: these are different numbering
        // systems and both are small integers.
        assert_eq!(evdev_for_usage(0x04), Some(Evdev(30)));
        assert_ne!(evdev_for_usage(0x04), Some(Evdev(0x04)));
    }

    #[test]
    fn the_modifiers_are_all_in_the_table() {
        for usage in MODIFIERS {
            assert!(
                evdev_for_usage(usage).is_some(),
                "{usage:#06x} is a modifier with no code"
            );
        }
    }

    #[test]
    fn a_usage_the_platform_has_no_key_for_is_absent() {
        // Defined by the page, absent from Linux: locking caps lock.
        assert_eq!(evdev_for_usage(0x82), None);
        // Not defined at all.
        assert_eq!(evdev_for_usage(0xC0), None);
    }

    #[test]
    fn the_mapping_goes_back_the_other_way() {
        for (usage, _) in TABLE {
            let Some(code) = evdev_for_usage(*usage) else {
                panic!("{usage:#06x} is in the table and did not map");
            };
            let back = usage_for_evdev(code).expect("a usage");
            // The shared code comes back as the first of its two usages.
            let expected = if *usage == 0x32 { 0x31 } else { *usage };
            assert_eq!(back, expected, "{usage:#06x} came back as {back:#06x}");
        }
    }
}
