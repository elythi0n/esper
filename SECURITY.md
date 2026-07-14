# Security

OwnView is remote desktop software. It can see everything on a screen and type
anything into it, which makes it one of the highest-consequence things a person
installs. This document says what we claim, what we do not, and how to report a
problem.

## Reporting a vulnerability

**Do not open a public issue.**

Email **security@ownview.invalid** — *(placeholder: replace with a monitored
address and publish a PGP key before the first release)*.

- We aim to acknowledge within **72 hours**.
- We follow **90-day coordinated disclosure**, shortened by agreement if a fix ships sooner.
- We will credit you unless you ask us not to.

There is no bug bounty yet.

## The honest claim

OwnView does not claim to be uncrackable. No software is, and a product making
that claim has either not been examined or is not being straight with you.

What we do claim:

1. **A small attack surface**, because most of the usual surface does not exist.
2. **Only audited cryptography** — no primitive is written here.
3. **Defense in depth** — no single layer's failure yields the session.
4. **Auditability** — the whole system can be read; there is no hosted component to take on trust.
5. **A real process** for the day something is found anyway.

### Not independently audited

**OwnView has not had a third-party security audit.** This will remain here,
stated plainly, until one happens and its report is published. Anyone deploying
OwnView in a high-risk setting should weigh that.

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
exchange that OwnView's entire trust model rests on are therefore unreviewed by
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

## What the design removes

The strongest thing about OwnView's posture is architectural rather than
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

- **Root or Administrator on either endpoint.** They own the framebuffer and the keyboard regardless of what OwnView does.
- **A user who pairs with someone they should not have.** Social engineering is not a cryptographic problem.
- **Hardware keyloggers, screen photography, shoulder surfing.**
- **A malicious OS, compromised firmware, or a hostile hypervisor.**
- **Traffic analysis.** Packet sizes and timing leak coarse activity. Padding is not implemented.
- **Anonymity.** OwnView protects confidentiality and integrity, not the fact that a connection happened.

## Supported versions

Pre-1.0. Only the latest release receives fixes.
