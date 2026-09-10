#!/usr/bin/env bash
#
# `docs/local-api.md` and the routes the server registers say the same thing.
#
# # Why this is a check and not a review
#
# The local API is the one surface in this product written for somebody else's
# program: the window uses it, and so can a dashboard, a script or an MCP
# adapter. A surface like that with no document is one people work out by
# reading Go — and a document of it is exactly the thing that goes stale, since
# a route is one line to add and nothing anywhere fails when the prose does not
# follow.
#
# So both directions:
#
#   1. every route the server registers is in the document;
#   2. every route the document names is registered.
#
# (2) is what makes (1) worth anything: a document listing four of thirty-one
# would otherwise pass.
#
# Read out of the source rather than off a running server, because a route that
# needs a machine in a particular state to be reachable is still a route.
#
# Run from the repository root. Exit 1 on any finding.
set -euo pipefail

SERVER=internal/server/server.go
DOC=docs/local-api.md
fail=0

for f in "$SERVER" "$DOC"; do
  if [ ! -f "$f" ]; then
    echo "  ✗ local-api: $f is missing; this check is stale"
    exit 1
  fi
done

# `mux.HandleFunc("GET /devices/{fingerprint}/grant", …)` → `GET /devices/{fingerprint}/grant`
registered=$(sed -nE 's/^[[:space:]]*mux\.HandleFunc\("([A-Z]+ [^"]+)".*/\1/p' "$SERVER" | sort -u)

# The document writes each one in a table cell as `GET /health`, in backticks.
documented=$(grep -oE '`(GET|POST|DELETE|PUT|PATCH) /[^`]*`' "$DOC" |
  tr -d '`' | sed -E 's/\?.*$//' | sort -u)

# Floors, because two empty lists agree about everything.
count=$(printf '%s\n' "$registered" | sed '/^$/d' | wc -l)
said=$(printf '%s\n' "$documented" | sed '/^$/d' | wc -l)
if [ "$count" -lt 20 ]; then
  echo "  ✗ local-api: $SERVER parsed as $count route(s), which is not this server"
  exit 1
fi
if [ "$said" -lt 20 ]; then
  echo "  ✗ local-api: $DOC parsed as $said route(s), which is not this document"
  exit 1
fi

for route in $(printf '%s\n' "$registered" | tr ' ' '@'); do
  human=${route//@/ }
  if ! printf '%s\n' "$documented" | grep -qxF "$human"; then
    echo "  ✗ local-api: the server answers \`$human\` and $DOC does not mention it —"
    echo "    a route somebody can only find by reading Go"
    fail=1
  fi
done
for route in $(printf '%s\n' "$documented" | tr ' ' '@'); do
  human=${route//@/ }
  if ! printf '%s\n' "$registered" | grep -qxF "$human"; then
    echo "  ✗ local-api: $DOC names \`$human\` and nothing registers it —"
    echo "    a document promising a route that answers 404"
    fail=1
  fi
done

# **And the posture is stated, because it is the part somebody has to get
# right to use this at all.** A document that listed the routes and left out
# the token, the loopback bind or the Host check would be a document that
# teaches people to write a client that cannot connect.
for must in "Bearer" "loopback" "Host header" "rebinding" "unattended"; do
  if ! grep -qi -- "$must" "$DOC"; then
    echo "  ✗ local-api: $DOC does not mention \"$must\", which a caller has to know about"
    fail=1
  fi
done

if [ "$fail" != "0" ]; then
  echo ""
  echo "  This API is the one surface in Esper written for somebody else's"
  echo "  program. Both lists have to say the same thing."
  exit 1
fi
printf '  ✓ local-api: %s routes, each documented and each registered, and the posture is stated\n' "$count"
