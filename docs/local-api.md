# The local control-plane API

`esper api` serves a small HTTP API on loopback. It is what the Esper window
talks to, and it is what anything else on this machine — a dashboard, a script,
an MCP adapter — can talk to instead. It is **not** a peer protocol: nothing
across the network ever reaches it. For what two machines say to each other,
see [`protocol.md`](protocol.md).

This document is held to the code by `scripts/check-local-api.sh`, which
compares the routes below against the ones the server registers, in both
directions.

## Getting in

Three things, all of them required, none of them negotiable.

**It binds loopback.** `esper api --listen 127.0.0.1:<port>` by default. There
is an `--exposed` flag for somebody who means it; everything below still
applies.

**A bearer token.** `esper api` writes one to `$ESPER_HOME/api.token`, mode
`0600`, and refuses to start if that file is readable by anyone else. Send it
as `Authorization: Bearer <token>`. It is compared in constant time. Without
it: `401` and a `WWW-Authenticate` header.

**Addressed by address, never by name.** The Host header must be the literal
IP this server bound — `127.0.0.1:7645`, not `localhost:7645`. A request
addressed by a name gets `404` and nothing else, deliberately: a page probing
this from a browser learns nothing about what is here.

That last one is the **DNS-rebinding defence**. A page in a browser can send
requests to `127.0.0.1` under a hostname it controls; the name resolves to
loopback, the browser treats the origin as its own, and the page reads the
reply. Accepting only IP literals closes it, because a name cannot be rebound
to something a check that refuses names will accept.

Together with the token — a header, which a form cannot set and a page cannot
read — that is the whole of the posture, and it is the same for reads and for
writes. Nothing further is needed and nothing less would do.

## Reading

| | |
|---|---|
| `GET /health` | Whether this is an Esper control plane and it is up. |
| `GET /status` | What this machine can do: capture, encoder, input, overlays, mDNS, broker, how many devices are paired, and what kind of machine it is. The same object `esper doctor --json` carries as `machine`. |
| `GET /identity` | This machine's own key and fingerprint. See below. |
| `GET /host` | Whether this machine is serving its screen, and how. |
| `GET /sessions` | The local record of what happened. `?most=<n>` bounds it; a value that is not a number is a `400`. |
| `GET /devices` | Every paired device, what it may do, when it was paired. |
| `GET /devices/reachable` | Which of them this machine can see on the network now. |
| `GET /config` | Every setting, its value, and where that value came from. |
| `GET /events` | Server-Sent Events: one `session` event per line added to the record, as it happens. |

### This machine's identity

```json
{ "key": "69aca716fe96975c",
  "public_key": "00fb8180…cfb43962",
  "words": ["jumbo", "puppet", …],
  "file": "…/identity.key" }
```

`key` is the fingerprint — sixty-four bits, the form every screen shows and
the form that names a device in a command. `public_key` is the whole key.
**Pin on `public_key`**: a fingerprint is a birthday bound, not an identity.
`words` is the same fingerprint as eight words, which is what two people read
to each other; the order is part of the check.

These are the engine's own arithmetic, asked for rather than derived here — one
hash, one alphabet, one word list, so the two halves of this product cannot
come to disagree about what a machine is.

### The event stream

`GET /events` is a **notification, not a replay**: nothing is sent for what
happened before the stream opened, because `/sessions` is the record and a
stream that tried to be both would be a second, worse copy of a file.

A reader that falls behind loses lines rather than growing this process's
memory. At most **64 streams** may be open at once; the next one is refused
with `503` and a sentence saying so, *before* any stream headers go out — a
`200` followed by an error in the body is what every SSE client reads as a
connection that opened.

## Writing

| | |
|---|---|
| `POST /engine/restart` | Stop and start the engine beside this control plane. |
| `POST /host` | Start or stop serving this machine's screen. |
| `POST /config` | Change one setting, or take one back out. |
| `POST /devices/{fingerprint}/grant` | Give a device a permission. |
| `POST /devices/{fingerprint}/revoke` | Take one away. |
| `DELETE /devices/{fingerprint}` | Unpair it entirely. |
| `POST /devices/{fingerprint}/keep` | Make a one-session pairing last. |
| `POST /devices/{fingerprint}/schedule` | When it may connect unattended, in local time. |
| `POST /devices/{fingerprint}/lock-after` | Whether a session with it leaves this screen locked. |
| `POST /devices/{fingerprint}/groups` | Which groups it is in. |
| `POST /devices/{fingerprint}/wake` | Send it a magic packet. |
| `POST /devices/{fingerprint}/lock` | Lock that machine's screen. |
| `POST /devices/{fingerprint}/logoff` | End the login session on it. |
| `POST /devices/{fingerprint}/reboot` | Restart it. |

The last three act on the **far** machine and each needs a permission that host
granted this one; a refusal comes back from there, not from here.

`{fingerprint}` is the device's fingerprint in hex — the `fingerprint.hex` of
`GET /devices`. A prefix that names exactly one device is enough.

Every body is JSON and every body is **bounded**: 4096 bytes for a device or
setting change, 1024 for the smaller ones. A larger one is refused rather than
read.

## Pairing

| | |
|---|---|
| `POST /pair/offer` | Offer a code somebody else types. |
| `GET /pair/offer` | What that offer is doing now. |
| `DELETE /pair/offer` | Withdraw it. |
| `POST /pair/join` | Take up somebody else's offer. |
| `POST /pair/accept` | Accept a device that is asking. |
| `POST /invite` | Offer a link, through a broker you run. |
| `GET /invite` | What that invitation is doing now. |
| `DELETE /invite` | Withdraw it. |

### Offering a code

`POST /pair/offer` starts a listener and answers with what it is offering:

```json
{ "code": "7DAH-7HKC", "port": 40681,
  "words": ["prairie", "toaster", "honey", "cover", "window", "siren", "lens", "album"],
  "expires_at": 1789400000, "state": "offering" }
```

`GET /pair/offer` answers the same thing while it runs, with `state` becoming
`paired`, `expired` or `failed`; a `failed` one carries `detail`.

**`words` is not optional and not decoration.** The code proves that whoever
typed it was read it; only the fingerprint proves the machine at the other end
is the one that was meant. An offer arriving from the engine without them is
refused here rather than passed on, and the window will not show a code it has
no fingerprint for — a blank where the check belongs reads as "nothing to
check".

Both this and `POST /invite` read the engine's `--json` stream, not its printed
report. See [`scripting.md`](scripting.md).

## What this API will not do

**It will not grant `unattended`.** That permission means a device can open a
session with nobody present to agree to it, and it is granted where the person
is: `esper grant <device> unattended`, in a terminal.

**It will not change six settings.** `host.serve_without_indicator`,
`audit.retain`, `audit.max_size`, `files.root`, `forward.allow` and
`broker.key` are refused with `403` and the command to run instead. Each either
removes the ability of whoever is in front of the screen to know what is
happening to it, widens what a paired device may reach without anybody granting
it anything, or is a credential — and a credential is typed where the person
is. See [`threat-model.md`](threat-model.md).

Neither is protection from an attacker who already runs as this user; both
mean that **no ordinary path** through the product reaches these without
somebody at the machine.

**It does not serve the record to anywhere else.** There is no transmit path
for `sessions.log` anywhere in Esper, and `make check-trail` is what keeps that
true.

## Answers you will get

| | |
|---|---|
| `200` | It worked. |
| `400` | The request was not one — a bad number, a body that is not JSON. |
| `401` | No token, or the wrong one. |
| `403` | A thing this API will not do. See above. |
| `404` | Addressed by a name, or a setting or device that does not exist. |
| `502` | The engine refused, and its own words are in the body. |
| `503` | Too many event streams are open. |
