#!/usr/bin/env bash
# The RPM, built the way Fedora would build it, then installed.
#
# # Why this exists
#
# `packaging/rpm/esper.spec` opened with a paragraph admitting it had never
# been built — this project is developed on Arch, which has no `rpmbuild` — and
# `scripts/check-packages.sh` compared its `%files` list against the manifest
# because that was the most anyone could check from here.
#
# It was worth admitting and it was not enough. The first time the spec met
# `rpmbuild`, three things were wrong that no amount of reading had found:
#
#   - **`BuildRequires` was short a compiler.** `openh264-sys2` builds the
#     codec from C++ and Fedora's build root has only a C compiler, so the
#     build ran for several minutes and stopped at `failed to find tool "g++"`.
#   - **A Go test could not run in a build system.** It shelled out to
#     `systemd-analyze verify --user` and inherited `XDG_RUNTIME_DIR` from
#     whoever ran it. Every developer machine has one; `%check` does not, and
#     the verdict was "Failed to initialize manager", which says nothing about
#     the unit under test.
#   - **A Rust test asserted that two statements run within 20 ms.** True on an
#     idle laptop, false on a machine compiling a workspace beside it.
#
# None of those is a packaging mistake exactly. They are all the same mistake:
# something true of the machine it was written on, written down as if it were
# true of the software.
#
# # What it does
#
# Builds the source package and both binary packages in a pinned Fedora, with
# the project's own suite running in `%check`, then installs what comes out in
# a *clean* Fedora — one with no toolchain, no source and no build root — and
# runs the binaries. That second half is the part `rpmbuild` does not do and
# the part a person downloading the file is actually doing.
#
# This compiles the whole workspace. It is tens of minutes, and it is not part
# of `make all`.
#
# Usage: scripts/check-rpm.sh
set -uo pipefail

here=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
run="$here/scripts/in-container.sh"
builder=esper-rpmbuild

fail=0
checked=0
note_fail() { printf '  ✗ rpm: %s\n' "$1"; fail=1; }

command -v docker >/dev/null 2>&1 || {
  echo "  ✗ rpm: no docker — this check needs a Fedora to build in" >&2; exit 1; }

version=$(tr -d 'v \n' < "$here/VERSION")
tarball="$here/dist/esper-${version}.tar.gz"
[ -f "$tarball" ] || { echo "  ✗ rpm: no $tarball — run \`make dist-source\` first" >&2; exit 1; }

# The builder image, from the pinned Fedora. Built here rather than by the
# harness so that the minutes it costs are visible to whoever started this.
echo "  … rpm: preparing the build image"
if ! docker build -q -f "$here/packaging/container/rpmbuild.Containerfile" -t "$builder" "$here" >/dev/null 2>&1; then
  echo "  ✗ rpm: could not build the rpmbuild image" >&2
  exit 1
fi

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
mkdir -p "$work/out"
chmod 777 "$work/out"

# ── Build ───────────────────────────────────────────────────────────────────
#
# With the network, because `%build` fetches the crate and module graphs from
# the registries, exactly as a Fedora build system would.
echo "  … rpm: building (the whole workspace, and the suite in %check)"
checked=$((checked + 1))
if ! "$run" --net --ro "$here/dist:/src" --ro "$here/packaging/rpm:/spec" --rw "$work/out:/out" \
     --built "$builder" -- sh -c "
set -e
cp /src/esper-${version}.tar.gz ~/rpmbuild/SOURCES/
cp /spec/esper.spec ~/rpmbuild/SPECS/
rpmbuild -ba ~/rpmbuild/SPECS/esper.spec
cp -r ~/rpmbuild/RPMS ~/rpmbuild/SRPMS /out/
# Left readable and removable by whoever started this. The container builds as
# its own user, and a directory it owns is one the host cannot clean up — which
# turns a temporary directory into a permanent one, quietly, every run.
#
# The copied directories only: /out itself belongs to the host, and chmod
# needs ownership rather than write permission, so including it fails the whole
# command after a build that had already succeeded.
chmod -R a+rwX /out/RPMS /out/SRPMS
" > "$work/build.log" 2>&1; then
  note_fail "rpmbuild failed. The last of it:"
  tail -25 "$work/build.log" | sed 's/^/      /'
  echo ""
  echo "  A spec that has never been built is a spec nobody knows is broken."
  exit 1
fi

# `rpmbuild` warns rather than fails about several things worth not shipping —
# a changelog date that is not the weekday it names was one, and it was there
# from the day the file was written.
checked=$((checked + 1))
warnings=$(grep -A20 "^RPM build warnings:" "$work/build.log" | tail -n +2 | grep -v '^$' || true)
if [ -n "$warnings" ]; then
  note_fail "rpmbuild accepted the spec with warnings, which are the defects it does not stop for:
$(printf '%s' "$warnings" | sed 's/^/      /')"
fi

found=$(find "$work/out/RPMS" -name '*.rpm' 2>/dev/null | sort)
checked=$((checked + 1))
count=$(printf '%s' "$found" | grep -c . || true)
if [ "$count" -lt 2 ]; then
  note_fail "the spec declares two packages and produced $count"
fi

# ── Install, on a Fedora with no toolchain ──────────────────────────────────
#
# A different container from the one that built them: the build image has Go,
# Rust, cmake and a source tree in it, and a package that only works there is a
# package that only works where it was made.
echo "  … rpm: installing what came out, on a clean Fedora"
checked=$((checked + 1))
rpms=$(printf '%s' "$found" | grep -v debuginfo | grep -v debugsource | sed "s|$work/out|/rpms|" | tr '\n' ' ')
#
# `dnf install`, not `rpm -i --nodeps`, because the second one is a way of not
# asking the question. The spec says `Requires: systemd`, and a person types
# the command that has to satisfy it. That needs the network, and it is the
# only step here that does.
lifecycle=$("$run" --net --ro "$work/out:/rpms" --caps chown,fowner,dac_override,fsetid,setuid,setgid,mknod fedora -- sh -c "
set -e
dnf install -y $rpms >/dev/null 2>&1 || { echo 'dnf would not install them'; exit 1; }

esper version >/dev/null            || { echo 'esper did not run'; exit 1; }
esper-engine --version >/dev/null   || { echo 'esper-engine did not run'; exit 1; }
esper-broker --version >/dev/null   || { echo 'esper-broker did not run'; exit 1; }

# rpm's own view of whether what is on disk is what it shipped.
wrong=\$(rpm -V esper esper-broker 2>&1 || true)
[ -z \"\$wrong\" ] || { echo \"rpm -V: \$wrong\"; exit 1; }

# systemd reading the units this package installed, rather than this script
# reading them. It is the authority on the question and it answers a wider one:
# it caught both units starting a binary the packages do not install, which is
# what `%h/.local/bin/esper` and \`/usr/local/bin/esper-broker\` were doing.
#
# The Debian half of this cannot ask systemd — debian:13-slim has none — and
# checks the ExecStart paths by hand instead. Here there is a real systemd, so
# it is asked.
mkdir -p /tmp/runtime
export XDG_RUNTIME_DIR=/tmp/runtime
verdict=\$(systemd-analyze verify --user /usr/lib/systemd/user/esper.service 2>&1) \
  || { echo \"systemd refuses the user unit: \$verdict\"; exit 1; }
verdict=\$(systemd-analyze verify /usr/lib/systemd/system/esper-broker.service 2>&1) \
  || { echo \"systemd refuses the broker unit: \$verdict\"; exit 1; }

rpm -e esper esper-broker
for f in /usr/bin/esper /usr/bin/esper-engine /usr/bin/esper-broker; do
  [ ! -e \"\$f\" ] || { echo \"\$f survived an erase\"; exit 1; }
done
echo ok
" 2>&1 | tail -1)
[ "$lifecycle" = "ok" ] || note_fail "on Fedora: $lifecycle"

# ── The dependency rpm worked out for itself ────────────────────────────────
#
# The .deb has to be told its glibc floor, because this project assembles that
# archive by hand. RPM derives it, and checking that it did is how we know the
# binaries in the package are the dynamically linked ones we think they are —
# a statically linked or stripped-wrong build would simply not have the
# requirement, and nothing else here would notice.
checked=$((checked + 1))
engine_rpm=$(printf '%s' "$found" | grep -v -- '-broker' | grep -v debuginfo | grep -v debugsource | head -1)
if [ -n "$engine_rpm" ]; then
  # The same path as the container sees it. Written with sed rather than bash
  # substitution because the pattern holds a `/` either way and the expression
  # that results is unreadable.
  inside=$(printf '%s' "$engine_rpm" | sed "s|$work/out|/rpms|")
  needs=$("$run" --ro "$work/out:/rpms" fedora -- \
    rpm -qp --requires "$inside" 2>/dev/null | grep -c 'libc.so.6(GLIBC_' || true)
  [ "${needs:-0}" -gt 0 ] || note_fail "the esper package requires no versioned glibc symbol, which a dynamically linked build always does"
else
  note_fail "could not find the esper package among what was built"
fi

if [ "$checked" -lt 5 ]; then
  echo "  ✗ rpm: only $checked checks ran, which is not all of them" >&2
  exit 1
fi

if [ "$fail" -ne 0 ]; then
  echo ""
  echo "  A spec is a set of claims about a build system. The only thing that"
  echo "  reads them as claims is the build system."
  exit 1
fi
echo "  ✓ rpm: $checked checks — the spec builds on Fedora, the suite passes in %check, and both packages install, run and erase on a clean one"
