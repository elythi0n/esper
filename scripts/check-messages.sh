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
  # **`pair` was missing from this list**, and it is the one command in the
  # product where a person is reading something off a screen — it answered
  # `pair: unknown option` with no program name at all.
  "pair --nonsense"
  "devices --nonsense"
  "config --nonsense"
  "identity --nonsense"
  "input --nonsense"
  "version --nonsense"
  # The commands that take a device rather than a flag. A fingerprint never
  # begins with a dash, so a mistyped option is an option — these read one as
  # a fingerprint and answered "no paired device's fingerprint starts with
  # `--nonsense`", which is a program telling somebody their typo is a
  # machine.
  "schedule --nonsense"
  "lock-after --nonsense"
  "keep --nonsense"
  "forget --nonsense"
  "group --nonsense somewhere"
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
# A person who asks for help on a *command* gets an answer, on stdout, with a
# nought. Anything else is a program saying that asking was the mistake —
# falling through to the general list, running the command anyway, or calling
# `--help` an option it does not have.
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

# **One question, one answer, three binaries.**
#
# `version --json` is what a deployment asks all three of, and each answered
# differently: the front door wrote an object, the engine printed its three
# human lines and exited zero — the flag accepted and ignored — and the broker
# refused it. A script asking what is installed got a table from one of them
# and no reason to look further until something downstream broke on it.
#
# So: every binary answers with an object, every object carries the same three
# keys, and all three agree about the version and the protocol. The broker
# carries a rendezvous version as well, which is its own and is not compared.
checked=$((checked + 1))
versions=""
for binary in "$front" "$engine" "$broker"; do
  said=$("$binary" version --json 2>/dev/null || true)
  one=$(printf '%s' "$said" | python3 -c '
import json, sys
d = json.load(sys.stdin)
for k in ("version", "buildTime", "protocol"):
    if k not in d:
        raise SystemExit(1)
print(d["version"], d["protocol"])
' 2>/dev/null || true)
  if [ -z "$one" ]; then
    printf '  ✗ messages: `%s version --json` did not answer with an object carrying version, buildTime and protocol: %s\n' \
      "$(basename "$binary")" "$(printf '%s' "$said" | head -c 80)"
    fail=1
    continue
  fi
  versions="$versions$one\n"
done
# And they agree. Two halves of one release that disagree about the protocol
# is the failure the shared VERSION file exists to prevent.
agreed=$(printf '%b' "$versions" | sort -u | grep -c . || true)
if [ "$agreed" -gt 1 ]; then
  printf '  ✗ messages: the three binaries report different versions or protocols:\n'
  printf '%b' "$versions" | sed 's/^/      /'
  fail=1
fi

# And a flag none of them has is refused by all of them, in one wording.
checked=$((checked + 1))
for binary in "$front" "$engine" "$broker"; do
  if "$binary" version --porcelain >/dev/null 2>&1; then
    printf '  ✗ messages: `%s version --porcelain` was accepted — a flag nothing has\n' \
      "$(basename "$binary")"
    fail=1
  fi
done

# **Help fits in an eighty-column terminal.**
#
# It is read in one, and a line wider than that wraps wherever the terminal
# decides — usually mid-word. `grant --help` listed fourteen permission names
# on one 131-column line: the list somebody opens that help *to read* was the
# one thing on the screen they could not.
#
# Measured through the front door, because that is the name people type and the
# usage line carries it: the same text is a few columns wider when the engine
# is invoked directly, which is the binary its own first line says not to run.
checked=$((checked + 1))
wide=""
for command in connect host cp terminal forward invite grant revoke schedule \
               lock-after devices group wake screenshot discover lock logoff \
               reboot version identity sessions doctor config; do
  # shellcheck disable=SC2086
  over=$("$front" $command --help 2>/dev/null | awk 'length($0) > 78 { print length($0); }' | head -1)
  [ -n "$over" ] && wide="$wide [$command: ${over}c]"
done
# And the general help, which is the first thing anybody reads.
over=$("$front" help 2>/dev/null | awk 'length($0) > 78 { print length($0); }' | head -1)
[ -n "$over" ] && wide="$wide [help: ${over}c]"
if [ -n "$wide" ]; then
  printf '  ✗ messages: these help texts are wider than an eighty-column terminal:%s\n' "$wide"
  fail=1
fi

# **One sentence for "that device is not here".**
#
# Twelve commands take a device, and the ones that match a name or a
# fingerprint prefix are matching by the same rule — so a refusal that read
# `0 paired devices match "kit"` beside `no paired device is called "kit"` was
# one question with two answers, and the count-of-nought one is the worse
# sentence of the two.
#
# Asked of a store with devices in it, because "none is paired at all" is a
# different fact and deserves its own sentence.
named_home=$(mktemp -d)
cp "$here/testdata/fixtures/device_store_screens.json" "$named_home/devices.json"
wordings=""
for command in wake lock logoff reboot screenshot terminal connect; do
  checked=$((checked + 1))
  said=$(ESPER_HOME="$named_home" "$front" "$command" definitely-not-a-device 2>&1 </dev/null || true)
  first=$(printf '%s\n' "$said" | grep -v '^$' | head -1)
  case "$first" in
    *"no paired device is called"*) ;;
    *) wordings="$wordings
      $command: $first" ;;
  esac
done
if [ -n "$wordings" ]; then
  printf '  ✗ messages: these refuse an unknown device in their own words:%s\n' "$wordings"
  fail=1
fi
rm -rf "$named_home"

# **One word, one meaning.** `key` names the whole public key in every binary
# that prints one, and `fingerprint` names the short form derived from it.
#
# These are not interchangeable and look it only in a listing: the key is what
# a peer pins, and the fingerprint is sixty-four bits of a hash — a birthday
# bound, not an identity. A device that printed its fingerprint under the word
# `key`, beside a broker printing the real thing under the same word, is how
# somebody comes to compare sixty-four bits and believe they have checked a
# machine.
home_for_keys=$(mktemp -d)
for pair in "$front|esper identity" "$broker|esper-broker identity"; do
  bin=${pair%%|*}
  what=${pair##*|}
  checked=$((checked + 1))
  # shellcheck disable=SC2086
  said=$(ESPER_HOME="$home_for_keys" $bin identity 2>/dev/null || true)
  key=$(printf '%s\n' "$said" | awk '/^  key /{print $2}')
  if [ "${#key}" != "64" ]; then
    printf '  ✗ messages: `%s` prints a %s-character key, and a key is sixty-four\n' \
      "$what" "${#key}"
    printf '%s\n' "$said" | sed 's/^/      /'
    fail=1
  fi
done
# And both answer `--json` with the same three field names, so a reader that
# knows one knows the other.
if command -v python3 >/dev/null 2>&1; then
  for pair in "$front|esper identity" "$broker|esper-broker identity"; do
    bin=${pair%%|*}
    what=${pair##*|}
    checked=$((checked + 1))
    # shellcheck disable=SC2086
    obj=$(ESPER_HOME="$home_for_keys" $bin identity --json 2>/dev/null || true)
    if ! printf '%s' "$obj" | python3 -c '
import json, sys
d = json.load(sys.stdin)
assert len(d["public_key"]) == 64, d["public_key"]
assert len(d["fingerprint"]["hex"]) == 16, d["fingerprint"]
assert len(d["fingerprint"]["words"]) == 8, d["fingerprint"]
assert d["file"]
' 2>/dev/null; then
      printf '  ✗ messages: `%s --json` does not answer with public_key and fingerprint: %s\n' \
        "$what" "$(printf '%s' "$obj" | head -c 90)"
      fail=1
    fi
  done
fi
rm -rf "$home_for_keys"

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
