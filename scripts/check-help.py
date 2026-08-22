#!/usr/bin/env python3
"""Every command that takes a flag answers `--help` with its own flags.

# Why this is a check and not a review

Asking for help is never a failure — the engine's dispatcher says so, and it
routes `--help` to `usage_for(command)` before anything runs. What it does when
that lookup returns nothing is fall through to the general help, which is the
right answer for a command that takes no options and the wrong one for a command
that takes four: somebody who asks `esper-engine invite --help` gets a list of
subcommands and never learns that `--for` and `--expires` exist.

That is how five commands ended up with undiscoverable flags — `discover`'s
`--porcelain` and `--seconds`, `invite`'s `--for` and `--expires`, `forward`'s
`--stdio`, `terminal`'s `--resume`, and `pair`'s own pair of them. None of it
looks wrong in a diff: the flag is parsed, the command works, and the only way
to find out is to ask for help and read what comes back.

So this reads three things out of the source and compares them:

  1. **Which commands the dispatcher knows**, from the match in `main.rs`.
  2. **Which flags each one parses**, from the match arms in its `*_cmd.rs`.
  3. **Which ones have a usage of their own**, from `usage_for`.

A command that parses a flag and has no usage is a finding. So is a usage for a
command the dispatcher does not know, which is a help text nothing can reach.

Run from the repository root. Exit 1 on any finding.
"""

import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
ENGINE = ROOT / "core" / "crates" / "esper-engine" / "src"
MAIN = ENGINE / "main.rs"

# Commands that are not commands: the dispatcher's own words for asking.
NOT_COMMANDS = {"help", "--help", "-h", "version", "--version", "-v"}

# A flag every command takes and none documents, because it is the dispatcher's.
EVERYWHERE = {"--help"}


def commands_the_dispatcher_knows() -> set[str]:
    """Every word `main.rs` matches on, including the alternates."""
    text = MAIN.read_text(encoding="utf-8")
    found: set[str] = set()
    # Two arm shapes: `Some("a" | "b")`, and `Some(verb @ ("a" | "b"))` where
    # the command itself is wanted by the body.
    for arm in re.finditer(
        r'Some\(\s*(?:[a-z_]+\s*@\s*)?\(?\s*("[a-z-]+"(?:\s*\|\s*"[a-z-]+")*)\s*\)?\s*\)',
        text,
    ):
        found |= set(re.findall(r'"([a-z-]+)"', arm.group(1)))
    return found - NOT_COMMANDS


def commands_with_a_usage() -> set[str]:
    """The commands `usage_for` answers for."""
    text = MAIN.read_text(encoding="utf-8")
    start = text.find("fn usage_for(")
    if start < 0:
        raise SystemExit("main.rs no longer has usage_for; this check is stale")
    end = text.find("\n}", start)
    return set(re.findall(r'"([a-z-]+)"\s*=>', text[start:end]))


def flags_a_file_parses(path: Path) -> set[str]:
    """Flags matched in an arm, including `"-x" | "--long"` alternates."""
    flags: set[str] = set()
    for line in path.read_text(encoding="utf-8").split("\n"):
        if "=>" not in line:
            continue
        flags |= set(re.findall(r'"(--[a-z][a-z-]*)"', line.split("=>")[0]))
    return flags - EVERYWHERE


def advertised() -> set[str]:
    """The commands the general help lists.

    A command the help does not name is not one a person is expected to type —
    `loopback` drives a diagnostic and `serve` is spoken to by the control
    plane — so the general help is the honest answer for it. What this check is
    about is a command somebody is *told* about and then cannot ask about.
    """
    text = MAIN.read_text(encoding="utf-8")
    start = text.find("fn print_help(")
    if start < 0:
        raise SystemExit("main.rs no longer has print_help; this check is stale")
    end = text.find("\n}", start)
    block = text[start:end]
    # The list is one Rust string with line continuations, so the source reads
    # `\n  \` then the name — match on the name and its description, which is
    # the shape every entry has whatever the continuations do around it.
    block = block[block.find("Commands:") :]
    return set(re.findall(r'^\s*([a-z][a-z-]{1,12})\s{2,}[a-z]', block, re.M))


def parsers() -> dict[str, tuple[Path, str]]:
    """Which function reads each command's arguments.

    Taken from the dispatcher rather than from a list here, because a list here
    is a second thing to keep in step — and because several commands share a
    file while parsing different flags: `--group` reaches `change`, which is
    `grant` and `revoke`, and not `keep` or `forget`, which sit beside it.
    """
    text = MAIN.read_text(encoding="utf-8")
    found: dict[str, tuple[Path, str]] = {}
    # `Some("grant") => ... devices_cmd::change(&args[1..], Change::Grant)`
    for arm in re.finditer(
        r'Some\(\s*(?:[a-z_]+\s*@\s*)?\(?\s*("[a-z-]+"(?:\s*\|\s*"[a-z-]+")*)\s*\)?\s*\)\s*=>\s*(.{0,400}?)(?=\n\s{8}Some\(|\n\s{8}None|\n\s{4}\})',
        text,
        re.S,
    ):
        names = re.findall(r'"([a-z-]+)"', arm.group(1))
        call = re.search(r'([a-z_]+_cmd)::([a-z_]+)\s*\(', arm.group(2))
        if not call:
            continue
        path = ENGINE / f"{call.group(1)}.rs"
        if not path.exists():
            continue
        for name in names:
            found[name] = (path, call.group(2))
    return found


def flags_a_function_parses(path: Path, function: str) -> set[str]:
    """Flags matched inside one function's body."""
    text = path.read_text(encoding="utf-8")
    start = text.find(f"fn {function}(")
    if start < 0:
        return set()
    # To the next item at column nought, which is where a function ends here.
    end = text.find("\n}\n", start)
    body = text[start : end if end > 0 else len(text)]
    flags: set[str] = set()
    for line in body.split("\n"):
        flags |= set(re.findall(r'"(--[a-z][a-z-]*)"', line))
    return flags - EVERYWHERE


def main() -> int:
    known = commands_the_dispatcher_knows()
    with_usage = commands_with_a_usage()
    problems: list[str] = []

    if len(known) < 15:
        problems.append(
            f"the dispatcher parsed as {len(known)} command(s), which is not this engine"
        )

    # A usage nothing can reach.
    for orphan in sorted(with_usage - known):
        problems.append(
            f"`usage_for` answers for {orphan!r}, and the dispatcher has no such command — "
            "a help text nothing can reach"
        )

    # A command somebody is told about, that takes flags, and cannot be asked.
    told_about = advertised()
    if len(told_about) < 15:
        problems.append(
            f"the general help lists {len(told_about)} command(s), which is not this engine"
        )
    reads = parsers()
    silent: list[str] = []
    for command in sorted(known & told_about):
        if command in with_usage:
            continue
        where = reads.get(command)
        flags = flags_a_function_parses(*where) if where else set()
        if flags:
            silent.append(f"{command} ({', '.join(sorted(flags))})")

    for one in silent:
        problems.append(
            f"`{one.split(' ')[0]}` takes flags and `--help` answers with the general help: {one}"
        )

    counted = (
        f"{len(known)} commands, {len(told_about)} of them advertised, "
        f"{len(with_usage)} with a usage of their own"
    )
    if problems:
        print("")
        for p in problems:
            print(f"  ✗ help: {p}")
        print("")
        print("  Asking for help is never a failure, and neither is asking about a")
        print("  command whose flags nobody wrote down. Add a usage and name it in")
        print("  `usage_for`.")
        return 1

    print(f"  ✓ help: {counted} — every command that takes a flag answers for itself")
    return 0


if __name__ == "__main__":
    sys.exit(main())
