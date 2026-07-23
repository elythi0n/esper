# systemd units

Two units, and they are not the same shape: [`esper.service`](esper.service)
runs the engine as the person using it, and
[`esper-broker.service`](esper-broker.service) runs a broker as a service
account on a server. See [`../README.md`](../README.md) for which you want.

## `esper.service` — a **user** service

Esper runs as the user whose screen and shell it exposes. That is not a
limitation to work around: the engine needs no privilege to capture a display
the user is already looking at or to start a shell as them, and running it as
root would turn a compromise of the engine into a compromise of the machine
rather than of one account.

```sh
mkdir -p ~/.config/systemd/user
cp esper.service ~/.config/systemd/user/
systemctl --user daemon-reload
systemctl --user enable --now esper
```

### On a headless server, enable lingering

```sh
sudo loginctl enable-linger $USER
```

Without it systemd stops user services when the last session ends, so Esper
would stop the moment you disconnect — and you could not reconnect to start it
again. This is the single most common way a working install becomes an
unreachable one.

## Settings

The unit runs `esper host` with no arguments, because a service manager has
nowhere to put them. Everything a flag would say lives in
`~/.esper/config.toml`:

```sh
esper-engine config example > ~/.esper/config.toml   # every setting, commented out
$EDITOR ~/.esper/config.toml
esper-engine config show                             # each value, and where it came from
systemctl --user restart esper
```

An empty file — or no file — produces a working system: the defaults are the
secure set, and the file is a list of deliberate departures from them. A key
that is not a setting is a **startup error**, so a typo stops the service
rather than quietly leaving something off.

The one setting that widens what a paired device can *reach* is
`forward.allow`. Empty — the default — means a forward reaches this machine's
own loopback and nothing else; every other target is a line somebody wrote.

## Hardening

The unit removes what the engine never uses: no new privileges, a private
`/tmp`, a read-only home except its own state directory, no kernel tunables, no
namespaces, no writable-executable memory, and a syscall filter limited to
`@system-service`.

Two settings are deliberately *not* tightened:

- **`ProtectProc=default`.** The engine reads `/proc` on Linux to check whether
  a shell it started is still alive, and `invisible` would break teardown's
  ability to notice a survivor.
- **`ProtectHome=read-only` with an explicit `ReadWritePaths`.** Narrower than
  `ProtectHome=true`, which would hide the identity key entirely, and wider
  than nothing. The engine writes exactly one directory.

## Verifying a change to the unit

```sh
systemd-analyze verify packaging/systemd/esper.service
```

Worth running rather than reading. It caught `StartLimitIntervalSec` and
`StartLimitBurst` in `[Service]` where they belong in `[Unit]` — systemd
ignores them silently in the wrong section, so the restart rate limiting would
have been absent with nothing to show it. A unit file that looks right is not
the same as one that is.

(It will complain that `~/.local/bin/esper` is not executable until you have
installed it. That one is expected.)

## What there is no unit for

There is **no system-wide service for the engine**, and that is a decision. A
root daemon would be needed for unattended access at the login screen — before
any user session exists — and that is Phase 14 work with its own security
review. Shipping one now would put a privileged component on servers before it
has been reviewed.

The broker's unit below is a system service and is not that: it runs as a
service account with no capabilities, it never touches a display or a session,
and it holds nothing but its own key.

## `esper-broker.service` — a **system** service

The broker is the only part of Esper meant to face the internet, and it is
packaged accordingly: `DynamicUser=yes` — no account to create and none to
forget to remove — no capabilities at all, a read-only filesystem apart from
`StateDirectory=`, one address family, and a syscall filter without the
privileged, resource and obsolete groups.

`systemd-analyze security esper-broker.service` reports an exposure of **1.1**.
What remains is inherent to a network service: it needs internet sockets, it
needs the host's network, and it cannot have an IP allow list, because the
point of a broker is that anybody you invite can reach it.

Running one, and turning forwarding on, is in [`../README.md`](../README.md).

## What `systemd-analyze security` says about a *user* unit

Run against `esper.service` it reports 4.7, and several of the lines it counts
against the unit are artefacts of analysing a user service as though it were a
system one: "Service runs as root user" (it does not — it runs as you), and a
column of `CapabilityBoundingSet=` findings for capabilities a user service
never has. Adding those directives to a user unit would score better and change
nothing.

The lines worth acting on are the ones about what the *process* can reach, and
those are set.

## The syscall filter and the decoder's own jail

`SystemCallFilter=@system-service` is a deliberate choice and it was **checked
rather than reasoned about**, because reasoning about it gave the wrong answer.

Esper's decoder — the one place a peer's bytes reach a large C parser — puts
itself in a seccomp and Landlock jail before decoding anything, and **refuses
to decode at all** if it cannot. Reading `systemd-analyze syscall-filter
@system-service` suggests the group excludes the `seccomp` and `landlock_*`
calls that jail needs, which would mean the shipped unit quietly disabling the
strongest protection in the product.

It does not. Verified by running it:

```sh
systemd-run --user --wait --pipe \
  --property=SystemCallFilter=@system-service \
  --property=SystemCallErrorNumber=EPERM \
  esper-engine connect <address> --seconds 1
#   decoder    sandboxed — seccomp enforcing, landlock, address space capped…
```

And the check is meaningful, because denying those calls does break it, loudly
and without a fallback:

```sh
systemd-run --user … --property='SystemCallFilter=~@sandbox' …
# decode-worker: refusing to decode without a sandbox: could not install the
# syscall filter: Operation not permitted
```

If you narrow the filter in a drop-in, run that check afterwards.
