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
