#!/usr/bin/env python3
"""`unsafe` appears only where FFI needs it, and every block says why.

`security-engineering.md` has said this since it was written:

> **`#![forbid(unsafe_code)]` at the top of every crate that does not need
> FFI.** Not `deny` — `forbid`, so it cannot be locally overridden.
>
> Crates that do need it carry an **unsafe inventory**: every block documented
> with what invariant it relies on and why it holds. Reviewed as a unit.

The workspace implements the first half by setting `unsafe_code = "forbid"` in
`[workspace.lints.rust]`, which every crate inherits with `lints.workspace =
true`. That is stricter than the design says, and it made the carve-out
impossible rather than deliberate: `forbid` cannot be relaxed by an inner
`allow`, so a crate needing FFI has to stop inheriting the workspace lints —
and a crate that stops inheriting them loses `unwrap_used`, `expect_used`,
`panic` and `indexing_slicing` at the same time, silently.

So this script is the second half, and it is what makes opting out safe:

1. **Every crate inherits the workspace lints, except those listed below.**
   A crate quietly dropping `lints.workspace = true` is the escape hatch this
   exists to close.
2. **A listed crate must re-declare every workspace lint, whole.** Compared
   against the workspace's own tables, in both directions, so the two cannot
   drift and the crate cannot relax something else on the way past.
   `unsafe_code` is the single permitted difference, and it must be `"deny"` —
   not `allow`, and not absent — so a block is allowed at its own site only.
3. **`unsafe` appears only in listed crates.** Anywhere else it cannot compile
   anyway; this catches it before the compiler does and says which rule it
   broke.
4. **Every `unsafe` block is accounted for by an inventory entry.** The file
   holding blocks must carry a `# Unsafe inventory` section in its module
   documentation, with one bullet per block. Counted, so adding a block
   without documenting the invariant it rests on fails.

The inventory is per file rather than per crate so that the documentation sits
next to the code it describes, and so the count is checkable.
"""

import re
import sys
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
CORE = ROOT / "core"
CRATES = CORE / "crates"

# crate -> why it cannot be written without FFI. Every entry is a claim
# somebody can check, and the list is meant to stay very short.
NEEDS_FFI: dict[str, str] = {
    "esper-capture-win": (
        "Desktop Duplication is COM. D3D11CreateDevice, AcquireNextFrame and "
        "the staging-texture readback have no pure-Rust equivalent, and the "
        "damage metadata is the reason this API was chosen over the newer one"
    ),
    "esper-input-uinput": (
        "`/dev/uinput` is configured entirely through `ioctl`, and an ioctl is "
        "a syscall with a caller-supplied opcode and a caller-supplied pointer. "
        "No crate can make that safe, because the safety condition is a fact "
        "about the opcode that no type carries"
    ),
    "esper-encode-retune": (
        "Changing a setting on a *running* openh264 encoder has one route: "
        "`Encoder::raw_api`, which is unsafe because the binding's own "
        "`reinit` is private. Rebuilding instead costs a keyframe, which is "
        "the largest frame on the link least able to carry one"
    ),
    "esper-input-win": (
        "SendInput, GetSystemMetrics and SetProcessDpiAwarenessContext are "
        "user32 calls. There is no pure-Rust path to synthesising input on "
        "Windows the way x11rb is one on X11"
    ),
}

def rust_files():
    """Every Rust file in every crate — `tests/` and `benches/` included.

    **Not only `src/`, and that was a hole.** A test is code that runs, links
    against the same crate and is read by the same reviewer; an `unsafe` block
    hidden in `tests/` was invisible to this script while being just as much a
    thing somebody has to justify. Found by writing one: a tripwire test that
    pokes a dependency through its raw API sat outside every count here and the
    gate went on saying every block was accounted for.
    """
    for where in ("src", "tests", "benches"):
        yield from CRATES.glob(f"*/{where}/**/*.rs")


# An `unsafe` block, and an inventory bullet.
UNSAFE_BLOCK = re.compile(r"\bunsafe\s*\{")
UNSAFE_OTHER = re.compile(r"\bunsafe\s+(fn|impl|trait)\b")
INVENTORY_HEAD = re.compile(r"^//!\s*#+\s*Unsafe inventory\s*$", re.M)
INVENTORY_ITEM = re.compile(r"^//!\s*-\s+\*\*", re.M)


def workspace_lints() -> dict[str, dict]:
    """The lint tables the workspace sets, for a crate to be held to."""
    with (CORE / "Cargo.toml").open("rb") as f:
        manifest = tomllib.load(f)
    return manifest.get("workspace", {}).get("lints", {})


def crate_manifest(crate: str) -> dict:
    with (CRATES / crate / "Cargo.toml").open("rb") as f:
        return tomllib.load(f)


def main() -> int:
    problems: list[str] = []
    workspace = workspace_lints()
    want_clippy = workspace.get("clippy", {})
    want_rust = dict(workspace.get("rust", {}))
    if want_rust.pop("unsafe_code", None) != "forbid":
        raise SystemExit(
            "the workspace no longer sets `unsafe_code = \"forbid\"`, which is the "
            "rule this check exists to carve one hole in; it is stale"
        )
    if not any(v == "deny" for v in want_clippy.values()):
        raise SystemExit(
            "the workspace sets no clippy lint to deny, so this check has nothing "
            "to hold an opted-out crate to; it is stale"
        )

    crates = sorted(p.name for p in CRATES.iterdir() if (p / "Cargo.toml").is_file())
    listed = set(NEEDS_FFI)

    for crate in crates:
        manifest = crate_manifest(crate)
        lints = manifest.get("lints", {})
        inherits = lints.get("workspace") is True

        if crate not in listed:
            if not inherits:
                problems.append(
                    f"{crate} does not set `lints.workspace = true`, so it does not "
                    f"inherit `unsafe_code = \"forbid\"` — nor `unwrap_used`, "
                    f"`expect_used`, `panic` or `indexing_slicing`. Add it, or add the "
                    f"crate to NEEDS_FFI in this script with the reason"
                )
            continue

        # A listed crate must re-declare what it gave up.
        if inherits:
            problems.append(
                f"{crate} is listed as needing FFI and still inherits the workspace "
                f"lints, where `unsafe_code` is `forbid` — which cannot be relaxed "
                f"locally, so the crate cannot contain FFI. Remove it from NEEDS_FFI "
                f"or stop inheriting"
            )
            continue
        rust = lints.get("rust", {})
        level = rust.get("unsafe_code")
        if level != "deny":
            problems.append(
                f"{crate} sets `unsafe_code` to {level!r}; it must be \"deny\", so "
                f"that each block is allowed at its own site and nowhere else"
            )
        # **Every workspace lint, not only the denials.** Opting out of
        # `forbid(unsafe_code)` must not quietly opt out of anything else, so
        # the two tables are compared whole and `unsafe_code` is the single
        # permitted difference.
        theirs_clippy = lints.get("clippy", {})
        for lint, want in want_clippy.items():
            if theirs_clippy.get(lint) != want:
                problems.append(
                    f"{crate} opted out of the workspace lints and sets clippy "
                    f"`{lint}` to {theirs_clippy.get(lint)!r} rather than {want!r}"
                )
        for lint in set(theirs_clippy) - set(want_clippy):
            problems.append(
                f"{crate} sets clippy `{lint}`, which the workspace does not. A crate "
                f"that opted out to hold FFI should not also be relaxing other rules"
            )
        theirs_rust = {k: v for k, v in rust.items() if k != "unsafe_code"}
        if theirs_rust != want_rust:
            problems.append(
                f"{crate}'s `[lints.rust]` is {theirs_rust!r} and the workspace's is "
                f"{want_rust!r}, ignoring `unsafe_code`. They must match: opting out "
                f"for FFI is not a reason to differ about anything else"
            )

    # Where `unsafe` actually is.
    for source in sorted(rust_files()):
        crate = source.relative_to(CRATES).parts[0]
        text = source.read_text()
        blocks = len(UNSAFE_BLOCK.findall(text))
        others = UNSAFE_OTHER.findall(text)
        shown = source.relative_to(ROOT)

        if crate not in listed:
            if blocks or others:
                problems.append(
                    f"{shown} contains `unsafe` and {crate} is not listed as needing "
                    f"FFI. If it does, add it to NEEDS_FFI with the reason; if not, "
                    f"this is the rule the whole engine rests on"
                )
            if "allow(unsafe_code)" in text:
                problems.append(
                    f"{shown} allows `unsafe_code` and {crate} is not listed as "
                    f"needing FFI"
                )
            continue

        if not blocks and not others:
            continue

        # A file with blocks must inventory them.
        if not INVENTORY_HEAD.search(text):
            problems.append(
                f"{shown} has {blocks} `unsafe` block(s) and no `# Unsafe inventory` "
                f"section in its module documentation. Every block is documented with "
                f"the invariant it relies on and why it holds — that is what makes the "
                f"carve-out reviewable as a unit"
            )
            continue
        entries = len(INVENTORY_ITEM.findall(text))
        if entries != blocks:
            problems.append(
                f"{shown} has {blocks} `unsafe` block(s) and {entries} inventory "
                f"entr{'y' if entries == 1 else 'ies'}. One each, so a block added "
                f"without its invariant fails rather than passing quietly"
            )

    if problems:
        print()
        for line in problems:
            print(f"  ✗ {line}")
        print()
        print(f"  {len(problems)} problem(s). See the header of this script for why.")
        return 1

    inventoried = sum(
        len(UNSAFE_BLOCK.findall(s.read_text()))
        for s in rust_files()
        if s.relative_to(CRATES).parts[0] in listed
    )
    print(
        f"  {len(crates)} crates: {len(crates) - len(listed)} forbid unsafe outright, "
        f"{len(listed)} need FFI and inventory {inventoried} block(s)"
    )
    print("  ✓ unsafe appears only where FFI needs it, and every block says why")
    return 0


if __name__ == "__main__":
    sys.exit(main())
