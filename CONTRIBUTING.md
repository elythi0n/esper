# Contributing

## Before you start

```sh
make ci     # everything CI runs: isolation, format, lint, test
```

If that is green, your change will pass CI.

## The tests that need a display

The X11 backends — capture, injection, the session indicator, the viewer window
— can only be tested against a real X server, so those tests **skip when there
is none**. A green `make ci` on a machine with no `DISPLAY` has not run them.

To run them, give them a display nobody is looking at:

```sh
Xvfb :99 -screen 0 1600x900x24 -nolisten tcp -noreset &
DISPLAY=:99 ESPER_DISPLAY_TESTS=1 make ci
```

Two details in that line are load-bearing:

- **`-noreset`.** An X server without it resets when its *last* client
  disconnects, destroying every resource and closing every connection. This
  suite is a stream of short-lived clients across parallel tests, so the count
  reaches zero regularly — and a connection being established at that instant
  dies with `ECONNRESET`. Measured: 2 runs in 8 failed without it, 0 in 8 with
  a client held open throughout. A real desktop always has a window manager
  attached, which is why only a bare test server shows this.
- **`ESPER_DISPLAY_TESTS=1`.** Tests that inject keystrokes, take the input
  focus, or draw over the whole screen are off unless asked for. A suite that
  did that to your desktop whenever you ran it is a suite you would turn off.

Never point these at a display you are using.

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
