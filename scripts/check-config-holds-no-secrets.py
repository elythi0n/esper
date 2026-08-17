#!/usr/bin/env python3
"""No secret is a setting, and the device store is not config.

# Why this is a gate and not a warning

The tracker asked for a *warning* when something credential-shaped turns up in
the config file. A warning is the wrong instrument. By the time a private key
is in `config.toml` it has been written to a file with ordinary permissions,
very likely committed to somebody's dotfiles repository, and a line of output
at startup does not take it back.

What can be prevented is the thing that puts it there: a **setting** that
invites it. Nobody writes a private key into a config file unmarked; they write
it because there is a key for it. So the rule is about the settings table
rather than about anybody's file, and it is checkable before a release rather
than after an incident.

# Two rules

**One: no setting may be credential-shaped.** A key whose name is `secret`,
`token`, `password`, `passphrase`, `private_key` or similar is an invitation,
and this refuses it. The one exception is listed below with its reason, and
listing it is the point: an exception nobody had to argue for is how the rule
stops meaning anything.

**Two: the device store is not reachable from config.** The file that says who
this machine trusts has a separate lifecycle from the file that says how it
behaves — a config that could name it would let somebody point the trust store
at a file they control, which is the whole security model in one line. So no
setting may name it, and no setting's value may be used to find it.

# What this cannot check

Whether somebody has put a secret in a config file *anyway*, under a key meant
for something else. Nothing here reads anybody's file, deliberately: a checker
that opened `~/.esper/config.toml` looking for secrets would be a program that
reads secrets.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
TABLE = ROOT / "core" / "crates" / "esper-config" / "src" / "settings.rs"

# Words that make a setting an invitation to paste a credential.
#
# `key` on its own is deliberately *not* here: it is the commonest word in this
# tree for a **public** key, which is a thing to publish rather than protect,
# and a rule that refused it would be a rule everybody worked around.
CREDENTIAL_SHAPED = re.compile(
    r"(secret|password|passphrase|token|credential|private[_.]?key|api[_.]?key)",
    re.IGNORECASE,
)

# Settings that look credential-shaped and are not, each with the argument.
#
# Adding one without a reason is how this check stops meaning anything.
EXCUSED: dict[str, str] = {
    "broker.key": (
        "a broker's *public* key, pinned by whoever uses that broker. It is "
        "printed by `esper-broker serve` on startup for anybody to copy, and "
        "pinning it is what stops somebody else's broker being used instead. "
        "Publishing it is the intended use."
    ),
}

# Names of the file that says who this machine trusts.
STORE_NAMES = ("devices.json", "device_store", "devices_path")


def declared() -> list[tuple[str, str]]:
    """Every setting, as (key, the text of its declaration)."""
    text = TABLE.read_text()
    blocks = re.findall(r"Setting\s*\{(.*?)\n    \}", text, re.S)
    out: list[tuple[str, str]] = []
    for b in blocks:
        found = re.search(r'key: "([a-z_.]+)"', b)
        if found:
            out.append((found.group(1), b))
    return out


def main() -> int:
    problems: list[str] = []
    settings = declared()

    # **A floor.** A parser that matched nothing would report a clean table in
    # exactly the words of a clean table, and every rule below would pass over
    # an empty list.
    if len(settings) < 20:
        sys.exit(
            f"check-config-holds-no-secrets: only {len(settings)} settings were found in "
            f"{TABLE.name}, which is too few to be the real table — the parser has broken"
        )

    # One: nothing credential-shaped, except what is argued for.
    for key, _ in settings:
        hit = CREDENTIAL_SHAPED.search(key)
        if not hit:
            continue
        if key in EXCUSED:
            continue
        problems.append(
            f"  ✗ `{key}` is credential-shaped ({hit.group(1)!r}).\n"
            f"      A setting is an invitation: nobody pastes a secret into a config file\n"
            f"      unless there is a key asking for one. Take the value from the\n"
            f"      environment, from a file named by a setting, or from the keychain —\n"
            f"      or add it to EXCUSED with the argument for why it is not a secret."
        )

    # And the exceptions have to still exist, or the list is a fiction that
    # grows.
    keys = {k for k, _ in settings}
    for excused in EXCUSED:
        if excused not in keys:
            problems.append(
                f"  ✗ `{excused}` is excused and is not a setting any more.\n"
                f"      Remove it from EXCUSED: an exception for something that does not\n"
                f"      exist is an exception nobody will notice becoming wrong."
            )

    # Two: the device store is not config.
    for key, block in settings:
        for name in STORE_NAMES:
            if name in block:
                problems.append(
                    f"  ✗ `{key}` names the device store ({name}).\n"
                    f"      The file that says who this machine trusts has a lifecycle of its\n"
                    f"      own, and a setting that could point at it would let a config file\n"
                    f"      choose the trust store — which is the security model."
                )
        # **Plural, not singular.** `device.name` is what *this* machine calls
        # itself, which is a preference and belongs here. `devices.*` would be
        # about the list of machines it trusts, which does not — and the two
        # differ by one letter, which is exactly why this is written down
        # rather than left to whoever reads the key next.
        if key.startswith("devices.") or any(w in key for w in ("paired", "trust")):
            problems.append(
                f"  ✗ `{key}` is a setting about the machines this one trusts.\n"
                f"      Those belong in the device store, which `esper grant` and\n"
                f"      `esper revoke` write and which is not config."
            )

    if problems:
        print("  ✗ the settings table invites something it should not:\n")
        print("\n\n".join(problems))
        return 1

    print(
        f"  ✓ {len(settings)} settings, none credential-shaped "
        f"({len(EXCUSED)} argued exception(s)), and none naming the device store"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
