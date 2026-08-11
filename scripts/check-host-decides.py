#!/usr/bin/env python3
"""The decisions a host makes alone never cross the wire.

Two of this product's controls are the host's and nobody else's: **when** a
device may walk in unasked, and **whether the screen is locked**. Both are
enforced at the host, and the reason they are worth having is that the far end
has no say in either — a schedule the controller could report on, negotiate, or
supply the time for would not be a control, and a lock state it could assert
would be a switch for turning the policy off.

That is a claim about code, and a careful reading of it was true last month.
This is the mechanical half, and it has two directions:

1. **The rule does not go out.** Nothing schedule-shaped or lock-shaped may
   appear in `esper-proto`, which is every byte either end can send. A type
   that appeared there would be one the protocol could carry, whatever the
   sending code happened to do with it today.

2. **Nothing peer-supplied comes in.** The policy that makes these decisions
   may read exactly two fields of what the peer sent — what it wants, and what
   kind of session it is. A third would be a value from the far end reaching a
   decision the far end is not part of.

**The outcome does cross**, and that is the point rather than an exception: the
refusal codes exist so a device learns it was refused and why, without learning
the rule. Each entry below names its code and this check requires it to be
declared.

What this cannot check is what a *human* does with the settings file. It checks
the code, which is the part that could change without anybody deciding to.
"""

import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
PROTOCOL = ROOT / "core" / "crates" / "esper-proto" / "src"
POLICY = ROOT / "core" / "crates" / "esper-engine" / "src" / "consent.rs"
CODES = PROTOCOL / "codes.rs"

# Each decision, what would show it had stopped being the host's alone, and the
# code that tells a device the outcome without telling it the rule.
ALONE = {
    "when a device may walk in unasked": {
        "why": (
            "a schedule the far end could read, negotiate or supply the time "
            "for would not be a control"
        ),
        "absent": [
            (r"\bSchedule\b", "a schedule type"),
            (r"\bWeekday\b", "a day of the week"),
            (r"unattended_when", "a device's hours"),
            (r"\besper_clock\b", "the host's clock"),
        ],
        "code": "OUT_OF_SCHEDULE",
    },
    "whether the screen at the host is locked": {
        "why": (
            "a lock state the far end could assert would be a switch for "
            "turning the policy off"
        ),
        "absent": [
            (r"\bLockedHint\b", "logind's lock hint"),
            (r"\besper_presence\b", "the presence crate"),
            (r"deny_when_locked", "the policy's own name"),
        ],
        "code": "HOST_IS_LOCKED",
    },
    "what a session already open may ask to be allowed": {
        "why": (
            "a list the far end could read would be the host's rule travelling "
            "as data, and the next step from there is a controller that filters "
            "the question for the host"
        ),
        "absent": [
            (r"MAY_BE_ASKED_FOR", "the list of what may be asked for"),
            (r"\baskable\b", "whether a permission is askable"),
            (r"may_be_asked", "whether a permission may be asked for"),
            (r"\bKnocking\b", "how often a session may ask"),
            (r"AGAIN_AFTER", "how long a refusal holds"),
        ],
        "code": "NO_ASKING_HERE",
    },
}

# **What the peer names in a question is not a rule input.** A `PermissionAsk`
# carries one permission, which is the *subject* of the question rather than a
# value the rule reads — the same standing as `request.want` below, and bounded
# by the enum in the same way. What must not cross is which permissions may be
# asked for, how often, and whether this machine asks at all: those are in the
# list above.

# What the policy may read from what the peer sent. Two fields: what it wants,
# and which kind of session. Anything else is a value from the far end reaching
# a decision the far end is not part of.
MAY_READ = {"want", "kind"}

# Comments talk about all of this on purpose — this file's own docstring does.
# Stripped before matching so prose cannot fail the check and, more
# importantly, cannot pass it either.
COMMENT = re.compile(r"^\s*(//|///|//!|\*)")


def code_of(path: Path) -> str:
    """The file's code, with comment lines removed."""
    return "\n".join(
        line for line in path.read_text().splitlines() if not COMMENT.match(line)
    )


def main() -> int:
    problems: list[str] = []

    if not PROTOCOL.is_dir() or not POLICY.is_file():
        print("  ✗ this check has drifted, not the code: it cannot find its subjects")
        return 1

    protocol = {p: code_of(p) for p in sorted(PROTOCOL.rglob("*.rs"))}
    declared = CODES.read_text()

    for decision, rules in ALONE.items():
        for pattern, what in rules["absent"]:
            for path, text in protocol.items():
                if re.search(pattern, text):
                    problems.append(
                        f"{path.relative_to(ROOT)} mentions {what}, so "
                        f"{decision} can now cross the wire — and "
                        f"{rules['why']}"
                    )
        # And the outcome has to be sayable, or a refused device is left
        # guessing at a rule it is not allowed to see.
        if f"{rules['code']}: Self" not in declared:
            problems.append(
                f"Reason::{rules['code']} is not declared, so a device refused "
                f"over {decision} cannot be told which of the host's rules it "
                f"met"
            )

    # The other direction: what the policy reads from the peer.
    policy = code_of(POLICY)
    read = set(re.findall(r"\brequest\.(\w+)", policy))
    extra = read - MAY_READ
    if extra:
        problems.append(
            f"{POLICY.relative_to(ROOT)} reads request.{{{', '.join(sorted(extra))}}} "
            f"from what the peer sent. The policy may read only "
            f"{', '.join(sorted(MAY_READ))}: anything else is a value from the "
            f"far end reaching a decision the far end is not part of"
        )
    if not read:
        problems.append(
            f"{POLICY.relative_to(ROOT)} reads nothing from the request, which "
            f"means this check has stopped looking at the policy rather than "
            f"that the policy stopped reading"
        )

    if problems:
        print()
        for p in problems:
            print(f"  ✗ {p}")
        print(f"\n  {len(problems)} problem(s). See the header of this script for why.")
        return 1

    print(f"  {len(ALONE)} decision(s) the host makes alone: none of them crosses the wire")
    print(
        "  ✓ the rules stay at the host and only the outcomes are sent, and the "
        "policy reads two fields of what the peer said"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
