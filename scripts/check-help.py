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

# Commands that are not commands: the dispatcher's own words for asking, and
# the flag spellings of the one that is. `version` itself is a command — it
# takes `--json` and has a usage of its own — so it is held to the same rules
# as every other.
NOT_COMMANDS = {"help", "--help", "-h", "--version", "-v"}

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


def front_door() -> tuple[set[str], set[str]]:
    """What `esper` hands to the engine, and what its own help names.

    # Why this is checked at all

    `esper` is the binary people run; `esper-engine` says in its first line that
    running it directly is for diagnostics. So a verb the front door hands on
    and does not advertise is a verb nobody finds — and six were in exactly that
    state: `lock`, `logoff`, `reboot`, `group`, `wake` and `input` all worked
    and appeared in no help anywhere a person would look.

    The other direction is worse: a help that names a command nothing handles
    answers "unknown command" to somebody who read it and typed what it said.

    Both lists come out of `cmd/esper/main.go` — the passthrough map and the
    help text — so neither is written down twice.
    """
    text = (ROOT / "cmd" / "esper" / "main.go").read_text(encoding="utf-8")

    start = text.find("var engineCommands = map[string]bool{")
    if start < 0:
        raise SystemExit("cmd/esper/main.go no longer has engineCommands; this check is stale")
    end = text.find("\n}", start)
    handed = set(re.findall(r'"([a-z][a-z-]*)":\s*true', text[start:end]))

    start = text.find("func printHelp(")
    if start < 0:
        raise SystemExit("cmd/esper/main.go no longer has printHelp; this check is stale")
    end = text.find("\n}", start)
    # Two spaces, the name, two or more spaces, then a lowercase description —
    # the shape every entry in that help has. `host enable` and friends are
    # subcommands of a verb already listed, so only the first word counts.
    named = {
        line.split()[0]
        for line in re.findall(r"^  ([a-z][a-z-]* .*)$", text[start:end], re.M)
        if len(line.split()) > 1
    }

    # Floors, because a regex that stopped matching would verify two empty sets
    # and print a clean report.
    if len(handed) < 15 or len(named) < 20:
        raise SystemExit(
            f"the front door parsed as {len(handed)} handed and {len(named)} named, "
            "which is not this program"
        )
    # `esper` runs these itself rather than handing them on, so they are named
    # and not in the map — which is right, and is what this subtracts.
    its_own = {"doctor", "sessions", "version", "help", "api", "broker", "esper"}
    return handed, named - its_own


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
        # The body is bounded so the match cannot run away, and generously: an
        # arm with a paragraph of comment in it is still an arm, and a cap
        # that a comment pushes a command past is a check that goes quiet when
        # somebody explains themselves.
        r'Some\(\s*(?:[a-z_]+\s*@\s*)?\(?\s*("[a-z-]+"(?:\s*\|\s*"[a-z-]+")*)\s*\)?\s*\)\s*=>\s*(.{0,2000}?)(?=\n\s{8}Some\(|\n\s{8}None|\n\s{4}\})',
        text,
        re.S,
    ):
        names = re.findall(r'"([a-z-]+)"', arm.group(1))
        # **Any module, not only the ones called `*_cmd`.** `pair` dispatches
        # into `pairing`, which that spelling does not match — so the command
        # this product is least able to afford a surprise in was the one
        # command this check could not see. The file has to exist, which is
        # what keeps `net::me()` and friends out.
        path = None
        function = ""
        for module, name in re.findall(r'\b([a-z_]+)::([a-z_]+)\s*\(', arm.group(2)):
            candidate = ENGINE / f"{module}.rs"
            if not candidate.exists():
                continue
            # The parser, where one is named; otherwise whatever this arm
            # reaches first in a file of its own.
            if name.startswith("parse"):
                path, function = candidate, name
                break
            if path is None:
                path, function = candidate, name
        if path is None:
            continue
        for name in names:
            found[name] = (path, function)
    return found


def flags_parsed_in_the_arm() -> dict[str, set[str]]:
    """Flags a command reads in the dispatcher itself, with no `*_cmd` behind it.

    Four commands are short enough to parse their own arguments where they are
    matched — `version`, `identity`, `input`, `loopback`. `parsers` cannot see
    those, because it looks for a call into a `*_cmd` file: so `input` gained
    `--json`, had no usage, and `--help` fell through to the general help
    listing thirty-two commands. The check that exists to find exactly that
    could not.
    """
    text = MAIN.read_text(encoding="utf-8")
    found: dict[str, set[str]] = {}
    for arm in re.finditer(
        r'Some\(\s*(?:[a-z_]+\s*@\s*)?\(?\s*("[a-z-]+"(?:\s*\|\s*"[a-z-]+")*)\s*\)?\s*\)\s*=>\s*(.{0,2000}?)(?=\n\s{8}Some\(|\n\s{8}None|\n\s{4}\})',
        text,
        re.S,
    ):
        body = arm.group(2)
        if re.search(r'([a-z_]+_cmd)::([a-z_]+)\s*\(', body):
            continue
        flags = set(re.findall(r'"(--[a-z][a-z-]*)"', body)) - EVERYWHERE - {"--version"}
        if not flags:
            continue
        for name in re.findall(r'"([a-z-]+)"', arm.group(1)):
            found.setdefault(name, set()).update(flags)
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
    # **A floor on the map itself.** Twice now a command has been invisible to
    # this check — once because its module is not called `*_cmd`, once because
    # its arm grew past a length cap — and both times the report said
    # everything was fine. A map that suddenly names half as many commands is
    # the shape both of those took.
    if len(reads) < 15:
        problems.append(
            f"only {len(reads)} command(s) were traced to a parser, which is not this engine — "
            "the dispatcher's arms are not being read"
        )
    inline = flags_parsed_in_the_arm()
    silent: list[str] = []
    for command in sorted(known & told_about):
        if command in with_usage:
            continue
        where = reads.get(command)
        flags = flags_a_function_parses(*where) if where else set()
        flags |= inline.get(command, set())
        if flags:
            silent.append(f"{command} ({', '.join(sorted(flags))})")

    for one in silent:
        problems.append(
            f"`{one.split(' ')[0]}` takes flags and `--help` answers with the general help: {one}"
        )

    # **And every command the help lists answers for itself**, flags or no
    # flags. The rule `usage_for` states is broader than "takes a flag" —
    # anything a person is told about, including a command that takes nothing
    # where that is worth saying. `forget`, `keep` and `join` all took an
    # argument, took no flag, and answered `--help` with a list of thirty-two
    # commands; `forget` unpairs a device entirely.
    for command in sorted(told_about - with_usage):
        if command not in known:
            continue
        problems.append(
            f"`{command}` is in the general help and `--help` answers with that same help — "
            "a person who asks about a command they were told about gets the list they "
            "were reading"
        )

    # And the front door's own help, which is the one people read.
    handed, named = front_door()
    for missing in sorted(handed - named):
        problems.append(
            f"`esper {missing}` works and the front door's help does not name it — a command "
            "somebody can only find by reading the source, or by running the binary they are "
            "told not to run"
        )
    for phantom in sorted(named - handed):
        problems.append(
            f"the front door's help names `{phantom}` and nothing handles it — `esper {phantom}` "
            "answers 'unknown command' under a help text that promised it"
        )

    counted = (
        f"{len(known)} commands, {len(told_about)} of them advertised, "
        f"{len(with_usage)} with a usage of their own, "
        f"{len(handed)} handed on by the front door and all of them in its help"
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

    print(f"  ✓ help: {counted} — every command a person is told about answers for itself")
    return 0


if __name__ == "__main__":
    sys.exit(main())
