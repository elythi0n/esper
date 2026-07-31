# Security

Esper is remote desktop software. It can see everything on a screen and type
anything into it, which makes it one of the highest-consequence things a person
installs. This document says what we claim, what we do not, and how to report a
problem.

## Reporting a vulnerability

**Do not open a public issue.**

Email **security@elythion.com**.

- We aim to acknowledge within **72 hours**.
- We follow **90-day coordinated disclosure**, shortened by agreement if a fix ships sooner.
- We will credit you unless you ask us not to.

There is no bug bounty yet.

## The honest claim

Esper does not claim to be uncrackable. No software is, and a product making
that claim has either not been examined or is not being straight with you.

What we do claim:

1. **A small attack surface**, because most of the usual surface does not exist.
2. **Only audited cryptography** — no primitive is written here.
3. **Defense in depth** — no single layer's failure yields the session.
4. **Auditability** — the whole system can be read; there is no hosted component to take on trust.
5. **A real process** for the day something is found anyway.

### Not independently audited

**Esper has not had a third-party security audit.** This will remain here,
stated plainly, until one happens and its report is published. Anyone deploying
Esper in a high-risk setting should weigh that.

Nor are all of its cryptographic dependencies. Every dependency is either
covered by a published third-party audit we import, or recorded as an
exemption — and the exemptions include things that matter:

| Crate | Status |
|---|---|
| `sha2`, `subtle` | covered by an imported audit |
| **`curve25519-dalek`** | **exemption — no audit we import covers it** |
| **`ed25519-dalek`** | **exemption** |
| **`spake2`** | **exemption — and its own README states it has never been audited** |

The curve arithmetic, the signature scheme and the password-authenticated key
exchange that Esper's entire trust model rests on are therefore unreviewed by
us and uncovered by the audit sets we trust. There is no audited Rust
alternative to any of them today; we chose the best-provenance option in each
case and are telling you which they are.

Those three are the first thing an external audit should look at — ahead of any
code we wrote ourselves. `make vet-deps` reproduces this list.

### The video codec, which is 155 000 lines of exempted C

`openh264-sys2` vendors Cisco's H.264 implementation and compiles it. That is
by far the largest exemption in the tree and it will not be audited by this
project, so here is the position rather than silence:

**The two directions are not equally dangerous, and the difference is the whole
argument.**

- On a **host**, openh264 *encodes*. Its input is pixels this machine captured
  from its own screen. A peer cannot reach it: there is no bitstream from
  anyone else anywhere near the encoder. A memory-safety bug in the encoder is
  reachable only by whatever is already on the host's own display.
- On a **controller**, openh264 *decodes*. Its input is a bitstream a peer
  sent. **That is a large C parser eating attacker-controlled input**, which is
  the worst thing in the product to treat as trusted.

So the decoder is the one that gets defended, and it is defended structurally
rather than by hoping:

| | |
|---|---|
| The decoder runs sandboxed | seccomp-bpf, AppContainer, App Sandbox — no filesystem, no network, no ability to reach the session |
| A fuzz target sits on its input path | continuously, not once |
| It is a separate process | a crash is a dropped session, not a compromised machine |

**The sandbox exists.** On Linux the decoder runs in its own process behind
four layers, chosen because they fail differently:

| Layer | Stops | Fails if |
|---|---|---|
| `RLIMIT_AS`, 768 MB | the decoder allocating for a picture a peer declared | the kernel ignores it, which it does not |
| Landlock | reaching any file, by any path | the kernel is older than 5.13 |
| seccomp-bpf, 18 syscalls | everything else — including all networking | the filter has a gap |
| A separate process | a compromise reaching the session keys or the input injector | the parent trusts what the child sends it |

Each is **verified by attempting the forbidden thing**, not by reporting that
the layer was applied: a probe process tries to read a file, open a socket, and
allocate past the cap, and the test asserts it is denied or killed. A fifth
test drives thirty real frames through the jailed decoder, because a sandbox
that breaks decoding is an outage rather than a sandbox.

The fuzz target on the decoder's input path runs with openh264's C
instrumented for both AddressSanitizer and coverage. That is not the default:
`cargo fuzz` sets `RUSTFLAGS` and never `CFLAGS`, so left alone the C is built
with neither — which means a heap overflow inside the decoder is triggered and
not reported, and libFuzzer gets no feedback from inside the parser at all.
Measured on this project's development machine, instrumenting the C and
seeding the corpus took the coverage from 147 points to 1 759, and the
original 147 were all in this project's own bounds checks rather than in
openh264. `make fuzz` sets the flags; it needs clang, because gcc rejects them.

Two new dependencies carry this, both exempted in `cargo-vet` and both worth
naming for their provenance: `seccompiler` is AWS's, from Firecracker, and
`landlock` is written by the maintainer of the Landlock kernel subsystem.

**Windows and macOS have no sandbox yet.** `Sandbox::apply` returns an error
there rather than an empty success, so a controller cannot be built that
silently runs an unjailed decoder — AppContainer and the App Sandbox arrive
before the decoder does.

Also worth stating: the codec is built **from source**, not downloaded. Cisco
distributes a prebuilt binary that carries their patent licence, and using it
would mean a product whose entire premise is that it contacts nobody fetching a
shared library from a third party at run time. Building from source moves H.264
patent licensing onto whoever ships the binary, which for a self-hosted
open-source tool is the honest place for it to sit.

### The one parser that runs before anybody is authenticated

Everything else in this product reads bytes from a peer that has already proved
its key in a TLS handshake. **LAN discovery does not.** The mDNS reader takes
multicast packets from anyone on the segment, before any handshake, and an
attacker does not need to be paired or even to be talking to us on purpose. By
reachability it is the widest surface here.

So it is the narrowest code:

| | |
|---|---|
| Safe Rust, no dependency | it speaks the protocol itself rather than taking a general DNS-SD stack, which measured at eight crates, ~30 000 lines and ~500 `unsafe` occurrences |
| A fuzz target on its input path | `mdns_packets`, in the standing list |
| Bounded name decompression | a pointer must point strictly before itself *and* before every earlier jump, so a pointer loop is impossible rather than unlikely — and note that a length limit cannot catch one, because a loop of pointers accumulates no labels |
| Malformed packets are counted, not fatal | a responder that could be stopped by one bad packet could be stopped by anyone on the network |

**And nothing it reads is trusted.** A fingerprint in an advertisement is a
claim: it decides whether a device is *offered* as connectable, and the
connection itself proves the key. A peer that advertises a paired device's
fingerprint is listed and then refused, having gained a place in a list. The
address comes from the packet's source rather than the record it carries, so a
forged address record changes nothing.

What it costs, stated rather than glossed: **an advertisement tells the segment
that this machine runs Esper.** That is the residual in the threat model's
A4, it is why `discovery.mdns: false` exists, and the port remains open either
way.

### Screen capture, which is 180 000 lines of exempted Rust

X11 capture rests on `x11rb`, and with `x11rb-protocol` behind it that is about
180 000 lines this project will not audit either. It is a much better position
than the codec, and the reasons are worth stating rather than assumed:

**It is all safe Rust.** `x11rb-protocol` — the 144 000-line part, and the part
that actually parses what the display server sends back — contains no `unsafe`
at all. `x11rb`'s own `unsafe` exists only behind its `allow-unsafe-code`
feature, which is reachable only through `dl-libxcb`, and neither is enabled
here: the build asks for `randr`, `damage`, `shm` and `xfixes` and nothing else.
So a bug in this code is a wrong answer or a panic, not memory corruption. That
is a different class of problem from a bug in 155 000 lines of C.

**Most of it is generated.** The protocol modules are produced from the X11
protocol descriptions rather than written, which means the interesting surface
is far smaller than the line count suggests — and that a review should read the
generator and the hand-written connection code, not 144 000 lines of
serialisers.

**Nothing links a display library.** `x11rb` speaks the X11 protocol over a
socket instead of binding libX11, so `ldd` on the engine shows no `libX11`, no
`libxcb`, no `libwayland`. A headless server carries the engine without a
desktop being installed anywhere near it, and there is no C in the capture path
to sandbox.

**Who is on the other end matters.** The input here comes from the machine's own
display server, not from a peer. An X server that lies to its clients has
already taken the screen, the keyboard and the clipboard by more direct means —
X11 offers a client no isolation from the server or from other clients, which is
a property of the protocol and the reason Wayland exists. Capture is therefore
*not* a trust boundary in the way the decoder is, and it is not sandboxed.

## What the design removes

The strongest thing about Esper's posture is architectural rather than
defensive. These categories do not apply because the thing they attack does not
exist:

| Category | Why it does not apply |
|---|---|
| Credential stuffing, weak or reused passwords | There are no passwords |
| Server breach exposing users or sessions | There is no server |
| Insider access at a vendor | There is no vendor |
| Session token theft, fixation, replay | There are no tokens; auth is a per-connection mutual handshake |
| Account takeover, password-reset abuse | There are no accounts |
| Compelled disclosure of a user directory | No directory exists |
| Malicious auto-update pushing a backdoor | No update channel is operated |
| Telemetry exfiltration | No telemetry exists |

## How it is built

- **Ed25519 device identity**, generated locally, never transmitted.
- **SPAKE2 pairing** with a short single-use code — one online guess per attempt, so a 40-bit code is sufficient where a password would not be.
- **Hybrid X25519 + ML-KEM-768 key agreement**, so traffic recorded today does not become readable when quantum computing arrives. Both must be broken.
- **`#![forbid(unsafe_code)]`** in every crate that does not need FFI; the ones that do carry a documented, reviewed unsafe inventory.
- **No `unwrap`, `expect` or `panic!` in the engine** — a panic reachable from the network is a denial of service. Enforced by clippy, not by convention.
- **Overflow checks on in release builds.**
- **Continuous fuzzing** of every parser that touches untrusted input.
- **Hard caps checked before allocation** — a peer claiming a 4 GiB frame costs a rejected header, not four gigabytes.
- **Supply chain**: minimal dependencies, `cargo-deny` and `cargo-vet` in CI, reproducible builds, signed releases.
- **No endpoint literal ships in any binary.** This is verified mechanically by `scripts/smoke.sh` on every build, which is what makes "operates no infrastructure" a checkable claim rather than a promise.

## Out of scope

Stated so the list above is not mistaken for a guarantee:

- **Root or Administrator on either endpoint.** They own the framebuffer and the keyboard regardless of what Esper does.
- **A user who pairs with someone they should not have.** Social engineering is not a cryptographic problem.
- **Hardware keyloggers, screen photography, shoulder surfing.**
- **A malicious OS, compromised firmware, or a hostile hypervisor.**
- **Traffic analysis.** Packet sizes and timing leak coarse activity. Padding is not implemented.
- **Anonymity.** Esper protects confidentiality and integrity, not the fact that a connection happened.

## Supported versions

Pre-1.0. Only the latest release receives fixes.
