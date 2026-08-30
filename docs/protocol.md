# The Esper protocol

What goes on the wire between two Esper devices, written so that somebody who
is not this project could implement it.

That is the point of writing it down. Esper has **one implementation**, so no
disagreement between two of them has ever caught anything — which is a real gap
and is named as one in [`threat-model.md`](threat-model.md). A specification is
the first step out of it.

This describes **protocol version 1**. Versioning, and what a change to it
costs, is in [`compatibility.md`](compatibility.md).

## The shape of it

```text
   QUIC connection  (TLS 1.3, raw public keys, no certificate authority)
   └── stream 0            control: the session's own messages
   └── stream n            media, input, clipboard, files, a terminal, a forward
        └── frame           u32 length (big-endian) + CBOR map
             └── message      a tagged union, one per purpose
```

Four layers, and each does one job: QUIC carries streams, TLS authenticates the
two devices to each other, the framing delimits messages, and CBOR encodes
them.

## Identity

A device **is** an Ed25519 key pair. There are no accounts, no usernames and no
server that issues anything.

The **fingerprint** is what a person reads: sixteen hexadecimal characters, and
the same value rendered as eight words from a fixed list of 256 for reading
aloud. The words and the hex are the same number; either can be typed.

The fingerprint is a *short* form and is for humans comparing two screens. It
is never what anything pins: the trust store holds whole keys, and the broker's
key is pinned in full, because pinning against sixty-four bits would be a
security decision made on a birthday bound.

## The handshake

TLS 1.3 with **raw public keys** ([RFC 7250]), not certificates. There is no
certificate authority anywhere in the system and nothing to expire: each side
presents its Ed25519 key, and each side checks the key it got against the one
it has stored for that device.

**Key agreement prefers a hybrid post-quantum group.** The TLS stack is
configured with `prefer-post-quantum`, so where both ends support it the
session key comes from X25519 combined with ML-KEM rather than from X25519
alone. The point is *harvest now, decrypt later*: a session recorded today by
somebody patient is not opened by a quantum computer arriving afterwards. It is
hybrid rather than post-quantum alone, so a flaw in the newer construction
leaves X25519 still standing.

A key that is not in the trust store ends the connection before any session
exists. That check is the whole of the access control at this layer — there is
no "connect first, authorise later".

Both `PROTOCOL_VERSION` values must be **equal**. They are not negotiated: a
mismatch closes with reason 100 naming both numbers, so the person is told
which end to upgrade rather than left with a session that half works.

[RFC 7250]: https://www.rfc-editor.org/rfc/rfc7250

## Pairing

Two devices that have never met use a **balanced PAKE** (SPAKE2) over a short
secret the two people share out of band — spoken aloud, typed from a screen.

The exchange is three messages: each side sends its SPAKE2 element, then a
confirmation, then the result. What the PAKE protects is that a passive
observer of the exchange, and an active attacker who does not know the short
secret, learn nothing usable — and each wrong guess costs an attacker one
attempt, rather than an offline search.

During pairing, and *only* during pairing, the transport accepts a key it has
never seen: at first contact neither side has the other's, and the code inside
the PAKE is what authenticates. Every other connection refuses an unknown key.

A pairing may be **one session only**, which is the default for an invitation.
It is spent when the session **opens**, not when it ends — a pairing spent on
the way out would hand a second session to anybody who could end the first one
abruptly, and an abrupt ending is when nobody is watching. It also lapses after
a day even unused.

## Framing

Every stream carries the same framing:

```text
┌──────────────┬───────────────────────────┐
│ u32 len (BE) │ CBOR map                  │
└──────────────┴───────────────────────────┘
```

This is the first parser attacker-controlled bytes reach, so an implementation
should hold it to a stricter standard than the rest:

- **Validate the length before allocating.** A peer claiming four gigabytes
  must cost a rejected header, not four gigabytes.
- **No panicking constructs on this path.** A panic reachable from the network
  is a denial of service.
- **Refuse with a reason code** rather than dropping the connection, so the far
  end learns what was wrong.

## Messages

CBOR maps, each a tagged union. They are grouped by purpose, and a stream
carries one group:

| Group | Carries |
|---|---|
| `MediaMessage` | Video and audio, and the negotiation around them |
| `InputMessage` | Keys, pointer motion, buttons, scroll |
| `CursorMessage` | The cursor's shape and position, sent apart from the picture so the far end can draw it without waiting for a frame |
| `ClipboardMessage` | Clipboard in either direction |
| `FileMessage` | File transfer in either direction |
| `TerminalMessage` | A shell session |

Each union has a **direction table**: which end may send which message. That
table is not decoration — a table wrong in the permissive direction is an
escalation hole, and this project has had one. An implementation should refuse
a message arriving from the wrong end.

### Keys are positions, not characters

An `InputMessage` carrying a key does **not** carry a character. It carries a
**USB HID usage** from page 0x07 — *where the key is on the keyboard* — and the
host maps that to whatever its own layout produces.

This is the single most consequential decision in the protocol. It means
somebody on a Finnish keyboard controlling a US-layout machine gets what the
*host* would type, which is what a remote desktop is for, and it means neither
end has to know the other's layout. An implementation that sent characters
would be a different protocol.

Scroll carries **notches** always and pixels optionally: a host that can only
click a wheel must never be handed a message it cannot honour.

## Streams

The control stream is opened first and lives for the session. Every other
stream is opened by the **controller**, for a purpose it names — there is no
stream the host starts, which is why a permission withdrawal travels on the
control stream rather than on the stream it concerns.

Stream purposes include input, clipboard, files, a terminal and a forward.

## Closing

Closures carry a **reason code**, so an ending is always explained:

| Code | Meaning |
|---|---|
| 1 | Closed by the person |
| 2 | The session finished |
| 3 | The host is shutting down |
| 4 | Superseded by another session |
| 5 | The peer went away |
| 6 | Stopped at the host |
| 100 | Protocol version mismatch |
| 101 | Malformed frame |
| 102 | Frame too large |
| 103 | Unknown message |

Codes below 100 are ordinary endings; 100 and above are faults. An
implementation should send the most specific one it has rather than a generic
close, because the code is what the far end shows a person.

## What the host decides

**Policy is evaluated at the host and only outcomes are sent.** A controller
never receives the rules, only what they permitted. That matters for anybody
implementing a controller: there is nothing to enforce at your end, and
nothing you can gain by not enforcing it — injection stops at the host, in the
code doing the injecting.

The permissions a host may hold for a device are `view`, `control`, clipboard
read and write, files in each direction, `audio`, `terminal`, `forward` and
`unattended`. They are per device and granted deliberately; pairing implies
none of them.

## Implementing this

The fixtures in `testdata/fixtures/` are the closest thing to a conformance
suite: they are real encoded messages, written by this implementation and read
by both halves of it. A second implementation that reads them and produces the
same bytes agrees with this one about the wire format, which is the part a
specification in prose is worst at pinning down.

What is **not** specified here, because it is not yet stable: the media
negotiation's details, and the local control-plane API, which is this project's
own front door rather than a peer protocol.
