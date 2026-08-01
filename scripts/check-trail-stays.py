#!/usr/bin/env python3
"""The session trail never leaves the machine, checked rather than promised.

Phase 15 asks for this in those words: *verified — the log never leaves the
machine, and there is no code path by which it could*. A sentence in a document
is not a verification, and neither is a careful reading that was true last
month. This is the mechanical half.

The claim has three parts, and each is checked here:

1. **The crate that owns the file cannot reach a network.** `esper-audit` reads
   and writes one path and does nothing else: no sockets, no subprocesses, no
   environment, no HTTP. If it grows any of those, the claim is void whatever
   the callers do.

2. **The readers are enumerated.** Only the places in the ledger below may read
   the trail, and each entry says where it goes. A new reader fails this check
   until somebody writes down where it sends what it read — which is exactly
   the moment to notice that it sends it anywhere.

3. **Each reader is itself local.** The one product reader answers `esper
   sessions` over this process's *stdout*, which is a pipe held by the control
   plane that spawned it. So the file it writes to is a pipe on this machine,
   and the check is that the reader has no socket of its own.

What this cannot check is the kernel, the filesystem, or a person with a copy of
the file. It checks the code, which is the part that could change without
anybody deciding to.
"""

import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
CRATES = ROOT / "core" / "crates"

# The crate that owns the file. Nothing in it may reach outside the filesystem.
OWNER = CRATES / "esper-audit"

# Where a reader is allowed to be, and where what it reads goes.
#
# The path is relative to `core/crates`. Every entry is a claim somebody can
# check by reading the file named.
READERS = {
    "esper-engine/src/serve.rs": (
        "answers `esper sessions` over this process's stdout, which is a pipe "
        "held by the control plane that spawned it — see the module's own note "
        "on the seam"
    ),
}

# What "can reach a network" looks like in Rust source. Deliberately broad: a
# false positive here is a comment somebody has to justify, and a false
# negative is the whole point of the check going quiet.
REACHES_OUT = [
    (r"\bstd::net\b", "std::net"),
    (r"\bTcpStream\b|\bTcpListener\b|\bUdpSocket\b", "a socket type"),
    (r"\bUnixStream\b|\bUnixListener\b|\bUnixDatagram\b", "a unix socket"),
    (r"\btokio::net\b", "tokio::net"),
    (r"\bquinn\b", "quinn"),
    (r"\breqwest\b|\bhyper\b|\bhttp\b", "an HTTP client"),
    (r"\bCommand::new\b|\bprocess::Command\b", "a subprocess"),
    (r"\besper_transport\b", "the transport crate"),
    (r"\besper_rendezvous\b", "the rendezvous crate"),
]

# Comments and documentation mention these words on purpose — this file's own
# ledger says "socket", and the audit crate's documentation talks about what it
# does not do. Stripped before matching so prose cannot fail the check and,
# more importantly, cannot *pass* it either.
COMMENT = re.compile(r"^\s*(//|///|//!|\*)")


def code_of(path: Path) -> str:
    """The file's code, with comment lines removed."""
    return "\n".join(
        line for line in path.read_text().splitlines() if not COMMENT.match(line)
    )


def sources(crate: Path):
    """Every Rust source in a crate's `src`, which is what ships."""
    return sorted((crate / "src").rglob("*.rs"))


def main() -> int:
    problems = []
    checked = 0

    # 1. The owner cannot reach out.
    if not OWNER.is_dir():
        print(f"  ✗ {OWNER} is not there — this check has drifted, not the code")
        return 1
    for path in sources(OWNER):
        checked += 1
        code = code_of(path)
        for pattern, what in REACHES_OUT:
            if re.search(pattern, code):
                problems.append(
                    f"{path.relative_to(ROOT)} mentions {what}: the crate that owns "
                    f"the trail must reach nothing but its own file"
                )

    # 2. and 3. Only the ledger's readers, and each of them local.
    readers_found = {}
    reads = re.compile(r"\.(read|for_peer)\s*\(")
    log_at = re.compile(r"Log::at|esper_audit::Log")
    for crate in sorted(CRATES.iterdir()):
        if not (crate / "src").is_dir():
            continue
        for path in sources(crate):
            rel = str(path.relative_to(CRATES))
            code = code_of(path)
            # A reader is a file that opens the log *and* reads from it. Both,
            # because `.read()` is a common method name and `Log::at` alone is
            # how a writer opens it.
            if log_at.search(code) and reads.search(code):
                readers_found[rel] = path

    for rel, path in sorted(readers_found.items()):
        checked += 1
        if rel not in READERS:
            problems.append(
                f"{rel} reads the trail and is not in this check's ledger. Add it "
                f"with where what it reads goes — and if the answer is 'off this "
                f"machine', that is the bug this check exists to find"
            )
            continue
        code = code_of(path)
        for pattern, what in REACHES_OUT:
            if re.search(pattern, code):
                problems.append(
                    f"{rel} reads the trail and mentions {what}. The ledger says it "
                    f"answers over stdout; a socket in the same file is a path this "
                    f"check cannot rule out"
                )

    for rel in READERS:
        if rel not in readers_found:
            problems.append(
                f"{rel} is in the ledger and reads nothing. Remove the entry: a "
                f"stale ledger is how a check stops being read"
            )

    # And the front door, which is what a person runs. It prints the trail; it
    # must not post it anywhere.
    front = ROOT / "cmd" / "esper" / "main.go"
    if front.is_file():
        checked += 1
        go = front.read_text()
        for pattern, what in [
            (r'"net/http"', "net/http"),
            (r'"net"\n', "net"),
            (r"http\.(Post|Get|Client)", "an HTTP call"),
        ]:
            if re.search(pattern, go):
                problems.append(
                    f"cmd/esper/main.go mentions {what}: the command that prints the "
                    f"trail must not be able to send it"
                )

    print(f"  {checked} file(s) checked, {len(readers_found)} reader(s) of the trail")
    for rel, why in sorted(READERS.items()):
        if rel in readers_found:
            print(f"    {rel} — {why}")
    if problems:
        print()
        for p in problems:
            print(f"  ✗ {p}")
        print(f"\n  {len(problems)} problem(s). See the header of this script for why.")
        return 1
    print("  ✓ the trail is read in one place, and that place has no way off this machine")
    return 0


if __name__ == "__main__":
    sys.exit(main())
