#!/usr/bin/env bash
# Verify the changelog has something to say about the version being released.
#
# **A changelog is only useful if it is written before the tag, not after.** The
# failure it guards against is small and certain: somebody tags a release, the
# artifacts go out, and the entry gets written a week later from memory or not
# at all. Once the tag exists the changelog is the only record of what changed
# for a person who was not in the repository, and reconstructing it is exactly
# the work nobody does.
#
# While `VERSION` is `v0.0.0` there is nothing to check: the project has never
# been released and `[Unreleased]` is the whole truth.
set -euo pipefail

here=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
cd "$here"

version=$(tr -d ' \n' < VERSION)
checks=0
fail=0
ok()  { printf '  ✓ %s\n' "$1"; checks=$((checks + 1)); }
bad() { printf '  ✗ %s\n' "$1"; checks=$((checks + 1)); fail=1; }

echo "  — checking the changelog covers this version"

[ -f CHANGELOG.md ] || { bad "there is no CHANGELOG.md"; echo "  $checks checks"; exit 1; }

if ! grep -q '^## \[Unreleased\]' CHANGELOG.md; then
  bad "the changelog has no [Unreleased] section for work in flight"
else
  ok "there is an [Unreleased] section to write into"
fi

if [ "$version" = "v0.0.0" ]; then
  echo "  · VERSION is $version, which has never been released — nothing to match"
  echo "  $checks checks"
  exit $fail
fi

# `## [1.2.3]` — the leading v is not repeated inside the brackets, which is
# what Keep a Changelog does and what the link definitions at the bottom of
# such a file expect.
if grep -qE "^## \[${version#v}\]" CHANGELOG.md; then
  ok "the changelog has a section for $version"
else
  bad "VERSION says $version and the changelog has no section for it"
  printf '      sections present: %s\n' \
    "$(grep -oE '^## \[[^]]+\]' CHANGELOG.md | tr -d '#[] ' | tr '\n' ' ')"
fi

echo "  $checks checks"
exit $fail
