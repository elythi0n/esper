# Esper

> Secure, private remote desktop. No account, no password, no server.

🚧 **Early development.** Linux hosts work end to end; Windows and macOS
hosts, the desktop app and the control-plane CLI do not exist yet.

---

## What it is

An open-source alternative to TeamViewer that **operates no infrastructure at
all**.

Devices pair once with a short code and know each other by public key
thereafter. There is no account to create, no password to guess, and **no
default server address compiled into the binary** — so there is nothing to opt
out of and nothing to take on trust.

On a LAN, machines find each other. On an overlay network — Tailscale,
WireGuard, ZeroTier, Nebula — Esper notices and rides it. Beyond that you may
run a broker yourself, and if you don't, Esper does less rather than routing
you through someone else.

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
| Finding a host: LAN, remembered address, your own broker | ✅ |
| Windows and macOS hosts | ⏳ |
| Desktop app, control-plane CLI | ⏳ |

## Building

Needs Go 1.26+, Rust 1.98+, and `make`. `nasm` is recommended.

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

Three binaries come out:

| Binary | Role |
|---|---|
| `esper` | The control plane: CLI, device store, engine supervisor |
| `esper-engine` | The realtime engine: capture, encode, transport, input |
| `esper-broker` | A stateless introducer and blind relay, run by you |

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

See [SECURITY.md](SECURITY.md) — including what Esper does **not** defend
against, and the plain statement that it has not yet been independently audited.

Short version: Ed25519 device identity, SPAKE2 pairing, hybrid post-quantum key
agreement, `forbid(unsafe_code)` outside FFI, no panicking constructs in the
engine, continuous fuzzing, and a build-time check that no binary contains an
endpoint it could phone home to.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md).

## License

MIT. See [LICENSE](LICENSE).
