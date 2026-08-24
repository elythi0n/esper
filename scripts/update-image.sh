#!/usr/bin/env bash
# Move one image pin forward, deliberately and with the change shown.
#
# Pins in `packaging/images.tsv` exist so that what runs is what somebody
# looked at. That makes updating them a thing which must be easy to do on
# purpose and impossible to do by accident — the opposite of a tag, which
# updates itself and tells nobody.
#
# This re-resolves the tag in the manifest, prints what moved, and rewrites the
# line with today's date. It does not decide whether the new image is one you
# want; that is the part a person does.
#
# Usage: scripts/update-image.sh <nickname> [<nickname>...]
#        scripts/update-image.sh --check          # report drift, change nothing
set -uo pipefail

here=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
manifest="$here/packaging/images.tsv"

only_check=0
if [ "${1:-}" = "--check" ]; then
  only_check=1
  shift
fi

names=("$@")
if [ ${#names[@]} -eq 0 ]; then
  mapfile -t names < <(grep -v '^#' "$manifest" | awk -F'\t' 'NR>1 && $1 != "" { print $1 }')
fi

# The digest for linux/amd64 specifically. A multi-architecture tag resolves to
# an index whose own digest covers every architecture, and pinning that would
# be a pin that still lets the bytes differ per machine.
resolve() {
  docker manifest inspect -v "$1" 2>/dev/null | python3 -c '
import json, sys
try:
    entries = json.load(sys.stdin)
except Exception:
    raise SystemExit(1)
entries = entries if isinstance(entries, list) else [entries]
for entry in entries:
    platform = entry.get("Descriptor", {}).get("platform", {})
    if platform.get("architecture") == "amd64" and platform.get("os") == "linux" and not platform.get("variant"):
        print(entry["Descriptor"]["digest"])
        break
else:
    raise SystemExit(1)
'
}

today=$(date -u +%Y-%m-%d)
moved=0
failed=0

for name in "${names[@]}"; do
  line=$(grep -v '^#' "$manifest" | awk -F'\t' -v n="$name" '$1 == n { print; exit }')
  if [ -z "$line" ]; then
    echo "  ✗ update-image: no image called \"$name\" in packaging/images.tsv" >&2
    failed=1
    continue
  fi

  reference=$(printf '%s' "$line" | cut -f2)
  was=$(printf '%s' "$line" | cut -f3)

  now=$(resolve "$reference")
  if [ -z "$now" ]; then
    echo "  ✗ update-image: could not resolve $reference — no network, or the tag is gone" >&2
    failed=1
    continue
  fi

  if [ "$now" = "$was" ]; then
    printf '  · %-12s %s is unchanged\n' "$name" "$reference"
    continue
  fi

  moved=1
  printf '  → %-12s %s\n      was %s\n      now %s\n' "$name" "$reference" "$was" "$now"

  if [ "$only_check" -eq 1 ]; then
    continue
  fi

  # Rewritten with python rather than sed: the purpose column is prose written
  # by a person and contains slashes, brackets and backticks.
  python3 - "$manifest" "$name" "$now" "$today" <<'PY'
import sys
from pathlib import Path

path, name, digest, today = Path(sys.argv[1]), sys.argv[2], sys.argv[3], sys.argv[4]
lines = path.read_text().split("\n")
for i, line in enumerate(lines):
    if line.startswith("#") or "\t" not in line:
        continue
    fields = line.split("\t")
    if fields[0] != name:
        continue
    fields[2] = digest
    fields[3] = today
    lines[i] = "\t".join(fields)
    break
path.write_text("\n".join(lines))
PY
done

if [ "$failed" -ne 0 ]; then
  exit 1
fi

if [ "$only_check" -eq 1 ] && [ "$moved" -ne 0 ]; then
  echo ""
  echo "  Those tags now point somewhere else. That is not a failure — it is what"
  echo "  tags do, and the pins are why it changed nothing here. Take the update"
  echo "  when you have a reason to, with scripts/update-image.sh <nickname>."
  exit 0
fi

if [ "$moved" -eq 0 ]; then
  echo "  ✓ update-image: every pin still names what its tag names"
fi
