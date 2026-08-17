#!/usr/bin/env bash
# What the desktop asks for, and whether the shipped window hears it.
#
# Two settings, both of which the window could ignore silently and did:
# reduced motion, and high contrast. Neither is a preference about looks —
# somebody who set them asked their operating system for them everywhere.
#
# # Why this is not the /design toggle
#
# The gate has a Full / Reduced control and the frame check drives it, which
# proves the *override* works. It does not prove that somebody who turned
# animations off in their desktop gets a window that honours it — a different
# path entirely: GTK's `gtk-enable-animations` → WebKitGTK →
# `prefers-reduced-motion` → the unlayered block in motion.css → every
# duration token → the animations themselves.
#
# So this runs the built window twice, with animations on and off in a GTK
# settings file of its own, and reads back what the page says the media query
# and `--dur-base` resolved to. The window reports both on stderr when it
# first paints.
set -uo pipefail

cd "$(dirname "$0")/.."

APP="./build/esper-app"
DISPLAY_NUM="${DISPLAY_NUM:-:91}"

if [ ! -x "$APP" ]; then
  echo "  ✗ $APP is not built. \`make build-app\` first."
  exit 1
fi

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"; kill %1 >/dev/null 2>&1; pkill -x esper-engine >/dev/null 2>&1' EXIT
export ESPER_HOME="$WORK/home"
mkdir -p "$ESPER_HOME"
# Its own port, so an `esper api` somebody is running cannot leave the window
# on its first-run screen — which paints nothing and reports nothing.
export ESPER_API_ADDR="${ESPER_API_ADDR:-127.0.0.1:8698}"

rm -f "/tmp/.X${DISPLAY_NUM#:}-lock"
Xvfb "$DISPLAY_NUM" -screen 0 1024x700x24 >/dev/null 2>&1 &
sleep 1

# What the window said when it painted, with animations set one way.
ask() {
  local animations="$1"
  local config="$WORK/config-$animations"
  mkdir -p "$config/gtk-3.0"
  cat > "$config/gtk-3.0/settings.ini" <<INI
[Settings]
gtk-enable-animations=$animations
INI

  local log; log="$(mktemp)"
  # **A fresh webview profile.** WebKitGTK keeps localStorage under the data
  # and cache directories, shared across runs and unaffected by ESPER_HOME —
  # so a check that did not isolate them would be measuring what a developer's
  # window happened to store last time. It measured exactly that once.
  XDG_CONFIG_HOME="$config" XDG_DATA_HOME="$config/data" XDG_CACHE_HOME="$config/cache" \
    DISPLAY="$DISPLAY_NUM" "$APP" > "$log" 2>&1 &
  local pid=$!
  local line="" waited=0
  while [ "$waited" -lt 250 ]; do
    line="$(grep -m1 'window ready in' "$log")"
    [ -n "$line" ] && break
    sleep 0.1
    waited=$((waited + 1))
  done
  kill "$pid" >/dev/null 2>&1
  wait "$pid" 2>/dev/null
  pkill -x esper-engine >/dev/null 2>&1
  sleep 0.4
  rm -f "$log"
  printf '%s' "$line"
}

# The theme out of a reported line.
#
# Reported by the page at first paint alongside the timing, because "the
# window opens in the right theme" is a claim about the *first frame* and
# nothing outside the webview can see one.
theme_of() {
  printf '%s' "$1" | grep -oP 'theme \K[a-z]+'
}

# The base duration out of a reported line, in milliseconds.
#
# Parsed rather than matched: CSS resolves `--dur-base` to `.2s` or `200ms`
# depending on how it was written, and a check that grepped for one string
# would pass or fail on the *spelling* of a duration rather than its value.
base_ms() {
  local raw
  raw="$(printf '%s' "$1" | grep -oP '\-\-dur-base \K[^,)]+')"
  case "$raw" in
    *ms) printf '%s' "${raw%ms}" ;;
    *s)  awk -v v="${raw%s}" 'BEGIN { printf "%d", v * 1000 }' ;;
    *)   printf 'unknown' ;;
  esac
}

fail=0

full="$(ask 1)"
if [ -z "$full" ]; then
  echo "  ✗ reduced motion: the window never painted with animations enabled"
  exit 1
fi
echo "  animations on:   $full"
case "$full" in
  *"motion full"*) ;;
  *) echo "  ✗ with gtk-enable-animations=1 the window should report motion full"; fail=1 ;;
esac
if [ "$(base_ms "$full")" = "0" ]; then
  echo "  ✗ every duration is zero with animations enabled, so nothing animates at all"
  fail=1
fi

off="$(ask 0)"
if [ -z "$off" ]; then
  echo "  ✗ reduced motion: the window never painted with animations disabled"
  exit 1
fi
echo "  animations off:  $off"
case "$off" in
  *"motion reduced"*) ;;
  *) echo "  ✗ with gtk-enable-animations=0 the window should report motion reduced"; fail=1 ;;
esac
# **The media query is not the promise.** Honouring it means the durations
# actually collapse, which is what the unlayered reduced-motion block does —
# and that is the link a stylesheet reorganisation would break silently.
off_ms="$(base_ms "$off")"
if [ "$off_ms" != "0" ]; then
  echo "  ✗ the media query matched but --dur-base resolved to ${off_ms}ms rather than 0"
  fail=1
fi

# ── And the desktop's contrast setting, the same way ──────────────────────
#
# `prefers-contrast: more` is not a preference about looks: somebody who set
# it asked their desktop for higher contrast everywhere, and until this the
# window could not hear it at all — a whole accessibility setting with no
# effect. GTK expresses it as `gtk-theme-name` ending in `-hc`, which
# WebKitGTK maps to the media query.
#
# The theme has to be right in the **first frame**, so the page reports it
# with its first paint rather than being inspected afterwards: a window that
# mounts dark and corrects itself is a flash somebody sees every launch.
contrast_theme() {
  local wants="$1"
  local config="$WORK/contrast-$wants"
  mkdir -p "$config/gtk-3.0"
  cat > "$config/gtk-3.0/settings.ini" <<INI
[Settings]
gtk-enable-animations=1
gtk-theme-name=$wants
INI

  # **A theme that exists.** WebKitGTK maps the *name* `HighContrast` to
  # `prefers-contrast: more`, and a made-up name like `Adwaita-hc` reports
  # nothing at all — which the first version of this check spent a while
  # calling a product bug. `GTK_THEME` as well as the settings file, because
  # under Xvfb there is no settings daemon to read the file.
  local log; log="$(mktemp)"
  XDG_CONFIG_HOME="$config" XDG_DATA_HOME="$config/data" XDG_CACHE_HOME="$config/cache" \
    GTK_THEME="$wants" DISPLAY="$DISPLAY_NUM" "$APP" > "$log" 2>&1 &
  local pid=$!
  local line="" waited=0
  while [ "$waited" -lt 250 ]; do
    line="$(grep -m1 'window ready in' "$log")"
    [ -n "$line" ] && break
    sleep 0.1
    waited=$((waited + 1))
  done
  kill "$pid" >/dev/null 2>&1
  wait "$pid" 2>/dev/null
  pkill -x esper-engine >/dev/null 2>&1
  sleep 0.4
  rm -f "$log"
  printf '%s' "$line"
}

ordinary="$(contrast_theme Adwaita)"
echo "  ordinary theme:  $ordinary"
if [ "$(theme_of "$ordinary")" != "dark" ]; then
  echo "  ✗ with no contrast preference the window should open in its own theme (dark)"
  fail=1
fi

louder="$(contrast_theme HighContrast)"
echo "  high contrast:   $louder"
if [ "$(theme_of "$louder")" != "hc" ]; then
  echo "  ✗ the desktop asked for more contrast and the window opened in $(theme_of "$louder")"
  fail=1
fi

if [ "$fail" -ne 0 ]; then
  echo
  echo "  ✗ the desktop's settings are not honoured in the shipped window"
  exit 1
fi
echo "  ✓ the desktop reaches the built window: motion collapses, and more contrast opens high contrast"
