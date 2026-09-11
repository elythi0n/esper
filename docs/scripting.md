# Reading Esper from a script

Every command that **reports** answers `--json` as well as printing. The printed
form is written for a person and is free to change; the object is the contract.

This document lists the commands that answer, and what each object holds.
`scripts/check-scripting.sh` runs the binaries and holds this list to them, in
both directions — a command that starts answering `--json` and is not here is a
failure, and so is a line here for a command that does not.

## The exit status comes first

Every one of these exits the way its printed form does, so a script that only
wants a yes or no never has to parse anything:

```sh
if esper doctor --json > /dev/null; then echo "this machine can host"; fi
```

## `esper version --json`

```json
{ "version": "v1.2.0", "buildTime": "2026-01-01T00:00:00Z", "protocol": 1 }
```

All three binaries answer this with the same three keys. `protocol` is what two
machines have to agree on, and changes for different reasons from `version` —
see [`compatibility.md`](compatibility.md).

## `esper doctor --json`

The whole diagnostic: this program, the engine beside it, what the machine can
do, how it can be reached, and everything the printed report would have marked
with a ⚠.

```json
{
  "esper":   { "version": "v1.2.0", "built": "…", "protocol": 1 },
  "engine":  { "path": "…", "reached": true, "version": "v1.2.0", "protocol": 1 },
  "machine": { "capture": {…}, "encoder": {…}, "input": {…}, "paired": 0, … },
  "state":   "/home/somebody/.esper",
  "ways_in": [ { "name": "given", "available": true, "detail": "…" }, … ],
  "ready":   false,
  "warnings": [ "…" ]
}
```

`machine` is the engine's own status, with the same field names the local API
answers with. Each capability is `{ "available", "detail", "attempts" }`:
`detail` is one line, and `attempts` is every backend that was tried, present
only where something is missing and there was more than one thing to try.

`engine.reached` is `false` where the engine could not be started or spoken to
at all, and `engine.trouble` says why. `machine` is absent in that case.

In `ways_in`, `available` is `null` for `remembered`: that rung belongs to the
*controller's* store, and a host cannot see whether somebody else remembers it.
A missing rung's `detail` says how to have it rather than only that it is
missing.

`--session --json` adds a `session` object holding the self test's steps:

```json
"session": {
  "steps": [ { "name": "paired", "ok": true, "detail": "…",
               "figures": { "seconds": 0.33 } }, … ],
  "warnings": [], "verdict": "…", "ok": true
}
```

A failed step carries `evidence`: the raw lines the thing that failed produced.
`figures` are the numbers behind the sentence, for a reader that would rather
not parse prose.

## `esper devices --json`

The device store: every paired device, what it may do, when it was paired, and
its fingerprint in both forms.

## `esper sessions --json`

The local record of what happened, newest last. `key` and `peer` are empty on a
line about no peer at all — somebody at the machine changed a setting, or the
record trimmed itself.

## `esper identity --json`

```json
{ "public_key": "059b1e82…8a099827",
  "fingerprint": { "hex": "be26a51e6e1fc9a1", "words": ["rustic", …] },
  "file": "…/identity.key" }
```

The same three field names `devices --json` uses for the same facts.
`public_key` is the whole key, which is what a peer pins; `fingerprint.hex` is
sixty-four bits of it, which is how every other part of Esper names a device on
a command line, and `fingerprint.words` is the form a person reads aloud.
**Pin on `public_key`** — sixty-four bits is a birthday bound, not an identity.

## `esper input --json`

```json
{ "available": false, "detail": "nothing — this machine is view only",
  "attempts": [ "x11 xtest: …", "wayland-portal: …" ] }
```

The same three keys `doctor --json` uses for the same capability. This is the
answer to "why does nothing I type arrive".

## `esper sandbox --json`

```json
{ "enforcing": true,
  "layers": { "address_space": true, "open_files": true, "no_new_privs": true,
              "landlock": true, "seccomp": true, "killing": true },
  "detail": "seccomp enforcing, landlock, address space capped, no new privileges",
  "reachable": ["nothing but the bytes it is handed: no file, no socket, no other process"],
  "audit_requested": false }
```

The jail a peer's video is decoded in. `enforcing` is the whole answer;
`layers` says which parts applied, and **`reachable` says what a compromised
decoder could touch** — the consequence rather than the layer, because "no
landlock" is a fact about a kernel and "it can read this machine's identity
key" is a fact about the machine.

**The exit status is the answer too**: nought when the sandbox is in force,
one when it is not.

Nothing decodes a peer's bitstream here and no session is started. Setting
`ESPER_SANDBOX_AUDIT=1` turns the syscall filter into log-and-allow so a
syscall missing from it can be found; **a session still refuses a decoder that
is not enforcing**, whatever is set in the environment, and this command
reports `enforcing: false` rather than pretending otherwise.

## `esper discover --json`

```json
{ "paired": [ { "fingerprint": "…", "address": "…", "name": "…" } ],
  "candidates": [ … ], "unreadable": 0 }
```

`paired` is what this machine can connect to now; `candidates` need a code read
off the other machine's screen. `unreadable` counts packets on the network this
could not parse, which is either another implementation or somebody probing.

`--porcelain` answers the same browse as one line per device, tab separated —
`kind`, fingerprint, address, name — for a shell rather than a program. Asking
for both is refused: they are two shapes of one list.

## `esper pair --json` and `esper invite --json`

These two are **streams**, not single objects: one JSON object per line,
flushed as it happens, because both run for minutes and say two things — here
is the way in, and here is who used it.

```json
{"event":"offering","code":"7DAH-7HKC","address":"0.0.0.0:40681","port":40681,
 "expires_in":59,"fingerprint":{"hex":"a9df5d28f8ca7404","words":["prairie",…]}}
{"event":"paired","name":"elythion","fingerprint":{…},"granted":["view"],"once":false}
```

`invite --json` answers `{"event":"offering","link":"esper://join/…",
"broker":"…","for":"…","expires_at":…,"fingerprint":{…}}` and then the same
`paired` line. `pair <addr> --code <code> --json` answers `{"event":"joining",
"address":"…"}` first.

**Read the stream, never the printed form.** The printed form is written for
the person reading a code off one screen and a fingerprint aloud, and it is
free to change; what a parser over it would get wrong is the code somebody
types and the words they read to confirm a machine.

A line a reader does not recognise should be skipped rather than treated as an
error — an engine that grows a new event must not stop an older reader working.
Anything that is not JSON at all is the engine explaining a refusal in its own
words; keep it and show it.

## `esper config --json`

Every setting, its value, and where that value came from — the file, the
environment, a flag, or the default.

## The broker

`esper-broker` is a separate program, run by whoever operates a broker. It
answers the same two questions in the same shapes:

- **`esper-broker version --json`** — the same three keys as `esper version
  --json`.
- **`esper-broker identity --json`** — the same three field names as `esper
  identity --json`. `public_key` here is the value peers pass to
  `--broker-key`.

`check-messages` holds both binaries to one meaning for `key`: it is always the
whole key, never the fingerprint.

## What does not answer

`connect`, `host`, `terminal`, `forward`, `cp`, `screenshot`, `pair` and the
rest **do** things rather than report, and their output is progress. Where one
of them produces a result worth reading it is a file, not a stream to parse.
