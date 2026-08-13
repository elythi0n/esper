//! Physical keys on the AT bus: HID usages to set-1 scan codes.
//!
//! # Why scan codes and not virtual keys
//!
//! `SendInput` will take either. It has to be scan codes.
//!
//! A virtual key is what a character *means* after the layout has had its say:
//! `VK_OEM_1` is semicolon on a US layout and `ö` on a German one, and
//! `VK_Y`/`VK_Z` swap between US and German. The wire carries a physical
//! position, for the reason [`crate::hid`] gives — a controller pressing the
//! key left of Enter must press the key left of Enter at the host, whatever
//! either machine's layout calls it. Injecting a virtual key would ask Windows
//! to re-derive a position from a meaning, using the *host's* layout, and the
//! answer would be a different key.
//!
//! `KEYEVENTF_SCANCODE` skips that entirely: the scan code goes in as though
//! the keyboard sent it, and the host's layout turns it into whatever it
//! turns it into. Which is the whole point.
//!
//! # The extended flag is not decoration
//!
//! Set 1 ran out of one-byte codes, so nineteen keys are a prefix byte `E0`
//! followed by a code **another key already uses**. Insert is `E0 52` and
//! keypad `0` is `0x52`; Up is `E0 48` and keypad `8` is `0x48`; right alt is
//! `E0 38` and left alt is `0x38`.
//!
//! So a code alone does not identify a key, and [`Set1`] carries both — a
//! struct rather than two arguments, because the one thing that must never
//! happen is the flag being dropped somewhere between the table and
//! `SendInput`. Dropped, the arrow keys type digits and AltGr becomes Alt,
//! which silently breaks every layout that needs it.
//!
//! `SendInput` synthesises the `E0` itself from `KEYEVENTF_EXTENDEDKEY`, so
//! the prefix is not in the numbers here.
//!
//! # Where the numbers come from
//!
//! **For the base set — scan codes `0x01` to `0x58`, which is most of a
//! keyboard — the evdev code and the scan code are the same number.** `KEY_A`
//! is 30 and set-1 `a` is `0x1E`, which is 30. Linux took its main-keyboard
//! codes from the AT numbering and never renumbered them.
//!
//! So that half of this table is not a second table anybody had to get right:
//! it is [`crate::hid`]'s evdev code, unchanged. XKB is what establishes it —
//! `keycodes/xfree86` is the scan code plus the X offset for that range and
//! `keycodes/evdev` is the evdev code plus the same offset, and for every key
//! in the range the two files give the same number.
//! `scripts/check-scancodes.py` checks that, then holds this table to
//! xfree86's own numbers.
//!
//! Above `0x58` XFree86 assigned numbers it invented, so nothing outside this
//! file speaks for the twenty-one extended keys or for keypad equals and the
//! six JIS keys. Those are the part genuinely written from the scan-code
//! specification, and they are the part a keyboard will settle. The test below constrains them the only way that is possible
//! without a keyboard: every extended code must be one of the twenty-one the
//! specification lists, and every plain code must be inside the base range or
//! one of the named exceptions. That second rule caught a real mistake — the
//! JIS keypad comma written as `0x5C`, which is not a base code at all and
//! collided with right meta's `E0 5C`.
//!
//! # What is not here
//!
//! [`ABSENT`] names every HID usage this backend has no code for, with the
//! reason. Recorded rather than implied, because a key that is silently
//! missing on one platform and present on another is the kind of gap nobody
//! finds until somebody presses it — so a test requires the two tables to
//! account for every usage between them.

/// A set-1 scan code, with the flag that says whether `E0` precedes it.
///
/// The two travel together because nineteen codes mean two different keys
/// depending on the flag. See the module note.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Set1 {
    /// The code itself, without any prefix byte.
    pub code: u8,
    /// Whether this key is the `E0`-prefixed one.
    pub extended: bool,
}

impl Set1 {
    /// A code in the base set.
    #[must_use]
    pub const fn plain(code: u8) -> Self {
        Self {
            code,
            extended: false,
        }
    }

    /// A code the `E0` prefix belongs to.
    #[must_use]
    pub const fn extended(code: u8) -> Self {
        Self {
            code,
            extended: true,
        }
    }
}

/// The table: HID usage (page 0x07) to set-1 scan code.
const TABLE: &[(u16, u8, bool)] = &[
    (0x04, 0x1E, false), // a
    (0x05, 0x30, false), // b
    (0x06, 0x2E, false), // c
    (0x07, 0x20, false), // d
    (0x08, 0x12, false), // e
    (0x09, 0x21, false), // f
    (0x0A, 0x22, false), // g
    (0x0B, 0x23, false), // h
    (0x0C, 0x17, false), // i
    (0x0D, 0x24, false), // j
    (0x0E, 0x25, false), // k
    (0x0F, 0x26, false), // l
    (0x10, 0x32, false), // m
    (0x11, 0x31, false), // n
    (0x12, 0x18, false), // o
    (0x13, 0x19, false), // p
    (0x14, 0x10, false), // q
    (0x15, 0x13, false), // r
    (0x16, 0x1F, false), // s
    (0x17, 0x14, false), // t
    (0x18, 0x16, false), // u
    (0x19, 0x2F, false), // v
    (0x1A, 0x11, false), // w
    (0x1B, 0x2D, false), // x
    (0x1C, 0x15, false), // y
    (0x1D, 0x2C, false), // z
    (0x1E, 0x02, false), //
    (0x1F, 0x03, false), //
    (0x20, 0x04, false), //
    (0x21, 0x05, false), //
    (0x22, 0x06, false), //
    (0x23, 0x07, false), //
    (0x24, 0x08, false), //
    (0x25, 0x09, false), //
    (0x26, 0x0A, false), //
    (0x27, 0x0B, false), //
    (0x28, 0x1C, false), // enter
    (0x29, 0x01, false), // escape
    (0x2A, 0x0E, false), // backspace
    (0x2B, 0x0F, false), // tab
    (0x2C, 0x39, false), // space
    (0x2D, 0x0C, false), // minus
    (0x2E, 0x0D, false), // equal
    (0x2F, 0x1A, false), // left bracket
    (0x30, 0x1B, false), // right bracket
    (0x31, 0x2B, false), // backslash
    (0x32, 0x2B, false), // the non-US key in the same place, which Linux does not
    (0x33, 0x27, false), // semicolon
    (0x34, 0x28, false), // apostrophe
    (0x35, 0x29, false), // grave
    (0x36, 0x33, false), // comma
    (0x37, 0x34, false), // full stop
    (0x38, 0x35, false), // slash
    (0x39, 0x3A, false), // caps lock
    (0x3A, 0x3B, false), //
    (0x3B, 0x3C, false), //
    (0x3C, 0x3D, false), //
    (0x3D, 0x3E, false), //
    (0x3E, 0x3F, false), //
    (0x3F, 0x40, false), //
    (0x40, 0x41, false), //
    (0x41, 0x42, false), //
    (0x42, 0x43, false), //
    (0x43, 0x44, false), //
    (0x44, 0x57, false), //
    (0x45, 0x58, false), //
    (0x46, 0x37, true),  // print screen
    (0x47, 0x46, false), // scroll lock
    (0x49, 0x52, true),  // insert
    (0x4A, 0x47, true),  // home
    (0x4B, 0x49, true),  // page up
    (0x4C, 0x53, true),  // delete
    (0x4D, 0x4F, true),  // end
    (0x4E, 0x51, true),  // page down
    (0x4F, 0x4D, true),  // right
    (0x50, 0x4B, true),  // left
    (0x51, 0x50, true),  // down
    (0x52, 0x48, true),  // up
    (0x53, 0x45, false), // num lock
    (0x54, 0x35, true),  // keypad divide
    (0x55, 0x37, false), //
    (0x56, 0x4A, false), //
    (0x57, 0x4E, false), //
    (0x58, 0x1C, true),  // keypad enter
    (0x59, 0x4F, false), //
    (0x5A, 0x50, false), //
    (0x5B, 0x51, false), //
    (0x5C, 0x4B, false), //
    (0x5D, 0x4C, false), //
    (0x5E, 0x4D, false), //
    (0x5F, 0x47, false), //
    (0x60, 0x48, false), //
    (0x61, 0x49, false), //
    (0x62, 0x52, false), //
    (0x63, 0x53, false), //
    (0x64, 0x56, false), // the extra key on a 102-key keyboard
    (0x65, 0x5D, true),  // application
    (0x67, 0x59, false), // keypad equals
    (0x7F, 0x20, true),  // mute
    (0x80, 0x30, true),  // volume up
    (0x81, 0x2E, true),  // volume down
    (0x87, 0x73, false), // international 1 (ro)
    (0x88, 0x70, false), // international 2 (katakana/hiragana)
    (0x89, 0x7D, false), // international 3 (yen)
    (0x8A, 0x79, false), // international 4 (henkan)
    (0x8B, 0x7B, false), // international 5 (muhenkan)
    (0x8C, 0x7E, false), // international 6 (jis keypad comma)
    (0xE0, 0x1D, false), // left control
    (0xE1, 0x2A, false), // left shift
    (0xE2, 0x38, false), // left alt
    (0xE3, 0x5B, true),  // left meta
    (0xE4, 0x1D, true),  // right control
    (0xE5, 0x36, false), // right shift
    (0xE6, 0x38, true),  // right alt
    (0xE7, 0x5C, true),  // right meta
];

/// Every HID usage this backend has no scan code for, and why.
///
/// # Why the reasons are in the binary
///
/// A key missing from one platform's table and present in another's is a key
/// that works on one host and silently does nothing on the other, and nobody
/// finds it until somebody presses it. The test below requires this list plus
/// [`TABLE`] to account for **every** usage [`crate::hid`] has, so adding a key
/// to Linux and forgetting Windows fails to compile a passing test run rather
/// than shipping.
///
/// A caller asking for one of these gets [`crate::InputError::NoSuchKey`],
/// which is the same answer Linux gives for a usage it has no code for.
pub const ABSENT: &[(u16, &str)] = &[
    (
        0x48,
        "pause is E1 1D 45 — an E1 prefix, not E0, so the extended flag cannot \
         express it, and injecting the bare 0x45 would be num lock",
    ),
    (
        0x66,
        "the power button is ACPI hardware rather than a key on the AT bus",
    ),
    (
        0x85,
        "keypad comma has no set-1 code that AT keyboards agree on",
    ),
    (
        0x90,
        "hangul mode is 0xF2, which only Korean keyboards send and only a \
         Korean IME reads",
    ),
    (0x91, "hanja mode is 0xF1, on the same terms as hangul"),
    (0xB6, "no AT keyboard has a keypad left parenthesis"),
    (0xB7, "no AT keyboard has a keypad right parenthesis"),
    // F13 to F24. Keyboards that have them do not agree on the codes, and a
    // guess here would be a key that types something else.
    (0x68, "F13 to F24 have no code the AT specification fixes"),
    (0x69, "F13 to F24 have no code the AT specification fixes"),
    (0x6A, "F13 to F24 have no code the AT specification fixes"),
    (0x6B, "F13 to F24 have no code the AT specification fixes"),
    (0x6C, "F13 to F24 have no code the AT specification fixes"),
    (0x6D, "F13 to F24 have no code the AT specification fixes"),
    (0x6E, "F13 to F24 have no code the AT specification fixes"),
    (0x6F, "F13 to F24 have no code the AT specification fixes"),
    (0x70, "F13 to F24 have no code the AT specification fixes"),
    (0x71, "F13 to F24 have no code the AT specification fixes"),
    (0x72, "F13 to F24 have no code the AT specification fixes"),
    (0x73, "F13 to F24 have no code the AT specification fixes"),
    // The Sun-style editing block. Linux has evdev codes for these because
    // Sun keyboards existed; the AT bus never carried them.
    (
        0x74,
        "a Sun-style editing key, which the AT bus never carried",
    ),
    (
        0x75,
        "a Sun-style editing key, which the AT bus never carried",
    ),
    (
        0x76,
        "a Sun-style editing key, which the AT bus never carried",
    ),
    (
        0x77,
        "a Sun-style editing key, which the AT bus never carried",
    ),
    (
        0x78,
        "a Sun-style editing key, which the AT bus never carried",
    ),
    (
        0x79,
        "a Sun-style editing key, which the AT bus never carried",
    ),
    (
        0x7A,
        "a Sun-style editing key, which the AT bus never carried",
    ),
    (
        0x7B,
        "a Sun-style editing key, which the AT bus never carried",
    ),
    (
        0x7C,
        "a Sun-style editing key, which the AT bus never carried",
    ),
    (
        0x7D,
        "a Sun-style editing key, which the AT bus never carried",
    ),
    (
        0x7E,
        "a Sun-style editing key, which the AT bus never carried",
    ),
    // Japanese input-mode keys with no distinct code of their own.
    (
        0x92,
        "a Japanese input-mode key with no distinct set-1 code",
    ),
    (
        0x93,
        "a Japanese input-mode key with no distinct set-1 code",
    ),
    (
        0x94,
        "a Japanese input-mode key with no distinct set-1 code",
    ),
];

/// The scan code for a HID usage, if the AT bus has one for that key.
#[must_use]
pub fn set1_for_usage(usage: u16) -> Option<Set1> {
    TABLE
        .iter()
        .find(|(hid, _, _)| *hid == usage)
        .map(|(_, code, extended)| Set1 {
            code: *code,
            extended: *extended,
        })
}

/// The HID usage for a scan code, for a controller running on Windows turning
/// its own keyboard's events into protocol messages.
///
/// The first match wins, which matters for exactly one code, the same one it
/// matters for on Linux: `0x2B` is both the backslash key and the non-US key
/// in the same place, and a controller should send the one every keyboard has.
#[must_use]
pub fn usage_for_set1(code: Set1) -> Option<u16> {
    TABLE
        .iter()
        .find(|(_, c, e)| *c == code.code && *e == code.extended)
        .map(|(hid, _, _)| *hid)
}

/// Every usage this backend can inject, in order.
#[must_use]
pub fn usages() -> Vec<u16> {
    TABLE.iter().map(|(hid, _, _)| *hid).collect()
}

/// Why this usage has no scan code, if it has none.
#[must_use]
pub fn why_absent(usage: u16) -> Option<&'static str> {
    ABSENT
        .iter()
        .find(|(hid, _)| *hid == usage)
        .map(|(_, why)| *why)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use std::collections::{BTreeMap, BTreeSet};

    use super::*;
    use crate::hid;

    /// The twenty-one codes the specification prefixes with `E0`.
    ///
    /// Named so the table can be held to it. An extended flag on a code that is
    /// not here would be a key nothing sends, and the flag *missing* from one that
    /// is here is the arrow-keys-type-digits bug.
    const E0_CODES: [u8; 21] = [
        0x1C, 0x1D, 0x20, 0x2E, 0x30, 0x35, 0x37, 0x38, 0x47, 0x48, 0x49, 0x4B, 0x4D, 0x4F, 0x50,
        0x51, 0x52, 0x53, 0x5B, 0x5C, 0x5D,
    ];

    /// Plain codes outside the base range `0x01..=0x58` that are nonetheless real.
    ///
    /// Keypad equals and the JIS keys. Every other plain code must be in the base
    /// range, and that rule is what caught the JIS keypad comma being written as a
    /// code the base range does not contain.
    const PLAIN_ABOVE_THE_BASE: [u8; 7] = [0x59, 0x70, 0x73, 0x79, 0x7B, 0x7D, 0x7E];

    /// The top of the base set: codes above this need the `E0` prefix or are one
    /// of [`PLAIN_ABOVE_THE_BASE`].
    const BASE_TOP: u8 = 0x58;

    #[test]
    fn the_table_is_sorted_and_has_no_duplicate_usage() {
        let mut seen = BTreeSet::new();
        let mut last = 0;
        for (usage, _, _) in TABLE {
            assert!(
                *usage > last || last == 0,
                "usage 0x{usage:02X} is out of order after 0x{last:02X}"
            );
            assert!(seen.insert(*usage), "usage 0x{usage:02X} appears twice");
            last = *usage;
        }
    }

    #[test]
    fn only_the_key_in_two_places_maps_twice() {
        // A code alone does not identify a key, so the thing that must be
        // unique is the pair. Insert is E0 52 and keypad 0 is 0x52; both are
        // correct and they are different keys.
        let mut by_pair: BTreeMap<(u8, bool), Vec<u16>> = BTreeMap::new();
        for (usage, code, extended) in TABLE {
            by_pair.entry((*code, *extended)).or_default().push(*usage);
        }
        let shared: Vec<_> = by_pair.iter().filter(|(_, us)| us.len() > 1).collect();
        assert_eq!(
            shared.len(),
            1,
            "more than one scan code is claimed by two keys: {shared:?}"
        );
        let ((code, extended), usages) = shared.first().expect("one shared code");
        assert_eq!(
            (*code, *extended, usages.as_slice()),
            (0x2B, false, &[0x31, 0x32][..]),
            "the only key allowed in two places is backslash and the non-US \
             key beside it"
        );
    }

    #[test]
    fn an_extended_code_is_one_the_specification_prefixes() {
        for (usage, code, extended) in TABLE {
            if *extended {
                assert!(
                    E0_CODES.contains(code),
                    "usage 0x{usage:02X} is marked extended with code 0x{code:02X}, \
                     which the specification does not prefix with E0"
                );
            }
        }
    }

    #[test]
    fn a_plain_code_is_in_the_base_set_or_a_named_exception() {
        // The rule that caught the JIS keypad comma written as 0x5C, which is
        // not a base code and is right meta's code with the prefix.
        for (usage, code, extended) in TABLE {
            if !*extended {
                assert!(
                    (0x01..=BASE_TOP).contains(code) || PLAIN_ABOVE_THE_BASE.contains(code),
                    "usage 0x{usage:02X} injects plain 0x{code:02X}, which is neither in \
                     the base set nor a named exception — if it is real, name it"
                );
            }
        }
    }

    #[test]
    fn every_extended_code_is_used() {
        // The other direction: a code in E0_CODES that no key maps to means
        // either the list is wrong or a key is missing from the table.
        let used: BTreeSet<u8> = TABLE
            .iter()
            .filter(|(_, _, e)| *e)
            .map(|(_, c, _)| *c)
            .collect();
        for code in E0_CODES {
            assert!(
                used.contains(&code),
                "0x{code:02X} is listed as an E0 code and no key uses it"
            );
        }
    }

    #[test]
    fn the_two_platforms_account_for_every_key_between_them() {
        // The point of the whole module. A key added to the Linux table and
        // forgotten here would be a key that works on a Linux host and does
        // nothing on a Windows one, with nothing saying so.
        let linux: BTreeSet<u16> = hid::usages().into_iter().collect();
        let windows: BTreeSet<u16> = usages().into_iter().collect();
        let excused: BTreeSet<u16> = ABSENT.iter().map(|(u, _)| *u).collect();

        let unaccounted: Vec<_> = linux
            .difference(&windows)
            .filter(|u| !excused.contains(u))
            .map(|u| format!("0x{u:02X}"))
            .collect();
        assert!(
            unaccounted.is_empty(),
            "Linux can inject these and Windows has neither a code nor a \
             recorded reason: {unaccounted:?}"
        );

        let invented: Vec<_> = windows
            .difference(&linux)
            .map(|u| format!("0x{u:02X}"))
            .collect();
        assert!(
            invented.is_empty(),
            "these have a Windows code and are not in the HID table at all: {invented:?}"
        );
    }

    #[test]
    fn nothing_is_both_absent_and_present() {
        let present: BTreeSet<u16> = usages().into_iter().collect();
        for (usage, why) in ABSENT {
            assert!(
                !present.contains(usage),
                "usage 0x{usage:02X} has a scan code and is also recorded as \
                 absent because {why:?} — a stale reason is how a check stops \
                 being read"
            );
        }
    }

    #[test]
    fn every_absence_gives_a_reason_and_every_usage_is_one_the_protocol_accepts() {
        let mut seen = BTreeSet::new();
        for (usage, why) in ABSENT {
            assert!(seen.insert(*usage), "usage 0x{usage:02X} is excused twice");
            assert!(
                why.len() > 20,
                "usage 0x{usage:02X} is excused with {why:?}, which does not say why"
            );
            assert!(
                (0x04..=0xE7).contains(usage),
                "0x{usage:02X} is not a usage on page 0x07"
            );
        }
        for (usage, _, _) in TABLE {
            assert!(
                (0x04..=0xE7).contains(usage),
                "0x{usage:02X} is not a usage on page 0x07"
            );
        }
    }

    #[test]
    fn the_modifiers_are_all_in_the_table() {
        // A modifier with no code is a modifier that cannot be released, and a
        // stuck one is the worst kind.
        for usage in hid::MODIFIERS {
            assert!(
                set1_for_usage(usage).is_some(),
                "modifier 0x{usage:02X} has no scan code"
            );
        }
    }

    #[test]
    fn the_right_hand_modifiers_are_extended_and_the_left_are_not() {
        // Getting this wrong makes right alt behave as left alt, which breaks
        // AltGr and with it every layout that needs it to type a character.
        for (usage, want_extended) in [
            (0xE0u16, false), // left control
            (0xE1, false),    // left shift
            (0xE2, false),    // left alt
            (0xE3, true),     // left meta
            (0xE4, true),     // right control
            (0xE5, false),    // right shift — the one right-hand key that is not
            (0xE6, true),     // right alt
            (0xE7, true),     // right meta
        ] {
            let got = set1_for_usage(usage).expect("a modifier");
            assert_eq!(
                got.extended, want_extended,
                "usage 0x{usage:02X} has the wrong extended flag"
            );
        }
    }

    #[test]
    fn the_navigation_block_and_the_keypad_share_codes_and_stay_distinct() {
        // Ten codes, each meaning two keys. If the flag is ever dropped
        // between here and `SendInput`, the navigation key becomes the keypad
        // one: the arrows type digits and Delete types a decimal point. That
        // is the single most likely way for this module to be wrong in a way
        // a person notices immediately and a test might not.
        for (navigation, keypad, code, what) in [
            (0x49u16, 0x62u16, 0x52u8, "insert / keypad 0"),
            (0x4C, 0x63, 0x53, "delete / keypad ."),
            (0x4A, 0x5F, 0x47, "home / keypad 7"),
            (0x4B, 0x61, 0x49, "page up / keypad 9"),
            (0x4D, 0x59, 0x4F, "end / keypad 1"),
            (0x4E, 0x5B, 0x51, "page down / keypad 3"),
            (0x50, 0x5C, 0x4B, "left / keypad 4"),
            (0x4F, 0x5E, 0x4D, "right / keypad 6"),
            (0x52, 0x60, 0x48, "up / keypad 8"),
            (0x51, 0x5A, 0x50, "down / keypad 2"),
        ] {
            let nav = set1_for_usage(navigation).expect("a navigation key");
            let pad = set1_for_usage(keypad).expect("a keypad key");
            assert_eq!(nav, Set1::extended(code), "{what}: the navigation half");
            assert_eq!(pad, Set1::plain(code), "{what}: the keypad half");
            assert_ne!(nav, pad, "{what}");
        }
        // Keypad 5 has no navigation twin, being the middle of the block.
        assert_eq!(set1_for_usage(0x5D).expect("keypad 5"), Set1::plain(0x4C));
    }

    #[test]
    fn the_mapping_goes_back_the_other_way() {
        for (usage, code, extended) in TABLE {
            let there = set1_for_usage(*usage).expect("in the table");
            assert_eq!(there.code, *code);
            assert_eq!(there.extended, *extended);
            let back = usage_for_set1(there).expect("and back");
            // 0x32 is the non-US key sharing backslash's code, so it comes
            // back as 0x31. Every other key is its own answer.
            let expected = if *usage == 0x32 { 0x31 } else { *usage };
            assert_eq!(back, expected, "usage 0x{usage:02X} did not round trip");
        }
    }

    #[test]
    fn a_usage_the_bus_has_no_key_for_is_absent_and_says_why() {
        assert!(set1_for_usage(0x48).is_none(), "pause has no scan code");
        assert!(
            why_absent(0x48).expect("a reason").contains("E1"),
            "the reason should say what makes pause different"
        );
        assert!(set1_for_usage(0x0000).is_none());
        assert!(why_absent(0x0000).is_none());
    }
}
