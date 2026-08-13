#!/usr/bin/env python3
"""Every setting this product declares is read, and every setting it reads is
declared.

Twice now a setting has been a **promise the product did not keep**, and both
times it looked exactly like a working feature:

- `audit.retain` was in the settings table with a `prune` that had **no
  caller**. Somebody who set thirty days got a machine that kept everything
  for ever and said nothing. Worse than an unimplemented feature, because the
  setting was there to be found and to be trusted.
- `session.buffer` was promised in `capture-and-codec.md` — `auto | <ms>`, in
  a document that describes the shipped product — with no setting, no
  plumbing and no caller at all.

A person who writes a line in a config file has done their half. If nothing
reads it, nothing tells them: the file parses, the value appears in
`esper config`, and the machine behaves as though the line were not there.

# Two directions, because both are silent

**A declared key nothing reads is a switch that does nothing.** Wire it up, or
put it in the ledger below with the reason — which converts "quietly does
nothing" into "written down as not built yet".

**A key read but not declared is a typo**, and a typo is worse than a missing
setting: `settings::flag("host.plain_backgroud", false)` compiles, always
returns the default, and there is no line of output anywhere that would say
why the feature never switched on.

# What counts as read

The key's name appearing in a `.rs` file under `core/crates`, outside the
settings table itself, excluding files under a `tests/` directory and
`#[cfg(test)] mod ... { ... }` blocks.

Test modules are cut by matching braces rather than at the first
`#[cfg(test)]` — the same rule, and the same reason, as `check-reached.py`: an
attribute on a single item is not the start of a test region, and cutting
there reported live code as dead. And tests are excluded at all because a
setting read *only* by its own test is precisely the `audit.retain` case:
eighteen references and every one of them in a test.
"""

import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
TABLE = ROOT / "core" / "crates" / "esper-config" / "src" / "settings.rs"
CRATES = ROOT / "core" / "crates"

# key -> why nothing reads it. Every entry is a claim somebody can check.
#
# Empty today, and that is the point of adding this while it is: a check that
# arrives with a list of exceptions is a check nobody believes.
LEDGER: dict[str, str] = {}

# The helpers that take a key. A call to one of these with a name the table
# does not have is the typo this check exists to catch.
ASKED = re.compile(r"settings::(?:flag|text|whole|number)\(\s*\"([^\"]+)\"")


def strip_test_modules(text: str) -> str:
    """Remove `#[cfg(test)] mod ... { ... }` blocks, matching braces."""
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


def production() -> list[tuple[Path, str]]:
    """Every production Rust file, with its test modules removed."""
    files = []
    for f in sorted(CRATES.rglob("*.rs")):
        if "/tests/" in str(f) or f == TABLE:
            continue
        files.append((f, strip_test_modules(f.read_text())))
    return files


def declared() -> list[str]:
    text = TABLE.read_text()
    keys = re.findall(r'key: "([a-z_.]+)"', text)
    if not keys:
        sys.exit("check-settings-kept: the settings table has no keys, so this check is broken")
    return keys


def main() -> int:
    problems: list[str] = []
    keys = declared()
    files = production()

    # One: every declared key is read somewhere that ships.
    for key in keys:
        read_in = [f for f, text in files if key in text]
        excused = LEDGER.get(key)
        if read_in and excused:
            problems.append(
                f"{key} is in the ledger as not read, and is read in "
                f"{read_in[0].relative_to(ROOT)}. Remove the entry: a stale ledger is how "
                f"a check stops being read"
            )
        elif not read_in and not excused:
            problems.append(
                f"nothing outside its own table reads {key}, so setting it does nothing. "
                f"Wire it up, or add it to this script's ledger with the reason"
            )

    # Two: every key the code asks for is one the table has.
    known = set(keys)
    for f, text in files:
        for asked in ASKED.findall(text):
            if asked not in known:
                problems.append(
                    f"{f.relative_to(ROOT)} reads {asked!r}, which the settings table does "
                    f"not declare. A key that is not there reads as its default for ever, "
                    f"and nothing says so"
                )

    if problems:
        print()
        for line in problems:
            print(f"  ✗ {line}")
        print()
        print(f"  {len(problems)} problem(s). See the header of this script for why.")
        return 1

    excused = len(LEDGER)
    print(f"  {len(keys)} settings: {len(keys) - excused} read, {excused} recorded as not")
    print("  ✓ every setting is read, and every setting read is declared")
    return 0


if __name__ == "__main__":
    sys.exit(main())
