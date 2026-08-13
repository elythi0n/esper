#!/usr/bin/env python3
"""The Windows scan-code table is derived, not transcribed.

`esper-input/src/set1.rs` maps HID usages to AT set-1 scan codes, and a wrong
entry types the wrong character on every Windows host. There is no Windows
machine here to press a key on, so the question is what can be established
without one.

# The fact that does most of the work

**For the AT base set — scan codes 0x01 to 0x58, which is most of a keyboard —
the Linux evdev code and the set-1 scan code are the same number.** `KEY_A` is
30 and set-1 `a` is 0x1E, which is 30. Linux took its main-keyboard codes from
the AT numbering and never renumbered them.

So the base half of the Windows table is not a second table anybody had to get
right. It is `esper-input/src/hid.rs`'s evdev code, unchanged. What this script
does is establish that, from data nobody here wrote, and then hold the table to
it.

# How it is established

XKB ships two keycode files naming the same keys:

- `keycodes/xfree86` is the AT set-1 scan code plus the X protocol's offset,
  for the base range. That is what XFree86's keyboard driver did.
- `keycodes/evdev` is the evdev code plus the same offset.

If the claim above holds, those two files must give the **same number** for
every key whose xfree86 code is in the base range. They do, for all of them.
That comparison is offset-independent, which matters: an earlier version of
this script joined the files through the offset, and changing the offset from 8
to 7 shifted both sides equally, so the check passed having verified nothing.

# What it cannot speak for, and says so

Above 0x58 XFree86 assigned numbers it made up, so the files say nothing about
the twenty-one extended keys — right control, right alt, the arrows, the
editing block, keypad divide and enter, the meta keys, the volume keys — nor
about keypad equals and the six JIS keys, which sit above the base range with
real codes. Those are the part of the table that is genuinely written from the
specification, they are constrained by the unit tests in `set1.rs`, and a
keyboard will settle them.

It skips rather than fails when the XKB files are absent, the way
`check-clock.py` does about `zoneinfo`: a machine without xkeyboard-config
installed is not a machine with a wrong table.

# What sabotage established

Nine attempts. Caught: a wrong scan code, a wrong digit row, a usage with a
code and no HID entry, a wrong X offset, the wrong XKB file, a parser dropping
rows, and all of the above in combination with both self-checks removed —
because the table is compared against xfree86's *own* numbers, so the
third-party file is the data this runs on rather than a second opinion about
it.

Not caught, and understood: deleting the equality assertion while the files
are still correct. With correct data the claim holds anyway, so removing the
line that states it changes nothing observable. It exists to fire when XKB
changes underneath, and the wrong-file and wrong-offset attempts are what show
it fires then. No check can catch its own deletion from the inside; what it can
do is make sure no single deletion leaves it blind, and that is what the
combination attempt tested.
"""

import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
HID = ROOT / "core" / "crates" / "esper-input" / "src" / "hid.rs"
SET1 = ROOT / "core" / "crates" / "esper-input" / "src" / "set1.rs"

XKB = Path("/usr/share/X11/xkb/keycodes")
EVDEV, XFREE86 = XKB / "evdev", XKB / "xfree86"

# XFree86 keycodes are set-1 scan codes only up to here. Above it they are
# numbers XFree86 invented for keys the base set had no room for.
BASE_TOP = 0x58
# The X protocol offset both files carry. Used only to report scan codes in the
# messages — never to join the two files, because a wrong offset cancels out.
X_OFFSET = 8

# How many keys must confirm the claim before this script believes it did any
# work. 80 rather than the 86 that confirm it today, so that XKB dropping a key
# does not fail the build, and high enough that a broken read cannot hide.
FLOOR = 80

NAMED = re.compile(r"^\s*<([A-Z0-9_+]+)>\s*=\s*(\d+)\s*;", re.M)
HID_ENTRY = re.compile(r"\(\s*0x([0-9A-Fa-f]+)\s*,\s*(\d+)\s*\)")
SET1_ENTRY = re.compile(r"\(\s*0x([0-9A-Fa-f]+)\s*,\s*0x([0-9A-Fa-f]+)\s*,\s*(true|false)\s*\)")


def keycodes(path: Path) -> dict[str, int]:
    """Symbolic name -> raw XKB code. The first definition of a name wins."""
    out: dict[str, int] = {}
    for name, code in NAMED.findall(path.read_text()):
        out.setdefault(name, int(code))
    return out


def rust_table(path: Path, marker: str, pattern: re.Pattern[str]) -> list[tuple[str, ...]]:
    """A Rust table's entries, refusing to return a subset.

    The count is checked against every line that looks like an entry, because a
    parser that silently drops rows would report a table it had not read — and
    a dropped key is a key that silently does nothing on Windows.
    """
    text = path.read_text()
    start = text.find(marker)
    if start < 0:
        raise SystemExit(f"{path.name} no longer contains {marker!r}; this check is stale")
    body = text[start : text.find("\n];", start)]
    found = pattern.findall(body)
    every = re.findall(r"\(\s*0x[0-9A-Fa-f]+\s*,", body)
    if len(found) != len(every):
        raise SystemExit(
            f"read {len(found)} of {len(every)} entries from {path.name}'s {marker!r} — "
            f"the parser is losing lines, and a lost key is a key that silently does nothing"
        )
    return found


def main() -> int:
    if not EVDEV.is_file() or not XFREE86.is_file():
        print(f"  · no XKB keycode files at {XKB} — skipping, this needs xkeyboard-config")
        return 0

    problems: list[str] = []

    # One: establish that an evdev code in the base range *is* the scan code.
    evdev, xfree86 = keycodes(EVDEV), keycodes(XFREE86)
    confirmed = 0
    # evdev code -> the scan code xfree86 gives it. Built from xfree86's own
    # numbers rather than from evdev's, so the third-party file is the data
    # this check runs on and not merely a second opinion about it.
    scancode_of: dict[int, set[int]] = {}
    for name in evdev.keys() & xfree86.keys():
        scancode = xfree86[name] - X_OFFSET
        if not 0x01 <= scancode <= BASE_TOP:
            continue
        scancode_of.setdefault(evdev[name] - X_OFFSET, set()).add(scancode)
        if evdev[name] == xfree86[name]:
            confirmed += 1
        else:
            problems.append(
                f"XKB gives {name} evdev {evdev[name] - X_OFFSET} and scan code "
                f"0x{scancode:02X}, and this check rests on those being the same "
                f"number for the base set. Either XKB has renumbered something or "
                f"the claim in this script's header is wrong"
            )

    if confirmed < FLOOR:
        problems.append(
            f"only {confirmed} keys confirm that an evdev code is its scan code, and "
            f"at least {FLOOR} should — so this script has established nothing rather "
            f"than the table being right. Check that {EVDEV.name} and {XFREE86.name} "
            f"still name keys the same way"
        )

    # Two: hold the table to it.
    hid = {int(u, 16): int(e) for u, e in rust_table(HID, "const TABLE:", HID_ENTRY)}
    ours = [
        (int(u, 16), int(c, 16), ext == "true")
        for u, c, ext in rust_table(SET1, "const TABLE:", SET1_ENTRY)
    ]

    checked = beyond = 0
    for usage, code, extended in sorted(ours):
        evcode = hid.get(usage)
        if evcode is None:
            problems.append(
                f"usage 0x{usage:02X} has a scan code and is not in the HID table at all"
            )
            continue
        if extended or not 0x01 <= code <= BASE_TOP:
            # The extended keys and the ones above the base range are the part
            # written from the specification. Nothing here can speak for them.
            beyond += 1
            continue
        # Compared against xfree86's own scan code for that key, so this
        # comparison reads third-party data rather than restating the claim
        # above. Both checks are kept: this one catches a typo in the table,
        # and the one above catches XKB changing under it.
        wanted = scancode_of.get(evcode)
        if not wanted:
            beyond += 1
            continue
        if len(wanted) > 1:
            problems.append(
                f"usage 0x{usage:02X} (evdev {evcode}) has more than one scan code in "
                f"xfree86: {sorted(hex(c) for c in wanted)} — decide this entry by hand"
            )
            continue
        want = next(iter(wanted))
        if code != want:
            problems.append(
                f"usage 0x{usage:02X} injects scan code 0x{code:02X}; XKB's xfree86 "
                f"keycodes give that key (evdev {evcode}) scan code 0x{want:02X}"
            )
            continue
        checked += 1

    if problems:
        print()
        for line in problems:
            print(f"  ✗ {line}")
        print()
        print(f"  {len(problems)} problem(s). See the header of this script for why.")
        return 1

    print(
        f"  {confirmed} keys confirm an evdev code is its scan code in the base set; "
        f"{checked} table entries derived from it, {beyond} written from the specification"
    )
    print("  ✓ the base half of the Windows scan-code table is derived, not transcribed")
    return 0


if __name__ == "__main__":
    sys.exit(main())
