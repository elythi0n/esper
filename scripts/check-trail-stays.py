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

3. **Each reader is itself local, and says how it knows.** The Rust reader
   answers `esper sessions` over this process's *stdout*, which is a pipe held
   by the control plane that spawned it — so the check there is that it has no
   socket of its own. The Go packages that do open a socket are listed in
   `SERVERS` below, and each has to contain the mechanisms it claims to be
   protected by; the list says what absence would mean.

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


# The Go packages allowed to serve the trail over a socket, and what each must
# prove about itself.
#
# **This list is the honest version of the claim.** Before the local API the
# claim was "the log never leaves the machine, and there is no code path by
# which it could", and it was true. A local API changes it: a dashboard and an
# MCP adapter both need what happened on this machine, and both are local. So
# does a supervisor watching the host it started. The claim is now narrower and
# still mechanical — the log leaves this *process* only where something below
# says what it demands of whoever asks, and every one of those demands is a
# pattern that has to be in the source rather than a sentence in a comment.
#
# The two are guarded differently because they are reached differently. The API
# is a port, so it is loopback by default and gated by a token compared in
# constant time. The supervision channel is a unix socket in a directory nobody
# else can enter, and the kernel is asked which process connected — a stronger
# answer than a token, and available only because the asker is a child of the
# process that made the socket.
SERVERS = {
    "internal/server": {
        "why": (
            "the local API: what a dashboard, a desktop app or an MCP adapter "
            "asks this machine about itself"
        ),
        # Each entry: a pattern that must appear, and what its absence would mean.
        "must": [
            (
                r'DefaultAddr\s*=\s*"127\.0\.0\.1:',
                "bind loopback by default — a listener that defaults anywhere else "
                "exposes the trail to a network by omission",
            ),
            (
                r"subtle\.ConstantTimeCompare",
                "compare its token in constant time — every process on this machine "
                "can reach loopback, so the token is the whole of the access control, "
                "and a compare that returns early leaks it",
            ),
            (
                r"--exposed",
                "refuse to bind off loopback without an explicit flag, and name it in "
                "the refusal, so nobody exposes the trail by mistyping an address",
            ),
        ],
    },
    "internal/engine": {
        "why": (
            "the channel a supervised host answers on — it carries whether the "
            "host is working and nothing from the trail, and is listed here "
            "because a socket in this tree has to say what guards it"
        ),
        "must": [
            (
                r'net\.Listen\("unix"',
                "listen on a unix socket rather than a port — an address family "
                "that cannot leave this machine is a stronger statement than "
                "binding loopback, and this channel has no reason to be "
                "reachable at all",
            ),
            (
                r"MkdirTemp",
                "put that socket in a directory of its own, created 0700, so the "
                "filesystem is the first thing keeping other people out",
            ),
            (
                r"SO_PEERCRED",
                "ask the kernel which process connected and refuse anything but "
                "the child it started — every process belonging to the same "
                "person can reach that socket, so without this check another "
                "program could answer a health check and hand back an extract "
                "of the trail on the engine's behalf",
            ),
        ],
    },
}


def go_side(problems: list[str]) -> int:
    """The Go control plane: it may render the trail, and serve it only so far.

    What this cannot check is what a *caller* does with the answer. A program on
    this machine that can read the token file can already read the log itself,
    which is the argument that makes the API's existence acceptable — not the
    absence of one.
    """
    checked = 0
    go_root = ROOT / "internal"
    for path in sorted(go_root.rglob("*.go")):
        if path.name.endswith("_test.go"):
            continue
        package = str(path.parent.relative_to(ROOT))
        text = path.read_text()
        if not re.search(r'"net/http"|"net"|net\.Listen', text):
            continue
        checked += 1
        if package not in SERVERS:
            problems.append(
                f"{path.relative_to(ROOT)} can open a socket and is not in this "
                f"check's list of packages allowed to serve the trail. Add it with "
                f"what it is for and what it proves about itself — and if it cannot "
                f"prove loopback, a token and an explicit opt-out, it must not have "
                f"the trail"
            )
    for package, rules in SERVERS.items():
        source = "\n".join(
            p.read_text()
            for p in sorted((ROOT / package).glob("*.go"))
            if not p.name.endswith("_test.go")
        )
        if not source:
            problems.append(
                f"{package} is in this check's list and has no source: a stale list "
                f"is how a check stops being read"
            )
            continue
        checked += 1
        for pattern, must in rules["must"]:
            if not re.search(pattern, source):
                problems.append(f"{package} does not {must}")

    # The front door itself must not listen: it renders the trail, and a second
    # listener in the same binary would be one nobody had reasoned about.
    front = ROOT / "cmd" / "esper" / "main.go"
    if front.is_file():
        checked += 1
        go = front.read_text()
        if re.search(r"net\.Listen|http\.ListenAndServe|http\.Serve\b", go):
            problems.append(
                "cmd/esper/main.go opens a listener of its own: the one place that "
                "may serve the trail is in the list above, where what it has to "
                "prove is written down"
            )
        if re.search(r"http\.(Post|Get|Client)", go):
            problems.append(
                "cmd/esper/main.go makes an HTTP request: the command that prints "
                "the trail must not be able to send it anywhere"
            )
    return checked


def enclosing_fn(lines: list[str], at: int) -> tuple[str, str]:
    """The function a line is inside: its visibility and its name.

    Nearest preceding `fn`, which is enough here because these files have no
    nested functions around the log.
    """
    for i in range(at, -1, -1):
        m = re.match(r"\s*(pub(?:\([^)]*\))?\s+)?(?:async\s+)?fn\s+(\w+)", lines[i])
        if m:
            return (m.group(1) or "").strip(), m.group(2)
    return "", ""


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
        # **And the reading has to stay in the file.** Checking one file for
        # sockets only means anything while nothing else can call the function
        # that reads: a `pub fn` here lets any module in the crate route the
        # trail wherever it likes, and this check follows no call graph.
        #
        # Found the hard way. The supervision channel wanted to answer the
        # same question as `esper sessions`, so this function was made `pub`
        # and called from a file holding a unix socket — and every part of
        # this check went on passing.
        lines = code.splitlines()
        for i, line in enumerate(lines):
            if not log_at.search(line):
                continue
            visibility, name = enclosing_fn(lines, i)
            if visibility:
                problems.append(
                    f"{rel} reads the trail inside `{visibility} fn {name}`. It has to "
                    f"be private: this check reads one file for sockets, which proves "
                    f"nothing if anything in the crate can call the reader and send "
                    f"the answer somewhere else"
                )

    for rel in READERS:
        if rel not in readers_found:
            problems.append(
                f"{rel} is in the ledger and reads nothing. Remove the entry: a "
                f"stale ledger is how a check stops being read"
            )

    # And the Go control plane, which is where the trail is *rendered* — and,
    # since the local API, where it can also be served.
    checked += go_side(problems)

    print(f"  {checked} file(s) checked, {len(readers_found)} reader(s) of the trail")
    for package, rules in sorted(SERVERS.items()):
        print(f"    {package} — {rules['why']}")
    for rel, why in sorted(READERS.items()):
        if rel in readers_found:
            print(f"    {rel} — {why}")
    if problems:
        print()
        for p in problems:
            print(f"  ✗ {p}")
        print(f"\n  {len(problems)} problem(s). See the header of this script for why.")
        return 1
    print(
        "  ✓ the trail is read in one place, and leaves this process only where "
        "something above says what it demands of whoever asks"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
