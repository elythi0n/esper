#!/usr/bin/env bash
#
# `docs/scripting.md` lists every command that answers `--json`, and this holds
# that list to the binaries in both directions.
#
# # Why this is a check and not a review
#
# The printed form of a report is written for a person and is free to change;
# the object is the contract, and a contract with no document is one nobody can
# rely on. But a document is exactly the thing that goes stale — `--json` was
# added to four commands in one afternoon, and a hand-written list would have
# been wrong by the evening.
#
# So both directions:
#
#   1. every command the document names answers `--json` with an object;
#   2. every command whose own `--help` offers `--json` is in the document.
#
# (2) is what makes (1) worth anything: a document naming three of eight would
# otherwise pass.
#
# Run from the repository root, against `build/`. Exit 1 on any finding.
set -euo pipefail

BIN=${BIN:-build}
DOC=docs/scripting.md
fail=0

if [ ! -x "$BIN/esper" ]; then
  echo "  ✗ scripting: no $BIN/esper — run 'make build' first"
  exit 1
fi
if [ ! -f "$DOC" ]; then
  echo "  ✗ scripting: $DOC is missing, and the JSON surface is a contract with no document"
  exit 1
fi

# Every command the front door hands on or runs itself, from its own help.
#
# The first word of each entry, because several carry their arguments in the
# same line — `sessions [n]`, `version [--json]`, `host enable`.
commands=$("$BIN/esper" --help 2>/dev/null |
  sed -nE 's/^  ([a-z][a-z-]*)( .*)?$/\1/p' | sort -u)

# The document's own list: a heading of the form "## `esper <command> --json`",
# and the streaming pair, whose heading names two commands at once.
documented=$(
  {
    sed -nE 's/^## `esper ([a-z-]+) --json`.*/\1/p' "$DOC"
    sed -nE 's/^## `esper ([a-z-]+) --json` and `esper ([a-z-]+) --json`.*/\1\n\2/p' "$DOC"
  } | sort -u
)

# **The ones that stream.** `pair` and `invite` answer one object per line and
# then wait — for a person to read a code out, or for somebody to open a link —
# so running them to completion is waiting for a peer that is not coming. What
# is checked is the first line, which is the part a caller needs.
streams="pair invite"

# What actually offers it, asked of the binary rather than of the source.
# **Its own usage, not any help it happens to print.** `esper help --help` is
# the general help, which lists `version [--json]` — so a check that only
# grepped the output would call `help` a command that answers `--json`.
offers=""
for c in $commands; do
  said=$("$BIN/esper" "$c" --help 2>&1 || true)
  case "$said" in
    "usage: esper $c"*) ;;
    *) continue ;;
  esac
  if printf '%s' "$said" | grep -q -- '--json'; then
    offers="$offers$c
"
  fi
done
offers=$(printf '%s' "$offers" | sed '/^$/d' | sort -u)

# A floor: two empty lists agree about everything.
count=$(printf '%s\n' "$offers" | sed '/^$/d' | wc -l)
if [ "$count" -lt 5 ]; then
  echo "  ✗ scripting: only $count command(s) offer --json, which is not this build —"
  echo "    the help was parsed wrong, and an empty comparison passes"
  exit 1
fi

for c in $documented; do
  if ! printf '%s\n' "$offers" | grep -qx "$c"; then
    echo "  ✗ scripting: $DOC documents \`esper $c --json\` and its own help does not offer it"
    fail=1
  fi
done
for c in $offers; do
  if ! printf '%s\n' "$documented" | grep -qx "$c"; then
    echo "  ✗ scripting: \`esper $c --json\` answers and $DOC does not say what it answers with —"
    echo "    an undocumented contract is one somebody works out by trial"
    fail=1
  fi
done

# And each one really answers an object, not a table with a flag it ignored.
# `discover` listens on the network and `doctor` starts an engine, so both are
# given a home of their own and the short listen.
home=$(mktemp -d)
trap 'rm -rf "$home"' EXIT
for c in $documented; do
  case " $streams " in
    *" $c "*)
      # A stream: take the first line and stop, rather than waiting for
      # whoever the command is waiting for.
      case "$c" in
        pair) out=$(ESPER_HOME="$home" timeout 10 "$BIN/esper" pair --listen 127.0.0.1:0 --json 2>/dev/null | head -1 || true) ;;
        # With no broker configured this refuses and says why, which is not
        # JSON and is the right answer — so an empty first line is not a
        # finding here, and the refusal is checked below.
        invite) out=$(ESPER_HOME="$home" timeout 10 "$BIN/esper" invite --json 2>/dev/null | head -1 || true)
                [ -z "$out" ] && continue ;;
      esac
      ;;
    *)
      case "$c" in
        discover) out=$(ESPER_HOME="$home" "$BIN/esper" "$c" --seconds 1 --json 2>/dev/null || true) ;;
        *) out=$(env -u DISPLAY -u WAYLAND_DISPLAY -u XAUTHORITY ESPER_HOME="$home" \
                 "$BIN/esper" "$c" --json 2>/dev/null || true) ;;
      esac
      ;;
  esac
  if ! printf '%s' "$out" | python3 -c 'import json,sys; json.load(sys.stdin)' 2>/dev/null; then
    echo "  ✗ scripting: \`esper $c --json\` did not answer with JSON:"
    printf '%s\n' "$out" | head -3 | sed 's/^/      /' || true
    fail=1
  fi
done

# A stream's first line says what it is, and a reader branches on that word.
# Without it, every line looks the same and a `paired` cannot be told from an
# `offering`.
first=$(ESPER_HOME="$home" timeout 10 "$BIN/esper" pair --listen 127.0.0.1:0 --json 2>/dev/null | head -1 || true)
if ! printf '%s' "$first" | python3 -c '
import json, sys
d = json.load(sys.stdin)
assert d["event"] == "offering", d
assert d["code"] and d["port"] and d["fingerprint"]["words"]
' 2>/dev/null; then
  echo "  ✗ scripting: \`esper pair --listen … --json\` did not begin with an offering:"
  printf '%s\n' "$first" | head -2 | sed 's/^/      /' || true
  fail=1
fi

# **And the broker, which is its own program.** Whoever runs one scripts it the
# same way, and a second binary is exactly where a shape drifts: it is not in
# the loop above, so without this its `--json` commands could answer anything.
broker_commands=$("$BIN/esper-broker" help 2>/dev/null |
  sed -nE 's/^  ([a-z][a-z-]*)( .*)?$/\1/p' | sort -u)
broker_offers=""
for c in $broker_commands; do
  said=$("$BIN/esper-broker" "$c" --help 2>&1 || true)
  case "$said" in
    "usage: esper-broker $c"*) ;;
    *) continue ;;
  esac
  if printf '%s' "$said" | grep -q -- '--json'; then
    broker_offers="$broker_offers$c
"
  fi
done
broker_offers=$(printf '%s' "$broker_offers" | sed '/^$/d' | sort -u)
broker_count=$(printf '%s\n' "$broker_offers" | sed '/^$/d' | wc -l)
if [ "$broker_count" -lt 2 ]; then
  echo "  ✗ scripting: only $broker_count broker command(s) offer --json, which is not this build"
  exit 1
fi
for c in $broker_offers; do
  if ! grep -q "esper-broker $c --json" "$DOC"; then
    echo "  ✗ scripting: \`esper-broker $c --json\` answers and $DOC does not mention it"
    fail=1
  fi
  out=$(ESPER_HOME="$home" "$BIN/esper-broker" "$c" --json 2>/dev/null || true)
  if ! printf '%s' "$out" | python3 -c 'import json,sys; json.load(sys.stdin)' 2>/dev/null; then
    echo "  ✗ scripting: \`esper-broker $c --json\` did not answer with JSON:"
    printf '%s\n' "$out" | head -3 | sed 's/^/      /' || true
    fail=1
  fi
done

if [ "$fail" != "0" ]; then
  echo ""
  echo "  The JSON surface is what a monitoring check, a support script and a"
  echo "  packaging test read. Both lists have to say the same thing."
  exit 1
fi
printf '  ✓ scripting: %s commands of `esper` and %s of `esper-broker` answer --json, each documented and each answering an object\n' \
  "$count" "$broker_count"
