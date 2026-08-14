#!/usr/bin/env bash
# Produce the release source tarball.
#
#     dist-source.sh <outdir>
#
# **What a stranger downloads.** Everything that is tracked and nothing that is
# not: no `.git`, no build output, no editor state — and, most of the point of
# this project's isolation rule, nothing from the planning tree it is developed
# inside. `check-published.sh` unpacks what this produces and checks exactly
# that, on the artifact rather than on the working copy, because the working
# copy is not what anybody receives.
#
# Reproducible, for the same reason the binaries are: two people who make the
# tarball from the same commit should be able to compare notes with a hash
# rather than with a diff. `tar` on its own is not — it records mtimes, uids
# and whatever order the filesystem hands back — so each of those is pinned.
set -euo pipefail

outdir=${1:?output directory}

here=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
cd "$here"

version=$(tr -d ' \n' < VERSION)
: "${SOURCE_DATE_EPOCH:=$(date -u +%s)}"

git rev-parse --git-dir >/dev/null 2>&1 || {
  echo "not a git checkout: there is no list of tracked files to export" >&2
  exit 1
}

mkdir -p "$outdir"
outdir=$(cd -- "$outdir" && pwd)
name=esper-${version#v}
out=$outdir/$name.tar.gz

# The working tree's tracked files, not `HEAD`: what is being released is what
# is here, and a tarball of `HEAD` would silently omit a change somebody
# forgot to commit rather than letting the release checklist catch it.
git ls-files -z \
  | LC_ALL=C sort -z \
  | tar --create --file - --null --files-from=- \
      --transform="s|^|$name/|" \
      --format=gnu \
      --mtime="@$SOURCE_DATE_EPOCH" \
      --owner=root --group=root --numeric-owner \
  | gzip -9n > "$out"

echo "$out"
