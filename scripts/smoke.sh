#!/usr/bin/env bash
# Post-build smoke test: the binaries exist, run, and agree with each other.
#
# The Go control plane and the Rust engine are built by different toolchains
# from one VERSION file. If they ever disagree about the release or the protocol
# version, the seam between them is broken in a way no unit test would catch —
# each half would pass its own suite.
set -euo pipefail
cd "$(dirname "$0")/.."

BIN=build
fail=0
check() { if [ "$2" = "$3" ]; then printf '  ✓ %s\n' "$1"; else printf '  ✗ %s\n      got:  %s\n      want: %s\n' "$1" "$2" "$3"; fail=1; fi; }

for b in ownview ownview-engine ownview-broker; do
  if [ ! -x "$BIN/$b" ]; then
    echo "  ✗ $BIN/$b missing — run 'make build' first"; exit 1
  fi
done

expected_version=$(cat VERSION)

go_version=$("$BIN/ownview" version | head -1 | awk '{print $2}')
eng_version=$("$BIN/ownview-engine" version | head -1 | awk '{print $2}')
brk_version=$("$BIN/ownview-broker" version | head -1 | awk '{print $2}')

check "ownview reports the VERSION file"        "$go_version"  "$expected_version"
check "ownview-engine reports the VERSION file" "$eng_version" "$expected_version"
check "ownview-broker reports the VERSION file" "$brk_version" "$expected_version"

go_proto=$("$BIN/ownview" version | awk '/protocol/{print $2}')
eng_proto=$("$BIN/ownview-engine" version | awk '/protocol/{print $2}')
brk_proto=$("$BIN/ownview-broker" version | awk '/protocol/{print $2}')

check "engine agrees with the control plane on the protocol version" "$eng_proto" "$go_proto"
check "broker agrees with the control plane on the protocol version" "$brk_proto" "$go_proto"

# Exit codes are part of the CLI contract: scripts branch on them.
"$BIN/ownview" version >/dev/null 2>&1; check "ownview version exits 0" "$?" "0"
set +e; "$BIN/ownview" definitely-not-a-command >/dev/null 2>&1; rc=$?; set -e
check "ownview rejects an unknown command with exit 1" "$rc" "1"

# OwnView operates no infrastructure, so a shipped binary must contain nothing
# it could phone home to. This check is that claim's proof, and the reason the
# claim is checkable rather than merely stated.
#
# What counts as an endpoint is deliberately narrow, and has been narrowed twice
# after false positives — because a check that cries wolf gets switched off,
# which is worse than not having one:
#
#   1. Bare domains do not count. Go embeds every module path and source
#      filename in its build info, so `github.com/elythi0n/ownview` is in every
#      binary we will ever ship and is not something the program can connect to.
#   2. Documentation hosts do not count. `getrandom` carries the error string
#      "Node.js ES modules are not directly supported, see https://docs.rs/..."
#      — a pointer for a human reading a diagnostic, not an address the program
#      dials.
#
# What does count: a scheme-prefixed URL to a host that could serve data, and
# any routable IP literal. Every allowlist entry below names why it is there;
# adding one without a reason is how this check stops meaning anything.
echo "  — scanning binaries for anything connectable"

# Hosts that exist only to be read by a person, never dialled by the program.
doc_hosts='docs\.rs|doc\.rust-lang\.org|rust-lang\.github\.io|www\.w3\.org|(www\.)?rfc-editor\.org|datatracker\.ietf\.org|tools\.ietf\.org'

# Loopback, link-local, multicast, broadcast, unspecified, and the RFC 5737
# documentation ranges are not endpoints anyone can phone home to.
harmless_ip='^(0\.|127\.|169\.254\.|22[4-9]\.|23[0-9]\.|24[0-9]\.|25[0-5]\.|192\.0\.2\.|198\.51\.100\.|203\.0\.113\.|255\.255\.255\.255)'

for b in ownview ownview-engine ownview-broker; do
  urls=$(strings "$BIN/$b" \
         | grep -oEi '(https?|wss?|quic|ftp)://[a-z0-9.-]+' \
         | grep -vEi "://($doc_hosts)" | sort -u || true)
  ips=$(strings "$BIN/$b" \
        | grep -oE '\b([0-9]{1,3}\.){3}[0-9]{1,3}\b' \
        | grep -vE "$harmless_ip" | sort -u || true)

  if [ -n "$urls" ] || [ -n "$ips" ]; then
    printf '  ✗ %s contains something connectable:\n' "$b"
    [ -n "$urls" ] && printf '%s\n' "$urls" | sed 's/^/      url: /'
    [ -n "$ips" ]  && printf '%s\n' "$ips"  | sed 's/^/      ip:  /'
    echo "      OwnView ships no default endpoint. If this is a dependency's,"
    echo "      that dependency needs justifying or replacing. If it is a"
    echo "      documentation link in a diagnostic string, add it to doc_hosts"
    echo "      above WITH A REASON."
    fail=1
  else
    printf '  ✓ %s has no URL and no routable IP literal\n' "$b"
  fi
done

if [ "$fail" -ne 0 ]; then echo; echo "  smoke test failed"; exit 1; fi
echo "  ✓ smoke test passed"
