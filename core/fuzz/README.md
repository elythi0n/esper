# Fuzz targets

Continuous fuzzing of every parser that touches untrusted input, because that is
where a real vulnerability is most likely to be.

```sh
rustup toolchain install nightly     # cargo-fuzz needs nightly
cargo install cargo-fuzz
cargo +nightly fuzz run frame_decode
```

| Target | Input | Contract |
|---|---|---|
| `frame_decode` | Arbitrary bytes on a control stream | Returns. Never panics, never hangs. |
| `messages` | Arbitrary bytes decoded as **every** message type, plus the validators a host calls on the result | Same. A payload can be valid CBOR and a hostile *shape*, which the framing target never sees. |
| `replay` | Arbitrary `u64` sequence numbers in arbitrary order | `check` returns. Covers the bitmap shifts, where an off-by-one lives. |

**Adding a message to the protocol without adding it to `messages` is how a
parser goes unfuzzed.** The list there is deliberately explicit rather than
generic, so the omission is visible in a diff.

Corpora are committed as they grow. A crash found here is a security bug, not a
test failure — see `SECURITY.md`.

**More targets arrive with the code they protect**: every control message, the
media reassembler, clipboard payloads and file metadata each get one. A parser
without a fuzz target is not finished.
