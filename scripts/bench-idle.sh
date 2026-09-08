#!/usr/bin/env bash
# What an idle screen costs, on a screen of a given size.
#
# # Why this is its own script
#
# The claim is "an idle 4K screen under 50 kbit/s", and an idle screen is the
# one thing the capture path is *most* optimised for and the least likely to be
# measured by accident: every other measurement in this tree drives a pattern
# that changes on every frame. What this establishes is the floor — what a
# session costs when nobody is doing anything, which is most of most sessions.
#
# # Why a still image and not a blank screen
#
# A solid colour is a screen the encoder can describe in almost no bytes, and a
# number taken from one would say nothing about a real desktop. This paints a
# still that has the things a desktop has — text-like detail, edges, a
# gradient — and then leaves it alone. The *content* is realistic; the
# *stillness* is what is being measured.
#
# Usage: bench-idle.sh [WIDTHxHEIGHT] [seconds]
set -u

root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"

size="${1:-3840x2160}"
seconds="${2:-20}"
width="${size%x*}"
height="${size#*x}"

for tool in Xvfb xsetroot python3; do
  command -v "$tool" >/dev/null 2>&1 || {
    printf '  — skipped the idle measurement: no %s\n' "$tool"
    exit 0
  }
done
if [ ! -x "$root/build/esper-engine" ]; then
  printf '  — skipped the idle measurement: the engine is not built\n'
  exit 0
fi

tmp=$(mktemp -d)
display=""
xv=""
finish() {
  [ -n "$xv" ] && kill "$xv" 2>/dev/null
  rm -rf "$tmp"
}
trap finish EXIT

for d in $(seq 80 120); do
  [ -e "/tmp/.X${d}-lock" ] && continue
  display=$d
  break
done
[ -n "$display" ] || { printf '  ✗ idle: no free display number\n'; exit 1; }

Xvfb ":$display" -screen "0" "${width}x${height}x24" >/dev/null 2>&1 &
xv=$!
sleep 1

# A still with a desktop's worth of detail in it: text-shaped runs of edges,
# tiled across the whole screen. It is drawn *once*, by a client that exits —
# which is the point. Nothing repaints it, so from the capture path's side this
# is a screen nobody is touching.
#
# A tile rather than an image because putting an image on a root window needs a
# program that stays resident to own it, and a resident program is a program
# that might repaint. `xsetroot` draws and leaves.
#
# The texture is deliberately hard: high-frequency edges everywhere are what a
# screen full of text costs an encoder, so a number taken here is a ceiling
# rather than a flattering case.
python3 - "$tmp/tile.xbm" <<'TILE'
import sys

w = h = 256
rows = []
for y in range(h):
    row = []
    for x in range(0, w, 8):
        byte = 0
        for b in range(8):
            # Text-shaped runs whose spacing never repeats inside the tile:
            # a tile that repeats every 64 pixels is the easiest thing an
            # intra-coded picture can be handed.
            run = ((x + b) * 7 + y * 13) % 29
            if run < 11 and (y % 17) < 11:
                byte |= 1 << b
        row.append(byte)
    rows.append(row)
flat = [b for r in rows for b in r]
with open(sys.argv[1], "w", encoding="ascii") as f:
    f.write("#define t_width %d\n#define t_height %d\n" % (w, h))
    f.write("static unsigned char t_bits[] = {\n")
    f.write(",".join("0x%02x" % b for b in flat))
    f.write("\n};\n")
TILE

DISPLAY=":$display" xsetroot -bitmap "$tmp/tile.xbm" -fg '#c8c8d0' -bg '#202028' || {
  printf '  ✗ idle: the still could not be put on the screen\n'
  exit 1
}
sleep 1

# Captured from the X server, which is the path a real host takes — not the
# synthetic backend, which animates and so can never be idle.
out=$(DISPLAY=":$display" "$root/build/esper-engine" loopback \
  --backend x11 --seconds "$seconds" --fps 30 2>&1)

rate=$(printf '%s' "$out" | awk '/^  bitrate/{print $2}')
captured=$(printf '%s' "$out" | awk '/^  frames/{print $2}')
encoded=$(printf '%s' "$out" | awk '/^  frames/{print $4}')
if [ -z "$rate" ] || [ -z "$encoded" ]; then
  printf '  ✗ idle: no bitrate came back\n'
  printf '%s\n' "$out" | tail -12 | sed 's/^/      /'
  exit 1
fi

printf '  · idle %s for %ss: %s kbit/s, %s frame(s) captured and %s encoded\n' \
  "$size" "$seconds" "$rate" "$captured" "$encoded"

fail=0
# **The rate, which is the target.** Worth reading with its duration beside it:
# an idle screen sends one picture and then nothing, so this number is that
# picture amortised over however long the run was, and a longer run always
# reports a smaller one. What it is *not* is a stream anybody is paying for.
if [ "$rate" -ge 50 ] 2>/dev/null; then
  printf '  ✗ idle: an idle %s screen costs %s kbit/s, and the target is 50\n' "$size" "$rate"
  printf '%s\n' "$out" | grep -E "frames|converted|quality" | sed 's/^/      /'
  fail=1
fi

# **And the property the rate is a consequence of**, which is the one that
# cannot be flattered by a duration or by an easy still: a screen nobody is
# touching produces *pictures nobody has to send*. At 30 fps a twenty-second
# run could have encoded six hundred; anything past a handful means the damage
# path is reporting changes on a screen that is not changing.
# **And that the capture path waited rather than spun.** A backend that
# grabbed the screen at thirty frames a second and let the gate throw them away
# would report the same bitrate while spending a 4K `XShmGetImage` six hundred
# times on a screen nobody touched — the cost moves from the link to the host's
# CPU, where the person whose machine it is pays it.
if [ "$captured" -gt 60 ] 2>/dev/null; then
  printf '  ✗ idle: %s frames were captured from a screen nobody touched — the capture path is spinning\n' "$captured"
  printf '%s\n' "$out" | grep -E "per stage|capture " | sed 's/^/      /'
  fail=1
fi

if [ "$encoded" -gt 5 ] 2>/dev/null; then
  printf '  ✗ idle: %s frames were encoded from a screen nobody touched\n' "$encoded"
  printf '%s\n' "$out" | grep -E "frames|per stage|capture" | sed 's/^/      /'
  fail=1
fi

# ── The control, because two bars that cannot fail are not bars ──────────
#
# Everything above rests on the capture path waiting for damage, and on an
# idle screen it waits for all of it — so no sabotage of the gate *or* of the
# capturer's damage handling moves these numbers: there is nothing to let
# through. Numbers that cannot be made to move are numbers nobody should
# trust, so the same run is made against a screen that never stops moving. If
# that one is also quiet, the instrument is broken and the idle result means
# nothing.
control=$("$root/build/esper-engine" loopback \
  --backend synthetic --size "$width"x"$height" --seconds 4 --fps 30 2>&1)
control_encoded=$(printf '%s' "$control" | awk '/^  frames/{print $4}')
control_rate=$(printf '%s' "$control" | awk '/^  bitrate/{print $2}')
# Compared as a ratio rather than against a number: how many frames a 4K
# pattern can be *rendered* in four seconds is a fact about this machine — it
# managed fifteen — and a bar written as an absolute would be a bar that fails
# on a slower one for a reason that has nothing to do with the claim. What has
# to hold is that the two cases are not the same measurement.
if [ -z "$control_encoded" ] || [ "$control_encoded" -lt 5 ] 2>/dev/null; then
  printf '  ✗ idle: a screen that never stops moving encoded %s frame(s), so this measures nothing\n' \
    "${control_encoded:-no}"
  printf '%s\n' "$control" | grep -E "frames|bitrate|capture" | sed 's/^/      /'
  fail=1
elif [ "$control_encoded" -lt $((encoded * 5)) ] 2>/dev/null; then
  printf '  ✗ idle: moving encoded %s and idle encoded %s — too close to tell apart\n' \
    "$control_encoded" "$encoded"
  fail=1
elif [ "$control_rate" -lt $((rate * 5)) ] 2>/dev/null || [ "$control_rate" -lt 50 ] 2>/dev/null; then
  printf '  ✗ idle: moving cost %s kbit/s against idle %s — too close to tell apart\n' \
    "$control_rate" "$rate"
  fail=1
else
  printf '  · moving %s for 4s: %s kbit/s, %s encoded — what the same bars say about a busy screen\n' \
    "$size" "$control_rate" "$control_encoded"
fi

if [ "$fail" -ne 0 ]; then
  exit 1
fi
printf '  ✓ idle: %s at %s kbit/s over %ss, %s picture(s) encoded — against %s kbit/s and %s for the same screen in motion\n' \
  "$size" "$rate" "$seconds" "$encoded" "$control_rate" "$control_encoded"
