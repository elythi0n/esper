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

**Stream fixtures** (`.bin`). `viewer_stream.bin` is a session's pictures as
the engine writes them for a window: a magic number, a version, and records
carrying the `avcC` description and each access unit. Every record states its
own length, so a reader can walk past a kind it has never heard of — which is
the difference between a format that can grow and one that cannot, and was
worth a version bump on the day it was noticed rather than a year later. Written by
`esper-media`'s `viewer_stream`, regenerated with `make viewer-fixture`, and
read by **three** implementations — Rust checks it in
`core/crates/esper-media/tests/viewer_fixture.rs` against a reader written
from the format's documentation rather than from the writer, and the page
parses it with its own TypeScript reader and hands every picture to a real
`VideoDecoder` in `web/scripts/check-viewer.mjs`, in Chromium and in the
WebKitGTK the window actually ships.

It is deliberately **not** held to byte identity the way the wire fixtures
are. Those are ours to encode, so a difference is our drift; this one contains
openh264's output, and holding a third party's encoder to byte identity would
make an upstream release look like a broken format. What is checked is
everything the format promises: the magic, the version, the record layout, and
that every length adds up.

**Store fixtures** (`.json`). `device_store.json` is a device store as
`esper-crypto` writes one, and `device_fingerprints.json` is the fingerprint
derived from each of sixteen keys. Rust checks them in
`core/crates/esper-crypto/tests/store_fixtures.rs`; Go in
`internal/devices/devices_test.go`.

`device_store_screens.json` is a third, and it is not held to a format — it is
held to the *screens*. The browser gates drive the real control plane, so
until it existed they measured whatever the developer's own machine held, which
on a fresh one is nothing: no device row, no permission chip, no pairing word
had ever been rendered under `check-a11y`, `check-ink`, `check-words` or
`check-overflow`. This store carries the cases a screen has to survive rather
than the cases a parser does — a name nobody would type, a name that is not
Latin, all fourteen permissions on one row, a pairing already spent, a device
paired and never seen. `scripts/gate-rig.sh` serves it; `make check-screens`
runs the gates against it. It is deliberately separate from `device_store.json`,
because that one is a shared corpus two implementations are held to and adding
a device to it would move every count they assert.

**Its times are moved, not used as written.** The rig shifts every timestamp in
the store by one amount when it stages it, so that the newest device was seen
five seconds ago and the gaps between the rest are the ones the file records.
Without that the fixture ages: written with a device seen seconds ago, it
renders "4h" by the afternoon and "3d" by Thursday, and the states at the top
of the scale — seconds, minutes, a one-time pairing that has **not** yet
lapsed — stop being reachable after the first hour. Shifted, one run renders
`7s`, `2m`, `2h`, `2d`, `3d` and "never", and both `once` and `spent`, every
time.

**And `session_log_screens.jsonl` beside it**, for the same reason one screen
along: without it the Activity screen was measured empty. It carries what that
screen has to draw and an empty log cannot — a session this machine opened
*outward*, which is the arrow's other direction; a refusal, which is the line
somebody opens the screen to find; a transfer with a byte count; and a device
since unpaired, where the log keeps the key and the name is gone. Its times are
shifted like the store's, and its keys are the store's keys, so a line and a
row are about the same machine.

**It is generated from the Rust types, and that is not fastidiousness.** Written
by hand first, it had fourteen of twenty-one lines silently dropped: a line
whose event does not deserialise is skipped, so the screen showed a third of
the fixture and nothing said so. The shapes were plausible and wrong —
`permissions` where the type says `granted`, `lock_after` where it says
`after`. `esper-audit`'s `screens_fixture.rs` is what stops that coming back:
every line must parse, and the counts must hold.

`weak_keys.json` is a list rather than a scenario: Ed25519's eight small-order
points, which are the keys no device may be trusted under. It exists because
the two halves need the same answer and only one of them has a curve library —
Rust refuses such a key when it loads the store, and Go serves the device list
from that same file with no engine running. `esper-crypto`'s `weak_keys.rs`
asserts every entry is one *it* refuses; `internal/devices/weak_test.go`
asserts the same file is what Go compiled in. Neither side owns the set, which
is the arrangement the wire fixtures and the word list already use.

`device_store_unusable.json` is what that rule is measured on: one ordinary
device, one whose key is not a point at all, and one of small order. Both
halves list two of the three. The middle one is deliberate — Go cannot tell a
non-point from a point without the curve, and the engine refuses the *session*
rather than the listing, so the difference is narrow and written down rather
than left to be discovered.

**What the Japanese name does and does not prove.** It is there so that a name
which is not Latin goes through the code that measures, truncates and lays out
one. Whether it *renders* depends on the machine: a host with no CJK font draws
tofu, and the gates pass on tofu because a box has contrast like any other
glyph. The Arabic in the same name is the half that exercises right-to-left
text on a machine that has the fonts for it.

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
