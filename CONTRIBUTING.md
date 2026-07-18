# Contributing

## Before you start

```sh
make ci     # everything CI runs: isolation, format, lint, test
```

If that is green, your change will pass CI.

## The rules that are not negotiable

These are enforced mechanically, and a change that breaks one will fail the
build rather than reaching review.

**This repository must be self-contained.** No file may reference a path outside
it. `make check-isolation` enforces this.

**No `unwrap`, `expect` or `panic!` in engine code.** A panic reachable from the
network is a denial of service. Tests and build scripts are exempt; everything
else returns an error. Clippy enforces it.

**`#![forbid(unsafe_code)]`** in every crate that does not need FFI. The crates
that do carry a documented inventory of every `unsafe` block and the invariant it
relies on.

**Validate before you allocate.** Anything derived from a peer — a length, a
count, a size — is checked against a cap before it is used to size a buffer.

**Every parser that touches untrusted input gets a fuzz target.** A parser
without one is not finished.

**The wire protocol is implemented twice**, in `core/crates/esper-proto` and
`internal/proto`. Changing a message means changing both, adding a fixture to
`testdata/fixtures`, and confirming both languages re-encode it byte-identically.
If they disagree, the protocol is ambiguous — fix the encoders; never regenerate
the fixture to match whichever side you ran.

Field order is part of the wire format, because neither implementation sorts map
keys. Reorder a struct field on one side and the tests will tell you.

## Adding a dependency

A decision with a reason, and the reason goes in the pull request. A malicious
dependency reaches everything silently, so the bar is higher than convenience.
`cargo deny check` must pass, and the license must be on the allowlist.

## Style

`make fmt` before committing. Comments explain *why*; the code already says
what. Most code needs none.
