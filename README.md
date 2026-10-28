<p align="center">
  <img src="docs/assets/banner.png" alt="Esper: no account, no password, no server" width="100%">
</p>

<p align="center">
  Secure, private, open-source remote desktop.
  <br>
  <strong><a href="https://esper.st">esper.st</a></strong> · <a href="docs/">Docs</a> · <a href="SECURITY.md">Security</a>
</p>

🚧 **Early development.** Esper works end to end on Linux desktops running X11
or Sway. There's no release to download yet, and GNOME on Wayland, Windows and
macOS are still to come. [Features](#features) lists exactly what's ready.

---

## What it is

Esper lets you see and control another computer, like TeamViewer, but without
anyone else's servers in between.

You pair two devices once, with a short code. After that they recognise each
other on their own. There's no account and no password, and Esper has no
server address built in, so there's nothing to opt out of.

How Esper reaches another machine:

- **On a local network**, devices find each other.
- **On an overlay network** (Tailscale, WireGuard, ZeroTier, Nebula), Esper
  detects it and connects over it directly.
- **Anywhere else**, you can run your own broker. If you don't, Esper won't
  route you through anyone else's.

Every connection is encrypted end to end between the two devices, whichever
way it travels, including through your own relay.

It also works well for servers with no screen at all:

- `esper terminal` opens a shell on the host.
- `esper forward` reaches a service on the host's loopback, like `ssh -L`. A
  forward can only reach the host's loopback unless the host's owner allows a
  wider target.
- `esper forward --stdio` works as an ssh `ProxyCommand`, like `ssh -W`, so
  ssh, scp, rsync and git can run over Esper.

## Why

Most remote desktop products decide who may see your screen, on their own
servers. That control is their business model.

Most open-source alternatives send you through a public server by default, so
a typical install still has a stranger in the middle.

And connecting is awkward. Reading out a nine-digit ID and a password that
keeps changing is a workaround for two machines that don't trust each other,
even when they're both yours.

## Features

| Category | Feature | Status |
|---|---|:---:|
| Connecting | **No account, no password, no Esper servers** | ✅ |
| Connecting | Pair once with a short code, then just connect | ✅ |
| Connecting | Works on your network, over Tailscale or WireGuard, or through your own broker | ✅ |
| Connecting | Invite someone with a single link | ✅ |
| Remote desktop | Linux desktops on X11 or Sway | ✅ |
| Remote desktop | Sound, clipboard and drag-and-drop files | ✅ |
| Remote desktop | Several machines side by side in one window | ✅ |
| Remote desktop | Record a session, and the other side always sees it | ✅ |
| Remote desktop | GNOME and KDE on Wayland | 🚧 |
| Remote desktop | Windows | 🚧 |
| Remote desktop | macOS | 🗓️ |
| Servers | A shell that survives a dropped connection | ✅ |
| Servers | Port forwarding, plus ssh, scp, rsync and git over Esper | ✅ |
| Control | Choose what each device may do, and when it may connect unasked | ✅ |
| Control | Wake, lock or restart a machine from afar | ✅ |
| Privacy | **End-to-end encrypted, with post-quantum key exchange** | ✅ |
| Privacy | No telemetry or update checks, and the activity log stays on your machine | ✅ |
| Privacy | Reproducible builds you can check byte for byte | ✅ |
| Release | First tagged release | 🗓️ |

✅ Ready · 🚧 In progress · 🗓️ Planned

## Installing

There's no release to download yet. You can build the packages yourself from a
checkout:

```sh
make package-deb                 # → dist/*.deb
sudo dpkg -i dist/esper_*.deb    # esper and the engine
```

[`packaging/`](packaging/) also has a `PKGBUILD` for Arch and a `.spec` for
Fedora. `esper-broker` is a separate package everywhere, because a machine
that only introduces other machines doesn't need the engine.

If you'd rather not use a package, `make install` puts the binaries under
`/usr/local`.

### Checking what you downloaded

Esper's builds are reproducible: build a release's source with the toolchain it
names and you get the exact same binary, byte for byte. Anyone can confirm the
binary matches the code without having to trust us.
`make check-reproducible` runs the same check the project uses, and
[`docs/releasing.md`](docs/releasing.md) explains what it covers.

Esper never checks for updates, because asking a server would tell it that
your machine exists, what it runs and roughly where it is. Updates come from
your package manager, or from you.

## Your first connection

You need two machines. **There** is the one you want to reach, and **here** is
the one you're sitting at.

**1. There: allow it to be reached.**

```sh
esper host enable          # starts now and at login, via systemd --user
```

On a machine nobody logs into, also run `sudo loginctl enable-linger $USER`.
Without it, systemd stops user services when your last session ends, which
looks like Esper crashing every time you disconnect. `esper host enable` warns
you if it's missing.

**2. There: offer a code.**

```sh
esper pair --listen 7645
```

This prints a code, the address to use, and **eight words**. The code proves
both ends typed the same thing. The words prove the other machine is the one
you meant, so read them aloud.

**3. Here: use the code.**

```sh
esper pair <there>:7645 --code XXXX-XXXX
```

Both ends print the same eight words. If they don't match, something is in
between you, so stop.

**4. Here: use the connection.**

```sh
esper devices              # what you've paired, and what each may do
esper terminal <name>      # a shell there
esper connect <name>       # its screen
esper forward <name> 5432  # its loopback port 5432, on yours
```

That's it. You made no account, registered nothing, and no server of ours was
involved.

### Connecting to someone outside your network

If neither machine can reach the other, they need a place to meet. Esper
doesn't provide one, so you run your own broker on a machine both can reach:

```sh
esper broker serve --listen :7443
```

It prints a key. Add the address and key to `~/.esper/config.toml` as
`broker.address` and `broker.key`, then create an invitation:

```sh
esper invite --for Dad
```

This prints a single link, which is all the other person needs. They paste it
into `esper join`, or click it if they have the window installed.

Treat the link like a password: anyone who has it can pair with you until it
expires. It expires on its own. [`docs/broker.md`](docs/broker.md) covers
running a broker.

## The window

```sh
make build-app             # needs pnpm, and webkit2gtk-4.1 on Linux
make install-app           # → ~/.local/bin, plus a desktop entry
```

There's nothing else to set up. The window pairs devices, opens remote
sessions side by side, shows what happened on this machine, turns hosting on
and off, and opens `esper://` invitation links. It starts everything it needs
by itself.

The window isn't in the distribution packages yet, so install it this way for
now.

## Scripting

Every command that reports something also accepts `--json`: `version`,
`doctor`, `devices`, `sessions`, `identity`, `input`, `discover` and `config`.
The exit status is the same either way, so a yes-or-no check doesn't need to
parse anything:

```sh
if esper doctor --json > /dev/null; then echo "this machine can host"; fi
```

`pair` and `invite` also accept `--json`, and print one JSON object per line
as things happen. Both can run for minutes, and they report two events: the
way in is ready, and someone used it.

[`docs/scripting.md`](docs/scripting.md) describes each object. No part of
Esper parses another part's printed output, and `make check-scraping` keeps it
that way.

`esper api` serves the same answers over HTTP on loopback, behind a bearer
token, for dashboards and integrations. [`docs/local-api.md`](docs/local-api.md)
lists every route, explains how to authenticate, and covers the two things the
API won't do.

Checks keep both documents in sync with the code. A command that gains
`--json` without being documented fails the build, and so does a documented
route that nothing serves.

## Building

You need Go 1.26+, Rust 1.98+, `make` and `cmake`. `nasm` is recommended.

Some distributions ship older versions. Checked on 2026-09-12:

| | Go | Rust | New enough? |
|---|---|---|---|
| Fedora 43 | 1.26.8 | 1.98.0 | yes |
| Debian 13 (trixie) | 1.24 | 1.85.1 | **no**, both are too old |

On Debian and Ubuntu, `apt install golang-go rustc` succeeds but `make` then
fails on the version. Install the toolchains from [rustup](https://rustup.rs)
and [go.dev/dl](https://go.dev/dl/) instead, or install a package from
[Installing](#installing), which needs no toolchain.

Without `nasm`, the H.264 encoder builds without its x86 assembly and runs 2%
to 6% slower, with the same output size. The build warns but doesn't fail, and
the binary reports the slower path at run time.

```sh
sudo pacman -S nasm       # Arch
sudo apt install nasm     # Debian, Ubuntu
brew install nasm         # macOS
winget install nasm       # Windows
```

Common targets:

```sh
make            # check-isolation, build, test
make build      # esper, esper-engine and esper-broker → ./build
make test       # every test, both languages
make lint       # clippy -D warnings + go vet + golangci-lint
make ci         # everything CI runs
make help       # list targets
```

The binaries:

| Binary | Role |
|---|---|
| `esper` | The control plane: CLI, device store, engine supervisor, local API |
| `esper-engine` | The realtime engine: capture, encode, transport, input |
| `esper-broker` | A stateless introducer and blind relay, which you run yourself |
| `esper-app` | The window, built with `make build-app` |

## How it works

The **engine is written in Rust.** It's a realtime pipeline with a budget of a
few milliseconds per frame, it calls hardware APIs through FFI, and it can't
afford garbage-collection pauses. The **control plane is written in Go.** It
provides the CLI, a supervised daemon, configuration and the local API.

They run as **separate processes** and talk over a local socket using
length-prefixed CBOR. Media never crosses that socket. Rust handles everything
from capture to display, and the socket carries only commands and state, at
about ten messages a second.

```
┌─ esper (Go) ────────────────┐        ┌─ esper-engine (Rust) ───────────┐
│  CLI · devices · policy     │  CBOR  │  capture → encode → QUIC        │
│  local API · supervision    │ ◄────► │  QUIC → decode → present        │
└─────────────────────────────┘ socket └─────────────────────────────────┘
```

Two implementations of one protocol can drift apart without anyone noticing. A
shared set of fixture files prevents that: both languages decode the same
bytes and must encode them back identically.

## Security

- [`SECURITY.md`](SECURITY.md): what the project claims, and how to report a
  vulnerability.
- [`docs/threat-model.md`](docs/threat-model.md): who might attack Esper, from
  where, and what stops them, including the gap Wayland leaves open on wlroots
  compositors.
- [`docs/protocol.md`](docs/protocol.md): what goes over the wire, detailed
  enough for someone else to implement.

Together they say what Esper does **not** defend against. Esper has not been
independently audited yet.

In short: Ed25519 device identity, SPAKE2 pairing, hybrid post-quantum key
agreement, `forbid(unsafe_code)` outside FFI, no panicking code in the engine,
continuous fuzzing, and a build-time check that no binary contains an address
it could phone home to.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md), and
[CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md) for how disagreements should be
handled.

## License

MIT. See [LICENSE](LICENSE).
