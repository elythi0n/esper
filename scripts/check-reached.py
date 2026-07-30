#!/usr/bin/env python3
"""Every name the security path declares must be reached by production code.

Three times in one week this project shipped a security type that nothing
called: `SessionRegistry`, constructed only by its own test; `PermissionChange`,
in the protocol and reaching nobody; and `Permission::Unattended`, with
eighteen references in the tree and every one of them in a test. So it granted
nothing, and withholding it prevented nothing.

**Rust cannot find these.** `dead_code` works on private items and on binaries,
and every one of these lived as a `pub` item in a library crate — where `pub`
means "somebody outside might use it" and nothing is ever dead. That is exactly
the blind spot, and this is the check that covers it.

The failure it prevents is specific and nasty: a security feature whose only
callers are tests *looks exactly like a security feature*. A reviewer reads the
enum, reads a test asserting the enum behaves, and moves on.

# The ledger, and why it is the important half

Some names legitimately have no producer: a permission for a feature that is
not built, a reason code for a state this product cannot detect. Listing them
with a reason is what makes this survive. Without a ledger the check fails on
day one, somebody adds `--allow-everything`, and it never speaks again.

So the ledger below converts "silently unimplemented" into "written down as
unimplemented", and the check fails **in both directions**:

- A name that is unreached and not in the ledger is a feature that does not
  exist and nobody said so.
- A name that is in the ledger and *is* reached means the ledger is stale, and
  a stale ledger is how a check stops being read. Delete the entry.

# What counts as reached

A reference in a `.rs` file under `core/crates`, excluding: files under a
`tests/` directory, `#[cfg(test)] mod ... { ... }` blocks, comment lines, and
the declaration itself. Test modules are found by matching braces rather than
by cutting at the first `#[cfg(test)]` — an attribute on a single item is not
the start of a test region, and cutting there reported live code as dead when
this was first written.

A variant built by a helper in its own crate counts, because the helper is what
production calls. `Event::TerminalClosed` is built by `terminal_closed()` in the
audit crate, and an earlier version of this check called it dead.
"""

import re
import sys
from pathlib import Path

# name -> why it has no producer. Every entry is a claim somebody can check.
LEDGER = {
    # Features that are not built. Each is a tracked item, not an oversight.
    "Permission::Audio": "audio is not built; nothing can grant what nothing serves",
    "Permission::Record": "recording is not built",
    "Reason::OUT_OF_SCHEDULE": "schedules are not built; `unattended` is all-or-nothing today",
    "Reason::RELAY_FULL": "the relay is not built",
    # States this product cannot produce, which is a different thing from a
    # state it has not got round to producing.
    "Reason::PEER_KEY_CHANGED": (
        "undetectable: with no stable identifier other than the key, a changed key "
        "is indistinguishable from an unknown one, and that is UNKNOWN_PEER_KEY"
    ),
    "Reason::UNKNOWN_MESSAGE": (
        "serde refuses an unknown variant before anything can name it, so such a "
        "frame is MALFORMED_FRAME and this code cannot be reached"
    ),
    "Reason::IDLE_TIMEOUT": (
        "quinn closes an idle connection itself, with no application code to carry"
    ),
    "Reason::PAIRING_FAILED": "pairing refuses through PairError, before a session exists to close",
    "Reason::PAIRING_EXPIRED": "as PAIRING_FAILED",
    "Reason::PAIRING_RATE_LIMITED": "as PAIRING_FAILED",
    # Deliberate choices to do something better than refusing.
    "Reason::NO_INPUT_BACKEND": (
        "a host that cannot inject narrows the session to view-only and says so, "
        "rather than refusing it: a session somebody can watch beats no session"
    ),
    "Reason::NO_CAPTURE_BACKEND": (
        "the backend chooser falls back to the synthetic screen and names what it "
        "chose, so there is no host that has nothing to capture"
    ),
    "Reason::NO_DISPLAY": "as NO_CAPTURE_BACKEND",
}

CRATES = Path("core/crates")


def strip_test_modules(text: str) -> str:
    """Remove `#[cfg(test)] mod ... { ... }` blocks, matching braces.

    An attribute on a single item is not the start of a test region. Cutting at
    the first `#[cfg(test)]` reported `Permission::Unattended` as unreached the
    day after it was wired up, because a test-only constructor sat above the
    code that used it.
    """
    out: list[str] = []
    i = 0
    pat = re.compile(r"#\[cfg\(test\)\]\s*(?:#\[[^\]]*\]\s*)*mod\s+\w+\s*\{")
    while True:
        m = pat.search(text, i)
        if not m:
            out.append(text[i:])
            return "".join(out)
        out.append(text[i : m.start()])
        depth, j = 1, m.end()
        while j < len(text) and depth:
            if text[j] == "{":
                depth += 1
            elif text[j] == "}":
                depth -= 1
            j += 1
        i = j


def body_of(text: str, header: str) -> tuple[int, int]:
    """Byte range of a declaration's body, so it is not its own reference."""
    k = text.find(header)
    if k < 0:
        return (0, 0)
    depth, j = 1, text.index("{", k) + 1
    while j < len(text) and depth:
        if text[j] == "{":
            depth += 1
        elif text[j] == "}":
            depth -= 1
        j += 1
    return (k, j)


def declared(path: Path, header: str, pattern: str) -> list[str]:
    text = path.read_text()
    lo, hi = body_of(text, header)
    if lo == hi:
        sys.exit(f"check-reached: {path} has no `{header}`")
    return re.findall(pattern, text[lo:hi], re.M)


def references(prefix: str, names: set[str], home: Path, header: str) -> dict[str, int]:
    found = dict.fromkeys(names, 0)
    for f in sorted(CRATES.rglob("*.rs")):
        if "/tests/" in str(f):
            continue
        prod = strip_test_modules(f.read_text())
        lo, hi = body_of(prod, header) if f == home else (0, 0)
        for m in re.finditer(rf"{prefix}([A-Za-z_]+)\b", prod):
            if lo <= m.start() < hi:
                continue
            name = m.group(1)
            if name not in found:
                continue
            line_no = prod.count("\n", 0, m.start())
            line = prod.split("\n")[line_no]
            if line.lstrip().startswith("//"):
                continue
            found[name] += 1
    return found


FAMILIES = [
    (
        "permissions",
        "Permission::",
        CRATES / "esper-proto/src/message.rs",
        "pub enum Permission {",
        r"^    ([A-Z]\w*),$",
    ),
    (
        "reason codes",
        "Reason::",
        CRATES / "esper-proto/src/codes.rs",
        "impl Reason {",
        r"pub const ([A-Z_]+): Self",
    ),
    (
        "audit events",
        "Event::",
        CRATES / "esper-audit/src/lib.rs",
        "pub enum Event {",
        r"^    ([A-Z]\w*)\s*[\{,]",
    ),
]


def main() -> int:
    if not CRATES.is_dir():
        sys.exit("check-reached: run me from the repository root")
    problems: list[str] = []
    used_ledger: set[str] = set()
    for label, prefix, home, header, pattern in FAMILIES:
        names = declared(home, header, pattern)
        if not names:
            sys.exit(f"check-reached: found no {label} to check — the pattern has rotted")
        counts = references(prefix, set(names), home, header)
        unreached = [n for n in names if counts[n] == 0]
        listed = [n for n in names if f"{prefix}{n}" in LEDGER]
        # Every declared name marks its ledger entry as accounted for, whether
        # it turned out to be reached or not: an entry for a name that *is*
        # reached is stale, which is a different problem from an entry for a
        # name that does not exist, and reporting both for one mistake would
        # send somebody looking for a rename that never happened.
        for n in names:
            key = f"{prefix}{n}"
            if key in LEDGER:
                used_ledger.add(key)
        for n in unreached:
            key = f"{prefix}{n}"
            if key not in LEDGER:
                problems.append(
                    f"{key} is declared and nothing in production reaches it.\n"
                    f"      Either wire it up, or add it to the ledger in this script\n"
                    f"      with the reason it cannot be reached."
                )
        for n in listed:
            key = f"{prefix}{n}"
            if counts[n] > 0:
                problems.append(
                    f"{key} is in the ledger as unreachable, and is reached "
                    f"{counts[n]} time(s).\n"
                    f"      Remove the entry: a stale ledger is how a check stops "
                    f"being read."
                )
        print(f"  {len(names):>3} {label}: {len(names) - len(unreached)} reached, "
              f"{len(unreached)} recorded as not")
    stale = set(LEDGER) - used_ledger
    for key in sorted(stale):
        problems.append(
            f"{key} is in the ledger and is not declared anywhere.\n"
            f"      It was renamed or removed; the entry should go with it."
        )
    if problems:
        print()
        for p in problems:
            print(f"  ✗ {p}")
        print(f"\n  {len(problems)} problem(s). See the header of this script for why.")
        return 1
    print("  ✓ every declared name is reached or recorded")
    return 0


if __name__ == "__main__":
    sys.exit(main())
