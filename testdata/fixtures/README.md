# Shared fixtures

Two kinds live here, for the same reason: Esper is two implementations of one
product, and anything both halves read has to be read the same way.

**Wire fixtures** (`.cbor`). Each file is one complete frame — a big-endian
`u32` length followed by the CBOR payload — as it appears on the wire.

**Both language implementations must decode every fixture to the same value, and
re-encode that value to byte-identical output.** The Rust side checks this in
`core/crates/esper-proto/tests/fixtures.rs`; the Go side in
`internal/proto/fixtures_test.go`. Both run in CI.

This exists because two independent implementations of one protocol drift
silently. The corpus caught its first drift before either side had a second
message: Go's canonical CBOR sorts map keys, ciborium emits struct fields in
declaration order, and the two produced different bytes for the same `Hello`.
Without a byte-level check that would have surfaced as a mysterious interop
failure months later.

**Store fixtures** (`.json`). `device_store.json` is a device store as
`esper-crypto` writes one, and `device_fingerprints.json` is the fingerprint
derived from each of sixteen keys. Rust checks them in
`core/crates/esper-crypto/tests/store_fixtures.rs`; Go in
`internal/devices/devices_test.go`.

The store is the file that says who a machine trusts, and the two halves read
it from opposite sides — Rust writes and enforces, Go shows. A disagreement
about it is a device listed that cannot connect, or one hidden that can. The
fingerprint fixture exists because the fingerprint is what two people read to
each other to confirm they mean the same device: a domain separator or word out
of step renders one key as two codes.

Regenerate a store fixture with

    ESPER_WRITE_FIXTURES=1 cargo test -p esper-crypto --test store_fixtures

## Adding a fixture

1. Add the message to both implementations.
2. Generate the bytes from **one** side.
3. Run both test suites. If they disagree, the protocol is ambiguous — fix the
   encoders, do not regenerate the fixture to match whichever side you ran.

Fixtures are committed and never edited by hand.
