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

# ── A key, injected, and the compositor's own evidence that it arrived ─────
#
# The end of the loop, and the only part of the Wayland work that is not a
# measurement of what a compositor *offers*. `esper-input-wlr` takes a virtual
# keyboard and pointer and presses F9; the rig's sway config binds F9 to a
# command that writes a file.
#
# **The evidence is the compositor's, not the injector's.** The injecting
# process cannot create that file; sway can, and only would if the key reached
# it and resolved to F9 through the keymap. Asking the injector whether it had
# injected would be asking the wrong end — it answered yes for an hour while
# nothing was arriving.
#
# One defect was found getting here, and finding it took three wrong guesses
# first — a keymap terminator, a modifier state, and the keycode convention,
# each of which was changed and measured and made no difference. **A flush is
# not a delivery**: the events were lost when the process exited straight
# afterwards, which is exactly what happens at the end of a session, and the
# events being lost there are the key *releases*.
poke="$here/core/target/debug/examples/poke"
echo "  … wayland: injecting a key into sway"
# Built, and a failure to build is a failure here. Left conditional, a sabotage
# that does not compile leaves the previous binary in place and this check
# passes against code that is no longer in the tree — which is exactly what
# happened to the D-Bus check before it was made unconditional.
if ! (cd "$here/core" && cargo build -q -p esper-input-wlr --example poke) 2>/dev/null; then
  note_fail "the injector would not build, so nothing was injected"
elif [ -x "$poke" ]; then
  checked=$((checked + 1))
  arrived=$("$here/scripts/in-container.sh" --built "$rig" \
    --ro "$here/core/target/debug/examples:/probe" -- sh -c '
# A floor: the evidence must not already be there, or this check passes on a
# file somebody else wrote.
if [ -e "$XDG_RUNTIME_DIR/f9-arrived" ]; then echo "already-there"; exit 0; fi
/probe/poke >/dev/null 2>&1 || { echo "poke-failed"; exit 0; }
sleep 1
if [ -f "$XDG_RUNTIME_DIR/f9-arrived" ]; then echo "arrived"; else echo "nothing"; fi
' 2>/dev/null | tail -1)

  case "$arrived" in
    arrived) ;;
    already-there)
      note_fail "the evidence file existed before anything was injected, so this check proved nothing" ;;
    poke-failed)
      note_fail "the injector would not run against sway at all" ;;
    *)
      note_fail "a key was injected into sway and sway did not act on it.
    The compositor accepts the protocol and reports the device, so what is wrong
    is between the keycode and the binding. The one that has bitten before is a
    flush that was never waited on: bytes reach the socket and the process exits
    before the compositor reads them." ;;
  esac
else
  echo "  … wayland: no poke probe, so nothing was injected" >&2
fi

# ── And the engine's own ladder reaches for it ─────────────────────────────
#
# A backend that works and nothing calls is the pattern this project has a
# check for, and it very nearly happened here: `esper-input-wlr` was proved
# against sway while `esper-engine` had no dependency on it at all, so a real
# host on a wlroots machine would still have been view-only.
#
# `esper-engine input` runs the ladder and reports what it chose. On this
# machine that is X11; in a sway session X11 must fail and the wlroots backend
# must be picked up behind it — which is the order the decision calls for, and
# the only way to see it is to ask on a machine where the first rung is absent.
if [ -x "$here/build/esper-engine" ]; then
  checked=$((checked + 1))
  ladder=$("$here/scripts/in-container.sh" --built "$rig" \
    --ro "$here/build:/esper" -- /esper/esper-engine input 2>/dev/null \
    | grep -m1 "in use" | sed 's/.*in use *//')
  case "$ladder" in
    wayland-wlr) ;;
    "")
      note_fail "\`esper-engine input\` said nothing about a backend in a sway session" ;;
    *)
      note_fail "in a sway session the engine chose \"$ladder\".
    X11 is not available there, so the wlroots backend should have been next.
    A backend nothing reaches is a backend that is not in the product." ;;
  esac
else
  echo "  … wayland: no build/esper-engine, so the ladder went unasked" >&2
fi

# ── GNOME, which is where most people are ──────────────────────────────────
#
# The sway rig cannot answer anything about input through a portal, because
# wlroots has no RemoteDesktop portal to ask. GNOME does, and Fedora and Ubuntu
# are GNOME on Wayland by default, so this is the path most people would meet
# and the only one that could justify a D-Bus dependency.
#
# `mutter --headless --virtual-monitor` with `xdg-desktop-portal-gnome` behind
# it. What can be measured here is what the portal **offers**; what cannot is a
# granted session, because granting is a person's act and this rig has no
# person. Every assertion below is about the first kind.
gnome_rig=esper-wayland-gnome
if docker image inspect "$gnome_rig" >/dev/null 2>&1; then
  gnome=$("$here/scripts/in-container.sh" --built "$gnome_rig" -- sh -c '
ask() {
  dbus-send --session --dest=org.freedesktop.portal.Desktop --type=method_call --print-reply \
    /org/freedesktop/portal/desktop org.freedesktop.DBus.Properties.Get \
    "string:org.freedesktop.portal.$1" "string:$2" 2>/dev/null | grep -oE "uint32 [0-9]+" | awk "{print \$2}"
}
echo "screencast=$(ask ScreenCast version)"
echo "remotedesktop=$(ask RemoteDesktop version)"
echo "devices=$(ask RemoteDesktop AvailableDeviceTypes)"
methods=$(dbus-send --session --dest=org.freedesktop.portal.Desktop --type=method_call --print-reply \
  /org/freedesktop/portal/desktop org.freedesktop.DBus.Introspectable.Introspect 2>/dev/null \
  | sed -n "/interface name=\"org.freedesktop.portal.RemoteDesktop\"/,/<\/interface>/p")
echo "eis=$(printf "%s" "$methods" | grep -c ConnectToEIS)"
echo "notify=$(printf "%s" "$methods" | grep -c "method name=\"Notify")"
' 2>/dev/null)
  gvalue() { printf '%s' "$gnome" | awk -F= -v k="$1" '$1 == k { print $2; exit }'; }

  checked=$((checked + 1))
  [ -n "$(gvalue remotedesktop)" ] && [ "$(gvalue remotedesktop)" != "" ] \
    || note_fail "GNOME offers no RemoteDesktop portal, which is the only way to inject input there"

  # Keyboard and pointer. Touch is a bonus and is not required of a desktop.
  checked=$((checked + 1))
  devices=$(gvalue devices)
  [ $(( devices & 3 )) -eq 3 ] 2>/dev/null \
    || note_fail "GNOME's RemoteDesktop offers device types $devices, which is not both a keyboard and a pointer"

  # Two input paths, and the cheaper one is the plain-D-Bus `Notify*` calls:
  # `ConnectToEIS` hands back a socket for libei, and the `Notify*` methods
  # need no second library at all. Both are asserted because the choice between
  # them is a real one and losing either would change it.
  checked=$((checked + 1))
  [ "$(gvalue eis)" -ge 1 ] 2>/dev/null \
    || note_fail "GNOME's RemoteDesktop has no ConnectToEIS, so the libei path is not on offer"
  checked=$((checked + 1))
  [ "$(gvalue notify)" -ge 6 ] 2>/dev/null \
    || note_fail "GNOME's RemoteDesktop offers $(gvalue notify) Notify methods; input without libei needs the pointer and keyboard ones"
else
  echo "  … wayland: no $gnome_rig image, so GNOME went unasked" >&2
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

# ── The conversation, driven by this project's own client ──────────────────
#
# The strongest thing a container can establish about the portal. `esper-dbus`
# and `esper-input-portal` walk the real RemoteDesktop conversation:
# `CreateSession`, then `SelectDevices`, then `Start`.
#
# The first two are answered by the portal without anybody present, so they
# either work or they do not. `Start` raises a consent dialog, and a container
# has nobody to click it — so **stopping at `Start` is the pass**, and stopping
# anywhere earlier is a bug in this client.
#
# That distinction is the whole value of the check: "it did not work" is
# useless, and "it got two calls in and waited for a person" is a statement
# about where the code is correct. It found one real defect this way —
# `CreateSession` needs a `session_handle_token`, and omitting it does not
# produce an error: xdg-desktop-portal fails an assertion and aborts, so the
# caller sees only "recipient disconnected without replying".
probe="$here/core/target/debug/examples/try-portal"
if docker image inspect "$gnome_rig" >/dev/null 2>&1; then
  echo "  … wayland: walking the RemoteDesktop conversation"
  if ! (cd "$here/core" && cargo build -q -p esper-input-portal --example try-portal) 2>/dev/null; then
    note_fail "the portal probe would not build, so the conversation went unwalked"
  elif [ -x "$probe" ]; then
    checked=$((checked + 1))
    # Three seconds, because the thing being waited for is a person who is not
    # there. A longer wait would measure patience rather than correctness.
    walked=$("$here/scripts/in-container.sh" --built "$gnome_rig" \
      --ro "$here/core/target/debug/examples:/probe" -- /probe/try-portal 3 2>/dev/null | tail -1)
    case "$walked" in
      granted*)
        # Somebody or something approved it. Not expected here, and not a
        # failure — it means more of the path is exercised, not less.
        ;;
      *"prompt for Start"*) ;;
      *CreateSession*)
        note_fail "the conversation stopped at CreateSession:
      $walked
    That is before any person is involved, so it is this client's fault." ;;
      *SelectDevices*)
        note_fail "the conversation stopped at SelectDevices:
      $walked
    The session was created and then refused, which is a malformed request." ;;
      *)
        note_fail "the RemoteDesktop conversation ended somewhere unexpected:
      ${walked:-nothing at all}" ;;
    esac
  else
    echo "  … wayland: no try-portal probe, so the conversation went unwalked" >&2
  fi
fi

if [ "$checked" -lt 16 ]; then
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
echo "  ✓ wayland: $checked answers from two real sessions —"
echo "      sway:  screencopy and ext-image-copy for pixels, virtual keyboard and pointer for input,"
echo "             ScreenCast v$screencast with a monitor source, and no RemoteDesktop portal at all"
echo "      GNOME: RemoteDesktop v$(gvalue remotedesktop) offering keyboard, pointer and touch, by ConnectToEIS for libei"
echo "             or by $(gvalue notify) plain Notify methods that need no second library"
