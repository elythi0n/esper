#!/usr/bin/env bash
# Build the control plane for every platform it claims, and look at what came out.
#
# **The matrix was a list and never an assertion.** `PLATFORMS` in the Makefile
# names five targets and `build-all` walks it, but nothing ran `build-all` and
# nothing looked at the result — so a change that broke the Windows or macOS
# build would have been found by whoever tried to ship one. The Rust half has
# `check-other-platforms.sh` for exactly this reason; this is the same argument
# for the Go half, which is cheap enough to build rather than merely check.
#
# It builds, and then it reads the file back. Building alone would pass for a
# `GOARCH` the toolchain quietly ignored: `file` saying `ARM aarch64` is what
# separates "the compiler was asked for arm64" from "the compiler produced
# arm64".
set -euo pipefail

here=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
cd "$here"

fail=0
checks=0
ok()  { printf '  ✓ %s\n' "$1"; checks=$((checks + 1)); }
bad() { printf '  ✗ %s\n' "$1"; checks=$((checks + 1)); fail=1; }

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

echo "  — building the control plane for every platform it claims"

# What `file` must say, per target. Written out rather than derived from the
# target name, because deriving it would mean the expectation and the thing it
# checks came from the same place.
want_linux_amd64='ELF 64-bit LSB.*x86-64'
want_linux_arm64='ELF 64-bit LSB.*ARM aarch64'
want_windows_amd64='PE32\+ executable.*x86-64'
want_darwin_amd64='Mach-O 64-bit.*x86_64'
want_darwin_arm64='Mach-O 64-bit.*arm64'

platforms=$(sed -n 's/^PLATFORMS *:= *//p' Makefile)
[ -n "$platforms" ] || { bad "no PLATFORMS in the Makefile to check"; exit 1; }

built=0
for p in $platforms; do
  os=${p%/*}; arch=${p#*/}
  out=$work/esper-$os-$arch
  if ! GOOS="$os" GOARCH="$arch" CGO_ENABLED=0 \
       go build -trimpath -buildvcs=false -o "$out" ./cmd/esper 2>"$work/err"; then
    bad "$os/$arch does not build"
    sed 's/^/      /' "$work/err" | head -5
    continue
  fi
  eval "want=\${want_${os}_${arch}:-}"
  if [ -z "$want" ]; then
    bad "$os/$arch built, and nothing here knows what it should look like"
    continue
  fi
  got=$(file -b "$out")
  if printf '%s' "$got" | grep -qE "$want"; then
    ok "$os/$arch builds, and is $os/$arch"
    built=$((built + 1))
  else
    bad "$os/$arch built something else: $got"
  fi
done

# **A matrix that shrank would otherwise pass.** Deleting four entries from
# `PLATFORMS` leaves one target building perfectly, and every check above
# green.
if [ "$built" -ge 5 ]; then
  ok "and all $built platforms in the matrix are accounted for"
else
  bad "only $built platforms built; the matrix names five"
fi

echo "  $checks checks"
exit $fail
