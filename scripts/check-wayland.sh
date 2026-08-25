#!/usr/bin/env bash
# What a Wayland session actually offers, asked of one.
#
# # Why this exists
#
# Every Wayland item in the tracker was deferred with the same sentence: the
# development machine is X11 under XFCE, with no ScreenCast portal and no
# RemoteDesktop portal installed. That is true, and it meant the Wayland plan
# rested on what the protocol documentation says rather than on what a session
# does — and those differ in ways that decide the design.
#
# `scripts/rigs/wayland.Containerfile` is a real sway session with a real
# portal, in a container, so that nothing has to be installed on somebody's
# working desktop to find out. This asks it the questions the design turns on
# and fails if any answer changes.
#
# # What it pins, and why each one matters
#
#   - **The compositor offers `zwlr_screencopy_manager_v1`.** On wlroots this is
#     capture without a portal, without PipeWire and without D-Bus — the same
#     shape as `x11rb` speaking X11 over a socket, and the reason a wlroots host
#     may never need the portal path at all.
#   - **It also offers `ext_image_copy_capture_manager_v1`**, the protocol meant
#     to replace it. Both being present is what makes choosing between them a
#     decision rather than a constraint.
#   - **It offers `zwp_virtual_keyboard_manager_v1` and
#     `zwlr_virtual_pointer_manager_v1`.** Input injection on wlroots needs no
#     portal either. This is the single most useful thing the rig found: the
#     fallback order in Phase 5 was written assuming portal-or-uinput.
#   - **The ScreenCast portal is present.**
#   - **The RemoteDesktop portal is absent.** This is a *negative* result and it
#     is asserted rather than merely observed, because it is a fact about
#     wlroots and not about this image: `xdg-desktop-portal-wlr` implements
#     ScreenCast only. If a future version gains RemoteDesktop, this check fails
#     and somebody revisits the fallback order — which is the point. A fact that
#     nothing watches is a fact that quietly stops being true.
#
# Usage: scripts/check-wayland.sh
set -uo pipefail

here=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
rig=esper-wayland

fail=0
checked=0
note_fail() { printf '  ✗ wayland: %s\n' "$1"; fail=1; }

command -v docker >/dev/null 2>&1 || {
  echo "  ✗ wayland: no docker — this check needs a compositor to ask, and this machine is X11" >&2
  exit 1; }

if ! docker image inspect "$rig" >/dev/null 2>&1; then
  echo "  … wayland: building the session rig"
  docker build -q -f "$here/scripts/rigs/wayland.Containerfile" -t "$rig" "$here" >/dev/null 2>&1 \
    || { echo "  ✗ wayland: could not build the rig" >&2; exit 1; }
fi

# One session, every question asked inside it. Starting a compositor per
# question would be slower and would also stop the answers being about the
# same session, which is the only thing that makes them comparable.
#
# Through the harness, so this gets the same treatment as every other
# container here: pinned image, no network, no capabilities. A portal that
# could reach the internet is not a portal this project has any use for.
answers=$("$here/scripts/in-container.sh" --built "$rig" -- sh -c '
present() {
  wayland-info 2>/dev/null | grep -q "$1" && echo "yes" || echo "no"
}
echo "output=$(swaymsg -t get_outputs 2>/dev/null | grep -c HEADLESS)"
echo "screencopy=$(present zwlr_screencopy_manager_v1)"
echo "ext_image_copy=$(present ext_image_copy_capture_manager_v1)"
echo "virtual_keyboard=$(present zwp_virtual_keyboard_manager_v1)"
echo "virtual_pointer=$(present zwlr_virtual_pointer_manager_v1)"
for iface in ScreenCast RemoteDesktop; do
  v=$(dbus-send --session --dest=org.freedesktop.portal.Desktop --type=method_call --print-reply \
    /org/freedesktop/portal/desktop org.freedesktop.DBus.Properties.Get \
    "string:org.freedesktop.portal.$iface" string:version 2>/dev/null | grep -oE "uint32 [0-9]+" | awk "{print \$2}")
  echo "portal_$iface=${v:-absent}"
done
sources=$(dbus-send --session --dest=org.freedesktop.portal.Desktop --type=method_call --print-reply \
  /org/freedesktop/portal/desktop org.freedesktop.DBus.Properties.Get \
  string:org.freedesktop.portal.ScreenCast string:AvailableSourceTypes 2>/dev/null \
  | grep -oE "uint32 [0-9]+" | awk "{print \$2}")
echo "source_types=${sources:-0}"
' 2>/dev/null)

value() { printf '%s' "$answers" | awk -F= -v k="$1" '$1 == k { print $2; exit }'; }

# A floor before anything is judged: if the session did not come up, every
# answer below is "no" and the check would read as a pile of findings about
# Wayland rather than one finding about the rig.
checked=$((checked + 1))
if [ "$(value output)" != "1" ]; then
  echo "  ✗ wayland: the rig's compositor has no output, so nothing below was measured" >&2
  printf '%s\n' "$answers" | sed 's/^/      /' >&2
  exit 1
fi

# ── Capture, without a portal ───────────────────────────────────────────────
for protocol in screencopy ext_image_copy; do
  checked=$((checked + 1))
  [ "$(value "$protocol")" = "yes" ] || note_fail "the compositor no longer offers $protocol.
    Capture on wlroots goes through this rather than through the portal; without
    it a wlroots host needs D-Bus and PipeWire to photograph its own screen."
done

# ── Input, without a portal ─────────────────────────────────────────────────
for protocol in virtual_keyboard virtual_pointer; do
  checked=$((checked + 1))
  [ "$(value "$protocol")" = "yes" ] || note_fail "the compositor no longer offers $protocol.
    This is what makes input on wlroots possible without a portal and without
    uinput, and the fallback order depends on it."
done

# ── The portal, and the half of it that is not there ────────────────────────
checked=$((checked + 1))
screencast=$(value portal_ScreenCast)
case "$screencast" in
  ''|absent) note_fail "the ScreenCast portal is absent, and it is the only capture path GNOME and KDE offer" ;;
  *) [ "$screencast" -ge 4 ] 2>/dev/null || note_fail "the ScreenCast portal is version $screencast, and this was written against 4" ;;
esac

checked=$((checked + 1))
sources=$(value source_types)
# Bit 1 is a monitor. A portal that can only offer windows cannot serve a
# remote desktop, and the bit is checked rather than the number so that a
# compositor gaining virtual sources does not read as a failure.
[ $(( sources & 1 )) -eq 1 ] 2>/dev/null \
  || note_fail "the ScreenCast portal offers source types $sources, which does not include a monitor"

checked=$((checked + 1))
remote=$(value portal_RemoteDesktop)
if [ "$remote" != "absent" ]; then
  note_fail "the RemoteDesktop portal is now present on wlroots, at version $remote.
    This check asserted its absence on purpose. Input on wlroots was planned
    around the virtual-keyboard and virtual-pointer protocols because
    xdg-desktop-portal-wlr implemented ScreenCast and nothing else. If that has
    changed, the fallback order should be revisited rather than this line
    relaxed: input on wlroots was planned around the virtual-device protocols
    precisely because no portal offered it."
fi

# ── And what Esper says when it lands in one ────────────────────────────────
#
# The rig's real purpose. On a Wayland session Esper refuses to serve, which is
# right, and until now explained itself with
#
#     X11 via DISPLAY: $DISPLAY variable not set
#
# — true, and about the wrong thing. The machine is not missing a variable, it
# is running Wayland. Somebody on Fedora or Ubuntu reads that, sets DISPLAY,
# and gets a different error.
#
# Asserted against the built binary in a real session, because this is exactly
# the class of message that reads fine in the source and is wrong on a machine
# the author does not have.
if [ -x "$here/build/esper-engine" ]; then
  checked=$((checked + 1))
  said=$("$here/scripts/in-container.sh" --built "$rig" \
    --ro "$here/build:/esper" --ro "$here/testdata:/testdata" -- sh -c '
export ESPER_HOME=/tmp/home; mkdir -p $ESPER_HOME
cp /testdata/fixtures/device_store.json $ESPER_HOME/devices.json
timeout 20 /esper/esper-engine host 2>&1 | grep -m1 presence' 2>/dev/null)

  case "$said" in
    *"Wayland session"*) ;;
    "") note_fail "the engine said nothing about presence in a Wayland session" ;;
    *) note_fail "the engine refused a Wayland session without naming Wayland:
      $said
    A person reading that sets DISPLAY and meets the next error." ;;
  esac
else
  echo "  … wayland: no build/esper-engine, so what Esper says here went unasked" >&2
fi

if [ "$checked" -lt 9 ]; then
  echo "  ✗ wayland: only $checked questions were asked, which is not all of them" >&2
  exit 1
fi

if [ "$fail" -ne 0 ]; then
  echo ""
  echo "  A Wayland session answers these, and the machine running this is not"
  echo "  one. The rig is scripts/rigs/wayland.Containerfile, and what each"
  echo "  answer is for is written above each check in this file."
  exit 1
fi
echo "  ✓ wayland: $checked answers from a real sway session — screencopy and ext-image-copy for pixels, virtual keyboard and pointer for input, ScreenCast v$screencast with a monitor source, and no RemoteDesktop portal"
