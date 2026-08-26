#!/usr/bin/env bash
# Bring up a headless GNOME compositor, run one command inside it, take it down.
#
# The sway rig's twin — see `wayland-session.sh` for why every wait is explicit
# and named. The shape is deliberately the same; what differs is the middle,
# and the differences are the point:
#
#   - mutter is started with `--headless --virtual-monitor`, which is the mode
#     GNOME's own testing uses, rather than a backend chosen by an environment
#     variable.
#   - There is no `swaymsg`, so readiness is the Wayland socket plus mutter's
#     own D-Bus name — `org.gnome.Mutter.ScreenCast` is the thing the portal
#     talks to, so a portal that came up before it would be a portal with
#     nothing behind it.
#   - The portal backend is `xdg-desktop-portal-gnome`, which unlike the
#     wlroots one implements RemoteDesktop, and that is the whole reason this
#     rig exists.
#
# Usage: gnome-session-rig <command> [args...]
set -uo pipefail

say() { printf '  rig: %s\n' "$*" >&2; }
die() { printf '  rig: %s\n' "$*" >&2; exit 1; }

[ $# -gt 0 ] || die "nothing to run — usage: gnome-session-rig <command> [args...]"
[ -n "${XDG_RUNTIME_DIR:-}" ] || die "no XDG_RUNTIME_DIR, so there is nowhere for a session to be"

await() {
  local limit=$1 what=$2
  shift 2
  local waited=0
  while ! "$@" >/dev/null 2>&1; do
    waited=$((waited + 1))
    if [ "$waited" -gt $((limit * 10)) ]; then
      die "gave up waiting for $what after ${limit}s"
    fi
    sleep 0.1
  done
}

started=()
cleanup() {
  local code=$?
  for pid in "${started[@]+"${started[@]}"}"; do
    kill "$pid" 2>/dev/null
  done
  wait 2>/dev/null
  exit "$code"
}
trap cleanup EXIT INT TERM

on_the_bus() {
  dbus-send --session --dest=org.freedesktop.DBus --type=method_call \
    --print-reply / org.freedesktop.DBus.ListNames 2>/dev/null | grep -q "$1"
}

# ── A session bus ───────────────────────────────────────────────────────────
bus_address=$(dbus-daemon --session --fork --print-address 2>/dev/null | head -1)
[ -n "$bus_address" ] || die "the session bus would not start"
export DBUS_SESSION_BUS_ADDRESS="$bus_address"
await 10 "the session bus to answer" dbus-send --session --dest=org.freedesktop.DBus \
  --type=method_call --print-reply / org.freedesktop.DBus.ListNames
say "session bus up"

# ── PipeWire ────────────────────────────────────────────────────────────────
#
# Logged to a file: PipeWire has a great deal to say about bluetooth and
# logind, none of which exist here, and a rig whose noise buries the
# measurement is a rig people stop reading.
pipewire > "$XDG_RUNTIME_DIR/pipewire.log" 2>&1 & started+=($!)
await 10 "the PipeWire socket" test -S "$XDG_RUNTIME_DIR/pipewire-0"
wireplumber > "$XDG_RUNTIME_DIR/wireplumber.log" 2>&1 & started+=($!)
say "pipewire up"

# ── The compositor ──────────────────────────────────────────────────────────
mutter --headless --virtual-monitor 1280x720 \
  > "$XDG_RUNTIME_DIR/mutter.log" 2>&1 & started+=($!)
await 30 "the Wayland socket" test -S "$XDG_RUNTIME_DIR/$WAYLAND_DISPLAY"

# mutter binds its socket before it has published the interfaces the portal
# needs. A portal that started first is a portal with nothing behind it, and it
# answers introspection perfectly while being useless.
await 30 "mutter's screen-cast interface" on_the_bus org.gnome.Mutter.ScreenCast
await 30 "mutter's remote-desktop interface" on_the_bus org.gnome.Mutter.RemoteDesktop
say "mutter up, with a virtual monitor"

# ── The portal ──────────────────────────────────────────────────────────────
mkdir -p "$HOME/.config/xdg-desktop-portal"
cat > "$HOME/.config/xdg-desktop-portal/portals.conf" <<'CONFIG'
[preferred]
default=gnome
CONFIG

/usr/lib/xdg-desktop-portal-gnome > "$XDG_RUNTIME_DIR/xdpg.log" 2>&1 & started+=($!)
/usr/lib/xdg-desktop-portal > "$XDG_RUNTIME_DIR/xdp.log" 2>&1 & started+=($!)

await 30 "the portal to appear on the bus" on_the_bus org.freedesktop.portal.Desktop
await 30 "the GNOME portal backend" on_the_bus org.freedesktop.impl.portal.desktop.gnome
say "portals up"

say "running: $1"
"$@"
