# systemd units

## `ownview.service` — a **user** service

OwnView runs as the user whose screen and shell it exposes. That is not a
limitation to work around: the engine needs no privilege to capture a display
the user is already looking at or to start a shell as them, and running it as
root would turn a compromise of the engine into a compromise of the machine
rather than of one account.

```sh
mkdir -p ~/.config/systemd/user
cp ownview.service ~/.config/systemd/user/
systemctl --user daemon-reload
systemctl --user enable --now ownview
```

### On a headless server, enable lingering

```sh
sudo loginctl enable-linger $USER
```

Without it systemd stops user services when the last session ends, so OwnView
would stop the moment you disconnect — and you could not reconnect to start it
again. This is the single most common way a working install becomes an
unreachable one.

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
systemd-analyze verify packaging/systemd/ownview.service
```

Worth running rather than reading. It caught `StartLimitIntervalSec` and
`StartLimitBurst` in `[Service]` where they belong in `[Unit]` — systemd
ignores them silently in the wrong section, so the restart rate limiting would
have been absent with nothing to show it. A unit file that looks right is not
the same as one that is.

(It will complain that `~/.local/bin/ownview` is not executable until you have
installed it. That one is expected.)

## What there is no unit for

There is **no system-wide service**, and that is a decision. A root daemon would
be needed for unattended access at the login screen — before any user session
exists — and that is Phase 14 work with its own security review. Shipping one
now would put a privileged component on servers before it has been reviewed.
