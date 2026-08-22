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
# Usage: scripts/check-messages.sh [path-to-esper-engine]
set -uo pipefail

here=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
engine="${1:-$here/build/esper-engine}"

if [ ! -x "$engine" ]; then
  echo "  ✗ messages: no engine at $engine — run \`make build\` first" >&2
  exit 1
fi

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

fail=0
checked=0
for try in "${tries[@]}"; do
  # shellcheck disable=SC2086 -- the arguments are this file's own words
  said=$(ESPER_HOME="$home" timeout 10 "$engine" $try 2>&1 | head -1)
  command=${try%% *}
  checked=$((checked + 1))

  want="esper-engine $command: unknown option \"--nonsense\""
  if [ "$said" != "$want" ]; then
    printf '  ✗ messages: `%s` said\n      %s\n    and every other command says\n      %s\n' \
      "$command" "$said" "$want"
    fail=1
  fi
done

# A floor: a run that tried nothing agrees with everything.
if [ "$checked" -lt 8 ]; then
  echo "  ✗ messages: only $checked command(s) were tried, which is not this engine" >&2
  exit 1
fi

if [ "$fail" -ne 0 ]; then
  echo ""
  echo "  The parser says what is wrong; the dispatcher says which program and"
  echo "  which command. A command that names itself as well says it twice."
  exit 1
fi
echo "  ✓ messages: $checked commands refuse an option they do not know, in the same words"
