#!/usr/bin/env bash
# The half of the portal conversation a container can never reach.
#
# `check-wayland` runs this client against a real `xdg-desktop-portal-gnome`,
# and what that can establish is **where** the conversation stops: a portal
# that refuses `CreateSession` is a different world from one that gets as far
# as `Start` and waits for somebody. It stops at the dialog, because a
# container has nobody in it to answer one.
#
# Everything past that dialog is what unattended access rests on — the grant,
# the token the portal hands back, and that token being sent again next time —
# and none of it had ever been exercised. A machine that asks again every
# session can be granted unattended access and still refuse every session,
# which is a promise broken at three in the morning.
#
# So this runs the client against a portal that answers: `scripts/stub-portal.py`,
# which is not a compositor and injects nothing. It records what it was asked,
# and the asking is what is checked.
set -u

root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"

fail=0
tmp=$(mktemp -d)
bus_pid=""
stub_pid=""
finish() {
  [ -n "$stub_pid" ] && kill "$stub_pid" 2>/dev/null
  [ -n "$bus_pid" ] && kill "$bus_pid" 2>/dev/null
  rm -rf "$tmp"
}
trap finish EXIT

for tool in dbus-daemon python3; do
  command -v "$tool" >/dev/null 2>&1 || {
    printf '  — skipped the portal checks: no %s\n' "$tool"
    exit 0
  }
done
python3 -c "import dbus, dbus.service, dbus.mainloop.glib, gi" 2>/dev/null || {
  printf '  — skipped the portal checks: python needs dbus and gi to stand up a portal\n'
  exit 0
}

# **Built every run, and the failure is fatal.**
#
# Not "built if it is missing". A probe that is already there is a probe from
# whenever it was last built, so a change that does not compile leaves the
# previous one in place and this check passes on code nobody is running — which
# is how `check-dbus` once reported a sabotage as green. The same trap, written
# down twice already in this tree.
probe="$root/core/target/debug/examples/try-portal"
if ! (cd core && cargo build -q -p esper-input-portal --example try-portal); then
  printf '  ✗ portal: the probe would not build, so nothing here was checked\n'
  exit 1
fi
if [ ! -x "$probe" ]; then
  printf '  ✗ portal: the probe built and is not at %s\n' "$probe"
  exit 1
fi

# **A bus of its own.** The session bus on a developer's machine has a real
# portal on it, and a check that talked to that would be asking somebody's
# desktop for input.
eval "$(dbus-daemon --session --print-address=1 --print-pid=1 --fork \
  | { read -r a; read -r p; printf 'export DBUS_SESSION_BUS_ADDRESS=%s; bus_pid=%s' "$a" "$p"; })"
export DBUS_SESSION_BUS_ADDRESS

python3 "$root/scripts/stub-portal.py" "$tmp/asked.json" > "$tmp/stub.log" 2>&1 &
stub_pid=$!
for _ in $(seq 1 50); do
  grep -q "ready" "$tmp/stub.log" 2>/dev/null && break
  sleep 0.1
done
if ! grep -q "ready" "$tmp/stub.log" 2>/dev/null; then
  printf '  ✗ portal: the stub portal never took the name\n'
  sed 's/^/      /' "$tmp/stub.log"
  exit 1
fi

# ── 1. A grant asked for, and kept ───────────────────────────────────────
first=$("$probe" 5 2>&1)
if ! printf '%s' "$first" | grep -q '^granted'; then
  printf '  ✗ portal: the client did not reach a granted session against a portal that answers\n'
  printf '%s\n' "$first" | sed 's/^/      /'
  fail=1
fi

asked_persist=$(python3 - "$tmp/asked.json" <<'PY'
import json, sys
asked = json.load(open(sys.argv[1]))
picks = [a["options"].get("persist_mode") for a in asked if a["member"] == "SelectDevices"]
print(str(picks[-1]) if picks else "nothing")
PY
)
# 2 is "until revoked". 1 lasts as long as the application runs, which for an
# engine a service manager restarts at every boot is no persistence at all.
if [ "$asked_persist" = "2" ]; then
  printf '  ✓ portal: the client asks for a grant that outlives it\n'
else
  printf '  ✗ portal: SelectDevices asked for persist_mode %s, and until-revoked is 2\n' "$asked_persist"
  fail=1
fi

token=$(printf '%s' "$first" | sed -n 's/^restore-token=//p')
if [ -n "$token" ] && [ "$token" != "none" ]; then
  printf '  ✓ and it reads back the token the portal handed it\n'
else
  printf '  ✗ portal: the portal handed back a token and the client reported %s\n' "${token:-nothing}"
  fail=1
fi

# ── 2. And sends it again, which is the whole point ───────────────────────
rm -f "$tmp/asked.json"
second=$("$probe" 5 "$token" 2>&1)
sent=$(python3 - "$tmp/asked.json" <<'PY'
import json, sys
asked = json.load(open(sys.argv[1]))
# The latest: reading the first would read a session that had nothing to send.
picks = [a["options"].get("restore_token") for a in asked if a["member"] == "SelectDevices"]
print(str(picks[-1]) if picks else "nothing")
PY
)
if [ "$sent" = "$token" ]; then
  printf '  ✓ and a second session offers that grant back rather than asking again\n'
else
  printf '  ✗ portal: the second session sent %s, and the grant was %s\n' "$sent" "$token"
  fail=1
fi
if ! printf '%s' "$second" | grep -q '^granted'; then
  printf '  ✗ portal: reopening a remembered grant did not reach a session\n'
  printf '%s\n' "$second" | sed 's/^/      /'
  fail=1
fi

# ── 3. A portal that will not keep one says so ────────────────────────────
#
# The answer that decides what this machine may advertise. A client that
# reported a token it had not been given would let a machine promise unattended
# access it cannot honour — so the stub is told to withhold it, and the client
# has to say `none` rather than invent one.
kill "$stub_pid" 2>/dev/null
wait "$stub_pid" 2>/dev/null
rm -f "$tmp/asked.json"
ESPER_STUB_NO_PERSIST=1 python3 "$root/scripts/stub-portal.py" "$tmp/asked.json" > "$tmp/stub2.log" 2>&1 &
stub_pid=$!
for _ in $(seq 1 50); do
  grep -q "ready" "$tmp/stub2.log" 2>/dev/null && break
  sleep 0.1
done
third=$("$probe" 5 2>&1)
if printf '%s' "$third" | grep -q '^restore-token=none'; then
  printf '  ✓ and a portal that will not keep a grant is reported as keeping none\n'
else
  printf '  ✗ portal: a portal that kept nothing was reported as %s\n' \
    "$(printf '%s' "$third" | sed -n 's/^restore-token=//p')"
  fail=1
fi

if [ "$fail" -ne 0 ]; then
  exit 1
fi
printf '  ✓ portal: a grant asked for, kept, offered back, and its absence reported\n'
