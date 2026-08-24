#!/usr/bin/env bash
# One mistake, one sentence — asked of the built binary, not of the source.
#
# # Why this exists
#
# Every command refuses an option it does not know, which is right and was
# already true. What was not true is that they agreed on how to say so. The
# same mistake came back in three phrasings and four shapes:
#
#     cp: unknown flag --nonsense
#     esper-engine lock: unknown option --nonsense
#     screenshot: unknown option --nonsense
#     esper-engine terminal: terminal: unexpected argument "--nonsense"
#
# — the command's name twice in one of them, the program's name missing from
# two, and "flag", "option" and "argument" for one thing. The product's own
# usage texts say `options:`, five times, so that is the word.
#
# None of it is visible in a diff. Each line reads perfectly where it is
# written; only putting them side by side shows it, which is what this does.
#
# # Why it drives the binary
#
# Because the message a person sees is assembled from two places — the parser
# says what is wrong, the dispatcher says which program and which command — and
# reading either alone cannot tell you what comes out. The doubled name lived
# exactly in that seam.
#
# # Why all three binaries
#
# A person meets all of them: they type `esper`, `esper` hands verbs to
# `esper-engine`, and they run `esper-broker` to host the introducer that the
# product ships no default for. Each had its own phrasing —
#
#     esper api: "--nonsense" is not an option
#     esper-broker serve: unexpected argument "--nonsense"
#
# — the second with a third word for it, and the first with the argument and
# the complaint the other way round from everything else.
#
# Usage: scripts/check-messages.sh [path-to-build-directory]
set -uo pipefail

here=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
built="${1:-$here/build}"
engine="$built/esper-engine"
front="$built/esper"
broker="$built/esper-broker"

for binary in "$engine" "$front" "$broker"; do
  if [ ! -x "$binary" ]; then
    echo "  ✗ messages: no binary at $binary — run \`make build\` first" >&2
    exit 1
  fi
done

home=$(mktemp -d)
trap 'rm -rf "$home"' EXIT

# Every command that parses options, with enough positional arguments to reach
# the parser. The option itself is one nothing will ever take.
#
# `session`, `host` and `connect` are not here: they take an option list long
# enough to be its own check, and they reach the parser only with a device to
# connect to.
tries=(
  "discover --nonsense"
  "cp --nonsense a elythion:b"
  "lock --nonsense"
  "logoff --nonsense"
  "reboot --nonsense"
  "wake --nonsense"
  "screenshot --nonsense"
  "invite --nonsense"
  "forward --nonsense"
  "terminal --nonsense 192.0.2.1:7420"
)

# The front door and the broker, which a person meets just as often.
front_tries=("api --nonsense" "doctor --nonsense" "version --nonsense" "sessions --nonsense")
broker_tries=("serve --nonsense" "identity --nonsense" "version --nonsense")

fail=0
checked=0

try_all() {
  local binary="$1" name="$2"
  shift 2
  for try in "$@"; do
    # shellcheck disable=SC2086 -- the arguments are this file's own words
    said=$(ESPER_HOME="$home" timeout 10 "$binary" $try 2>&1 | head -1)
    command=${try%% *}
    checked=$((checked + 1))

    want="$name $command: unknown option \"--nonsense\""
    if [ "$said" != "$want" ]; then
      printf '  ✗ messages: `%s %s` said\n      %s\n    and every other command says\n      %s\n' \
        "$name" "$command" "$said" "$want"
      fail=1
    fi
  done
}

try_all "$engine" "esper-engine" "${tries[@]}"
try_all "$front" "esper" "${front_tries[@]}"
try_all "$broker" "esper-broker" "${broker_tries[@]}"

# And the other half: asking is never a failure.
#
# All three binaries used to answer a request for help on a *command* by
# refusing it — the engine fell through to the general list, the front door ran
# the command, and the broker called `--help` an option it does not have. A
# person who asks gets an answer, on stdout, with a nought: anything else is a
# program saying that asking was the mistake.
asks=(
  "$engine|esper-engine|discover --help"
  "$engine|esper-engine|invite --help"
  "$engine|esper-engine|terminal --help"
  "$front|esper|doctor --help"
  "$front|esper|sessions --help"
  "$front|esper|version --help"
  "$broker|esper-broker|serve --help"
  "$broker|esper-broker|identity --help"
  "$broker|esper-broker|version --help"
)
for ask in "${asks[@]}"; do
  binary=${ask%%|*}
  rest=${ask#*|}
  name=${rest%%|*}
  argv=${rest#*|}
  command=${argv%% *}
  checked=$((checked + 1))

  # shellcheck disable=SC2086 -- the arguments are this file's own words
  out=$(ESPER_HOME="$home" timeout 10 "$binary" $argv 2>/dev/null)
  code=$?
  # shellcheck disable=SC2086
  errors=$(ESPER_HOME="$home" timeout 10 "$binary" $argv 2>&1 >/dev/null)

  if [ "$code" -ne 0 ]; then
    printf '  ✗ messages: `%s %s --help` exited %s — asking is not attempting\n' "$name" "$command" "$code"
    fail=1
  fi
  if [ -n "$errors" ]; then
    printf '  ✗ messages: `%s %s --help` wrote to stderr, so it cannot be piped:\n      %s\n' \
      "$name" "$command" "$(printf '%s' "$errors" | head -1)"
    fail=1
  fi
  case "$out" in
    "usage: $name $command"*) ;;
    *)
      printf '  ✗ messages: `%s %s --help` answered\n      %s\n    and a usage begins by naming the command somebody typed\n' \
        "$name" "$command" "$(printf '%s' "$out" | head -1)"
      fail=1
      ;;
  esac
done

# And asking costs nothing, which for one command means more than a nought.
#
# `esper-broker identity` creates the key if there is not one yet, and it took
# no notice of `--help` at all — so a person asking what the command did got a
# key file written into their home as the answer, and the same happened to
# whoever typed it here. A fresh state directory is handed to it and has to
# come back empty.
checked=$((checked + 1))
untouched=$(mktemp -d)
ESPER_HOME="$untouched" timeout 10 "$broker" identity --help >/dev/null 2>&1
left=$(find "$untouched" -type f | wc -l)
rm -rf "$untouched"
if [ "$left" -ne 0 ]; then
  printf '  ✗ messages: `esper-broker identity --help` created %s file(s) — asking is not doing\n' "$left"
  fail=1
fi

# A floor: a run that tried nothing agrees with everything.
if [ "$checked" -lt 26 ]; then
  echo "  ✗ messages: only $checked command(s) were tried, which is not this engine" >&2
  exit 1
fi

if [ "$fail" -ne 0 ]; then
  echo ""
  echo "  The parser says what is wrong; the dispatcher says which program and"
  echo "  which command. A command that names itself as well says it twice."
  exit 1
fi
echo "  ✓ messages: $checked checks across three binaries — one wording for a refusal, and asking is never a failure"
