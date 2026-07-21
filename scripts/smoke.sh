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

for b in esper esper-engine esper-broker; do
  if [ ! -x "$BIN/$b" ]; then
    echo "  ✗ $BIN/$b missing — run 'make build' first"; exit 1
  fi
done

expected_version=$(cat VERSION)

go_version=$("$BIN/esper" version | head -1 | awk '{print $2}')
eng_version=$("$BIN/esper-engine" version | head -1 | awk '{print $2}')
brk_version=$("$BIN/esper-broker" version | head -1 | awk '{print $2}')

check "esper reports the VERSION file"        "$go_version"  "$expected_version"
check "esper-engine reports the VERSION file" "$eng_version" "$expected_version"
check "esper-broker reports the VERSION file" "$brk_version" "$expected_version"

go_proto=$("$BIN/esper" version | awk '/protocol/{print $2}')
eng_proto=$("$BIN/esper-engine" version | awk '/protocol/{print $2}')
brk_proto=$("$BIN/esper-broker" version | awk '/protocol/{print $2}')

check "engine agrees with the control plane on the protocol version" "$eng_proto" "$go_proto"
check "broker agrees with the control plane on the protocol version" "$brk_proto" "$go_proto"

# Exit codes are part of the CLI contract: scripts branch on them.
"$BIN/esper" version >/dev/null 2>&1; check "esper version exits 0" "$?" "0"
set +e; "$BIN/esper" definitely-not-a-command >/dev/null 2>&1; rc=$?; set -e
check "esper rejects an unknown command with exit 1" "$rc" "1"

# Esper operates no infrastructure, so a shipped binary must contain nothing
# it could phone home to. This check is that claim's proof, and the reason the
# claim is checkable rather than merely stated.
#
# What counts as an endpoint is deliberately narrow, and has been narrowed twice
# after false positives — because a check that cries wolf gets switched off,
# which is worse than not having one:
#
#   1. Bare domains do not count. Go embeds every module path and source
#      filename in its build info, so `github.com/elythi0n/esper` is in every
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

for b in esper esper-engine esper-broker; do
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
    echo "      Esper ships no default endpoint. If this is a dependency's,"
    echo "      that dependency needs justifying or replacing. If it is a"
    echo "      documentation link in a diagnostic string, add it to doc_hosts"
    echo "      above WITH A REASON."
    fail=1
  else
    printf '  ✓ %s has no URL and no routable IP literal\n' "$b"
  fi
done

# Terminal mode's whole claim is that it works on a machine with no screen.
# Checked rather than assumed: a dependency that pulled in a display library
# would still build and still pass every test, and would fail on the first
# server someone tried it on.
echo "  — checking the engine needs no display"
if ldd "$BIN/esper-engine" 2>/dev/null \
   | grep -qiE 'libX11|libwayland|libGL|libEGL|libdrm|libgbm|libxcb'; then
  printf '  ✗ esper-engine links a display library:\n'
  ldd "$BIN/esper-engine" | grep -iE 'libX11|libwayland|libGL|libEGL|libdrm|libgbm|libxcb' | sed 's/^/      /'
  echo "      Terminal mode must run on a headless server. A display library"
  echo "      here means it will not."
  fail=1
else
  printf '  ✓ esper-engine links no display library\n'
fi

# And it must actually run with the display environment stripped.
if env -u DISPLAY -u WAYLAND_DISPLAY -u XDG_SESSION_TYPE -u XAUTHORITY \
     "$BIN/esper-engine" version >/dev/null 2>&1; then
  printf '  ✓ esper-engine runs with no display environment\n'
else
  printf '  ✗ esper-engine failed with the display environment stripped\n'
  fail=1
fi

# Naming a backend that cannot open must fail, not quietly hand back a test
# pattern. Somebody who said which screen they wanted and got a moving
# gradient instead would have no way to tell that capture never worked.
echo "  — checking a named backend is not silently substituted"
if env -u DISPLAY -u WAYLAND_DISPLAY -u XAUTHORITY \
     "$BIN/esper-engine" loopback --backend x11 --seconds 1 >/dev/null 2>&1; then
  printf '  ✗ --backend x11 succeeded with no display, so it fell back silently\n'
  fail=1
else
  printf '  ✓ --backend x11 fails rather than falling back to synthetic\n'
fi

# And the fallback itself has to work when nothing was named.
if out=$(env -u DISPLAY -u WAYLAND_DISPLAY -u XAUTHORITY \
     "$BIN/esper-engine" loopback --backend auto --size 320x240 --seconds 1 2>&1); then
  if printf '%s' "$out" | grep -q 'synthetic: in use'; then
    printf '  ✓ auto falls back to synthetic and says why\n'
  else
    printf '  ✗ auto did not report which backend it chose\n'
    fail=1
  fi
else
  printf '  ✗ --backend auto failed with no display\n'
  fail=1
fi

# A host with nothing paired is refused rather than started. A listening
# endpoint that can accept nobody has exactly one possible outcome, and it is a
# confusing failure at the far end.
echo "  — checking a host with no paired devices refuses to start"
empty=$(mktemp -d)
if out=$(ESPER_HOME="$empty" "$BIN/esper-engine" host --listen 127.0.0.1:0 2>&1); then
  printf '  ✗ host started with no paired devices\n'
  fail=1
else
  if printf '%s' "$out" | grep -q 'pair'; then
    printf '  ✓ host refuses to start and says to pair first\n'
  else
    printf '  ✗ host refused but did not say what to do: %s\n' "$out"
    fail=1
  fi
fi
if out=$(ESPER_HOME="$empty" "$BIN/esper-engine" connect 127.0.0.1:1 2>&1); then
  printf '  ✗ connect started with no paired devices\n'
  fail=1
else
  printf '  ✓ connect refuses without a paired host\n'
fi
rm -rf "$empty"

# The whole media path, headless. This is the phase's exit criterion, and a
# smoke check rather than a unit test because it is the *binary* that has to
# work — a pipeline that runs in a test and not from the command line is not
# something anyone can use.
echo "  — running the media pipeline end to end, with no display"
if out=$(env -u DISPLAY -u WAYLAND_DISPLAY -u XAUTHORITY \
     "$BIN/esper-engine" loopback --size 320x240 --fps 15 --seconds 1 2>&1); then
  if printf '%s' "$out" | grep -q 'luma PSNR'; then
    printf '  ✓ loopback captured, converted, encoded and decoded\n'
  else
    printf '  ✗ loopback produced no decoded picture\n'
    printf '%s\n' "$out" | sed 's/^/      /'
    fail=1
  fi
else
  printf '  ✗ loopback failed\n'
  printf '%s\n' "$out" | sed 's/^/      /'
  fail=1
fi

# Terminal mode, through the binaries, on a machine with no display. This is
# Phase 2's exit criterion and it is a *smoke* check for the reason the media
# one is: every terminal test in the suite calls the service directly, and for
# two phases nothing else did — the feature was complete and no binary could
# open a shell. A test that starts below `main` cannot notice that.
echo "  — opening a shell through the built binaries, with no display"
term_a=$(mktemp -d); term_b=$(mktemp -d)
cleanup_terminal() {
  # `|| true` on the kill: some of these have already exited by the time this
  # runs, and under `set -e` a failed kill at the end of an `&&` list ends the
  # script — which showed up as a smoke test that failed after its last check
  # had passed.
  for pid in "${host_pid:-}" "${via_host:-}" "${broker_pid:-}" "${invite_pid:-}" \
             "${named_host:-}"; do
    if [ -n "$pid" ]; then kill "$pid" 2>/dev/null || true; fi
  done
  rm -rf "$term_a" "$term_b"
}
trap cleanup_terminal EXIT

# Pair the two homes. The listener prints the port it took and the code it
# generated, so nothing here has to pick a port and risk a collision.
ESPER_HOME="$term_a" timeout 60 "$BIN/esper-engine" pair --listen 127.0.0.1:0 \
  > "$term_a/pair.log" 2>&1 &
pair_pid=$!
code=""; port=""
for _ in $(seq 1 100); do
  # `|| true` on every one of these: under `set -e` with `pipefail`, a grep
  # that has not found its line *yet* is a failed pipeline, and would end the
  # script rather than round the loop one more time.
  code=$( { grep -oE '\-\-code [A-Z0-9-]+' "$term_a/pair.log" 2>/dev/null || true; } | head -1 | awk '{print $2}')
  port=$( { grep -oE 'pair [^ ]*:[0-9]+ --code' "$term_a/pair.log" 2>/dev/null || true; } | head -1 | sed 's/.*://;s/ --code//')
  [ -n "$code" ] && [ -n "$port" ] && break
  sleep 0.1
done
if [ -z "$code" ] || [ -z "$port" ]; then
  printf '  ✗ pairing never printed a code and a port\n'
  sed 's/^/      /' "$term_a/pair.log"
  fail=1
else
  ESPER_HOME="$term_b" timeout 60 "$BIN/esper-engine" pair "127.0.0.1:$port" --code "$code" \
    > "$term_b/pair.log" 2>&1 || true
  wait "$pair_pid" || true

  # Terminal is deliberately **not** a default permission, so it has to be
  # granted — which is the other half of what this checks.
  fp=$( { ESPER_HOME="$term_a" "$BIN/esper-engine" devices | grep -oE '^  [0-9a-f]{8,}' || true; } | head -1 | tr -d ' ')
  if [ -z "$fp" ]; then
    printf '  ✗ pairing left no device in the list\n'
    sed 's/^/      /' "$term_a/pair.log" "$term_b/pair.log"
    fail=1
  else
    ESPER_HOME="$term_a" "$BIN/esper-engine" grant "$fp" terminal >/dev/null

    env -u DISPLAY -u WAYLAND_DISPLAY -u XAUTHORITY ESPER_HOME="$term_a" \
      timeout 120 "$BIN/esper-engine" host --listen 127.0.0.1:0 --no-mdns \
      > "$term_a/host.log" 2>&1 &
    host_pid=$!
    host_addr=""
    for _ in $(seq 1 100); do
      host_addr=$( { awk '/listening/{print $2}' "$term_a/host.log" 2>/dev/null || true; } | head -1)
      [ -n "$host_addr" ] && break
      sleep 0.1
    done

    if [ -z "$host_addr" ]; then
      printf '  ✗ the host never said what it was listening on\n'
      sed 's/^/      /' "$term_a/host.log"
      fail=1
    else
      out=$(printf 'echo smoke-shell-$((6*7))\nexit 3\n' | \
        env -u DISPLAY -u WAYLAND_DISPLAY -u XAUTHORITY ESPER_HOME="$term_b" TERM=dumb \
        timeout 60 "$BIN/esper-engine" terminal "$host_addr" 2>&1 || true)
      # The command ran on the far side: the shell expanded the arithmetic,
      # so the marker proves a real shell rather than an echo of the input.
      if printf '%s' "$out" | grep -q 'smoke-shell-42'; then
        printf '  ✓ a shell ran a command on the host and sent its output back\n'
      else
        printf '  ✗ no output came back from the remote shell\n'
        printf '%s\n' "$out" | sed 's/^/      /'
        fail=1
      fi
      # And the exit status came back, which is most of what scripting a
      # server needs.
      if printf '%s' "$out" | grep -q 'the shell exited: code 3'; then
        printf '  ✓ the shell'"'"'s exit status reached the controller\n'
      else
        printf '  ✗ the exit status did not arrive\n'
        printf '%s\n' "$out" | sed 's/^/      /'
        fail=1
      fi

      # **A host reached through a broker, with no address for it.** Tier 3
      # of the connectivity ladder, as far as one machine can prove it: the
      # rendezvous, the pinning, the introduction and the direct path are all
      # exercised; the hole punch is not, because there is no router between
      # two sockets on one host.
      echo "  — reaching a host through a broker, given no address for it"
      timeout 120 "$BIN/esper-broker" serve --listen 127.0.0.1:0 \
        --identity "$term_a/broker.key" > "$term_a/broker.log" 2>&1 &
      broker_pid=$!
      broker_addr=""
      broker_key=""
      for _ in $(seq 1 100); do
        broker_addr=$( { awk '/listening/{print $2}' "$term_a/broker.log" 2>/dev/null || true; } | head -1)
        broker_key=$( { awk '/^  key /{print $2}' "$term_a/broker.log" 2>/dev/null || true; } | head -1)
        [ -n "$broker_addr" ] && [ -n "$broker_key" ] && break
        sleep 0.1
      done

      if [ -z "$broker_addr" ] || [ -z "$broker_key" ]; then
        printf '  ✗ the broker never said where it was listening\n'
        sed 's/^/      /' "$term_a/broker.log"
        fail=1
      else
        env -u DISPLAY -u WAYLAND_DISPLAY -u XAUTHORITY ESPER_HOME="$term_a" \
          timeout 120 "$BIN/esper-engine" host --listen 127.0.0.1:0 --no-mdns \
          --via "$broker_addr" --broker-key "$broker_key" \
          > "$term_a/via.log" 2>&1 &
        via_host=$!
        waited=""
        for _ in $(seq 1 100); do
          if grep -q 'waiting to be found' "$term_a/via.log" 2>/dev/null; then
            waited=yes
            break
          fi
          sleep 0.1
        done

        if [ -z "$waited" ]; then
          printf '  ✗ the host never reached the broker\n'
          sed 's/^/      /' "$term_a/via.log"
          fail=1
        else
          # **No address for the host anywhere on this command line.**
          out=$(env -u DISPLAY -u WAYLAND_DISPLAY -u XAUTHORITY ESPER_HOME="$term_b" \
            timeout 60 "$BIN/esper-engine" connect --via "$broker_addr" \
            --broker-key "$broker_key" --seconds 2 2>&1 || true)
          # The tier is named in the output, so the assertion is about which
          # rung was used rather than about a phrase that happened to be
          # printed on the way.
          if printf '%s' "$out" | grep -q 'an introduction'; then
            printf '  ✓ a controller with no address for the host was introduced to it\n'
          else
            printf '  ✗ the broker did not get the two of them together\n'
            printf '%s\n' "$out" | sed 's/^/      /'
            fail=1
          fi
          if printf '%s' "$out" | grep -qE '[1-9][0-9]* shown'; then
            printf '  ✓ and the session that followed carried pictures\n'
          else
            printf '  ✗ the session through the broker carried nothing\n'
            printf '%s\n' "$out" | sed 's/^/      /'
            fail=1
          fi
        fi
        kill "$via_host" 2>/dev/null || true

        # **The support flow: one link, one paste.** The case the design says
        # genuinely needs a broker — somebody with no LAN, no overlay and no
        # patience — and the one where every step removed is a step they will
        # not get wrong. Two homes that have never met, and no address for
        # either of them anywhere on these command lines.
        echo "  — pairing two strangers with one link through the broker"
        inv_a=$(mktemp -d); inv_b=$(mktemp -d)
        ESPER_HOME="$inv_a" timeout 120 "$BIN/esper-engine" invite \
          --via "$broker_addr" --broker-key "$broker_key" --for "a stranger" --expires 5m \
          > "$inv_a/invite.log" 2>&1 &
        invite_pid=$!
        link=""
        for _ in $(seq 1 100); do
          link=$( { grep -oE 'esper://join/[A-Za-z0-9_-]+' "$inv_a/invite.log" 2>/dev/null || true; } | head -1)
          [ -n "$link" ] && break
          sleep 0.1
        done

        if [ -z "$link" ]; then
          printf '  ✗ invite printed no link\n'
          sed 's/^/      /' "$inv_a/invite.log"
          fail=1
        else
          out=$(ESPER_HOME="$inv_b" timeout 60 "$BIN/esper-engine" join "$link" 2>&1 || true)
          wait "$invite_pid" || true
          if printf '%s' "$out" | grep -q 'paired with'; then
            printf '  ✓ somebody who was sent a link paired through the broker\n'
          else
            printf '  ✗ the invitation was not taken up\n'
            printf '%s\n' "$out" | sed 's/^/      /'
            sed 's/^/      /' "$inv_a/invite.log"
            fail=1
          fi
          # And both ends recorded it, which is what makes the pairing real
          # rather than a message on a screen.
          for home in "$inv_a" "$inv_b"; do
            if ESPER_HOME="$home" "$BIN/esper-engine" devices | grep -q 'may  view, control'; then
              :
            else
              printf '  ✗ one side did not record the pairing\n'
              ESPER_HOME="$home" "$BIN/esper-engine" devices | sed 's/^/      /'
              fail=1
            fi
          done
          printf '  ✓ and both ends recorded it\n'
        fi
        rm -rf "$inv_a" "$inv_b"
        invite_pid=""
      fi
      kill "$broker_pid" 2>/dev/null || true

      # **A machine reached by name, with no address anywhere.** Where a
      # machine *is* changes; which machine it is does not. The address it
      # answered on the first time is remembered, and that is what makes the
      # second connection instant with no discovery and no broker.
      echo "  — connecting by name, using where it was last time"
      env -u DISPLAY -u WAYLAND_DISPLAY -u XAUTHORITY ESPER_HOME="$term_a" \
        timeout 120 "$BIN/esper-engine" host --listen 127.0.0.1:0 --no-mdns \
        > "$term_a/named.log" 2>&1 &
      named_host=$!
      named_addr=""
      for _ in $(seq 1 100); do
        named_addr=$( { awk '/listening/{print $2}' "$term_a/named.log" 2>/dev/null || true; } | head -1)
        [ -n "$named_addr" ] && break
        sleep 0.1
      done

      if [ -z "$named_addr" ]; then
        printf '  ✗ the host never said where it was listening\n'
        fail=1
      else
        # The first connection is by address, which is what teaches this
        # machine where that one lives.
        env -u DISPLAY -u WAYLAND_DISPLAY -u XAUTHORITY ESPER_HOME="$term_b" \
          timeout 60 "$BIN/esper-engine" connect "$named_addr" --seconds 1 --no-mdns \
          > "$term_b/first.log" 2>&1 || true
        # The device's name, as this machine recorded it at pairing.
        peer_name=$( { ESPER_HOME="$term_b" "$BIN/esper-engine" devices | grep -oE '^  [0-9a-f]{16}  .+' || true; } \
          | head -1 | sed 's/^  [0-9a-f]*  //')
        out=$(env -u DISPLAY -u WAYLAND_DISPLAY -u XAUTHORITY ESPER_HOME="$term_b" \
          timeout 60 "$BIN/esper-engine" connect "$peer_name" --seconds 1 --no-mdns 2>&1 || true)
        if printf '%s' "$out" | grep -q 'where it was last time'; then
          printf '  ✓ a machine named rather than addressed was found where it was last time\n'
        else
          printf '  ✗ connecting by name did not use the remembered address\n'
          printf '%s\n' "$out" | sed 's/^/      /'
          fail=1
        fi
      fi
      kill "$named_host" 2>/dev/null || true
      named_host=""

      # **A revocation has to reach a session that is already open.** One that
      # waits for the revoked device to choose to leave is a request, not a
      # revocation — and this is the only test that can catch it, because the
      # two halves are in different processes and the only thing they share is
      # the device store on disk.
      echo "  — revoking a device that is connected right now"
      { printf 'echo holding\n'; sleep 15; } | \
        env -u DISPLAY -u WAYLAND_DISPLAY -u XAUTHORITY ESPER_HOME="$term_b" TERM=dumb \
        timeout 60 "$BIN/esper-engine" terminal "$host_addr" > "$term_b/revoked.log" 2>&1 &
      client=$!
      sleep 3
      ESPER_HOME="$term_a" "$BIN/esper-engine" revoke "$fp" terminal >/dev/null
      wait "$client" || true
      if grep -q 'has been revoked' "$term_b/revoked.log"; then
        printf '  ✓ a live session ended, and the controller was told why\n'
      else
        printf '  ✗ revoking a permission did not reach the open session\n'
        sed 's/^/      /' "$term_b/revoked.log"
        fail=1
      fi

      # Unpairing goes further: that key may not even authenticate. `AnyOf` is
      # a list taken when the host started, so this is the check that the host
      # is reading the store rather than a copy of it from startup.
      echo "  — unpairing a device while the host runs"
      ESPER_HOME="$term_a" "$BIN/esper-engine" forget "$fp" >/dev/null
      refused=""
      for _ in $(seq 1 8); do
        sleep 1
        out=$(env -u DISPLAY -u WAYLAND_DISPLAY -u XAUTHORITY ESPER_HOME="$term_b" \
          timeout 30 "$BIN/esper-engine" connect "$host_addr" --seconds 2 2>&1 || true)
        if printf '%s' "$out" | grep -q 'does not accept this device'; then
          refused=yes
          break
        fi
      done
      if [ -n "$refused" ]; then
        printf '  ✓ a forgotten device cannot connect, and is told to pair again\n'
      else
        printf '  ✗ a forgotten device was still able to open a session\n'
        printf '%s\n' "$out" | sed 's/^/      /'
        fail=1
      fi
    fi
  fi
fi
cleanup_terminal
trap - EXIT
host_pid=""

if [ "$fail" -ne 0 ]; then echo; echo "  smoke test failed"; exit 1; fi
echo "  ✓ smoke test passed"
