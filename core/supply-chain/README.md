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

## Priorities for reducing exemptions

Not all exemptions are equal. Review in this order:

1. **Anything in the handshake path** — `aws-lc-rs`, `rustls`, `ed25519-dalek`, `curve25519-dalek`. A bug here is a broken security guarantee, not a crash.
2. **Anything parsing untrusted input** — `ciborium` and its tail. This is where a real vulnerability is most likely.
3. **Build-time only** (`cc`, `cmake`, `jobserver`) — lower consequence at runtime, but they execute on developer and CI machines.
4. Everything else.
