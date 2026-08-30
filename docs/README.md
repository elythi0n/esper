# Documentation

Start at the [README](../README.md) if you want to install Esper and connect to
something. These are the longer documents.

## For somebody using it

| | |
|---|---|
| [`broker.md`](broker.md) | Running a broker, if you need one — and why most people do not. Says which of its own commands have been run and which have not. |
| [`compatibility.md`](compatibility.md) | Which releases talk to which. The release number and the protocol version are different things and change for different reasons. |

## For somebody deciding whether to trust it

| | |
|---|---|
| [`../SECURITY.md`](../SECURITY.md) | What the project claims, what it does not, which dependencies are unaudited, and how to report a vulnerability. |
| [`threat-model.md`](threat-model.md) | Who might attack it, from where, and what stops them — or does not. Including the boundary Wayland does *not* close on wlroots compositors. |

## For somebody implementing or auditing it

| | |
|---|---|
| [`protocol.md`](protocol.md) | What goes on the wire, written so that somebody who is not this project could implement it. Esper has one implementation, and that is a gap. |
| [`../packaging/README.md`](../packaging/README.md) | How the packages are built and what is checked about them. |
| [`releasing.md`](releasing.md) | What a release is, what reproducibility does and does not cover, and which steps have actually been run. |

## For somebody working on it

[`../CONTRIBUTING.md`](../CONTRIBUTING.md) has the rules and the tests that need
a display. What follows is the part that is unusual about this repository, and
the part most likely to waste your afternoon if nobody tells you.

### The checks are the argument

`make ci` runs the ordinary things. It also runs about twenty checks with names
like `check-one-place`, `check-reached` and `check-vocabularies`, and those are
not lint: each one holds a **claim the code makes about itself**. A comment
saying "the only place a forwarded connection is dialled" is checked. A
permission type with no production caller fails. A word that Rust and Go spell
differently fails.

If one of them refuses your change, read what it says before working around it.
Several of them exist because a comment and its code had drifted apart and
nobody could tell which was right.

### A check must be shown to fail

The rule this project keeps relearning: **sabotage every guard you write.**
Break the thing it protects, watch it fail, put it back. A check that has never
failed has not been tested — and several in this tree passed over an empty set,
or asserted something true of both the right and the wrong implementation,
until somebody tried.

Two specific traps, both of which caught this project:

- **A check that builds a probe should build it unconditionally.** A sabotage
  that fails to compile otherwise leaves the previous binary in place, and the
  check passes against code that is no longer in the tree.
- **A comparison needs something to compare.** A capture checked against
  another tool on a blank grey screen cannot detect a red/blue channel swap,
  because red and blue are equal there.

### Things this machine is not

Several claims are about *other* machines, and there is a container for each so
that nothing has to be installed on yours.

| | What it is for | Costs |
|---|---|---|
| `make check-install` | The `.deb` opened by a real `dpkg`, installed, run and removed on Debian — and refused by a Debian too old for it | minutes |
| `make check-rpm` | The spec built by a real `rpmbuild` on Fedora, then installed on a clean one | tens of minutes: it compiles the whole workspace |
| `make check-wayland` | A real sway session and a real GNOME one: the protocols, the portals, a key injected, a frame captured | minutes |
| `make check-dbus` | This project's own D-Bus checked against `dbus-send` on a real session bus | minutes |
| `make check-images` | Every container image pinned to a digest somebody reviewed | seconds, no containers |

Every image is written down once in [`../packaging/images.tsv`](../packaging/images.tsv)
with a digest and a reason, and
[`../scripts/in-container.sh`](../scripts/in-container.sh) is the only place a
container starts — it takes the network away, drops every capability, and
mounts read-only unless a call site asks otherwise and says why.

### Writing a dependency instead of taking one

This project has declined large dependencies more than once and taken them
other times, and the test is the same each time: **is the dependency
proportionate to the job?** It speaks mDNS and D-Bus itself, because each was
tens of thousands of lines to do something narrow. It takes `x11rb` and
`wayland-client`, because speaking a display server's protocol is not narrow.

If you are about to add one, measure it first — crates, lines and `unsafe`
occurrences *not already in the tree* — and put the number in the commit
message. The argument is much shorter once there is a number in it.
