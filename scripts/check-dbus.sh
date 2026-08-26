#!/usr/bin/env bash
# This project's D-Bus, checked against the reference implementation's tool.
#
# # Why an oracle and not just tests
#
# `esper-dbus` speaks the wire format itself rather than take `zbus` — 30
# crates, ~188,000 lines and 845 `unsafe` occurrences, to call six methods on
# one interface. That is the same judgement as speaking mDNS
# ourselves, and it carries the same obligation: a format you write yourself
# has to be checked against something that is not you.
#
# Round-trip tests cover the symmetric half — marshal, read back, compare — and
# cannot catch a writer and a reader that are wrong the *same* way. Padding is
# exactly that kind of mistake: a byte too few on both sides agrees with itself
# perfectly and with nobody else.
#
# So this asks a **real session bus** the same questions twice, once through
# this crate and once through `dbus-send`, which ships with the reference
# implementation. The mDNS work was checked against `dnspython` for the same
# reason and found a real bug that way.
#
# # What it asks, and why each one
#
#   - **`ListNames`** — an array of strings, which exercises the array length,
#     the padding before the first element, and string termination, in the one
#     reply whose content something else can enumerate independently.
#   - **`GetId`** — a single string, and a value neither side can guess.
#   - **The portal versions** — read through `org.freedesktop.DBus.Properties`,
#     which returns a **variant**. That is the one type whose parsing depends
#     on a signature the *sender* chose rather than on what the caller asked
#     for, so it is the one most likely to be read by luck.
#
# Usage: scripts/check-dbus.sh
set -uo pipefail

here=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
rig=esper-wayland-gnome
probe="$here/core/target/debug/examples/ask-the-bus"

fail=0
checked=0
note_fail() { printf '  ✗ dbus: %s\n' "$1"; fail=1; }

command -v docker >/dev/null 2>&1 || {
  echo "  ✗ dbus: no docker — this needs a session bus, and a container is where one is" >&2
  exit 1; }

# Always built, never merely found. A sabotage of the marshaller that fails to
# compile would otherwise leave the previous binary in place, and this check
# would compare the *old* code against dbus-send and pass — which it did, once,
# and that is why this is unconditional.
echo "  … dbus: building the probe"
(cd "$here/core" && cargo build -q -p esper-dbus --example ask-the-bus) \
  || { echo "  ✗ dbus: the probe would not build" >&2; exit 1; }
[ -x "$probe" ] || { echo "  ✗ dbus: no probe at $probe after building it" >&2; exit 1; }

if ! docker image inspect "$rig" >/dev/null 2>&1; then
  echo "  … dbus: building the session rig"
  docker build -q -f "$here/scripts/rigs/wayland-gnome.Containerfile" -t "$rig" "$here" >/dev/null 2>&1 \
    || { echo "  ✗ dbus: could not build the rig" >&2; exit 1; }
fi

# One session, both tools inside it. Asking two different sessions would
# compare two different sets of names and call the difference a bug.
answers=$("$here/scripts/in-container.sh" --built "$rig" \
  --ro "$here/core/target/debug/examples:/probe" -- sh -c '
# Unique names (":1.7") are per connection, and the two tools are different
# connections, so only well-known names can be compared.
/probe/ask-the-bus names | grep -v "^:" | sort > /tmp/ours
dbus-send --session --dest=org.freedesktop.DBus --type=method_call --print-reply \
  / org.freedesktop.DBus.ListNames 2>/dev/null \
  | grep -oE "\"[^\"]+\"" | tr -d \" | grep -v "^:" | sort > /tmp/theirs

echo "ours_count=$(wc -l < /tmp/ours)"
echo "theirs_count=$(wc -l < /tmp/theirs)"
if [ "$(cat /tmp/ours)" = "$(cat /tmp/theirs)" ]; then echo "names=agree"; else echo "names=differ"; fi

echo "ours_id=$(/probe/ask-the-bus id)"
echo "theirs_id=$(dbus-send --session --dest=org.freedesktop.DBus --type=method_call --print-reply \
  / org.freedesktop.DBus.GetId 2>/dev/null | grep -oE "\"[^\"]+\"" | tr -d \")"

for i in ScreenCast RemoteDesktop; do
  echo "ours_$i=$(/probe/ask-the-bus portal $i)"
  echo "theirs_$i=$(dbus-send --session --dest=org.freedesktop.portal.Desktop --type=method_call --print-reply \
    /org/freedesktop/portal/desktop org.freedesktop.DBus.Properties.Get \
    "string:org.freedesktop.portal.$i" string:version 2>/dev/null \
    | grep -oE "uint32 [0-9]+" | awk "{print \$2}")"
done
' 2>/dev/null)

value() { printf '%s' "$answers" | awk -F= -v k="$1" '$1 == k { print $2; exit }'; }

# A floor first: if this crate could not reach the bus at all, everything below
# compares nothing against nothing and would pass.
checked=$((checked + 1))
ours_count=$(value ours_count)
if [ "${ours_count:-0}" -lt 5 ]; then
  echo "  ✗ dbus: this crate listed ${ours_count:-no} names on the bus, so nothing below was compared" >&2
  printf '%s\n' "$answers" | sed 's/^/      /' >&2
  exit 1
fi

checked=$((checked + 1))
[ "$(value names)" = "agree" ] || note_fail "the name lists differ — $(value ours_count) from this crate, $(value theirs_count) from dbus-send.
    An array of strings is the length, the padding before the first element and
    the terminator, all at once."

checked=$((checked + 1))
ours_id=$(value ours_id)
theirs_id=$(value theirs_id)
if [ -z "$ours_id" ] || [ "$ours_id" != "$theirs_id" ]; then
  note_fail "the bus id read as ${ours_id:-nothing} here and ${theirs_id:-nothing} through dbus-send"
fi

for interface in ScreenCast RemoteDesktop; do
  checked=$((checked + 1))
  ours=$(value "ours_$interface")
  theirs=$(value "theirs_$interface")
  if [ "$ours" != "$theirs" ]; then
    note_fail "$interface reads as version ${ours:-nothing} here and ${theirs:-nothing} through dbus-send.
    This one comes back inside a variant, whose signature the sender chooses."
  fi
done

if [ "$checked" -lt 5 ]; then
  echo "  ✗ dbus: only $checked comparisons ran, which is not all of them" >&2
  exit 1
fi

if [ "$fail" -ne 0 ]; then
  echo ""
  echo "  A wire format you write yourself agrees with itself by construction."
  echo "  The question is whether it agrees with the bus."
  exit 1
fi
echo "  ✓ dbus: $checked comparisons against dbus-send on a real session bus — the same $(value ours_count) names, the same bus id, and the same portal versions read out of a variant"
