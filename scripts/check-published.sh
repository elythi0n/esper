#!/usr/bin/env bash
# Check the artifact a stranger downloads, rather than the tree it came from.
#
# **The isolation rule is checked on the working copy, and the working copy is
# not what is published.** `check-isolation.sh` asks git what is tracked, which
# is exactly right while developing and says nothing about what ends up inside
# the tarball. A file excluded from the export, a path that only appears once
# the tree is unpacked somewhere else, a `.git` directory shipped by accident —
# none of those are visible from where that check stands.
#
# So this one builds the release tarball, unpacks it, and asks the questions
# again of the thing that came out. It also exercises the half of
# `check-isolation.sh` that nothing else does: with no `.git` to ask, it falls
# back to walking the filesystem, and that branch had never run.
set -euo pipefail

here=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
cd "$here"

fail=0
checks=0
ok()  { printf '  ✓ %s\n' "$1"; checks=$((checks + 1)); }
bad() { printf '  ✗ %s\n' "$1"; checks=$((checks + 1)); fail=1; }

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

echo "  — checking the source tarball, unpacked"

export SOURCE_DATE_EPOCH=1700000000
tarball=$(./scripts/dist-source.sh "$work")
tar -xzf "$tarball" -C "$work"
tree=$work/$(basename "$tarball" .tar.gz)

if [ -d "$tree" ]; then
  ok "the tarball unpacks into one directory named for the release"
else
  bad "the tarball did not unpack into $(basename "$tree")"
  echo "  $checks checks"
  exit 1
fi

# **The isolation rule, asked of the artifact.** No `.git` here, so the check
# takes its filesystem path — which is the branch a packager's machine would
# take, and the one nothing else exercises.
if (cd "$tree" && ./scripts/check-isolation.sh >/dev/null 2>&1); then
  ok "the unpacked tree references nothing outside itself"
else
  bad "the unpacked tree references something outside itself"
  (cd "$tree" && ./scripts/check-isolation.sh 2>&1 | sed 's/^/      /' | tail -20) || true
fi

# **And the scan is not vacuous.** With no `.git`, `check-isolation.sh` finds
# its files by walking the filesystem, and a walk that matched nothing would
# report a clean tree in exactly the same words as a tree that is clean. So a
# violation is planted in a copy and the check has to find it — this is the
# only place that branch is exercised at all, and an unexercised fallback is
# how a scan quietly starts scanning nothing.
planted=$work/planted
cp -a "$tree" "$planted"
# Assembled from two pieces rather than written out, because this file would
# otherwise contain the very name it is planting — and the checker would find
# it here, and be right to.
forbidden=$(printf 'TRACK''ER.md')
printf '// see the %s for why\n' "$forbidden" > "$planted/scripts/planted.go"
if (cd "$planted" && ./scripts/check-isolation.sh >/dev/null 2>&1); then
  bad "a planted reference to the planning tree was not found in the artifact"
else
  ok "and a planted reference in the artifact is found, so the scan is real"
fi

# Things that are in the working copy and must not be in a release.
present=
for unwanted in .git build dist core/target .repro; do
  [ -e "$tree/$unwanted" ] && present="$present $unwanted"
done
if [ -z "$present" ]; then
  ok "and carries no .git, no build output and no dist directory"
else
  bad "the tarball carries:$present"
fi

# **It has to be buildable.** A tarball that is missing a file nobody noticed
# is a tarball that fails on the first stranger's machine, and the cheapest
# proof that nothing essential was excluded is to build the half that takes
# seconds.
if (cd "$tree" && go build -trimpath -buildvcs=false -o /dev/null ./cmd/esper 2>"$work/build.err"); then
  ok "and the Go half builds from it, so nothing it needs was left out"
else
  bad "the unpacked tarball does not build"
  sed 's/^/      /' "$work/build.err" | head -10
fi

# The manifests the packaging reads have to survive the export too — the
# packaging is part of what is published, not scaffolding around it.
absent=
for needed in packaging/contents.tsv scripts/package-files.sh packaging/arch/PKGBUILD \
              packaging/rpm/esper.spec LICENSE VERSION; do
  [ -f "$tree/$needed" ] || absent="$absent $needed"
done
if [ -z "$absent" ]; then
  ok "and carries the packaging, so a distribution can build from it"
else
  bad "the tarball is missing:$absent"
fi

echo "  — and the tarball is reproducible"
second=$(./scripts/dist-source.sh "$work/again")
if cmp -s "$tarball" "$second"; then
  ok "two exports of the same tree are the same bytes"
else
  bad "two exports of the same tree differ"
fi

echo "  $checks checks"
exit $fail
