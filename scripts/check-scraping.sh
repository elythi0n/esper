#!/usr/bin/env bash
#
# No part of this product reads another part's *printed* output unless it says
# why.
#
# # Why this is a check and not a review
#
# `esper` and `esper-engine` ship together and talk to each other by running
# each other. When one parses the other's printed report with a regular
# expression, the report becomes a wire format nobody declared — written for a
# person, documented as free to change, and silently load-bearing.
#
# That has happened three times in this tree, and the third time nothing
# failed anywhere:
#
#   * `GET /identity` scraped `^\s*key\s+([0-9a-f]{8,64})`. The report started
#     printing the whole key under `key` and the fingerprint under its own
#     word; the regex went on matching, and every screen in the window drew an
#     identicon from the wrong value.
#   * the pairing routes scraped the code, the port and the eight words a
#     person reads aloud out of the sentence that shows them.
#   * the invitation route scraped the link, which *is* the secret.
#
# Each read correctly right up until somebody improved the prose. So: a Go
# file that runs a sibling binary and holds a regular expression has to be in
# the ledger below, with what it reads and why it is not asking for `--json`.
#
# Run from the repository root. Exit 1 on any finding.
set -euo pipefail

fail=0

# Where a scrape is allowed to be, and why there is no object to ask for.
#
# Every entry is a claim somebody can check by reading the file named. The
# right way off this list is a `--json` on the command being read, not a
# better regular expression.
ledger() {
  case "$1" in
    cmd/esper/doctor_session.go)
      echo "reads a *running* \`host\` and \`connect\` — how many pictures arrived, what address the host took, why it stopped. Those two stream for as long as a session lasts and have no object form; the pairing and the device list in the same file were moved to \`--json\` and are no longer read as prose"
      ;;
    cmd/esper-app/drop.go)
      echo "reads \`esper cp --progress\`, which exists for exactly this: the flag's whole purpose is a line a program can read, and it is documented as one"
      ;;
    cmd/esper-app/session.go|cmd/esper-app/shellapi.go)
      echo "holds a shape allowlist — a fingerprint, a route — and parses nothing any binary printed"
      ;;
    internal/server/pairing.go|internal/server/inviting.go)
      echo "holds shape allowlists for what a *caller* may send — an address, a code, a link. Both read the engine through \`--json\`"
      ;;
    *) return 1 ;;
  esac
}

files=$(grep -rl "exec.CommandContext\|exec.Command(" --include='*.go' cmd internal |
  grep -v '_test\.go$' | sort)
count=$(printf '%s\n' "$files" | sed '/^$/d' | wc -l)
if [ "$count" -lt 8 ]; then
  echo "  ✗ scraping: only $count file(s) run a sibling binary, which is not this tree"
  exit 1
fi

listed=0
for f in $files; do
  grep -q "regexp.MustCompile" "$f" || continue
  listed=$((listed + 1))
  if ! why=$(ledger "$f"); then
    echo "  ✗ scraping: $f runs another binary and holds a regular expression, and is not in"
    echo "    this check's ledger. If it reads that binary's printed output, ask for \`--json\`"
    echo "    instead; if it does not, add it with what the pattern is actually for"
    fail=1
    continue
  fi
  printf '    %s — %s\n' "$f" "$why"
done

# A floor on the ledger too: a run that examined nothing agrees with it.
if [ "$listed" -lt 4 ]; then
  echo "  ✗ scraping: only $listed file(s) were examined, so this check measured nothing"
  exit 1
fi

# **And the ones that were moved off prose stay off it.** Each of these read a
# sibling's report and now asks for the object; the old patterns coming back is
# the regression this line exists to catch.
for gone in "key\\\\s+(\\[0-9a-f\\]" "expires\\\\s+in (" "pair \\\\S\\*:(\\\\d+) --code"; do
  if grep -rnE "$gone" --include='*.go' cmd internal 2>/dev/null | grep -v '_test\.go' | head -1; then
    echo "  ✗ scraping: a pattern over a sibling's printed report is back (above)"
    fail=1
  fi
done

if [ "$fail" != "0" ]; then
  echo ""
  echo "  A report is written for a person and is free to change. Anything a"
  echo "  program depends on has to be something the program asked for."
  exit 1
fi
printf '  ✓ scraping: %s file(s) run a sibling binary, %s of them hold a pattern and each says what for\n' \
  "$count" "$listed"
