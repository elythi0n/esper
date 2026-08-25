#!/usr/bin/env bash
# Bring up a Wayland session, run one command inside it, take it down.
#
# # Why the waiting is explicit
#
# Four things have to come up in order — a session bus, PipeWire, the
# compositor, the portal — and each is ready at a moment nothing announces. The
# usual answer is `sleep 2`, and the usual result is a rig that passes on a
# quiet machine and fails on a busy one, which makes every failure ambiguous:
# the thing under test may be broken, or the rig may simply have been early.
#
# So each step waits for a fact and says which fact: a socket that exists, a
# name that is on the bus, an output the compositor admits to having. A step
# that times out says what it was waiting for and stops, rather than letting
# the next step fail for a reason that reads as somebody else's fault.
#
# Usage: wayland-session <command> [args...]
set -uo pipefail

say() { printf '  rig: %s\n' "$*" >&2; }
die() { printf '  rig: %s\n' "$*" >&2; exit 1; }

[ $# -gt 0 ] || die "nothing to run — usage: wayland-session <command> [args...]"
[ -n "${XDG_RUNTIME_DIR:-}" ] || die "no XDG_RUNTIME_DIR, so there is nowhere for a session to be"

# Wait for a shell condition to become true, or say what was never true.
# `$1` seconds, `$2` description, rest the condition.
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

# ── A session bus ───────────────────────────────────────────────────────────
#
# The portal is a D-Bus service and the compositor publishes on the same bus,
# so this is the first thing and everything else depends on it.
bus_address=$(dbus-daemon --session --fork --print-address --print-pid 2>/dev/null | head -1)
[ -n "$bus_address" ] || die "the session bus would not start"
export DBUS_SESSION_BUS_ADDRESS="$bus_address"
await 10 "the session bus to answer" dbus-send --session --dest=org.freedesktop.DBus \
  --type=method_call --print-reply / org.freedesktop.DBus.ListNames
say "session bus up"

# ── PipeWire ────────────────────────────────────────────────────────────────
#
# The ScreenCast portal hands back a PipeWire node, not pixels. Without a
# running PipeWire the portal still answers and the stream is empty, which is
# the failure that looks like a working portal.
# Logged to a file rather than to the terminal. PipeWire and WirePlumber say a
# great deal about bluetooth, libcamera and logind, none of which exist in a
# container and none of which matter here — and a rig whose own noise buries the
# measurement is a rig people stop reading. The logs are kept, not discarded, so
# a real failure is still there to be found.
pipewire > "$XDG_RUNTIME_DIR/pipewire.log" 2>&1 & started+=($!)
await 10 "the PipeWire socket" test -S "$XDG_RUNTIME_DIR/pipewire-0"
wireplumber > "$XDG_RUNTIME_DIR/wireplumber.log" 2>&1 & started+=($!)
say "pipewire up"

# ── The compositor ──────────────────────────────────────────────────────────
mkdir -p "$HOME/.config/sway"
cat > "$HOME/.config/sway/config" <<'CONFIG'
# No bar, no wallpaper, no keybindings: this session exists to be captured, and
# every extra surface is another thing a frame comparison has to explain.
#
# Xwayland off because the image does not ship it, and sway logs its absence as
# an error on every start — an error that is not a fault is an error somebody
# learns to scroll past.
#
# No `bg` either: setting one runs `swaybg`, which is not installed, and the
# complaint reads as an output problem rather than a missing program.
xwayland disable
default_border none
output HEADLESS-1 resolution 1280x720 position 0 0
CONFIG

sway --config "$HOME/.config/sway/config" > "$XDG_RUNTIME_DIR/sway.log" 2>&1 & started+=($!)
await 20 "the Wayland socket" test -S "$XDG_RUNTIME_DIR/$WAYLAND_DISPLAY"

# sway exports `SWAYSOCK` to the processes it starts. This script is its
# sibling rather than its child, so it has to find the socket itself — without
# this, every `swaymsg` answers "Unable to retrieve socket path", which reads
# as the compositor being down when it is up and fine.
await 20 "sway's control socket" sh -c \
  'ls "$XDG_RUNTIME_DIR"/sway-ipc.*.sock >/dev/null 2>&1'
SWAYSOCK=$(ls "$XDG_RUNTIME_DIR"/sway-ipc.*.sock | head -1)
export SWAYSOCK

# The socket existing is not the compositor being ready to serve: it binds
# before it has finished setting up outputs, and a capture against no output is
# a capture of nothing that reports success.
await 20 "the compositor to have an output" sh -c \
  'swaymsg -t get_outputs 2>/dev/null | grep -q HEADLESS'
say "sway up, with an output"

# ── The portal ──────────────────────────────────────────────────────────────
#
# Started rather than left to D-Bus activation: activation works and hides the
# failure, because a portal that cannot start looks exactly like a portal that
# has not been asked for anything yet.
mkdir -p "$HOME/.config/xdg-desktop-portal"
cat > "$HOME/.config/xdg-desktop-portal/portals.conf" <<'CONFIG'
[preferred]
default=wlr
org.freedesktop.impl.portal.ScreenCast=wlr
CONFIG

/usr/lib/xdg-desktop-portal-wlr > "$XDG_RUNTIME_DIR/xdpw.log" 2>&1 & started+=($!)
/usr/lib/xdg-desktop-portal > "$XDG_RUNTIME_DIR/xdp.log" 2>&1 & started+=($!)

on_the_bus() {
  dbus-send --session --dest=org.freedesktop.DBus --type=method_call \
    --print-reply / org.freedesktop.DBus.ListNames 2>/dev/null | grep -q "$1"
}
await 20 "the portal to appear on the bus" on_the_bus org.freedesktop.portal.Desktop
await 20 "the wlroots portal backend" on_the_bus org.freedesktop.impl.portal.desktop.wlr
say "portals up: $(ls /usr/share/xdg-desktop-portal/portals/ | tr '\n' ' ')"

# ── The thing we came for ───────────────────────────────────────────────────
say "running: $*"
"$@"
