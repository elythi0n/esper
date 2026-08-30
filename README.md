# Esper

> Secure, private remote desktop. No account, no password, no server.

🚧 **Early development.** Linux hosts work end to end, and there is a
desktop window — but **it cannot show you a screen yet**: the viewer is the
one part not built, so today the window pairs devices, shows what has
happened, and manages this machine, while looking at a remote screen is
`esper connect`. Windows and macOS hosts do not exist.

---

## What it is

An open-source alternative to TeamViewer that **operates no infrastructure at
all**.

Devices pair once with a short code and know each other by public key
thereafter. There is no account to create, no password to guess, and **no
default server address compiled into the binary** — so there is nothing to opt
out of and nothing to take on trust.

On a LAN, machines find each other. On an overlay network — Tailscale,
WireGuard, ZeroTier, Nebula — Esper notices, says so, and rides it: no hole
punched, no broker asked, because the address already works. Beyond that you
may run a broker yourself, and if you don't, Esper does less rather than
routing you through someone else.

Everything is end-to-end encrypted between device keys on every path, including
your own relay.

A headless server's useful face is not its screen, so terminal mode and port
forwarding are first-class rather than extras: a shell, and `ssh -L`-shaped
access to a service bound to the host's loopback that the network cannot see.
A forward reaches the host's loopback and nothing else unless the host's owner
writes down a wider target, and `--stdio` gives `ProxyCommand` the `ssh -W`
shape, so plain ssh, scp, rsync and git ride Esper's pairing and its way in.

## Why

Every remote desktop product answers "who is allowed to see this screen" with
"whoever our servers say." That is the wrong place for the answer to live, and
it is not incidental to those designs — it is the business.

The open alternatives mostly ship a public rendezvous server as the default, so
the common install still has a third party in the path.

And the connection ritual is bad. Reading a nine-digit ID and a rotating
password down a phone line is not a design; it is a workaround for having no
persistent trust between two machines that belong to the same person.

## Status

| | |
|---|---|
| Builds, tests, lints | ✅ |
| Wire protocol framing | ✅ Rust and Go, byte-compatible |
| Identity, pairing, transport | ✅ |
| Terminal mode | ✅ shells, reattach after a dropped link |
| Port forwarding | ✅ `ssh -L` and `ssh -W` shapes |
| Screen, video and input | ✅ Linux, X11 |
| Finding a host: LAN, a private network you already run, remembered address, your own broker | ✅ |
| Control plane: CLI, device store, local API | ✅ |
| Desktop window: devices, pairing, activity, settings | ✅ |
| The viewer — a remote screen inside the window | ⏳ the one thing missing |
| Windows and macOS hosts | ⏳ |

## Building

Needs Go 1.26+, Rust 1.98+, and `make`. `nasm` is recommended.

**Your distribution may not have those yet.** Checked in a container of each,
on 2026-09-12:

| | Go | Rust | Enough? |
|---|---|---|---|
| Fedora 43 | 1.26.8 | 1.98.0 | yes |
| Debian 13 (trixie) | 1.24 | 1.85.1 | **no** — both too old |

On Debian and Ubuntu, get the toolchains from upstream — [rustup](https://rustup.rs)
and [go.dev/dl](https://go.dev/dl/) — or install a package from
[Installing](#installing) instead, which needs no toolchain at all. This is
worth saying because `apt install golang-go rustc` succeeds and then `make`
fails on a version, which reads as the project being broken rather than the
packages being old.

The H.264 encoder is built from Cisco's source, and its x86 assembly needs an
assembler. Without one, openh264's build script *silently* falls back to a
C-only path — measured at 2.5% to 6% slower, with identical output size. The
build warns rather than failing, because a few per cent does not justify
blocking anyone, and the binary reports the slow path at run time so a session
that feels wrong can be explained.

```sh
sudo pacman -S nasm       # Arch
sudo apt install nasm     # Debian, Ubuntu
brew install nasm         # macOS
winget install nasm       # Windows
```

```sh
make            # check-isolation, build, test
make build      # both binaries → ./build
make test       # every test, both languages
make lint       # clippy -D warnings + go vet + golangci-lint
make ci         # everything CI runs
make help       # list targets
```

Four binaries come out:

| Binary | Role |
|---|---|
| `esper` | The control plane: CLI, device store, engine supervisor, local API |
| `esper-engine` | The realtime engine: capture, encode, transport, input |
| `esper-broker` | A stateless introducer and blind relay, run by you |
| `esper-app` | The window. `make build-app` builds it; it needs a webview (`webkit2gtk-4.1` on Linux) |

## Installing

No release has been tagged yet, so there is nothing to download. Building the
packages from a checkout works today:

```sh
make package-deb                 # → dist/*.deb
sudo dpkg -i dist/esper_*.deb    # esper and the engine
```

Arch has a `PKGBUILD` and Fedora a `.spec` in [`packaging/`](packaging/), and
`esper-broker` is a **separate package** everywhere — a machine that introduces
other machines has no reason to carry the engine.

Then, on the machine you want to reach:

```sh
esper host enable    # start now and at login, through systemd --user
```

`make install` puts the binaries under `/usr/local` instead, if you would
rather not use a package.

### Checking what you downloaded

Esper's builds are **reproducible**: given the source tarball for a release and
the toolchain it names, you get the published binary back, byte for byte. That
is the only way the claim that no infrastructure of ours is in the path can be
checked by somebody who does not trust us — the code is readable, and a binary
is not. `make check-reproducible` is the same check the project runs on itself;
[`docs/releasing.md`](docs/releasing.md) says what it covers and what it does
not.

**Esper never checks for updates.** A client that asks a server whether it is
current tells that server that this machine exists, what it runs and roughly
where it is, on a schedule. That is a census. Updates come from your package
manager or from you.

## Your first connection

Two machines. Call the one you want to reach **there**, and the one you are
sitting at **here**.

**1. There: let it be reached.**

```sh
esper host enable          # starts now, and at login, via systemd --user
```

On a machine nobody logs into, add `sudo loginctl enable-linger $USER` — without
it systemd stops user services when your last session ends, which looks exactly
like Esper crashing whenever you disconnect. `esper host enable` says so if it
is missing.

**2. There: offer a code.**

```sh
esper pair --listen 7645
```

It prints a code, the address to use, and **eight words**. The words are not
decoration: a code proves the same code was typed at both ends, and only the
words prove the machine at the other end is the one you meant. Read them
aloud.

**3. Here: use it.**

```sh
esper pair <there>:7645 --code XXXX-XXXX
```

Both ends print the same eight words. If they do not match, something is
between you — stop.

**4. Here: do something with it.**

```sh
esper devices              # what you have paired, and what each may do
esper terminal <name>      # a shell there
esper connect <name>       # its screen
esper forward <name> 5432  # its loopback port 5432, on yours
```

That is the whole of it. There is no account, nothing was registered, and no
server of ours was involved at any point.

### Somebody who is not on your network

If neither machine can reach the other, the two need somewhere to meet, and
Esper has no such place — so you run one:

```sh
esper broker serve --listen :7443     # on a machine both can reach
```

It prints a key. Put the address and that key in `~/.esper/config.toml` as
`broker.address` and `broker.key`, then:

```sh
esper invite --for "Dad"
```

That prints **one link**, and it is the whole exchange: they paste it into
`esper join`, or click it if they have the window installed. The link is the
secret — anybody holding it can pair with you until it expires — so send it
the way you would send a password, and it lapses on its own.

## The window

```sh
make build-app             # needs a webview: webkit2gtk-4.1 on Linux
make install-app           # → ~/.local/bin, plus a desktop entry
```

Opening it is the whole setup: it **starts its own control plane** if nothing
is already listening, so there is no second thing to run in a terminal. It
pairs devices, shows what has happened on the machine, turns serving on and
off, and opens `esper://` invitation links.

It is **deliberately not in the distribution packages yet**, because the
viewer is not built and a package called "remote desktop" that cannot show
you one would be a promise the software does not keep.

## How it is put together

The **engine is Rust** — a hard-realtime pipeline with a per-frame budget of a
few milliseconds, hardware APIs reached through FFI, and no tolerance for a
garbage collector. The **control plane is Go** — a CLI, a supervised daemon, a
config loader and a local API.

They are **separate processes**, not one binary with cgo, and they speak
length-prefixed CBOR over a local socket. The crucial property: **media never
crosses that seam.** Rust owns capture through to present, end to end; the
socket carries commands and state at roughly ten messages a second.

```
┌─ esper (Go) ──────────────┐        ┌─ esper-engine (Rust) ─────────┐
│  CLI · devices · policy     │  CBOR  │  capture → encode → QUIC        │
│  local API · supervision    │ ◄────► │  QUIC → decode → present        │
└─────────────────────────────┘ socket └─────────────────────────────────┘
```

Two independent implementations of one protocol drift silently, so they are held
together by a committed fixture corpus: both languages decode the same bytes and
must re-encode them byte-identically. It caught its first drift before there was
a second message.

## Security

[`SECURITY.md`](SECURITY.md) is what the project claims and how to report a
vulnerability. [`docs/threat-model.md`](docs/threat-model.md) is who might
attack it, from where, and what stops them — including the boundary Wayland
does not close on wlroots compositors. [`docs/protocol.md`](docs/protocol.md)
is what goes on the wire, written so somebody else could implement it.

Between them they say what Esper does **not** defend against, and state plainly
that it has not yet been independently audited.

Short version: Ed25519 device identity, SPAKE2 pairing, hybrid post-quantum key
agreement, `forbid(unsafe_code)` outside FFI, no panicking constructs in the
engine, continuous fuzzing, and a build-time check that no binary contains an
endpoint it could phone home to.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md), and
[CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md) for how disagreements are expected to
go.

## License

MIT. See [LICENSE](LICENSE).
