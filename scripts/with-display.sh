#!/usr/bin/env bash
# Run a command against a display of its own.
#
# # Why the suite cannot use the one that is already there
#
# `make test-rust` was `cargo test --workspace`, which on a machine with a
# desktop runs the X11 suites against **that desktop** — the developer's own
# screen. Three things follow and all of them happened:
#
#   - Capture reads pixels back and gets whatever is on screen, so
#     `GetImage` fails with a `Match` error the moment a window it measured
#     moves. Five tests failed this way on 2026-09-15 and none of them was a
#     defect.
#   - Presence draws a frame over everything and input takes the focus, on a
#     screen somebody is using.
#   - The resolution tests change the resolution. Of the desktop.
#
# # Why Xephyr and not just Xvfb
#
# `Xvfb` answers RandR with one mode and a maximum equal to its current size,
# so nothing that changes a resolution can be tested on it at all. `Xephyr` is
# a real X server with a real RandR 1.6 implementation and nine modes, and it
# nests inside `Xvfb` — so the whole arrangement is still headless and the
# tests that need modes have them.
#
# Usage: scripts/with-display.sh <command> [args...]
set -euo pipefail

command -v Xvfb   >/dev/null || { echo "  ✗ with-display: Xvfb is not installed"   >&2; exit 1; }
command -v Xephyr >/dev/null || { echo "  ✗ with-display: Xephyr is not installed" >&2; exit 1; }

# A display number nothing else is on. Checked rather than assumed: this runs
# beside other suites and a collision reads as a failure in whatever was
# running, not here.
pick_display() {
  local n
  for _ in $(seq 40); do
    n=$(( (RANDOM % 60) + 120 ))
    if [ ! -e "/tmp/.X${n}-lock" ] && ! xdpyinfo -display ":$n" >/dev/null 2>&1; then
      echo "$n"
      return 0
    fi
  done
  echo "  ✗ with-display: no free display number" >&2
  return 1
}

OUTER=$(pick_display)
INNER=$(pick_display)
while [ "$INNER" = "$OUTER" ]; do INNER=$(pick_display); done

outer_pid=""
inner_pid=""
cleanup() {
  [ -n "$inner_pid" ] && kill "$inner_pid" 2>/dev/null || true
  [ -n "$outer_pid" ] && kill "$outer_pid" 2>/dev/null || true
  rm -f "/tmp/.X${OUTER}-lock" "/tmp/.X${INNER}-lock"
}
trap cleanup EXIT INT TERM

# The outer one only exists to give Xephyr somewhere to draw.
Xvfb ":$OUTER" -noreset -screen 0 1920x1440x24 >/dev/null 2>&1 &
outer_pid=$!
for _ in $(seq 50); do
  xdpyinfo -display ":$OUTER" >/dev/null 2>&1 && break
  sleep 0.1
done
xdpyinfo -display ":$OUTER" >/dev/null 2>&1 || {
  echo "  ✗ with-display: Xvfb :$OUTER never answered" >&2; exit 1; }

# `-noreset` on both, and it is not a detail.
#
# An X server resets when its **last client disconnects**: it frees every
# resource and puts the screen configuration back. A test that changes the
# resolution, drops its connection to act out a crash, and reconnects therefore
# straddles "nothing connected" — the reconnect is refused with
# `ECONNRESET` and the resolution has already reverted, so the test measures
# the server rather than the restoration it was written for.
#
# 1280x1024 leaves room to grow inside the 1920x1440 above, so a test can ask
# for a *larger* mode and exercise the grow path — which needs the canvas
# resized before the CRTC, the opposite order from shrinking.
DISPLAY=":$OUTER" Xephyr ":$INNER" -noreset -screen 1280x1024 -resizeable -nolisten tcp >/dev/null 2>&1 &
inner_pid=$!
for _ in $(seq 50); do
  xdpyinfo -display ":$INNER" >/dev/null 2>&1 && break
  sleep 0.1
done
xdpyinfo -display ":$INNER" >/dev/null 2>&1 || {
  echo "  ✗ with-display: Xephyr :$INNER never answered" >&2; exit 1; }

modes=$(DISPLAY=":$INNER" xrandr 2>/dev/null | grep -cE '^ +[0-9]+x[0-9]+ ' || true)
echo "  display: Xephyr :$INNER inside Xvfb :$OUTER, ${modes} mode(s)"

# **Told, not guessed.** A suite that needs modes can skip when there are
# none, and a skip that can always fire is a test that never runs — so the rig
# says how many it arranged and the suite fails rather than skips when it
# cannot find them.
export ESPER_DISPLAY_MODES="$modes"

# **And nothing of the developer's own session comes through.**
#
# Several suites deliberately clear `DISPLAY` to test what a headless host
# does. Clearing `DISPLAY` is not being headless: Wayland is found through
# `WAYLAND_DISPLAY`, and the RemoteDesktop portal is found over the session bus
# — so on a machine with a desktop those suites went on finding a compositor
# and **took a real portal input grant on the developer's own screen**, then
# failed because the host they were testing was not headless after all.
#
# A private runtime directory as well, so anything that looks for a socket by
# convention rather than by variable finds an empty one.
runtime="$(mktemp -d)"

# **And it cannot lock, log out or reboot this machine.**
#
# A display of its own is enough for anything scoped to a display, and is no
# protection at all from the session manager: `loginctl lock-session` does not
# look at `DISPLAY`, it asks logind which session the *calling process* belongs
# to — which is the person's real one, however headless the test believes it
# is. `esper-presence`'s locker ladder reaches for it first, and its power
# actions reach `terminate-session self` and `systemctl reboot`.
#
# It is not theoretical: a suite run on 2026-09-16 locked the screen of the
# machine it was running on, in the middle of somebody working.
here="$(cd "$(dirname "$0")" && pwd)"
guard="$("$here/keep-off-the-real-session.sh")"

trap 'rm -rf "$runtime" "$guard"; cleanup' EXIT INT TERM

env -u WAYLAND_DISPLAY -u DBUS_SESSION_BUS_ADDRESS -u DBUS_STARTER_ADDRESS \
    -u XDG_SESSION_TYPE -u XDG_CURRENT_DESKTOP \
    XDG_RUNTIME_DIR="$runtime" \
    PATH="$guard:$PATH" \
    DISPLAY=":$INNER" "$@"
