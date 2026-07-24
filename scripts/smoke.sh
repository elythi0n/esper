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

# **The front door.** `esper` is the program a person has; the engine is an
# implementation detail with its own binary. A verb the engine owns must work
# when typed at `esper`, with the same output and the same exit code — which
# is what handing the process over rather than wrapping it buys.
echo "  — typing a verb at the front door"
direct=$("$BIN/esper-engine" devices 2>&1 || true)
front=$("$BIN/esper" devices 2>&1 || true)
if [ "$direct" = "$front" ]; then
  printf '  ✓ `esper devices` is `esper-engine devices`, output for output\n'
else
  printf '  ✗ the front door and the engine disagree\n'
  printf '%s\n' "$front" | sed 's/^/      /'
  fail=1
fi
# A word neither program has is refused by the front door rather than passed
# through into somebody else's error message.
set +e
"$BIN/esper" definitely-not-a-verb >/dev/null 2>&1; rc=$?
set -e
check "esper refuses a verb it does not have" "$rc" "1"

# **The control plane asking the engine what this machine can do.**
#
# The seam between the two binaries carried a handshake and nothing else until
# now. This is the first command that crosses it, and the answer is *probed* —
# the encoder really encodes a frame — so a report that says the encoder works
# is a report that watched it work.
echo "  — asking the engine what this machine can do"
out=$(env -u DISPLAY -u WAYLAND_DISPLAY -u XAUTHORITY "$BIN/esper" doctor 2>/dev/null || true)
# The assertion is on the **evidence** — the size of the keyframe it produced —
# not on the tick. A version that declared the encoder working without running
# it would print a tick, and a check that accepted one would agree with it.
if printf '%s' "$out" | grep -qE 'encoder  ✓.*[0-9]+ bytes for one keyframe'; then
  printf '  ✓ doctor probed the encoder by encoding a frame\n'
else
  printf '  ✗ doctor did not report a working encoder\n'
  printf '%s\n' "$out" | sed 's/^/      /'
  fail=1
fi
# With no display there is no screen to capture, and the honest report says so
# rather than calling the synthetic fallback a screen.
if printf '%s' "$out" | grep -q 'screen   ✗'; then
  printf '  ✓ and reported no screen where there is none, rather than a test pattern\n'
else
  printf '  ✗ doctor called something a screen with no display present\n'
  printf '%s\n' "$out" | sed 's/^/      /'
  fail=1
fi
# The report goes to stdout and the engine's own output to stderr with its
# origin on it, so `esper doctor > file` is a file of report.
noise=$(env -u DISPLAY -u WAYLAND_DISPLAY -u XAUTHORITY "$BIN/esper" doctor 2>/dev/null | grep -c 'OpenH264' || true)
if [ "$noise" = "0" ]; then
  printf '  ✓ the report on stdout has none of the engine'"'"'s own output in it\n'
else
  printf '  ✗ the engine'"'"'s output is mixed into the report\n'
  fail=1
fi

# The shipped unit files, checked by systemd rather than by reading them.
#
# A unit that has stopped parsing, or a directive in the wrong section, is
# silent: systemd ignores what it does not understand where it does not belong.
# That is how the restart rate limiting was absent from the engine's unit for a
# while — it was in `[Service]`, where those keys mean nothing.
#
# Skipped where there is no systemd, which is most developer machines that are
# not Linux, and which is why this is a check and not a gate.
if command -v systemd-analyze >/dev/null 2>&1; then
  echo "  — checking the shipped unit files parse"
  for unit in packaging/systemd/*.service; do
    # The verifier also resolves ExecStart, which is not installed here. That
    # one line is expected and is filtered rather than silenced wholesale.
    out=$( { systemd-analyze verify "$unit" 2>&1 || true; } \
      | grep -F "$unit" \
      | grep -v 'is not executable' || true)
    if [ -z "$out" ]; then
      printf '  ✓ %s parses\n' "$(basename "$unit")"
    else
      printf '  ✗ %s has something systemd would ignore\n' "$(basename "$unit")"
      printf '%s\n' "$out" | sed 's/^/      /'
      fail=1
    fi
  done
fi

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
             "${named_host:-}" "${service_pid:-}" "${forward_pid:-}" \
             "${denied_pid:-}" "${private_host:-}" "${invited_host:-}" \
             "${announcing:-}"; do
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

          # **An invitation is one session, not a key for ever.** Somebody who
          # pasted a link to get help with their printer should not still be
          # reachable by that helper next year, and "remember to unpair them"
          # is a thing people forget while they are relieved the printer
          # works. So the pairing is spent by the session it was made for.
          if ESPER_HOME="$inv_a" "$BIN/esper-engine" devices | grep -q 'one session'; then
            printf '  ✓ the pairing says it lasts one session\n'
          else
            printf '  ✗ an invited pairing looks like any other\n'
            ESPER_HOME="$inv_a" "$BIN/esper-engine" devices | sed 's/^/      /'
            fail=1
          fi

          # Taken before the session: the guest as it was when it still had a
          # pairing nobody had used.
          inv_c=$(mktemp -d)
          cp -a "$inv_b/." "$inv_c/"

          env -u DISPLAY -u WAYLAND_DISPLAY -u XAUTHORITY ESPER_HOME="$inv_a" \
            timeout 120 "$BIN/esper-engine" host --listen 127.0.0.1:0 --no-mdns \
            > "$inv_a/host.log" 2>&1 &
          invited_host=$!
          invited_addr=""
          for _ in $(seq 1 100); do
            invited_addr=$( { awk '/listening/{print $2}' "$inv_a/host.log" 2>/dev/null || true; } | head -1)
            [ -n "$invited_addr" ] && break
            sleep 0.1
          done

          if [ -z "$invited_addr" ]; then
            printf '  ✗ the invited host never said what it was listening on\n'
            sed 's/^/      /' "$inv_a/host.log"
            fail=1
          else
            out=$(env -u DISPLAY -u WAYLAND_DISPLAY -u XAUTHORITY ESPER_HOME="$inv_b" \
              timeout 60 "$BIN/esper-engine" connect "$invited_addr" --seconds 2 2>&1 || true)
            if printf '%s' "$out" | grep -q 'granted'; then
              printf '  ✓ the one session an invitation buys works\n'
            else
              printf '  ✗ the invited device could not open its one session\n'
              printf '%s\n' "$out" | sed 's/^/      /'
              fail=1
            fi

            # **And the second is refused by the host**, which is the half
            # that matters. The guest's own copy of the pairing is spent too,
            # so asking again from `$inv_b` would be refused by this side
            # before a packet left — a real check, and not this one.
            #
            # So the retry comes from a copy of the guest's home taken before
            # the session: the same identity and the same device list, with no
            # local knowledge that the pairing has been used. That is exactly
            # what somebody keeping a copy of their `~/.esper` would have.
            out=$(env -u DISPLAY -u WAYLAND_DISPLAY -u XAUTHORITY ESPER_HOME="$inv_c" \
              timeout 60 "$BIN/esper-engine" connect "$invited_addr" --seconds 2 2>&1 || true)
            if printf '%s' "$out" | grep -q 'does not accept this device'; then
              printf '  ✓ and a second is refused by the host, not by the guest\n'
            else
              printf '  ✗ an invited device opened a second session\n'
              printf '%s\n' "$out" | sed 's/^/      /'
              fail=1
            fi

            # The host tidied the record away rather than leaving a spent key
            # in its list.
            if ESPER_HOME="$inv_a" "$BIN/esper-engine" devices | grep -q 'no paired devices'; then
              printf '  ✓ and the host is no longer paired with them at all\n'
            else
              printf '  ✗ the spent pairing is still in the host'"'"'s list\n'
              ESPER_HOME="$inv_a" "$BIN/esper-engine" devices | sed 's/^/      /'
              fail=1
            fi
          fi
          kill "$invited_host" 2>/dev/null || true
          invited_host=""
        fi
        rm -rf "$inv_a" "$inv_b" "${inv_c:-/nonexistent}"
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

      # Placed here, after the ladder's own checks, deliberately: reaching a
      # host **remembers where it answered**, and the broker check below needs
      # the rung above the broker to have nothing to remember. A step that
      # quietly changes what a later step is testing is worse than a slow one.
      # **Announcing and finding a machine, through the built binaries.**
      # Discovery has unit tests, an interoperability test against an
      # independent DNS implementation, and — until now — nothing that ran the
      # two halves as two processes. That is the gap this suite exists for.
      #
      # Announced on **loopback only**, by naming the interface: the check
      # must not put this machine on somebody's actual network every time
      # somebody runs the test suite, and naming an interface is also the
      # override that would otherwise go untested.
      echo "  — announcing this machine and finding it again"
      wire_ip=$( { ip route get 1.1.1.1 2>/dev/null || true; } | sed -n 's/.* src \([^ ]*\).*/\1/p' | head -1)
      if [ -z "$wire_ip" ]; then
        printf '  · no address off loopback, so there is nothing to announce\n'
      else
        printf 'discovery.mdns_interfaces = "lo"\n' > "$term_a/config.toml"
        printf 'discovery.mdns_interfaces = "lo"\n' > "$term_b/config.toml"
        env -u DISPLAY -u WAYLAND_DISPLAY -u XAUTHORITY ESPER_HOME="$term_a" \
          timeout 120 "$BIN/esper-engine" host --listen "$wire_ip:0" \
          > "$term_a/announce.log" 2>&1 &
        announcing=$!
        said=""
        for _ in $(seq 1 100); do
          if grep -q 'announcing on this network' "$term_a/announce.log" 2>/dev/null; then
            said=yes
            break
          fi
          sleep 0.1
        done

        if [ -z "$said" ]; then
          printf '  ✗ the host never announced itself\n'
          sed 's/^/      /' "$term_a/announce.log"
          fail=1
        else
          # It says which wire it went out on, which is the answer to "why
          # can that machine not see me" on a host with three of them.
          if grep -q 'through 127.0.0.1' "$term_a/announce.log"; then
            printf '  ✓ the host says which interface it announced through\n'
          else
            printf '  ✗ the host did not say where it announced\n'
            sed 's/^/      /' "$term_a/announce.log"
            fail=1
          fi
          out=$(ESPER_HOME="$term_b" timeout 30 "$BIN/esper-engine" discover 2>&1 || true)
          if printf '%s' "$out" | grep -q 'ready to connect'; then
            printf '  ✓ a second engine found it by asking the network\n'
          else
            printf '  ✗ nothing was found on the network\n'
            printf '%s\n' "$out" | sed 's/^/      /'
            sed 's/^/      /' "$term_a/announce.log"
            fail=1
          fi
        fi
        kill "$announcing" 2>/dev/null || true
        announcing=""
        rm -f "$term_a/config.toml" "$term_b/config.toml"
      fi

      # **A path the person already trusts, and Esper saying so.** Tier 1
      # of the ladder: a machine reached over WireGuard, a tailnet or any
      # other overlay is on a network its owner already runs, so there is no
      # hole to punch and nothing for a broker to introduce.
      #
      # No CI runner has a tunnel, and this must be checkable anywhere — so
      # the check names the machine's **own** interface through
      # `connectivity.overlay_interfaces`, which is the override a person uses
      # when autodetection has not heard of their VPN client. What is being
      # tested is everything except the name: the routing question, the
      # classification, the rung and the word the session reports.
      echo "  — reaching a host over a network the user already runs"
      wire_if=$( { ip route get 1.1.1.1 2>/dev/null || true; } | sed -n 's/.* dev \([^ ]*\) .*/\1/p' | head -1)
      wire_ip=$( { ip route get 1.1.1.1 2>/dev/null || true; } | sed -n 's/.* src \([^ ]*\).*/\1/p' | head -1)
      if [ -z "$wire_if" ] || [ -z "$wire_ip" ]; then
        printf '  · no route off this machine, so there is no path to classify\n'
      else
        printf 'connectivity.overlay_interfaces = "%s"\n' "$wire_if" > "$term_b/config.toml"
        env -u DISPLAY -u WAYLAND_DISPLAY -u XAUTHORITY ESPER_HOME="$term_a" \
          timeout 120 "$BIN/esper-engine" host --listen "$wire_ip:0" --no-mdns \
          > "$term_a/private.log" 2>&1 &
        private_host=$!
        private_addr=""
        for _ in $(seq 1 100); do
          private_addr=$( { awk '/listening/{print $2}' "$term_a/private.log" 2>/dev/null || true; } | head -1)
          [ -n "$private_addr" ] && break
          sleep 0.1
        done

        if [ -z "$private_addr" ]; then
          printf '  ✗ the host never said what it was listening on\n'
          sed 's/^/      /' "$term_a/private.log"
          fail=1
        else
          out=$(env -u DISPLAY -u WAYLAND_DISPLAY -u XAUTHORITY ESPER_HOME="$term_b" \
            timeout 60 "$BIN/esper-engine" connect "$private_addr" --seconds 2 --no-mdns 2>&1 || true)
          if printf '%s' "$out" | grep -q 'a private network you already run'; then
            printf '  ✓ a path over a named overlay interface is reported as private\n'
          else
            printf '  ✗ a path over an overlay was not reported as private\n'
            printf '%s\n' "$out" | sed 's/^/      /'
            fail=1
          fi
          # And it is remembered as one: the address that works from anywhere
          # must survive the next session on the LAN, which is why it has a
          # slot of its own.
          if grep -q 'overlay_address' "$term_b/devices.json" 2>/dev/null; then
            printf '  ✓ and remembered in its own slot, not overwriting the last address\n'
          else
            printf '  ✗ the private address was not remembered\n'
            fail=1
          fi

          # **Being on somebody's private network is not authorisation.** The
          # claim that a private path skips no crypto and no pairing is worth
          # nothing as a comment, so here is a device the host has never
          # paired with, on the path Esper has just called private.
          #
          # It knows *where* the host is — the device list is copied — and it
          # has an identity of its own, generated on first use. That is
          # exactly the shape of the attack the tier could have invited: an
          # address on a trusted network, and no pairing behind it.
          term_c=$(mktemp -d)
          cp "$term_b/devices.json" "$term_c/devices.json"
          cp "$term_b/config.toml" "$term_c/config.toml"
          out=$(env -u DISPLAY -u WAYLAND_DISPLAY -u XAUTHORITY ESPER_HOME="$term_c" \
            timeout 60 "$BIN/esper-engine" connect "$private_addr" --seconds 2 --no-mdns 2>&1 || true)
          if printf '%s' "$out" | grep -q 'does not accept this device'; then
            printf '  ✓ an unpaired device is refused on a private path like any other\n'
          else
            printf '  ✗ an unpaired device was not refused on a private path\n'
            printf '%s\n' "$out" | sed 's/^/      /'
            fail=1
          fi
          rm -rf "$term_c"
        fi
        kill "$private_host" 2>/dev/null || true
        rm -f "$term_b/config.toml"
      fi

      # **A port forwarded to a service the network cannot see.** The other
      # half of terminal mode: a headless server's useful face is its
      # services, and the whole point is that the service stays bound to
      # loopback while the laptop reaches it anyway.
      #
      # Through the built binaries, for the reason the shell check above
      # exists: the unit tests all start below `main`.
      echo "  — forwarding a port to a service bound to the host's loopback"
      if ! command -v python3 >/dev/null 2>&1; then
        printf '  · python3 is not here, so the forwarding checks are skipped\n'
      else
        python3 - > "$term_a/service.log" 2>&1 <<'PYSVC' &
import socketserver

class Handler(socketserver.BaseRequestHandler):
    def handle(self):
        self.request.recv(1024)
        self.request.sendall(b"smoke-forward-42\n")

server = socketserver.ThreadingTCPServer(("127.0.0.1", 0), Handler)
print(server.server_address[1], flush=True)
server.serve_forever()
PYSVC
        service_pid=$!
        svc_port=""
        for _ in $(seq 1 100); do
          svc_port=$( { head -1 "$term_a/service.log" 2>/dev/null || true; } | tr -dc '0-9')
          [ -n "$svc_port" ] && break
          sleep 0.1
        done

        # Two free local ports, taken and released: the forwarder binds them a
        # moment later.
        lport=$(python3 -c 'import socket
s = socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1]); s.close()')
        lport2=$(python3 -c 'import socket
s = socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1]); s.close()')

        if [ -z "$svc_port" ]; then
          printf '  ✗ the test service never said what port it took\n'
          sed 's/^/      /' "$term_a/service.log"
          fail=1
        else
          # **Refused before a listener exists.** `forward` is granted to
          # nobody by default, and this device holds `terminal` and not this.
          out=$(env ESPER_HOME="$term_b" timeout 30 "$BIN/esper-engine" \
            forward "$lport:127.0.0.1:$svc_port" "$host_addr" 2>&1 || true)
          if printf '%s' "$out" | grep -q 'needs the forward permission'; then
            printf '  ✓ a device without the forward permission is refused\n'
          else
            printf '  ✗ a device without the forward permission was not refused\n'
            printf '%s\n' "$out" | sed 's/^/      /'
            fail=1
          fi

          ESPER_HOME="$term_a" "$BIN/esper-engine" grant "$fp" forward >/dev/null

          env ESPER_HOME="$term_b" timeout 60 "$BIN/esper-engine" \
            forward "$lport:127.0.0.1:$svc_port" "$host_addr" \
            > "$term_b/forward.log" 2>&1 &
          forward_pid=$!
          for _ in $(seq 1 100); do
            grep -q 'ctrl-c to stop' "$term_b/forward.log" 2>/dev/null && break
            sleep 0.1
          done

          got=$(python3 -c 'import socket, sys
c = socket.create_connection(("127.0.0.1", int(sys.argv[1])), 10)
c.sendall(b"hello\n")
c.shutdown(socket.SHUT_WR)
print(c.recv(100).decode().strip())' "$lport" 2>&1 || true)
          if [ "$got" = "smoke-forward-42" ]; then
            printf '  ✓ a connection to the local port reached the host'"'"'s loopback service\n'
          else
            printf '  ✗ nothing came back through the forward\n'
            printf '%s\n' "$got" | sed 's/^/      /'
            sed 's/^/      /' "$term_b/forward.log"
            fail=1
          fi

          # The target is in the audit log, and nothing that went through it.
          if grep -q '"event":"forward_opened"' "$term_a/sessions.log" 2>/dev/null; then
            printf '  ✓ the forward is in the host'"'"'s audit log, by target\n'
          else
            printf '  ✗ the forward is not in the audit log\n'
            fail=1
          fi

          # **A target the host never named is refused.** The allowlist is
          # nothing here, which means loopback and nothing else — so an
          # address on somebody else's network is refused without being
          # dialled.
          env ESPER_HOME="$term_b" timeout 60 "$BIN/esper-engine" \
            forward "$lport2:10.99.99.99:9" "$host_addr" \
            > "$term_b/forward-denied.log" 2>&1 &
          denied_pid=$!
          for _ in $(seq 1 100); do
            grep -q 'ctrl-c to stop' "$term_b/forward-denied.log" 2>/dev/null && break
            sleep 0.1
          done
          python3 -c 'import socket, sys
try:
    c = socket.create_connection(("127.0.0.1", int(sys.argv[1])), 10)
    c.sendall(b"hello\n")
    c.recv(100)
except OSError:
    pass' "$lport2" >/dev/null 2>&1 || true
          sleep 0.5
          if grep -q 'allowlist' "$term_b/forward-denied.log" 2>/dev/null; then
            printf '  ✓ a target outside the host'"'"'s allowlist is refused\n'
          else
            printf '  ✗ a target outside the allowlist was not refused\n'
            sed 's/^/      /' "$term_b/forward-denied.log"
            fail=1
          fi
          kill "$denied_pid" 2>/dev/null || true

          # **The `ssh -W` shape.** One connection on stdin and stdout, which
          # is what `ProxyCommand` runs — so plain ssh, scp and git ride the
          # same pairing and the same ladder.
          #
          # The assertion is on the **whole** of stdout, not on finding the
          # payload somewhere in it: `ProxyCommand` gives this process
          # somebody's ssh connection to write to, so a progress line printed
          # there is protocol corruption. A grep would have passed happily
          # while the banner was still going out with the bytes.
          got=$(printf 'hello\n' | env ESPER_HOME="$term_b" timeout 30 \
            "$BIN/esper-engine" forward --stdio "127.0.0.1:$svc_port" "$host_addr" 2>/dev/null || true)
          if [ "$got" = "smoke-forward-42" ]; then
            printf '  ✓ --stdio carried one connection, and nothing else, on stdout\n'
          else
            printf '  ✗ --stdio put something other than the connection on stdout\n'
            printf '%s\n' "$got" | sed 's/^/      /'
            fail=1
          fi

          # **A revocation reaches a forward that is open right now.** The
          # listener is still running and the session is still up; taking the
          # permission away has to end both, and say so — a local port that
          # goes quiet without a reason sends somebody looking at their
          # network for an hour.
          ESPER_HOME="$term_a" "$BIN/esper-engine" revoke "$fp" forward >/dev/null
          ended=""
          for _ in $(seq 1 100); do
            if grep -q 'the host ended it' "$term_b/forward.log" 2>/dev/null; then
              ended=yes
              break
            fi
            sleep 0.1
          done
          if [ -n "$ended" ]; then
            printf '  ✓ revoking forward ended the open forward, with a reason\n'
          else
            printf '  ✗ revoking forward did not reach the open forward\n'
            sed 's/^/      /' "$term_b/forward.log"
            fail=1
          fi

          kill "$forward_pid" 2>/dev/null || true
        fi
        kill "$service_pid" 2>/dev/null || true
      fi

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
