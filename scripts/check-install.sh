#!/usr/bin/env bash
# The packages, installed on the distributions they are for.
#
# # Why this exists
#
# `make check-packages` asks whether everything the build produced is in some
# package. That is a question about the manifest, and it was answered on this
# machine, which is Arch. It cannot ask the question a person downloading the
# file asks, which is whether `dpkg -i` works.
#
# The first time anything did ask, the answer was no — not in the way that
# fails, in the way that does not:
#
#     dpkg -i esper_0.0.0-1_amd64.deb     # Debian 12
#     Setting up esper (0.0.0-1) ...
#     $ esper-engine --version
#     /usr/bin/esper-engine: version `GLIBC_2.39' not found
#
# `Depends: libc6` was satisfied. It is satisfied on every Debian that has ever
# existed. The package installed, `dpkg -l` said `ii`, and the program could not
# start — which is the packaging README's own stated nightmare, "a package
# missing esper-engine installs cleanly and does nothing", one level deeper:
# `esper-engine` was *there*.
#
# # What it checks that inspection cannot
#
#   - that dpkg accepts an archive this project assembles from `ar` and `tar`,
#     which no tool on the development machine can open;
#   - that the binaries start against another distribution's glibc, rather than
#     against the one they were linked on;
#   - that `dpkg --verify` agrees with the md5sums the build wrote;
#   - that removing the package leaves nothing;
#   - and that the glibc floor **bites** — the package is offered to a Debian
#     too old for it and has to be refused. A dependency nothing ever rejects
#     is a dependency that proves nothing, which is how the empty one survived.
#
# Everything runs with no network, so nothing here can pass by downloading
# something the package was supposed to bring.
#
# Usage: scripts/check-install.sh
set -uo pipefail

here=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
dist="$here/dist"
run="$here/scripts/in-container.sh"

fail=0
checked=0

note_fail() { printf '  ✗ install: %s\n' "$1"; fail=1; }

if ! command -v docker >/dev/null 2>&1; then
  echo "  ✗ install: no docker — this check needs a container to be another distribution in" >&2
  exit 1
fi

version=$(tr -d 'v \n' < "$here/VERSION")
esper_deb="esper_${version}-1_amd64.deb"
broker_deb="esper-broker_${version}-1_amd64.deb"
for deb in "$esper_deb" "$broker_deb"; do
  if [ ! -f "$dist/$deb" ]; then
    echo "  ✗ install: no $dist/$deb — run \`make package-deb\` first" >&2
    exit 1
  fi
done

# The capabilities dpkg needs to unpack a package and no others: it sets
# ownership and modes on the files it writes. Naming them here rather than
# reaching for `--privileged` is the difference between a check that is allowed
# to install a package and a check that is allowed to do anything.
unpack_caps=chown,fowner,dac_override,fsetid,setuid,setgid

# ── The floor is the one the binaries actually have ──────────────────────────
#
# Read out of the package rather than out of `build/`, so that a stale tree
# cannot make a wrong package look right.
for pair in "esper:$esper_deb" "esper-broker:$broker_deb"; do
  pkg=${pair%%:*}
  deb=${pair#*:}
  checked=$((checked + 1))

  declared=$("$run" --ro "$dist:/dist" debian -- \
    dpkg-deb -f "/dist/$deb" Depends 2>/dev/null | grep -o '2\.[0-9]*' | head -1)

  unpacked=$(mktemp -d)
  # Extracted as the invoking user, not as the container's root: files the
  # host cannot afterwards delete are a check that litters /tmp on every run.
  "$run" --ro "$dist:/dist" --rw "$unpacked:/out" --user "$(id -u):$(id -g)" debian -- \
    dpkg-deb -x "/dist/$deb" /out >/dev/null 2>&1
  actual=$("$here/scripts/glibc-floor.py" "$unpacked"/usr/bin/* 2>/dev/null)
  rm -rf "$unpacked"

  if [ -z "$actual" ]; then
    note_fail "nothing in $pkg links glibc, so the floor in its control file is unchecked"
  elif [ "$declared" != "$actual" ]; then
    note_fail "$pkg declares \`libc6 (>= ${declared:-nothing})\` and its binaries need $actual.
    A floor below what the binaries need installs on a machine that cannot run them.
    A floor above it refuses a machine that could."
  fi
done

# ── It installs, it runs, it verifies, it removes ───────────────────────────
checked=$((checked + 1))
lifecycle=$("$run" --ro "$dist:/dist" --caps "$unpack_caps" debian -- sh -c "
set -e
dpkg -i /dist/$esper_deb >/dev/null
dpkg -i /dist/$broker_deb >/dev/null

esper version >/dev/null        || { echo 'esper did not run'; exit 1; }
esper-engine --version >/dev/null || { echo 'esper-engine did not run'; exit 1; }
esper-broker --version >/dev/null || { echo 'esper-broker did not run'; exit 1; }

# Every file the packages claim, where dpkg says it put it.
for f in \$(dpkg -L esper esper-broker | grep -v '^/\$'); do
  [ -e \"\$f\" ] || { echo \"dpkg lists \$f and it is not there\"; exit 1; }
done

# A unit that starts a file the package did not install is a service that
# installs, enables, and then fails with 203/EXEC — on a machine where
# everything looks present. Both packages shipped one: the units name the paths
# a hand install uses, and a package uses \`/usr/bin\`.
for unit in \$(dpkg -L esper esper-broker | grep '[.]service\$'); do
  for exe in \$(grep '^ExecStart=' \"\$unit\" | sed 's/^ExecStart=//' | awk '{print \$1}'); do
    case \"\$exe\" in
      /*) [ -x \"\$exe\" ] || { echo \"\$unit starts \$exe, which no package installed\"; exit 1; } ;;
      # Anything else is a systemd specifier or a bare name. \`%h/.local/bin\`
      # is the one that was actually shipped, and it points into whichever
      # home the service happens to start in — which a packaged unit must not
      # do, and which the absolute-path branch above silently skipped until a
      # sabotage put it back and this check passed anyway.
      *) echo \"\$unit starts \$exe, which is not an absolute path\"; exit 1 ;;
    esac
  done
done

# Nothing here needs a display library, and a server does not have one.
#
# The claim has always been that \`x11rb\` speaks the X protocol over a socket
# rather than binding libX11, so a host on a headless machine links nothing
# graphical. It was true and it had never been asked anywhere it could fail —
# this machine has every X library installed. A Debian container has none, so
# the question is a real one here.
#
# The list is what glibc itself provides plus libstdc++, which openh264's C++
# brings. Anything outside it is a new dependency somebody added without
# meaning to, and the failure it causes is an install that works on every
# desktop and on no server.
for bin in /usr/bin/esper /usr/bin/esper-engine /usr/bin/esper-broker; do
  extra=\$(ldd \"\$bin\" | awk '{print \$1}' | grep '^lib' | sed 's/[.]so.*//' \
    | sort -u | grep -vxE 'libc|libm|libgcc_s|libstdc[+][+]|libdl|libpthread|librt|libresolv' || true)
  [ -z \"\$extra\" ] || { echo \"\$bin links \$extra, which a headless server will not have\"; exit 1; }
done

# dpkg --verify reads the md5sums this project writes by hand. Anything it
# prints is a file whose recorded hash is not the file's hash.
wrong=\$(dpkg --verify esper esper-broker 2>&1 || true)
[ -z \"\$wrong\" ] || { echo \"dpkg --verify: \$wrong\"; exit 1; }

dpkg --purge esper esper-broker >/dev/null 2>&1
for f in /usr/bin/esper /usr/bin/esper-engine /usr/bin/esper-broker \
         /usr/lib/systemd/user/esper.service /usr/lib/systemd/system/esper-broker.service; do
  [ ! -e \"\$f\" ] || { echo \"\$f survived a purge\"; exit 1; }
done
echo ok
" 2>&1 | tail -1)

[ "$lifecycle" = "ok" ] || note_fail "on Debian: $lifecycle"

# ── The broker installs on its own ──────────────────────────────────────────
#
# It is a separate package because a different person installs it on a
# different machine, and that claim is only true if it needs nothing from the
# other one.
checked=$((checked + 1))
alone=$("$run" --ro "$dist:/dist" --caps "$unpack_caps" debian -- sh -c "
set -e
dpkg -i /dist/$broker_deb >/dev/null
esper-broker --version >/dev/null
[ ! -e /usr/bin/esper-engine ] || { echo 'the broker package brought the engine'; exit 1; }
echo ok
" 2>&1 | tail -1)
[ "$alone" = "ok" ] || note_fail "the broker package is not independently installable: $alone"

# ── And the floor bites ─────────────────────────────────────────────────────
#
# Offered to a Debian older than the binaries need. A refusal here is the
# check: if this install succeeds, the dependency is decorative again and the
# failure it is supposed to prevent has come back.
checked=$((checked + 1))
refused=$("$run" --ro "$dist:/dist" --caps "$unpack_caps" debian-old -- sh -c "
dpkg -i /dist/$esper_deb >/dev/null 2>/tmp/why
if [ \$? -eq 0 ]; then echo 'installed anyway'; else grep -o 'depends on libc6[^;]*' /tmp/why | head -1; fi
" 2>&1 | tail -1)
case "$refused" in
  "depends on libc6"*) ;;
  "installed anyway")
    note_fail "Debian 12 installed a package whose engine needs a newer glibc than it has.
    The floor is not doing anything, which is how the bare \`libc6\` survived being written." ;;
  *) note_fail "Debian 12 refused the package, but not for the reason expected: ${refused:-nothing}" ;;
esac

# A floor under the check itself: a run that tried nothing agrees with
# everything, and every one of these is a container that could have failed to
# start for reasons having nothing to do with the packages.
if [ "$checked" -lt 5 ]; then
  echo "  ✗ install: only $checked checks ran, which is not all of them" >&2
  exit 1
fi

if [ "$fail" -ne 0 ]; then
  echo ""
  echo "  A package that installs is not a package that runs. The difference is"
  echo "  another distribution, which this machine is not."
  exit 1
fi
echo "  ✓ install: $checked checks — the .debs install, run, verify and remove on Debian 13, every unit starts a file its package installed, nothing links a display library, the broker installs alone, and the glibc floor is refused by Debian 12"
