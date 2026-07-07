# Supply chain

`cargo vet` state. Every dependency is either **audited** — by us, or by a
published audit set we import — or **exempted**.

```sh
make vet-deps          # cargo vet check
cargo vet diff <crate> <old> <new>   # review a version bump
cargo vet certify <crate> <version>  # record an audit you performed
```

## What an exemption means, honestly

An exemption is **not** an audit. It records "this crate was in the tree when we
adopted `cargo vet`, and we have not reviewed it." It exists so adoption does
not require auditing the entire transitive graph before any work can happen.

The value is not in the exemptions. It is in what they make possible:

- **A new dependency cannot arrive silently.** Anything not already exempted or
  audited fails `cargo vet check`, including its transitive tail. Adding one is
  a decision with a reason, recorded in the pull request.
- **A version bump is reviewable.** `cargo vet diff` shows exactly what changed
  between the exempted version and the new one.
- **The list shrinks.** Every exemption removed is a crate someone read.

## Imported audit sets

We trust these publishers' audits rather than repeating their work:

| Set | Publisher |
|---|---|
| `mozilla` | Mozilla |
| `google` | Google |
| `bytecode-alliance` | Bytecode Alliance (wasmtime) |
| `isrg` | ISRG / Let's Encrypt (libprio-rs) |
| `zcash` | Zcash |
| `fermyon` | Fermyon (spin) |

Importing a set is itself a trust decision. These are organizations that publish
audits as a practice and stake a reputation on them.

## What the transport cost

Recorded because the number moved a lot in one commit, and a jump in exemptions
should be explainable rather than absorbed.

Adding `quinn` and `tokio` brought **27 new crates**: the QUIC implementation
itself (`quinn`, `quinn-proto`, `quinn-udp`), the async runtime (`tokio`, `mio`,
`socket2`, `slab`, `futures-*`), and support crates (`bytes`, `rustc-hash`,
`lru-slab`, `chacha20`, `rand`).

Four of them — `js-sys`, `wasm-bindgen*`, `web-time` — are WASM-target crates
that appear in the dependency graph but are **not linked on any target OwnView
builds for**. Verified with `cargo tree --target x86_64-unknown-linux-gnu`
rather than assumed.

What did *not* come in, deliberately: `quinn`'s default features enable
`platform-verifier`, which pulls in `rustls-platform-verifier`, a bundled
Mozilla root certificate store, and `jni`. OwnView has **no certificate
authority in its trust model** — peers are authenticated by raw public key
against keys stored at pairing — so a platform CA verifier is a general-purpose
trust mechanism at a boundary that deliberately does not want one.
`default-features = false` removes all of it, along with `ring` (the crypto
provider is `aws-lc-rs`, and two implementations of the same primitives is a
question about which one is running).

`cargo deny` is how it was noticed: the root bundle's licence is
CDLA-Permissive-2.0, which is not on the allowlist.

## Priorities for reducing exemptions

Not all exemptions are equal. Review in this order:

1. **Anything in the handshake path** — `aws-lc-rs`, `rustls`, `ed25519-dalek`, `curve25519-dalek`. A bug here is a broken security guarantee, not a crash.
2. **Anything parsing untrusted input** — `ciborium` and its tail. This is where a real vulnerability is most likely.
3. **Build-time only** (`cc`, `cmake`, `jobserver`) — lower consequence at runtime, but they execute on developer and CI machines.
4. Everything else.
