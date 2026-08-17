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

# The other half: handled and never sent

Counting references catches a name nobody mentions. It cannot catch a variant
that is **matched somewhere and produced nowhere** — where the arm is the whole
of the feature and reads, to a reviewer, exactly like the feature working. That
needs a scan that tells an expression from a pattern; `pattern_regions` is it,
and `CLASSIFIER_CORPUS` is what keeps it trustworthy.

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
    "Permission::Record": (
        "recording is built and this permission is not what governs it: a recording "
        "happens on the controller's own disk, so the host announces it rather than "
        "granting it — see the finding of 2026-09-08. Nothing checks this, and a "
        "session may not ask for it either"
    ),
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
    "stop_now": (
        "what a panic press throws to stop injection. The press has ended the "
        "session since Phase 5 and the product's own security document said it "
        "was 'handled before input dispatch' — which was true of the session "
        "and not of the injecting: the teardown aborts the task that injects, "
        "an abort lands at that task's next await, and a queue of messages "
        "could be applied after somebody had asked for everything to stop. "
        "The wiring cannot be tested, because the rest of a panic locks the "
        "screen of whoever is running the tests, so it is required here"
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


# ── Telling a construction from a pattern ────────────────────────────────
#
# `references` above counts mentions, which catches a variant nobody mentions
# at all and cannot catch **a variant that is handled and never sent** — the
# nastier shape, because a reviewer reads the match arm and moves on.
#
# Distinguishing them means knowing an expression from a pattern, and
# `Variant(_)` in a match reads exactly like `Variant(x)` in a call. There is
# no regex for that. What there is, and what this does, is a scan that tracks
# the few places Rust puts a pattern:
#
#   - between `match … {` and the `=>` of each arm
#   - between `if let` / `while let` / `let` and the `=` or `{` that ends it
#   - the second argument onward of `matches!(…)`
#   - after `|` inside any of those
#
# Everything else is an expression: a construction, a type, or a path. That is
# a heuristic, and a heuristic in a security check has to earn its place — so
# it is held to **a corpus of cases with known answers** that runs every time
# (`CLASSIFIER_CORPUS`). If a Rust construct this does not understand appears,
# the corpus is where it gets added, and until then the check reports what it
# can defend rather than a guess.
#
# Two deliberate limitations, stated rather than discovered:
#
#   - A construction *inside a match arm's body* is an expression, and the
#     scan gets that right (the arm's pattern ends at `=>`).
#   - A macro that takes a pattern and is not `matches!` would be read as an
#     expression. There are none in this tree; the corpus would catch the day
#     there are, because the count of construction sites would jump.

PATTERN_LEADS = re.compile(r"\b(?:if\s+let|while\s+let|let|match|matches!)\b")


def pattern_regions(text: str) -> list[tuple[int, int]]:
    """Every span of `text` that is a pattern position.

    Returned as half-open character ranges, in order, non-overlapping enough
    for a membership test — an arm inside an arm's body opens a new region and
    closes at its own `=>`.
    """
    regions: list[tuple[int, int]] = []

    # `match X { … }`: each arm's pattern runs from the start of the arm to
    # its `=>`. The arms are found by walking the braces of the match body,
    # because an arm body may itself contain a match.
    for m in re.finditer(r"\bmatch\b", text):
        body = text.find("{", m.end())
        if body < 0:
            continue
        depth, i, arm_start = 0, body, -1
        while i < len(text):
            ch = text[i]
            if ch == "{":
                depth += 1
                if depth == 1:
                    arm_start = i + 1
            elif ch == "}":
                depth -= 1
                if depth == 0:
                    break
            elif depth == 1 and ch == "," and arm_start >= 0:
                arm_start = i + 1
            elif depth == 1 and text.startswith("=>", i) and arm_start >= 0:
                regions.append((arm_start, i))
                arm_start = -1
                i += 1
            i += 1

    # `if let PAT = …`, `while let PAT = …`, `let PAT = …`, and `let PAT else`.
    for m in re.finditer(r"\b(?:if\s+let|while\s+let|let)\b", text):
        stop = len(text)
        for token in ("=", ";", "{"):
            at = text.find(token, m.end())
            if at >= 0:
                stop = min(stop, at)
        regions.append((m.end(), stop))

    # `matches!(x, PAT)` — everything after the first comma, to the closing
    # parenthesis.
    for m in re.finditer(r"\bmatches!\s*\(", text):
        depth, i, comma = 1, m.end(), -1
        while i < len(text) and depth > 0:
            ch = text[i]
            if ch in "([{":
                depth += 1
            elif ch in ")]}":
                depth -= 1
                if depth == 0:
                    break
            elif ch == "," and depth == 1 and comma < 0:
                comma = i + 1
            i += 1
        if comma >= 0:
            regions.append((comma, i))

    return regions


def in_any(regions: list[tuple[int, int]], at: int) -> bool:
    return any(lo <= at < hi for lo, hi in regions)


# Cases with known answers, so the scan above cannot rot silently. Each is a
# snippet, the variant to look for, and how many times it is *constructed*.
CLASSIFIER_CORPUS: list[tuple[str, str, int]] = [
    ("let a = Event::Paired { name: n };", "Event::Paired", 1),
    ("match e { Event::Paired { .. } => 1, _ => 0 }", "Event::Paired", 0),
    ("match e { Event::Paired { .. } => Event::Paired { name: n }, _ => x }", "Event::Paired", 1),
    ("if let Event::Paired { .. } = e { 1 } else { 0 }", "Event::Paired", 0),
    ("while let Event::Paired { .. } = next() {}", "Event::Paired", 0),
    ("matches!(e, Event::Paired { .. })", "Event::Paired", 0),
    ("matches!(e, Event::Paired { .. } | Event::Unpaired { .. })", "Event::Paired", 0),
    ("send(Event::Paired { name: n });", "Event::Paired", 1),
    ("fn f() -> Event { Event::Paired { name: n } }", "Event::Paired", 1),
    ("let x = if ok { Event::Paired { name: n } } else { other };", "Event::Paired", 1),
    # A nested match, where the inner arm must not swallow the outer body.
    (
        "match a { X::One => match b { Event::Paired { .. } => Event::Unpaired { k }, _ => y }, _ => z }",
        "Event::Paired",
        0,
    ),
    (
        "match a { X::One => match b { Event::Unpaired { .. } => Event::Paired { k }, _ => y }, _ => z }",
        "Event::Paired",
        1,
    ),
]


def constructions(prefix: str, names: set[str], home: Path, header: str) -> dict[str, int]:
    """How many times each name is *built*, rather than mentioned."""
    found = dict.fromkeys(names, 0)
    for f in sorted(CRATES.rglob("*.rs")):
        if "/tests/" in str(f):
            continue
        prod = strip_test_modules(f.read_text())
        lo, hi = body_of(prod, header) if f == home else (0, 0)
        patterns = pattern_regions(prod)
        lines = prod.split("\n")
        for m in re.finditer(rf"{prefix}([A-Za-z_]+)\b", prod):
            if lo <= m.start() < hi:
                continue
            name = m.group(1)
            if name not in found:
                continue
            if lines[prod.count("\n", 0, m.start())].lstrip().startswith("//"):
                continue
            if in_any(patterns, m.start()):
                continue
            found[name] += 1
    return found


def check_classifier() -> list[str]:
    """The corpus, run before the scan is trusted with anything."""
    problems = []
    for snippet, name, want in CLASSIFIER_CORPUS:
        patterns = pattern_regions(snippet)
        got = sum(
            1
            for m in re.finditer(re.escape(name) + r"\b", snippet)
            if not in_any(patterns, m.start())
        )
        if got != want:
            problems.append(
                f"the construction scan read {name} as built {got} time(s) in "
                f"{snippet!r}, and it is built {want}. The scan is wrong, so what "
                f"it says about the tree cannot be trusted"
            )
    return problems


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

    `Command` and `Report` are in this net as well as the wire messages. They
    are the control plane's channel to the engine rather than anything that
    leaves the machine, and they were the one protocol family nothing checked:
    a command declared on one side and answered nowhere is the same bug in the
    same shape, and it arrives the same way.

    # What this catches

    Two shapes, in two passes. Counting *references* catches a variant nobody
    mentions at all — added to the protocol and wired into neither end, which
    is the `PermissionChange` shape. Counting **constructions** catches the
    nastier one: a variant that is handled and never sent, where the match arm
    looks exactly like the feature working.

    The second needed a scan that tells an expression from a pattern, because
    `Variant(_)` in a match reads exactly like `Variant(x)` in a call. See
    `pattern_regions` and the corpus that holds it honest.
    """
    out = []
    for f in sorted((CRATES / "esper-proto/src").rglob("*.rs")):
        for m in re.finditer(r"pub enum (\w*(?:Message|Refusal|Reply|Command|Report))\s*\{", f.read_text()):
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

    # **The scan is checked before it is believed.** A heuristic that told a
    # construction from a pattern incorrectly would report live code as dead,
    # which is how a check earns the right to be ignored.
    problems += check_classifier()
    if problems:
        print()
        for p in problems:
            print(f"  ✗ {p}")
        return 1

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
    # How many references each name had, kept so the second pass below can
    # tell "matched and never built" from "not mentioned at all" and report
    # each mistake once.
    seen: dict[str, int] = {}
    for label, prefix, home, header, pattern in FAMILIES + unions:
        names = declared(home, header, pattern)
        if not names:
            sys.exit(f"check-reached: found no {label} to check — the pattern has rotted")
        counts = references(prefix, set(names), home, header)
        seen.update({f"{prefix}{n}": c for n, c in counts.items()})
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

    # ── Handled and never sent ───────────────────────────────────────────
    #
    # The half counting could not reach. A variant with references and **no
    # construction** is matched somewhere and produced nowhere: the arm looks
    # like the feature working, and nothing ever takes it.
    #
    # The same ledger governs it, because the legitimate cases are the same
    # ones — a state this product cannot produce is a state nothing
    # constructs.
    for label, prefix, home, header, pattern in FAMILIES + unions:
        names = declared(home, header, pattern)
        built = constructions(prefix, set(names), home, header)
        never = sorted(n for n, count in built.items() if count == 0)
        for name in never:
            key = f"{prefix}{name}"
            if key in LEDGER:
                used_ledger.add(key)
                continue
            # Already reported by the reference count as unreached entirely;
            # saying it twice is noise.
            if seen.get(key) == 0:
                continue
            problems.append(
                f"{key} is matched somewhere and built nowhere.\n"
                f"      Something handles it and nothing sends it, so the arm is the "
                f"whole of the feature. Either produce it, or record why it cannot be "
                f"produced in the ledger."
            )
        print(f"  {len(names):>3} {label}: {len(names) - len(never)} built")
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
