#!/usr/bin/env python3
"""The words Rust and Go both use for the same things are the same words.

Esper is two implementations of one product. Rust owns media, crypto and the
device store's writer; Go owns the control plane and reads that store. Where
they both name something, they name it twice — and two copies of a vocabulary
drift silently.

# Why drift here is not cosmetic

**A permission.** Rust deserialises `permissions` into an enum, so a word it
does not know fails the whole file. Go reads the same file. Add a permission in
Rust and not in Go, and the first store containing it makes the device list
report a broken trust store on a machine where nothing is wrong. Add one in Go
and not in Rust and it is worse: Go shows a device holding a permission that
nothing enforces.

**A lock-after choice.** The same, for what happens to the screen after a
session.

**The fingerprint.** The digest is what two people read to each other to
confirm they are looking at the same device. If the domain separator or the
word list differ by one character, the two halves of the product render the
same key as different codes — and the failure mode is that somebody decides
the device is not the one they meant.

Fixtures and unit tests catch a mismatch on both sides too. This catches it
without running anything, and it catches the direction the fixtures cannot: a
word added to Rust that Go has never seen, in a store no fixture holds yet.
"""

import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

RUST_PERMISSION = ROOT / "core" / "crates" / "esper-proto" / "src" / "message.rs"
RUST_DEVICE = ROOT / "core" / "crates" / "esper-crypto" / "src" / "device.rs"
RUST_FINGERPRINT = ROOT / "core" / "crates" / "esper-crypto" / "src" / "fingerprint.rs"
RUST_WORDLIST = ROOT / "core" / "crates" / "esper-crypto" / "src" / "wordlist.rs"

GO_PERMISSION = ROOT / "internal" / "devices" / "permission.go"
GO_WORDLIST = ROOT / "internal" / "devices" / "wordlist.go"

# `Self::View => "view",` in a `name()`/`word()` match arm.
RUST_ARM = re.compile(r'Self::(\w+)\s*=>\s*"([a-z_]+)"')
# `View Permission = "view"` or `LockAlways LockAfter = "always"`.
GO_CONST = re.compile(r'^\s*(\w+)\s+(Permission|LockAfter)\s*=\s*"([a-z_]+)"', re.M)
# The quoted words of a Rust or Go frozen string table.
QUOTED = re.compile(r'"([a-z]+)"')


def rust_arms(path: Path, after: str) -> list[str]:
    """The words of the match block that follows a marker, in order."""
    text = path.read_text()
    start = text.find(after)
    if start < 0:
        raise SystemExit(f"{path.name} no longer contains {after!r}; this check is stale")
    # To the end of the match block: the first line that closes it at the
    # arm's own indentation.
    end = text.find("\n        }", start)
    return [word for _, word in RUST_ARM.findall(text[start:end])]


def go_words(kind: str) -> list[str]:
    """The words of a Go const block, in declaration order."""
    return [word for _, k, word in GO_CONST.findall(GO_PERMISSION.read_text()) if k == kind]


def rust_wordlist() -> list[str]:
    text = RUST_WORDLIST.read_text()
    start = text.find("WORDS: [&str; WORDLIST_LEN] = [")
    if start < 0:
        raise SystemExit("the Rust word list is no longer a WORDLIST_LEN table; this check is stale")
    return QUOTED.findall(text[start : text.find("\n];", start)])


def go_wordlist() -> list[str]:
    text = GO_WORDLIST.read_text()
    start = text.find("var words = [256]string{")
    if start < 0:
        raise SystemExit("the Go word list is no longer a 256-entry table; this check is stale")
    return QUOTED.findall(text[start : text.find("\n}", start)])


def rust_string_const(path: Path, name: str) -> str:
    """The value of a `pub const NAME: &[u8] = b"...";`."""
    found = re.search(rf'{name}:\s*&\[u8\]\s*=\s*b"([^"]+)"', path.read_text())
    if not found:
        raise SystemExit(f"{path.name} no longer declares {name}; this check is stale")
    return found.group(1)


def go_string_const(path: Path, name: str) -> str:
    found = re.search(rf'{name}\s*=\s*"([^"]+)"', path.read_text())
    if not found:
        raise SystemExit(f"{path.name} no longer declares {name}; this check is stale")
    return found.group(1)


def compare(what: str, rust: list[str], go: list[str]) -> list[str]:
    """Complain about every way two lists of words can differ."""
    problems = []
    for missing in [w for w in rust if w not in go]:
        problems.append(
            f"{what}: Rust has {missing!r} and Go does not. A store holding it would read "
            f"as a broken trust store on a machine where nothing is wrong"
        )
    for extra in [w for w in go if w not in rust]:
        problems.append(
            f"{what}: Go has {extra!r} and Rust does not. Go would show something the "
            f"other half of the product does not enforce"
        )
    if not problems and rust != go:
        problems.append(
            f"{what}: the same words in a different order — Rust {rust}, Go {go}. "
            f"Order is what a listing and a fixture are compared in"
        )
    return problems


def main() -> int:
    problems: list[str] = []
    counted: list[str] = []

    permissions_rust = rust_arms(RUST_PERMISSION, "pub const fn name(self) -> &'static str {")
    permissions_go = go_words("Permission")
    problems += compare("permissions", permissions_rust, permissions_go)
    counted.append(f"{len(permissions_rust)} permissions")

    lock_rust = rust_arms(RUST_DEVICE, "pub const fn word(self) -> &'static str {")
    lock_go = go_words("LockAfter")
    problems += compare("what to do after a session", lock_rust, lock_go)
    counted.append(f"{len(lock_rust)} lock-after choices")

    domain_rust = rust_string_const(RUST_FINGERPRINT, "DOMAIN")
    domain_go = go_string_const(GO_WORDLIST, "FingerprintDomain")
    if domain_rust != domain_go:
        problems.append(
            f"the fingerprint domain separator differs — Rust {domain_rust!r}, "
            f"Go {domain_go!r}. The two halves would render one key as two codes"
        )
    counted.append("1 domain separator")

    words_rust, words_go = rust_wordlist(), go_wordlist()
    if len(words_rust) != 256 or len(words_go) != 256:
        problems.append(
            f"a fingerprint word list is not 256 words — Rust {len(words_rust)}, "
            f"Go {len(words_go)}. A byte would have no word, or two"
        )
    elif words_rust != words_go:
        differ = [
            f"{i}: Rust {r!r}, Go {g!r}"
            for i, (r, g) in enumerate(zip(words_rust, words_go))
            if r != g
        ]
        problems.append(
            "the fingerprint word lists differ at "
            + "; ".join(differ[:5])
            + (f" (and {len(differ) - 5} more)" if len(differ) > 5 else "")
        )
    counted.append(f"{len(words_rust)} fingerprint words")

    if problems:
        print()
        for line in problems:
            print(f"  ✗ {line}")
        print()
        print(f"  {len(problems)} problem(s). See the header of this script for why.")
        return 1

    print("  " + ", ".join(counted))
    print("  ✓ Rust and Go use the same words for the same things")
    return 0


if __name__ == "__main__":
    sys.exit(main())
