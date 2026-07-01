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

Corpora are committed as they grow. A crash found here is a security bug, not a
test failure — see `SECURITY.md`.

**More targets arrive with the code they protect**: every control message, the
media reassembler, clipboard payloads and file metadata each get one. A parser
without a fuzz target is not finished.
