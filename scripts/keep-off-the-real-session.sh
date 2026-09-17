#!/usr/bin/env bash
# Build a PATH directory that refuses to lock, log out, or reboot this machine.
#
# # Why this is not something a display rig can do
#
# `scripts/with-display.sh` gives a suite a display of its own, and that is
# enough for anything scoped to a display. It is **not** enough for the session
# manager, which is exactly what this product reaches for when it locks a
# screen or logs somebody off:
#
#   esper-presence/src/lock.rs    loginctl lock-session          ThisLoginSession
#   esper-presence/src/power.rs   loginctl terminate-session self
#   esper-presence/src/power.rs   gnome-session-quit --logout --no-prompt
#   esper-presence/src/power.rs   systemctl reboot
#
# `loginctl` does not look at `DISPLAY`. It asks logind over the *system* bus
# which session the calling process belongs to, and that is the person's real
# one however headless the test thinks it is. `lock.rs` already says so in its
# own comment — *"a locker that asks the session manager locks the person's
# screen rather than the one the host was showing"* — and it happened: a suite
# run on 2026-09-16 locked the screen of the machine it was running on, in the
# middle of somebody working.
#
# So the ladder is cut where it reaches the session manager, and nowhere else.
# The display-scoped lockers — `xdg-screensaver`, `swaylock`, `i3lock` — are
# left alone: under a display rig they lock the rig's screen, which is the
# whole point of having one.
#
# # Why a pass-through and not a wall
#
# `loginctl show-session` is how `locked.rs` *detects* whether a screen is
# locked, and that is a read a test should be able to do. A shim that refused
# everything would turn a working detector into a broken one and the suite
# would be measuring the shim. Only the verbs that change the machine are
# refused.
#
# Usage:  PATH="$(scripts/keep-off-the-real-session.sh):$PATH"
set -euo pipefail

# **Tidies up after itself, because its caller cannot.** A suite that sets its
# own `trap ... EXIT` would replace one registered here rather than add to it,
# so the directory has to survive not being cleaned up by anybody and still not
# accumulate. Anything left by a run that is over a day old was left by a run
# that is over.
find "${TMPDIR:-/tmp}" -maxdepth 1 -name 'esper-no-real-session-*' -type d -mtime +0 \
  -exec rm -rf {} + 2>/dev/null || true

dir="$(mktemp -d -t esper-no-real-session-XXXXXX)"

# (program, the verbs that change this machine)
#
# Matched on the *first* argument, because that is where each of these puts its
# verb. A refusal is exit 1 with a sentence on stderr, which is what these
# programs do when they will not do a thing — so the ladder above them falls
# through to the next rung exactly as it would on a machine with no seat.
make_shim() {
  local program="$1" refuse="$2"
  # The real one, found by looking past this directory.
  local real
  real="$(PATH="$(echo "$PATH" | tr ':' '\n' | grep -vxF "$dir" | paste -sd:)" command -v "$program" 2>/dev/null || true)"
  cat > "$dir/$program" <<INNER
#!/bin/sh
# Written by scripts/keep-off-the-real-session.sh. Not a product file.
case "\$1" in
  $refuse)
    echo "esper test rig: refusing '$program \$*' — a test may not lock, log out or reboot the machine it runs on" >&2
    exit 1
    ;;
esac
INNER
  if [ -n "$real" ]; then
    printf 'exec %s "$@"\n' "$real" >> "$dir/$program"
  else
    # Nothing real to pass to: behave like a machine that does not have it.
    printf 'echo "%s: not found" >&2\nexit 127\n' "$program" >> "$dir/$program"
  fi
  chmod +x "$dir/$program"
}

make_shim loginctl          'lock-session|lock-sessions|terminate-session|terminate-user|kill-session|kill-user|poweroff|reboot|suspend|hibernate'
make_shim systemctl         'reboot|poweroff|halt|kexec|suspend|hibernate|hybrid-sleep|emergency|rescue'
make_shim shutdown          '*'
make_shim gnome-session-quit '*'
make_shim qdbus             'org.kde.Shutdown'

echo "$dir"
