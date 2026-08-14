#!/usr/bin/env bash
# Build the same commit twice, from two different directories, and require the
# bytes to match.
#
#     check-reproducible.sh [--with-engine]
#
# **Because "reproducible" was written in three comments and had never been
# measured.** It was not true: Go stamps the VCS state of the tree it was built
# in, so a binary built from this checkout and one built from `git archive` of
# the same commit differed — and anyone who downloaded the source to check a
# release would have concluded it had been tampered with. rustc bakes in the
# directory it was invoked in, which survives `strip`.
#
# For a product whose claim is that it operates no infrastructure, an
# independently reproducible binary is what turns that claim into something a
# stranger can check rather than something they have to believe.
#
# # What varies, and what does not
#
# The **source directory** varies, which is the difference between a checkout
# and an unpacked tarball, and it is the one that found both defects. The
# **time** is pinned through `SOURCE_DATE_EPOCH`, because a release sets it and
# a build that embedded the wall clock could never match anything.
#
# What is *not* varied here is the machine: same compiler, same libc, same
# `$HOME`. Two machines agreeing is the claim that matters and it needs two
# machines; this is what one machine can prove on its own, which is that
# nothing about *where* the build ran is in the result.
#
# # Why the engine is opt-in
#
# The Go half rebuilds in seconds, so it runs in `make ci` and catches the
# whole class. The Rust half is two cold release builds of forty-odd crates —
# twenty minutes — so it runs in `ci-full` and on the release checklist.
set -euo pipefail

with_engine=no
[ "${1:-}" = "--with-engine" ] && with_engine=yes

here=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
cd "$here"

# Pinned, and not to "now": a value that changed between the two builds would
# make them differ for a reason that has nothing to do with what is being
# tested.
export SOURCE_DATE_EPOCH=1700000000

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

fail=0
checks=0
ok()  { printf '  ✓ %s\n' "$1"; checks=$((checks + 1)); }
bad() { printf '  ✗ %s\n' "$1"; checks=$((checks + 1)); fail=1; }

echo "  — building the same commit from two directories"

# The second tree is what a release tarball is: tracked files, no `.git`.
#
# The *working* copy of them rather than `HEAD`, so this measures the tree in
# front of you. Exporting `HEAD` would mean the check quietly stopped measuring
# anything the moment somebody had an uncommitted change, which is most of the
# time somebody runs it — and a check that skips when the code is being changed
# is a check that only ever sees code nobody is changing.
if ! git -C "$here" rev-parse --git-dir >/dev/null 2>&1; then
  echo "  · not a git checkout, so there is no second tree to build from"
  exit 0
fi
elsewhere=$work/elsewhere
mkdir -p "$elsewhere"
git -C "$here" ls-files -z | tar --null --files-from=- --create --file=- \
  | tar -x -C "$elsewhere"

version=$(cat VERSION)
build_time=$(date -u -d "@$SOURCE_DATE_EPOCH" +%Y-%m-%dT%H:%M:%SZ)
ldflags="-s -w -X main.version=$version -X main.buildTime=$build_time"

go build -trimpath -buildvcs=false -ldflags="$ldflags" -o "$work/esper.here" ./cmd/esper
(cd "$elsewhere" && go build -trimpath -buildvcs=false -ldflags="$ldflags" \
  -o "$work/esper.there" ./cmd/esper)

if cmp -s "$work/esper.here" "$work/esper.there"; then
  ok "esper is the same binary built from either directory"
else
  bad "esper differs depending on where it was built"
  printf '      %s\n' "$(sha256sum "$work/esper.here" | cut -c1-16)  in the checkout"
  printf '      %s\n' "$(sha256sum "$work/esper.there" | cut -c1-16)  in the exported tree"
  # What differs is more useful than that something does.
  diff <(go version -m "$work/esper.here") <(go version -m "$work/esper.there") \
    | grep -E '^[<>]' | head -6 | sed 's/^/      /' || true
fi

# **And the check has to be able to fail.** A build that is reproducible for a
# reason other than the one claimed — because both sides read the same stale
# output, say — would pass this silently. Stamping a different time must change
# the bytes.
go build -trimpath -buildvcs=false \
  -ldflags="-s -w -X main.version=$version -X main.buildTime=1999-01-01T00:00:00Z" \
  -o "$work/esper.other" ./cmd/esper
if cmp -s "$work/esper.here" "$work/esper.other"; then
  bad "a different build time produced the same bytes, so this check measures nothing"
else
  ok "and a different build time does change it, so the comparison is real"
fi

if [ "$with_engine" = no ]; then
  echo "  · the engine is not rebuilt here — twenty minutes; run"
  echo "    'make check-reproducible-full' or see the release checklist"
  echo "  $checks checks"
  exit $fail
fi

echo "  — and the engine, which is the twenty-minute half"

# **Two cargo target directories, on disk and not in `$work`.** `mktemp -d`
# lands in `/tmp`, which is a memory-backed filesystem on a lot of machines —
# two release builds of this workspace is several gigabytes, and filling
# `/tmp` takes the machine down with it rather than failing the check.
targets=$here/build/.reproducible
rm -rf "$targets"
mkdir -p "$targets"

remap_for() {
  printf -- '--remap-path-prefix=%s/core=/esper --remap-path-prefix=%s/registry=/cargo/registry' \
    "$1" "${CARGO_HOME:-$HOME/.cargo}"
}

RUSTFLAGS="$(remap_for "$here")" \
  cargo build --release --quiet --manifest-path core/Cargo.toml \
  --target-dir "$targets/here" --bin esper-engine --bin esper-broker
RUSTFLAGS="$(remap_for "$elsewhere")" \
  cargo build --release --quiet --manifest-path "$elsewhere/core/Cargo.toml" \
  --target-dir "$targets/there" --bin esper-engine --bin esper-broker

for b in esper-engine esper-broker; do
  if cmp -s "$targets/here/release/$b" "$targets/there/release/$b"; then
    ok "$b is the same binary built from either directory"
  else
    bad "$b differs depending on where it was built"
  fi
done

echo "  $checks checks"
exit $fail
