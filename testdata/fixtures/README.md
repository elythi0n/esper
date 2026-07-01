# Shared wire fixtures

Each `.cbor` file is one complete frame — a big-endian `u32` length followed by
the CBOR payload — as it appears on the wire.

**Both language implementations must decode every fixture to the same value, and
re-encode that value to byte-identical output.** The Rust side checks this in
`core/crates/ownview-proto/tests/fixtures.rs`; the Go side in
`internal/proto/fixtures_test.go`. Both run in CI.

This exists because two independent implementations of one protocol drift
silently. The corpus caught its first drift before either side had a second
message: Go's canonical CBOR sorts map keys, ciborium emits struct fields in
declaration order, and the two produced different bytes for the same `Hello`.
Without a byte-level check that would have surfaced as a mysterious interop
failure months later.

## Adding a fixture

1. Add the message to both implementations.
2. Generate the bytes from **one** side.
3. Run both test suites. If they disagree, the protocol is ambiguous — fix the
   encoders, do not regenerate the fixture to match whichever side you ran.

Fixtures are committed and never edited by hand.
