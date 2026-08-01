#!/usr/bin/env bash
#
# Run the test suite under contention and name anything that did not pass
# every time.
#
# # Why this exists
#
# A test that fails one run in three is worse than a test that fails always:
# the failure that matters becomes indistinguishable from the one that does
# not, and people learn to re-run. Two of them have been found in this
# repository, both by accident, and both had the same shape — **a test whose
# answer depended on how busy the machine was.**
#
#   * `a_burst_of_loss_produces_one_keyframe_request` counted every message in
#     the outbound queue and called them keyframe requests. The queue also
#     carries the ladders' hints, which are driven by a clock, so a slow loop
#     let one join the queue.
#   * `utf8_survives_including_wide_and_combining_characters` read from a
#     pseudo-terminal and decoded each read on its own, so a character split
#     across two reads became two replacement characters. Where the split fell
#     depended on the machine.
#
# Neither reproduced on an idle machine. Both reproduced with three suites
# competing, which is what this arranges.
#
# # Why it is not in `make ci`
#
# Nine full runs of the suite is twenty minutes. This is the thing to run when
# a test has been *seen* to fail once — and before believing that running it
# again fixed it.
#
# Usage: check-flaky.sh [rounds] [parallel]
set -uo pipefail

rounds="${1:-3}"
parallel="${2:-3}"
root="$(cd "$(dirname "$0")/.." && pwd)"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

# **The display tests are a third of the suite and CI runs them.** They need an
# X server, so a sweep without one covers less than it looks like it does — and
# a tool that skipped them quietly is how a blind spot survives.
#
# **But this never turns them on by itself.** They draw over the whole screen
# and *inject keystrokes*, so on a real desktop they are somebody's session
# being typed into — which is why they are gated on `ESPER_DISPLAY_TESTS` and
# why CI sets it against an `Xvfb` nobody is looking at. The first version of
# this script set that variable whenever `DISPLAY` was present, and ran
# input-injection tests against a live desktop. It is the caller's decision,
# and the caller makes it the same way CI does:
#
#     ESPER_DISPLAY_TESTS=1 DISPLAY=:99 make flaky
#
# with a display that is disposable.
display=""
if [ -n "${DISPLAY:-}" ] && [ -n "${ESPER_DISPLAY_TESTS:-}" ]; then
  display="1"
fi

printf '  running the suite %s times, %s at once\n' "$((rounds * parallel))" "$parallel"
if [ -n "$display" ]; then
  printf '  including the display tests, on %s\n\n' "$DISPLAY"
else
  printf '  NOT including the display tests, which draw over the screen and\n'
  printf '  inject keystrokes. Run with ESPER_DISPLAY_TESTS=1 and a DISPLAY you\n'
  printf '  do not mind losing, the way CI does. This sweep covers less than CI.\n\n'
fi

for round in $(seq 1 "$rounds"); do
  for p in $(seq 1 "$parallel"); do
    (
      cd "$root/core" || exit 1
      # `ESPER_DISPLAY_TESTS` is inherited when the caller set it, and never
      # set here: see the note above about whose desktop that is.
      # **The whole output, not only the names.** The first version kept the
      # `---- name ----` lines and nothing else, so a flake it found could be
      # named and not read — which is the exact complaint this script makes
      # about flaky tests. The failing blocks are extracted below; the full log
      # stays on disk for the run that needs it.
      cargo test --workspace > "$work/r${round}p${p}.log" 2>&1
      grep -E '^---- .* stdout ----' "$work/r${round}p${p}.log" \
        > "$work/r${round}p${p}.txt" || true
    ) &
  done
  wait
  # A round's worth of failures, named as they happen rather than at the end:
  # a run that takes twenty minutes should say something before it finishes.
  found=$(cat "$work/r${round}"p*.txt 2>/dev/null | wc -l)
  printf '  round %s of %s: %s failure(s)\n' "$round" "$rounds" "$found"
done

total=$((rounds * parallel))
# `sort | uniq -c` over every run, so a test that failed twice is reported as
# twice rather than as present.
summary=$(cat "$work"/*.txt 2>/dev/null |
  sed -E 's/^---- (.*) stdout ----$/\1/' | sort | uniq -c | sort -rn)

echo
if [ -z "$summary" ]; then
  printf '  ✓ every test passed all %s runs\n' "$total"
  exit 0
fi

# The logs are wanted from here on, so they stop being temporary.
trap - EXIT
printf '  ✗ these did not pass every run:\n\n'
printf '%s\n' "$summary" | while read -r count name; do
  printf '      %s of %s runs failed: %s\n' "$count" "$total" "$name"
done

# **And why.** One failure's own output, from the first run that saw it: a name
# says which test to look at and nothing about what to look for, and a flake
# that has just been reproduced after twenty minutes should not have to be
# reproduced again to be read.
printf '\n  what one of those failures said:\n\n'
printf '%s\n' "$summary" | head -1 | while read -r _count name; do
  for log in "$work"/*.log; do
    if grep -q "^---- $name stdout ----$" "$log" 2>/dev/null; then
      awk -v want="---- $name stdout ----" '
        $0 == want { on = 1 }
        on && /^---- / && $0 != want { exit }
        on { print "      " $0 }
      ' "$log"
      break
    fi
  done
done
printf '\n  the full logs of every run are kept in %s\n' "$work"
printf '\n  A test that fails sometimes is a test nobody can read a failure from.\n'
if [ -z "$display" ]; then
  printf '  And the display tests were not among them: see above.\n'
fi
printf '  See the header of this script for the two found so far and their shape.\n'
exit 1
