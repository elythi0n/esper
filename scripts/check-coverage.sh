#!/usr/bin/env bash
# A floor under the Go tests, and a floor under the floors.
#
# # Why a number at all
#
# Coverage is a poor measure of whether code is *tested*: every defect found in
# this tree last week was in a line coverage already counted as covered, and the
# thing that found them was sabotage. So this is not a quality target and the
# floors below are not aspirations.
#
# It is good at exactly one thing, and it is the thing that happened: **saying
# which code nothing has ever run.** `internal/service` sat at 0.0% — 332 lines
# wrapping systemd, including the promise that it will not overwrite a unit
# somebody else installed — and nothing in the suite, the review or the gates
# said so, because a package with no test file produces no failure.
#
# # The two floors
#
# **Per package**, a little under where each stands, so ordinary movement is
# free and a real drop is not. A package that has climbed well above its floor
# is reported, because a floor left far below the truth stops being one.
#
# **Every package has one.** A package that appears with no floor is a failure,
# not a default of zero: that is the case this exists for, and defaulting would
# reproduce it exactly.
set -euo pipefail

here=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
cd "$here"

# package                    floor
floors="
cmd/esper                    8
cmd/esper-app               38
internal/devices            87
internal/engine             80
internal/home               83
internal/proto              86
internal/server             50
internal/service            53
"
total_floor=48

profile=$(mktemp)
trap 'rm -f "$profile"' EXIT

echo "  running the Go tests with coverage…"
if ! go test -coverprofile="$profile" ./... > /dev/null 2>&1; then
  echo "  ✗ coverage: the tests do not pass, so there is nothing to measure" >&2
  go test ./... 2>&1 | grep -E "^(FAIL|---)" | head -20 >&2
  exit 1
fi

module=$(go list -m)
fail=0

floor_for() {
  awk -v want="$1" '$1 == want { print $2; found = 1 } END { if (!found) print "" }' <<< "$floors"
}

# Every package that has code of its own. A package with no test file never
# appears in the profile, which is precisely the hole this closes, so the list
# comes from the module rather than from the measurement.
packages=$(go list -f '{{if .GoFiles}}{{.ImportPath}}{{end}}' ./...)
measured=0

for pkg in $packages; do
  short=${pkg#"$module"/}
  floor=$(floor_for "$short")
  if [ -z "$floor" ]; then
    echo "  ✗ coverage: $short has no floor — add one to scripts/check-coverage.sh, even if it is 0 and says why"
    fail=1
    continue
  fi

  # Statements from the profile, which is per file: sum the counts for files
  # under this package's directory only.
  got=$(awk -F: -v dir="$pkg/" '
    NR > 1 && index($1, dir) == 1 {
      split($0, parts, " ")
      n = parts[2]; hits = parts[3]
      total += n
      if (hits > 0) covered += n
    }
    END { if (total > 0) printf "%.1f", 100 * covered / total; else print "0.0" }
  ' "$profile")
  measured=$((measured + 1))

  if awk -v got="$got" -v floor="$floor" 'BEGIN { exit !(got + 0 < floor + 0) }'; then
    echo "  ✗ coverage: $short is at ${got}% and its floor is ${floor}%"
    fail=1
  elif awk -v got="$got" -v floor="$floor" 'BEGIN { exit !(got + 0 > floor + 10) }'; then
    echo "  · $short is at ${got}%, well above its floor of ${floor}% — raise it"
  fi
done

# A run that measured nothing passes every floor it was given.
if [ "$measured" -lt 5 ]; then
  echo "  ✗ coverage: only $measured package(s) were measured, which is not this module" >&2
  exit 1
fi

total=$(awk 'NR > 1 { split($0, p, " "); total += p[2]; if (p[3] > 0) covered += p[2] } END { printf "%.1f", 100 * covered / total }' "$profile")
if awk -v got="$total" -v floor="$total_floor" 'BEGIN { exit !(got + 0 < floor + 0) }'; then
  echo "  ✗ coverage: the module is at ${total}% and its floor is ${total_floor}%"
  fail=1
fi

if [ "$fail" -ne 0 ]; then
  exit 1
fi
echo "  ✓ coverage: ${total}% of Go statements across $measured packages, every one above its floor"
