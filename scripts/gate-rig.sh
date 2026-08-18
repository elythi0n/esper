#!/usr/bin/env bash
# A machine the gates can measure: the same devices every time, on every machine.
#
# # Why this exists
#
# Every browser gate drives the real dev server, which reads the real control
# plane, which reads whoever's machine it is running on. That made the Devices
# screen — the product's main screen — measurable only in whatever state the
# developer's own store happened to be in, and on a fresh machine that state is
# **empty**: no device row, no permission, no fingerprint, no pairing word has
# ever been rendered under a11y, ink, words or overflow.
#
# So the rig makes an `ESPER_HOME` of its own from a fixture, starts the engine's
# API against it and the dev server beside it, and prints where to point a gate.
# The store is `testdata/fixtures/device_store_screens.json`, which holds the
# cases a screen has to survive rather than the cases a protocol has to: a name
# nobody would type, a name that is not Latin, every permission at once, a
# pairing already spent, and a device paired and never seen.
#
# Usage:  scripts/gate-rig.sh            # start, print the URL, wait
#         scripts/gate-rig.sh <command>  # start, run the command, stop
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
home="$(mktemp -d /tmp/esper-gate-home.XXXXXX)"
port="${ESPER_GATE_PORT:-5179}"
api_port="${ESPER_GATE_API_PORT:-8688}"

cleanup() {
  [ -n "${vite_pid:-}" ] && kill "$vite_pid" 2>/dev/null || true
  [ -n "${api_pid:-}" ] && kill "$api_pid" 2>/dev/null || true
  rm -rf "$home"
}
trap cleanup EXIT

# A port already answering is a server already serving, and its machine is not
# this one: a gate pointed at it measures somebody else's devices, or none. It
# is the same failure as running the gates with nothing answering at all, and
# it is silent in exactly the same way.
for taken in "$port" "$api_port"; do
  if curl -fsS -o /dev/null --max-time 1 "http://127.0.1.1:$taken/" 2>/dev/null ||
     curl -fsS -o /dev/null --max-time 1 "http://127.0.0.1:$taken/" 2>/dev/null; then
    echo "something is already answering on port $taken — stop it, or set ESPER_GATE_PORT" >&2
    exit 1
  fi
done

cp "$root/testdata/fixtures/device_store_screens.json" "$home/devices.json"
# A key of its own, so the fingerprint on screen is the rig's and not anybody's.
head -c 32 /dev/urandom > "$home/identity.key"
chmod 600 "$home/identity.key"

ESPER_HOME="$home" "$root/build/esper" api --listen "127.0.0.1:$api_port" > "$home/api.log" 2>&1 &
api_pid=$!

# The token is written when the API starts and read by the dev server when *it*
# starts, so the order matters: a dev server started first proxies no token and
# every screen says the control plane refused.
for _ in $(seq 1 50); do [ -s "$home/api.token" ] && break; sleep 0.1; done
if [ ! -s "$home/api.token" ]; then
  echo "the API wrote no token:" >&2; cat "$home/api.log" >&2; exit 1
fi

(cd "$root/web" && ESPER_HOME="$home" ESPER_API="http://127.0.0.1:$api_port" \
  npx vite --port "$port" --host 127.0.1.1 > "$home/vite.log" 2>&1) &
vite_pid=$!

for _ in $(seq 1 100); do
  if curl -fs -o /dev/null "http://127.0.1.1:$port/" 2>/dev/null; then break; fi
  sleep 0.2
done
answered=$(curl -s -o /dev/null -w "%{http_code}" "http://127.0.1.1:$port/api/devices" || true)
if [ "$answered" != "200" ]; then
  echo "the rig is up but the control plane answered $answered, so the screens would be empty" >&2
  cat "$home/vite.log" >&2
  exit 1
fi
count=$(curl -s "http://127.0.1.1:$port/api/devices" | grep -o '"public_key"' | wc -l)
echo "  rig: $count device(s) at http://127.0.1.1:$port/ (home $home)"

if [ "$#" -gt 0 ]; then
  "$@"
else
  wait "$vite_pid"
fi
