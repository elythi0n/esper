#!/usr/bin/env python3
"""The words Rust and Go both use for the same things are the same words.

Esper is two implementations of one product. Rust owns media, crypto and the
device store's writer; Go owns the control plane and reads that store. Where
they both name something, they name it twice — and two copies of a vocabulary
drift silently.

# Why drift here is not cosmetic

**A permission.** Rust deserialises `permissions` into an enum, so a word it
does not know fails the whole file. Go reads the same file. Add a permission in
Rust and not in Go, and the first store containing it makes the device list
report a broken trust store on a machine where nothing is wrong. Add one in Go
and not in Rust and it is worse: Go shows a device holding a permission that
nothing enforces.

**A lock-after choice.** The same, for what happens to the screen after a
session.

**The fingerprint.** The digest is what two people read to each other to
confirm they are looking at the same device. If the domain separator or the
word list differ by one character, the two halves of the product render the
same key as different codes — and the failure mode is that somebody decides
the device is not the one they meant.

**A permission, a third time.** The desktop window renders the same words, and
a window offering a permission the store cannot hold is a control that fails
on save; one missing a permission a device holds is a device shown with less
power than it has. TypeScript has no enum the file has to satisfy, so nothing
would fail — which is exactly why it is checked here rather than left to a
type.

Fixtures and unit tests catch a mismatch on both sides too. This catches it
without running anything, and it catches the direction the fixtures cannot: a
word added to Rust that Go has never seen, in a store no fixture holds yet.
"""

import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

RUST_PERMISSION = ROOT / "core" / "crates" / "esper-proto" / "src" / "message.rs"
RUST_DEVICE = ROOT / "core" / "crates" / "esper-crypto" / "src" / "device.rs"
RUST_AUDIT = ROOT / "core" / "crates" / "esper-audit" / "src" / "lib.rs"
RUST_FINGERPRINT = ROOT / "core" / "crates" / "esper-crypto" / "src" / "fingerprint.rs"
RUST_WORDLIST = ROOT / "core" / "crates" / "esper-crypto" / "src" / "wordlist.rs"

GO_PERMISSION = ROOT / "internal" / "devices" / "permission.go"
GO_WORDLIST = ROOT / "internal" / "devices" / "wordlist.go"

WEB_PERMISSION = ROOT / "web" / "src" / "api" / "permissions.ts"

# `Self::View => "view",` in a `name()`/`word()` match arm.
# An arm may carry a pattern between the variant and the arrow — the trail's
# events are `Self::Paired { .. } => "paired"`, where a permission is a bare
# `Self::View => "view"`. Both are one word for one thing.
RUST_ARM = re.compile(r'Self::(\w+)(?:\s*\{[^}]*\}|\s*\([^)]*\))?\s*=>\s*"([a-z_]+)"')
# `View Permission = "view"` or `LockAlways LockAfter = "always"`.
GO_CONST = re.compile(r'^\s*(\w+)\s+(Permission|LockAfter)\s*=\s*"([a-z_]+)"', re.M)
# The quoted words of a Rust or Go frozen string table.
QUOTED = re.compile(r'"([a-z]+)"')


def rust_arms(path: Path, after: str) -> list[str]:
    """The words of the match block that follows a marker, in order."""
    text = path.read_text()
    start = text.find(after)
    if start < 0:
        raise SystemExit(f"{path.name} no longer contains {after!r}; this check is stale")
    # To the end of the match block: the first line that closes it at the
    # arm's own indentation.
    end = text.find("\n        }", start)
    return [word for _, word in RUST_ARM.findall(text[start:end])]


def go_words(kind: str) -> list[str]:
    """The words of a Go const block, in declaration order."""
    return [word for _, k, word in GO_CONST.findall(GO_PERMISSION.read_text()) if k == kind]


def rust_wordlist() -> list[str]:
    text = RUST_WORDLIST.read_text()
    start = text.find("WORDS: [&str; WORDLIST_LEN] = [")
    if start < 0:
        raise SystemExit("the Rust word list is no longer a WORDLIST_LEN table; this check is stale")
    return QUOTED.findall(text[start : text.find("\n];", start)])


def go_wordlist() -> list[str]:
    text = GO_WORDLIST.read_text()
    start = text.find("var words = [256]string{")
    if start < 0:
        raise SystemExit("the Go word list is no longer a 256-entry table; this check is stale")
    return QUOTED.findall(text[start : text.find("\n}", start)])


def rust_string_const(path: Path, name: str) -> str:
    """The value of a `pub const NAME: &[u8] = b"...";`."""
    found = re.search(rf'{name}:\s*&\[u8\]\s*=\s*b"([^"]+)"', path.read_text())
    if not found:
        raise SystemExit(f"{path.name} no longer declares {name}; this check is stale")
    return found.group(1)


def go_string_const(path: Path, name: str) -> str:
    found = re.search(rf'{name}\s*=\s*"([^"]+)"', path.read_text())
    if not found:
        raise SystemExit(f"{path.name} no longer declares {name}; this check is stale")
    return found.group(1)


def web_words() -> list[str]:
    """The words of the `PERMISSIONS` tuple in the web bundle, in order."""
    text = WEB_PERMISSION.read_text()
    start = text.find("export const PERMISSIONS = [")
    if start < 0:
        raise SystemExit(f"{WEB_PERMISSION.name} no longer declares PERMISSIONS; this check is stale")
    return re.findall(r'"([a-z_]+)"', text[start : text.find("] as const;", start)])


def web_events() -> list[str]:
    """The event words of `AUTHORISED_BY`, in order.

    The window maps each of the trail's events to the permission that allowed
    it, because "show me everything that used `terminal`" is the question
    somebody opens an audit log with and the log answers a different one.
    """
    text = WEB_PERMISSION.read_text()
    start = text.find("export const AUTHORISED_BY")
    if start < 0:
        raise SystemExit(f"{WEB_PERMISSION.name} no longer declares AUTHORISED_BY; this check is stale")
    body = text[start : text.find("};", start)]
    return re.findall(r"^\s{2}([a-z_]+):", body, re.M)


def web_authorising_permissions() -> list[str]:
    """Which permissions `AUTHORISED_BY` names, so none is invented."""
    text = WEB_PERMISSION.read_text()
    start = text.find("export const AUTHORISED_BY")
    body = text[start : text.find("};", start)]
    return sorted({m for m in re.findall(r':\s*"([a-z_]+)"', body)})


def web_sensitive() -> list[str]:
    """The words of the `SENSITIVE` set, which decides which switches cannot move."""
    text = WEB_PERMISSION.read_text()
    start = text.find("export const SENSITIVE")
    if start < 0:
        raise SystemExit(f"{WEB_PERMISSION.name} no longer declares SENSITIVE; this check is stale")
    return re.findall(r'"([a-z_]+)"', text[start : text.find("]);", start)])


def go_sensitive() -> list[str]:
    """The wire words Go's `Sensitive` returns true for.

    Read from the true arm of its switch and translated through the const
    block, because the arm names identifiers and everything else in this
    check speaks the words that go in the file.
    """
    text = GO_PERMISSION.read_text()
    word_of = {name: word for name, kind, word in GO_CONST.findall(text) if kind == "Permission"}
    start = text.find("func (p Permission) Sensitive() bool {")
    if start < 0:
        raise SystemExit("permission.go no longer declares Sensitive; this check is stale")
    # The arm that returns true is the list; the one after it is the rest.
    first = text.find("case ", start)
    end = text.find("return true", first)
    if first < 0 or end < 0:
        raise SystemExit("permission.go's Sensitive is no longer a switch with a true arm; this check is stale")
    found = [word_of[name] for name in re.findall(r"\b([A-Z]\w+)\b", text[first:end]) if name in word_of]
    if not found:
        raise SystemExit("permission.go's Sensitive named no permission this check could read; it is stale")
    return found


def compare(what: str, rust: list[str], go: list[str], left: str = "Rust", right: str = "Go") -> list[str]:
    """Complain about every way two lists of words can differ."""
    problems = []
    for missing in [w for w in rust if w not in go]:
        problems.append(
            f"{what}: {left} has {missing!r} and {right} does not. A store holding it would read "
            f"as a broken trust store on a machine where nothing is wrong"
        )
    for extra in [w for w in go if w not in rust]:
        problems.append(
            f"{what}: {right} has {extra!r} and {left} does not. {right} would show something the "
            f"other half of the product does not enforce"
        )
    if not problems and rust != go:
        problems.append(
            f"{what}: the same words in a different order — {left} {rust}, {right} {go}. "
            f"Order is what a listing and a fixture are compared in"
        )
    return problems


def engine_usages() -> set[int]:
    """Every HID usage the engine can actually inject.

    Read out of the table itself rather than from a list beside it: the table
    is the thing that decides, and a second copy of its contents would be a
    second thing to keep in step.
    """
    text = (ROOT / "core/crates/esper-input/src/hid.rs").read_text(encoding="utf-8")
    body = text.split("const TABLE: &[(u16, u16)] = &[", 1)[1].split("];", 1)[0]
    return {int(m.group(1), 16) for m in re.finditer(r"\(0x([0-9A-Fa-f]+),\s*\d+\)", body)}


def window_usages() -> dict[str, int]:
    """What the page will send, by the name the browser gives each key."""
    text = (ROOT / "web/src/session/keys.ts").read_text(encoding="utf-8")
    body = text.split("USAGE_FOR_CODE: Readonly<Record<string, number>> = {", 1)[1].split("};", 1)[0]
    return {m.group(1): int(m.group(2), 16) for m in re.finditer(r"(\w+):\s*0x([0-9a-fA-F]+)", body)}


def main() -> int:
    problems: list[str] = []
    counted: list[str] = []

    permissions_rust = rust_arms(RUST_PERMISSION, "pub const fn name(self) -> &'static str {")
    permissions_go = go_words("Permission")
    problems += compare("permissions", permissions_rust, permissions_go)
    counted.append(f"{len(permissions_rust)} permissions")

    problems += compare("permissions", permissions_rust, web_words(), "Rust", "the window")

    # And which of them the host keeps to itself, because the window draws a
    # switch that cannot move for exactly these and a disagreement is either
    # a control that always fails or one that offers what the API refuses.
    problems += compare("host-only permissions", go_sensitive(), web_sensitive(), "Go", "the window")

    # Every event the trail records, against the window's map from event to
    # the permission that authorised it. An event added in Rust and not there
    # is a filter that silently hides lines; one there and not in Rust is a
    # filter that can never match.
    events_rust = rust_arms(RUST_AUDIT, "pub const fn tag(&self) -> &'static str {")
    problems += compare("trail events", events_rust, web_events(), "Rust", "the window")
    counted.append(f"{len(events_rust)} trail events")

    # And the permissions that map names must be permissions, not new words.
    invented = sorted(set(web_authorising_permissions()) - set(permissions_rust))
    if invented:
        problems.append(
            "the window authorises trail events with words that are not permissions: "
            + ", ".join(invented)
        )

    # **Every key the window can send is a key the engine can inject.**
    #
    # The wire carries HID usages — physical positions, so a key left of Enter
    # is that key on the far machine whatever either layout calls it — and the
    # W3C's `KeyboardEvent.code` values are defined against the same table.
    # That is what makes the page's map a name-to-number table rather than a
    # translation, and it is also what makes a wrong entry invisible: a usage
    # no platform has is a key that silently does nothing on the far side,
    # with no error anywhere.
    usages = engine_usages()
    window = window_usages()
    if len(window) < 100:
        problems.append(
            f"the window's key table parsed as {len(window)} entries, which is not a keyboard"
        )
    unknown = sorted(
        f"{name} (0x{usage:02X})" for name, usage in window.items() if usage not in usages
    )
    if unknown:
        problems.append(
            "the window would send keys the engine cannot inject: " + ", ".join(unknown)
        )
    # Two names on one usage is a key that presses another key, which reads
    # perfectly in a diff: both lines are plausible and only the pair is wrong.
    shared: dict[int, list[str]] = {}
    for name, usage in window.items():
        shared.setdefault(usage, []).append(name)
    doubled = sorted(
        f"0x{usage:02X} is both " + " and ".join(sorted(names))
        for usage, names in shared.items()
        if len(names) > 1
    )
    if doubled:
        problems.append("two keys in the window's table send the same usage: " + ", ".join(doubled))

    # The runs HID lays out as arithmetic, checked as arithmetic.
    #
    # Letters are alphabetical from 0x04, the digits run 1-9 from 0x1E with
    # zero after them, and the function keys run from 0x3A. Asserting the
    # shape rather than the numbers is what makes this more than a copy of
    # the table it is checking: an entry transposed anywhere in the middle of
    # a run moves every entry after it, and nothing else here would notice.
    runs: list[tuple[str, int]] = []
    runs += [(f"Key{chr(ord('A') + i)}", 0x04 + i) for i in range(26)]
    runs += [(f"Digit{i + 1}", 0x1E + i) for i in range(9)]
    runs += [("Digit0", 0x27)]
    runs += [(f"F{i + 1}", 0x3A + i) for i in range(12)]
    astray = [
        f"{name} is 0x{window[name]:02X} and HID puts it at 0x{usage:02X}"
        for name, usage in runs
        if window.get(name) != usage
    ]
    missing = [name for name, _ in runs if name not in window]
    if missing:
        problems.append(
            f"the window names no usage for {len(missing)} key(s) every keyboard has: "
            + ", ".join(missing[:6])
        )
    if astray:
        problems.append("the window's key table is off the HID order: " + ", ".join(astray[:6]))

    # And the other direction, as a count rather than a complaint: the engine
    # knows keys no browser names, which is ordinary — a Japanese conversion
    # key is not on this keyboard either.
    counted.append(
        f"{len(window)} keys the window can send of {len(usages)} the engine knows "
        f"({len(runs)} of them where HID's own arithmetic puts them)"
    )

    lock_rust = rust_arms(RUST_DEVICE, "pub const fn word(self) -> &'static str {")
    lock_go = go_words("LockAfter")
    problems += compare("what to do after a session", lock_rust, lock_go)
    counted.append(f"{len(lock_rust)} lock-after choices")

    domain_rust = rust_string_const(RUST_FINGERPRINT, "DOMAIN")
    domain_go = go_string_const(GO_WORDLIST, "FingerprintDomain")
    if domain_rust != domain_go:
        problems.append(
            f"the fingerprint domain separator differs — Rust {domain_rust!r}, "
            f"Go {domain_go!r}. The two halves would render one key as two codes"
        )
    counted.append("1 domain separator")

    words_rust, words_go = rust_wordlist(), go_wordlist()
    if len(words_rust) != 256 or len(words_go) != 256:
        problems.append(
            f"a fingerprint word list is not 256 words — Rust {len(words_rust)}, "
            f"Go {len(words_go)}. A byte would have no word, or two"
        )
    elif words_rust != words_go:
        differ = [
            f"{i}: Rust {r!r}, Go {g!r}"
            for i, (r, g) in enumerate(zip(words_rust, words_go))
            if r != g
        ]
        problems.append(
            "the fingerprint word lists differ at "
            + "; ".join(differ[:5])
            + (f" (and {len(differ) - 5} more)" if len(differ) > 5 else "")
        )
    counted.append(f"{len(words_rust)} fingerprint words")

    if problems:
        print()
        for line in problems:
            print(f"  ✗ {line}")
        print()
        print(f"  {len(problems)} problem(s). See the header of this script for why.")
        return 1

    print("  " + ", ".join(counted))
    print("  ✓ Rust, Go and the window use the same words for the same things")
    return 0


if __name__ == "__main__":
    sys.exit(main())
