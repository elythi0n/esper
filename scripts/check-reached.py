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
    "Permission::Record": "recording is not built",
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

# Functions whose absence of a caller was, or would be, a silent bug.
#
# The other half of this check, with the opposite polarity to the ledger: the
# ledger says "unreached is fine if you say why", and this says "these
# specific ones must be called". A `pub fn` in a library crate is never dead
# code, so nothing else notices when a call site goes away -- and twice now
# the call site was the whole feature.
#
# Deliberately short. Every entry is a bug that happened or would be silent;
# adding one without that would dilute the list into the 100-odd accessors and
# test builders that legitimately have no production caller.
MUST_BE_CALLED = {
    "set_mtu": (
        "the packetizer follows the path, and the path moves. Both of these "
        "existed with no caller while QUIC's estimate went 1351 -> 1382 -> 1414 "
        "-> 1162 in one session, and the transport silently refused every "
        "datagram that was then too large: the session looked alive and showed "
        "nothing"
    ),
    "set_monitors": (
        "the injector's monitor table, and the host's screens move. A pointer "
        "position is a fraction of a monitor, so without a caller every click "
        "after a layout change lands at the right fraction of the wrong screen "
        "-- or names a monitor that is gone and is refused, which looks like a "
        "session that stopped taking input"
    ),
    "is_sensitive": (
        "which permissions may not be granted without somebody confirming. "
        "Every reference to it was a test, so `grant <fp> unattended` wrote "
        "permanent unattended access and printed a success line -- while "
        "security.md said it required confirmation at the host"
    ),
    "release_all": (
        "what stops a key being left held down on somebody else's machine when "
        "a session ends however it ends"
    ),
}


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


DECL = re.compile(r"^\s*pub (?:const |async |unsafe )?fn (\w+)", re.M)
CALL = re.compile(r"\b([a-z_][a-z0-9_]*)\s*\(")


def called_in_production() -> set[str]:
    """Every function name that production code calls."""
    called: set[str] = set()
    for f in sorted(CRATES.rglob("*.rs")):
        if "/tests/" in str(f):
            continue
        for line in strip_test_modules(f.read_text()).split("\n"):
            t = line.lstrip()
            if t.startswith("//") or DECL.match(line):
                continue
            called.update(CALL.findall(line))
    return called


def message_unions() -> list[tuple[str, str, Path, str, str]]:
    """Every message enum the protocol declares, found rather than listed.

    So a union added later is covered without anybody remembering to add it
    here — which is the failure this whole script is about.

    # What this catches, and what it does not

    It counts *references*, and does not tell a construction from a match arm.
    So it catches a variant nobody mentions at all — added to the protocol and
    wired into neither end, which is the `PermissionChange` shape — and it does
    **not** catch a variant that is handled and never sent.

    Distinguishing the two needs to know an expression from a pattern, and
    `Variant(_)` in a match reads exactly like `Variant(x)` in a call. A
    heuristic on `=>` would miss `matches!` and would start reporting live code
    as dead, which is how a check earns the right to be ignored. Both ends
    match exhaustively with no `_` arm, so a variant that is never *handled*
    fails to compile; this covers the other half as far as counting can.
    """
    out = []
    for f in sorted((CRATES / "esper-proto/src").rglob("*.rs")):
        for m in re.finditer(r"pub enum (\w*(?:Message|Refusal|Reply))\s*\{", f.read_text()):
            name = m.group(1)
            out.append((
                f"{name} variants",
                f"{name}::",
                f,
                f"pub enum {name} {{",
                r"^    ([A-Z]\w*)\s*[\({,]",
            ))
    return out


def main() -> int:
    if not CRATES.is_dir():
        sys.exit("check-reached: run me from the repository root")
    problems: list[str] = []
    used_ledger: set[str] = set()

    # The functions that must have a caller, first: it is the shortest check
    # and the one whose failure is least ambiguous.
    called = called_in_production()
    for name, why in sorted(MUST_BE_CALLED.items()):
        if name not in called:
            problems.append(
                f"nothing in production calls {name}(), and something must:\n"
                f"      {why}"
            )
    print(f"  {len(MUST_BE_CALLED):>3} load-bearing functions: "
          f"{sum(1 for n in MUST_BE_CALLED if n in called)} called")

    unions = message_unions()
    # Counted together and reported as one line: twelve unions that are all
    # clean is twelve lines saying nothing. A failure names its own union.
    union_total = 0
    union_reached = 0
    for label, prefix, home, header, pattern in FAMILIES + unions:
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
        if (label, prefix, home, header, pattern) in unions:
            union_total += len(names)
            union_reached += len(names) - len(unreached)
        else:
            print(f"  {len(names):>3} {label}: {len(names) - len(unreached)} reached, "
                  f"{len(unreached)} recorded as not")
    if unions:
        print(f"  {union_total:>3} message variants across {len(unions)} unions: "
              f"{union_reached} reached")
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
