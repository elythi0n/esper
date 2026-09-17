# Threat model

Who might attack Esper, from where, and what stops them — or does not.

This is not the same document as [`../SECURITY.md`](../SECURITY.md), which says
what the project claims, which dependencies are unaudited, and how to report a
vulnerability. That one is about the product's honesty. This one is about the
shape of the attack surface: the assets, the boundaries, and what each kind of
adversary can actually reach.

Where the two disagree, `SECURITY.md` is the claim and this is the reasoning.
Tell us, because one of them is then wrong.

## What is worth taking

Ordered by what losing it costs.

| | Why it matters |
|---|---|
| **The input channel** | Anyone who can inject keystrokes into a machine owns that machine, and is limited only by what the account can do. This is the asset. |
| **The screen** | Everything on it: documents, messages, a password being typed, a recovery phrase. Reading it is enough for most attacks that follow. |
| **The device's private key** | It *is* the device. Whoever has it can be that machine to anybody who paired with it. |
| **The trust store** | Which devices are paired and what they may do. Writing to it is a permission grant nobody made. |
| **The audit trail** | The record of what happened. Its value is to the *defender*, and an attacker who can edit it has removed the evidence of everything above. |
| **A live session** | Confidentiality and integrity while it runs. |

## Trust boundaries

Four, and they are the places worth arguing about:

1. **The network**, between two peers. Assumed hostile throughout — every byte
   is authenticated and encrypted end to end, and the transport is the only
   thing holding that line.
2. **The broker**, if one is used. It is *outside* the trust boundary by
   design: see below.
3. **The pairing act.** The one moment a human decides another device may be
   trusted. Everything afterwards rests on it.
4. **The host account.** Esper runs as the person whose screen it serves, and
   anything else running as that person is inside this boundary. That is not a
   gap to be closed; it is what "runs as you" means. It is also the boundary
   that Wayland changes — see *another program on the host*, below.

## The adversaries

### A stranger on the same network

**Can reach:** the discovery announcements, and the port the host listens on.

**Stopped by:** pairing. An unpaired device completes no handshake; the
transport refuses a key it has never been introduced to, before any session
exists. Discovery advertises a fingerprint, and **nothing in a packet decides
who may connect** — a stranger who copies a paired fingerprint into their own
announcement gets listed and then refused at the handshake, having gained a
place in a list and nothing else.

**Not stopped:** they learn that a machine runs Esper, and its fingerprint.
Discovery is on by default and can be turned off; that is the trade, stated.

### Somebody watching the network path

**Can reach:** the packets.

**Stopped by:** end-to-end encryption between the two devices. A broker in the
middle is an introducer, not a party — it holds no session key and decrypts
nothing.

**Not stopped:** traffic analysis. Packet timing and volume say when a session
is happening and roughly how busy the screen is. Nothing here pads or delays to
hide that, and doing so would cost the latency the product exists for.

### The broker operator

A broker is optional and you run it yourself; see [`broker.md`](broker.md).
Treat its operator as an adversary anyway, because sometimes it is somebody
else.

**Can reach:** that two fingerprints asked to be introduced, when, and the
addresses they were introduced at.

**Stopped by:** the broker holding no session keys and decrypting nothing;
every request being signed by the device key it claims; and two peers being
introduced **only when each has named the other**, so a stranger who knows your
fingerprint connects, asks, and waits for ever. Peers pin the broker's whole
key, not a fingerprint of it.

**Not stopped:** the metadata above. A broker that logged it would know who met
whom and when. The shipped one stores nothing, which you can verify because you
run it — and if you did not build it, you are trusting its operator with that
metadata. With `--relay` on, the traffic flows through it as well, still
encrypted, and the volume is then visible too.

### A paired device that has turned hostile

Lost, stolen, or the person on the other end is no longer somebody you trust.
**This is the most likely real attack**, and the least exotic.

**Can reach:** exactly the permissions it holds — `view`, `control`,
`clipboard`, files in either direction, `audio`, `terminal`, `forward`,
`unattended` — and nothing else.

**Stopped by:** permissions being per device and granted deliberately, not
implied by pairing; **a permission being changed by fingerprint and never by
name** — a name is what that device called itself, two devices can share one,
and a device that wanted to be mistaken for another could call itself whatever
that one is called, so `esper grant`, `revoke`, `keep`, `forget`, `schedule`,
`lock-after` and `group` all take the fingerprint and say so when they are
given a name; sensitive grants needing confirmation at the host;
**one-session pairings** that are spent when the session opens, not when it
ends, so an abrupt disconnection does not hand out a second one; a pairing that
lapses after a day even unused; schedules that bound when a device may connect
unattended; and `esper-engine forget`, which removes the record entirely.

**Not stopped:** what they did while trusted. A device holding `control` that
was used to install something has done that, and revoking the pairing
afterwards does not undo it. The audit trail is what tells you.

**A pairing records how it happened.** "A device paired" reads exactly the
same whether somebody was standing at the keyboard reading a code aloud or a
link was pasted out of an email by whoever received it — and those are the two
answers an investigation is looking for. The line names which, and says when
the pairing is spent after one session, because a device let in by a link is a
materially different thing to find in a list a year later.

The trail is **kept under a policy and says when it applied one**:
`audit.retain` and `audit.max_size` remove its oldest lines, and a pass that
removed anything appends a `trail_trimmed` line saying how many went for age,
how many for size, and how many are left. Without it a reader cannot tell a
quiet week from a deleted one, and "the record is incomplete" is a thing an
investigation has to be able to establish from the record.

### The far end of a session you agreed to

The person you called for help, behaving badly.

**Stopped by:** the host deciding everything. Policy is evaluated at the host
and only outcomes are sent, so a controller that ignores a refusal gains
nothing — injection stops at the host, in the code doing the injecting. A
narrowed permission ends the part of the session it applied to. The panic key
ends every session at once and locks the screen; the pause key holds the
picture without ending anything.

**Not stopped:** the screen they were shown while you agreed. And on a host
with no panic key — see below — the way to stop is to end the session by other
means.

### Another program running as the same user on the host

**This is the boundary that changed, and it is worth reading even if you skip
the rest.**

Esper runs as the person whose screen it serves. Anything else running as that
person is inside the same boundary, and on **X11** that has always been true in
the strongest possible way: X11 has no isolation between clients, so any
program on the display can read the screen and synthesise input into any other
window. That is the platform, not a bug in it, and it is why X11 is not the
future of this product.

**Wayland was supposed to close that**, and on a wlroots compositor — sway,
Hyprland, river — it does not close it for these protocols. `zwlr_screencopy`,
`zwp_virtual_keyboard` and `zwlr_virtual_pointer` are granted by the compositor
to **anything that can open its socket**, which is any process running as the
same user. There is no prompt to approve and nothing in the desktop to revoke.
Esper uses exactly those protocols there, so on wlroots the honest statement is:
Esper's capture and injection are available to any program running as you, and
so is everyone else's.

On **GNOME and KDE** the same operations go through `xdg-desktop-portal`, which
does prompt and can be revoked in the desktop's own settings. That is a real
improvement and it is why the portal is preferred where one exists.

**What this means practically:** a compromised account is a compromised session,
on every display server, and more completely on X11 and wlroots than on GNOME.
Nothing Esper does can fix that from inside the account.

### A copy of this machine's state

A stolen laptop, a disk image, a backup, a home directory synced somewhere.
**This adversary needs no code execution at all** — only the bytes.

**Can reach:** `~/.esper`, and that is more than it looks. It holds this
machine's **identity key**, which is what every paired device knows this
machine by; the local API's **token**; the **device list**, which is the whole
of who may connect and what they may do; and the **record** of what happened.
A copy of that directory *is* this machine to every device that has paired
with it — there is nothing else to prove otherwise, because the identity is
the key and nothing about the hardware is bound into it.

**Stopped by:** file permissions, which stop another account on a running
machine and stop nothing once the disk leaves the building. So: **full-disk
encryption is the answer**, it is not Esper's to provide, and it *is* Esper's
to say out loud. `esper doctor` reports whether the state directory is on an
encrypted volume — asked of the kernel's device-mapper UUID rather than
guessed from a device's name, walking down through LVM to the layer that
actually encrypts. A volume it cannot classify is reported as unclassified
rather than as safe.

**Not stopped:** anything, if the volume is not encrypted. Encrypting the
identity key on its own with a passphrase or a platform keychain is a smaller
version of the same answer and a worse one on the platform this ships on
first: on Linux the keychain's own store lives in the same home directory, so
it travels in the same backup — it adds the login password and moves nothing
out of reach. It would also stop a host starting unattended, which is what
most Esper hosts are for. That trade is a decision the project owes and has
not made; until it does, the honest statement is the one above rather than a
setting that reads like protection.

**If it has already happened:** the key is what a peer trusts, so a copy of it
can impersonate this machine until every peer forgets it. `esper-engine forget`
on the *other* machines is what ends that, and re-pairing is what replaces it.
There is no revocation message and no key rotation in this version.

### Somebody at the machine

**Can reach:** everything a person sitting at an unlocked computer can reach.

**Stopped by:** nothing, and that is correct — physical access is out of scope
for every product of this kind. What Esper does do is the reverse: the person
at the machine is *told* when it is being watched, by an indicator that cannot
be covered by a fullscreen window and takes no clicks, and the host **refuses
to serve** where it cannot show one.

**Not stopped:** the indicator can be turned off with
`host.serve_without_indicator`, which exists for headless servers with no
screen to put one on. On a machine with a screen it is the wrong setting, and
the host says so when it starts.

It is also one of six settings the **local control plane will not change**. The
window can edit every other setting this machine has; these six are refused
with the command to run in a terminal instead, because each either removes the
ability of whoever is in front of the screen to know what is happening to it
(`host.serve_without_indicator`, `audit.retain`, `audit.max_size`) or widens
what a paired device may reach without anybody granting it anything
(`files.root`, `forward.allow`) — or is a credential, which is typed where the
person is (`broker.key`).

This is the same rule that stops `unattended` being granted over the control
plane, and it buys the same thing: not protection from an attacker who already
runs as this user — they can edit the file — but that **no ordinary path**
through the product reaches these without somebody at the machine.

### The defaults that are security decisions

A setting whose default decides something about security is listed here with
what the default is and why. **The list and the product are held to each other
by a test**, in both directions — a setting added to the code and not to this
table is a failure, and so is a row here for a setting that does not exist.

| Setting | Default | Why |
|---|---|---|
| `broker.address` | none | Esper operates no infrastructure and ships no default broker. Without one, the broker tiers do not exist |
| `broker.key` | none | A broker is pinned to a key or it is not pinned |
| `host.serve_without_indicator` | `false` | The indicator is how somebody at the machine knows it is being watched |
| `host.privacy_screen` | `false` | Off is what a person expects a screen to do |
| `host.lock_local_input` | `false` | Taking this machine's own keyboard away is a thing to ask for |
| `host.allow_asking` | `true` | A host that cannot ask serves only devices holding `unattended` |
| `host.lock_after_session` | `unattended` | Lock when nobody here agreed to it, which is the case where nobody is watching |
| `policy.deny_when_locked` | `false` | A locked screen is not by itself a refusal; making it one is a choice |
| `session.idle_timeout` | none | A timeout that killed a working session by default is data loss wearing a security feature's clothes |
| `files.enabled` | `false` | File transfer is a distinct capability and is asked for |
| `files.root` | none | Nothing is reachable until somebody says what is |
| `forward.allow` | none | Which means loopback and nothing else |
| `audit.retain` / `audit.max_size` | 90 days / 64 MB | Two limits, because they fail differently |
| `network.port_mapping` | `false` | A mapping is a hole in a firewall, opened by a program, on a router the person may share with a household or an office. What it buys is skipping an introducer — not making anything work that otherwise would not — and what it costs is being reachable from the whole internet rather than from whoever a broker introduced. The lifetime asked for is two minutes, renewed after one, so a host that is unplugged rather than shut down leaves the hole open for at most that long |
| `discovery.mdns` | `true` | The one deliberate exception: convenience on a network you are already on, and it announces a fingerprint rather than anything private |

**Two things are not settings, deliberately, and that is stronger than a
secure default.** The decoder's sandbox has no switch at all — a session
refuses a decoder that is not enforcing, there is no fallback, and `esper
sandbox` is the diagnostic that reports it. And what a device may do is a
*permission* per device, not a global flag: there is no `clipboard.inbound` to
leave on by accident, because a device either holds `clipboard_in` or it does
not.

**Every change to a setting is recorded**, whichever path it came by: the
command in a terminal and the window's own settings screen both go through
`esper-engine config`, and it writes a `setting_changed` line naming the
setting, what it was and what it is now. That line has no peer on it — nobody
connected, somebody at the keyboard did it — and it is written *before* the
file is, so shortening `audit.retain` leaves its own trace in the part of the
record that survives the shortening. **The value of a credential is never in
it**: a change to `broker.key` is recorded as having happened and the key
itself is not written down, because the trail is the file a person is told to
read and attach to a bug report.

The settings file is written `0600`, and a file that holds `broker.key` and is
readable by others is **said** at startup rather than refused. Refusing would be
a machine that will not start over a mode; the API token does refuse, because a
token *is* the credential for the local control plane and there is nothing else
it could be, while this file usually holds no credential at all.

### Somebody in the supply chain

**Can reach:** you, through a dependency or a build.

**Stopped by:** builds being reproducible, so the published binary can be
checked against the source; `cargo-deny` and `cargo-vet` over the dependency
graph; `unsafe` forbidden outside a declared inventory; and this project
declining large dependencies where a narrow one can be written — it speaks
mDNS, D-Bus and the parts of the desktop protocols it needs directly, which is
tens of thousands of lines of other people's code not in the binary.

**Not stopped:** a compromise of a dependency that is still in the graph, or of
the toolchain. No release has been signed yet, and the signing key is not held
offline yet; both are open.

## Deliberately outside the model

- **Physical access**, as above.
- **A compromised host account** — see the boundary above.
- **Denial of service against a machine you own.** Esper refuses rather than
  degrades in most places, which means an attacker who can make it refuse has
  stopped a session. That is judged the right trade: a remote desktop that
  keeps going when it cannot show an indicator is worse.
- **Traffic analysis**, as above.

## Where this model is weakest

Stated because a threat model that names no weak points is an advertisement.

1. **No third-party audit.** The whole thing is reasoned, not attacked by
   somebody paid to attack it.
2. **The privileged components do not exist yet.** Unattended access at the
   login screen needs something running with more than a user's rights, and
   that will be the largest surface in the product when it arrives. It is not
   here, so it is not modelled, and this section is where that will go.
3. **The decoder's sandbox is asserted, not measured.** A controller decodes
   video that a host chose, and the H.264 decoder is a large C library.
4. **The panic key does not exist on Wayland.** The platform gives an ordinary
   client no global hotkey, so a Wayland host has an indicator and no key to
   stop with. The mechanism that would provide one injects below the compositor
   and has its own costs.
5. **One implementation.** Nothing independent speaks this protocol, so no
   disagreement between two implementations has ever caught anything.
