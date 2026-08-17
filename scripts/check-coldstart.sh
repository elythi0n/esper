#!/usr/bin/env bash
# How long from `exec` to a screen somebody can use.
#
# The number the exit criterion names, measured rather than estimated, and
# measured the only way it can honestly be: the page says when it painted and
# the window turns that into a duration from process start.
#
# **Two arrangements, because they are different promises.** A window that
# finds a control plane already listening only has to draw; a window that has
# to start one — which is what opening the application from a desktop entry
# does — pays for the engine's handshake as well. Both are reported, and the
# second is the one a person meets.
#
# Runs under Xvfb, so a headless machine can hold the promise too. Xvfb is
# software rendering: a real GPU is faster, never slower, so a pass here is a
# pass on real glass.
set -uo pipefail

cd "$(dirname "$0")/.."

RUNS="${RUNS:-5}"
BUDGET_MS="${BUDGET_MS:-1000}"
DISPLAY_NUM="${DISPLAY_NUM:-:93}"
APP="./build/esper-app"
PLANE="./build/esper"

if [ ! -x "$APP" ]; then
  echo "  ✗ $APP is not built. \`make build-app\` first."
  exit 1
fi

# Its own home *and its own port*, so a developer's window state, paired
# devices, or an `esper api` they happen to be running cannot change the
# number — or, worse, leave the window on its first-run screen and make this
# look like a performance failure.
export ESPER_API_ADDR="${ESPER_API_ADDR:-127.0.0.1:8699}"
HOME_DIR="$(mktemp -d)"
trap 'rm -rf "$HOME_DIR"; kill %1 %2 >/dev/null 2>&1' EXIT
export ESPER_HOME="$HOME_DIR"

rm -f "/tmp/.X${DISPLAY_NUM#:}-lock"
Xvfb "$DISPLAY_NUM" -screen 0 1280x800x24 >/dev/null 2>&1 &
sleep 1

# median of a list on stdin
median() { sort -n | awk '{a[NR]=$1} END {print (NR%2) ? a[(NR+1)/2] : int((a[NR/2]+a[NR/2+1])/2)}'; }

measure() {
  local label="$1" want_plane="$2" times=()
  for _ in $(seq "$RUNS"); do
    if [ "$want_plane" = "yes" ]; then
      setsid "$PLANE" api --listen "$ESPER_API_ADDR" >/dev/null 2>&1 < /dev/null &
      local plane_pid=$!
      sleep 2
    fi

    local log; log="$(mktemp)"
    DISPLAY="$DISPLAY_NUM" "$APP" > "$log" 2>&1 &
    local app_pid=$!
    # Wait for the line, not a fixed sleep: a fixed sleep either flatters the
    # result or wastes the run.
    local ms="" waited=0
    while [ "$waited" -lt 200 ]; do
      ms="$(grep -oP 'window ready in \K[0-9]+' "$log" | head -1)"
      [ -n "$ms" ] && break
      sleep 0.1
      waited=$((waited + 1))
    done
    kill "$app_pid" >/dev/null 2>&1
    wait "$app_pid" 2>/dev/null
    if [ "$want_plane" = "yes" ]; then
      kill "$plane_pid" >/dev/null 2>&1
      pkill -x esper-engine >/dev/null 2>&1
      sleep 0.5
    else
      pkill -x esper-engine >/dev/null 2>&1
    fi
    rm -f "$log"

    if [ -z "$ms" ]; then
      echo "  ✗ $label: the window never reported a first paint"
      return 1
    fi
    times+=("$ms")
  done

  local mid; mid="$(printf '%s\n' "${times[@]}" | median)"
  local worst; worst="$(printf '%s\n' "${times[@]}" | sort -n | tail -1)"
  printf '  %-26s median %4dms   worst %4dms   (%s)\n' "$label" "$mid" "$worst" "${times[*]}"
  echo "$mid" > "$HOME_DIR/last-median"
  [ "$worst" -le "$BUDGET_MS" ]
}

echo "  cold start, $RUNS runs each, budget ${BUDGET_MS}ms:"
fail=0
measure "a plane already running" yes || fail=1
measure "starting its own plane" no || fail=1

if [ "$fail" -ne 0 ]; then
  echo
  echo "  ✗ cold start is over budget. The window has to be usable in ${BUDGET_MS}ms."
  exit 1
fi
echo "  ✓ cold start: every run under ${BUDGET_MS}ms, both arrangements"
