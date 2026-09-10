#!/usr/bin/env python3
"""Comments that say a thing happens in one place, held to it.

# Why this exists

This tree has been careful about a particular kind of claim: *"the only place a
forwarded connection is dialled"*, *"read in one place, so the two cannot mean
different things"*, *"inverted here, in one place"*. Each names a real property,
each explains why it matters, and each was, until now, a sentence.

Three separate faults this week were of exactly that shape — a comment
asserting a property the code beneath it did not have:

  - the device store's loader said "neither side ever lists a device the other
    does not" while one side dropped entries the other kept;
  - the supervisor said the engine "is told the name it was reached under" and
    never told it;
  - `keep` said widening a pairing is "exactly the kind of thing somebody
    should be able to find afterwards" and recorded it as something else.

None of those was visible in a diff. The comment and the code were written at
the same time and drifted apart later, or the comment described the intention
and the code the first attempt at it.

So the claims below are checked. Each is a pattern that may appear in exactly
one file, with the comment that says so and the thing that goes wrong if a
second one appears. A claim that stops being true fails here, in the same
commit that breaks it.

# What this is not

It is not a duplication linter. Three lines that look alike are not the
subject; a *safety property* stated as "only here" is. The list grows when
somebody writes another such comment, and the entry cites it.

Run from the repository root. Exit 1 on any finding.
"""

import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

# (what, pattern, the files it belongs in, why anywhere else is a fault)
#
# Most claims name one file. A few name a short list, and the list is the point:
# `AnyKeyForPairingOnly` said "confined to a host in pairing mode" while the
# broker used it too — legitimately, because introducing strangers is its whole
# job — so the sentence was narrower than the truth, and an auditor reading it
# would not have gone looking in a crate it did not name.
CLAIMS: list[tuple[str, str, str | list[str], str]] = [
    (
        "a forwarded connection is dialled",
        r"TcpStream::connect",
        "core/crates/esper-transport/src/forward.rs",
        "`ForwardRules::dial` is the check and the dial in one function, so that no "
        "future caller can have the second without the first. A dial anywhere else "
        "reaches a host's own network without the allowlist, and `localhost` that "
        "resolves elsewhere is exactly what the ordering there refuses.",
    ),
    (
        "this machine's name is read",
        r'settings::text\("device\.name"\)',
        "core/crates/esper-engine/src/net.rs",
        "The setting and `ESPER_DEVICE_NAME` are one answer, read once. A second "
        "reader is two places that can disagree about what this machine is called, "
        "and the label a peer shows somebody is the one that disagrees.",
    ),
    (
        "the wheel's direction is inverted",
        r"vertical: -notches",
        "core/crates/esper-input/src/wheel.rs",
        "The protocol is positive-down and Windows is positive-away, so exactly one "
        "place inverts. A second inversion cancels the first, and 'scrolling is "
        "backwards' is the bug two people fix in opposite directions having each "
        "tried it once. The window's own encoder had a second one, and it was "
        "removed rather than matched.",
    ),
    (
        "a decoder is spawned without insisting on a sandbox",
        r"spawn_for_diagnosis|spawn_self_for_diagnosis",
        [
            "core/crates/esper-decode/src/worker.rs",
            "core/crates/esper-engine/src/main.rs",
        ],
        "It exists because `ESPER_SANDBOX_AUDIT=1` turns the syscall filter into "
        "log-and-allow so a missing syscall can be found — and with a session "
        "refusing any decoder that is not enforcing, that hatch was unreachable "
        "and taught nobody anything. So there is one caller: `esper sandbox`, on a "
        "bitstream this machine made a moment earlier. **A peer's bitstream goes "
        "through `spawn_self`, which refuses**, and a second caller of this is how "
        "that becomes a preference instead of a property.",
    ),
    (
        "the authority that accepts any key is used",
        r"\bpairing_authority\(\)|\bAnyKeyForPairingOnly\b",
        [
            "core/crates/esper-transport/src/pair.rs",
            "core/crates/esper-transport/src/lib.rs",
            "core/crates/esper-engine/src/pairing.rs",
            "core/crates/esper-engine/src/invite_cmd.rs",
            "core/crates/esper-rendezvous/src/server.rs",
        ],
        "It accepts any well-formed key, because at first contact neither side has "
        "the other's yet and the code inside the PAKE does the authenticating. It "
        "belongs to pairing and to the broker, which introduces strangers and opens "
        "a session with nobody. Anywhere else is a session that authenticates no one.",
    ),
]

# Where a match does not count: a test may name the thing it is testing.
EXEMPT = re.compile(r"(/tests?/|_test\.go$|/benches/|/fuzz/)")


def matches(pattern: str) -> list[tuple[str, int]]:
    """Every file and line where the pattern appears, outside tests."""
    found = subprocess.run(
        ["git", "grep", "-n", "-E", pattern, "--", "*.rs", "*.go"],
        cwd=ROOT,
        capture_output=True,
        text=True,
        check=False,
    )
    out: list[tuple[str, int]] = []
    for line in found.stdout.split("\n"):
        if not line.strip():
            continue
        path, number, rest = line.split(":", 2)
        if EXEMPT.search(path):
            continue
        # A mention inside a comment is somebody explaining the rule, which is
        # the opposite of breaking it.
        if re.match(r"\s*(//|///|//!|#)", rest):
            continue
        out.append((path, int(number)))
    return out


def main() -> int:
    problems: list[str] = []
    checked = 0

    for what, pattern, where_it_belongs, why in CLAIMS:
        homes = [where_it_belongs] if isinstance(where_it_belongs, str) else where_it_belongs
        missing = [h for h in homes if not (ROOT / h).exists()]
        if missing:
            problems.append(
                f"the claim about where {what} names {', '.join(missing)}, which no longer exists"
            )
            continue
        home = homes[0]

        where = matches(pattern)
        checked += 1

        # A floor: a pattern that matches nothing proves nothing, and a rename
        # would otherwise turn this check off silently.
        if not where:
            problems.append(
                f"nothing matches {pattern!r} any more, so the claim that {home} is the only "
                f"place {what} is no longer being checked"
            )
            continue

        strangers = sorted({path for path, _ in where if path not in homes})
        if strangers:
            lines = ", ".join(f"{p}:{n}" for p, n in where if p not in homes)
            belongs = home if len(homes) == 1 else "only " + ", ".join(homes)
            problems.append(
                f"{what} belongs in {belongs}, and it also happens at {lines}.\n    {why}"
            )

    if checked < len(CLAIMS):
        print(f"  ✗ one-place: only {checked} of {len(CLAIMS)} claims were checked", file=sys.stderr)

    if problems:
        print("")
        for p in problems:
            print(f"  ✗ one-place: {p}")
        print("")
        print("  A comment that states a safety property is the property, until")
        print("  something checks it. Either the second one goes, or the comment does.")
        return 1

    print(f"  ✓ one-place: {checked} claims of the form \"the only place…\" are true of this tree")
    return 0


if __name__ == "__main__":
    sys.exit(main())
