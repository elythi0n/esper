#!/usr/bin/env bash
# No container image arrives by a name somebody else controls.
#
# # Why this exists
#
# A tag is a pointer, and not one this project holds. `debian:13-slim` is a
# different filesystem this month than last; `rust:1.98-slim-bookworm` is
# rebuilt whenever its own base gets a security update; `:nonroot` moves
# whenever distroless does. Pulling by tag means running whatever is behind the
# name at the moment of the pull, which is both a supply-chain decision made by
# nobody and a reproducibility hole in a project that checks it builds the same
# bytes twice.
#
# Both Containerfiles here were written with floating tags, which is the normal
# thing to do and was never noticed, because a floating tag is invisible until
# the day it moves — and on that day the change arrives with no commit, no
# author and no diff.
#
# So: every image is written down once in `packaging/images.tsv` with a digest
# and a reason, and nothing may name an image any other way. This checks that.
#
# What it does not claim: that the pinned images have no vulnerabilities. A pin
# is not a warranty, it is a record of which bytes were reviewed. Moving one is
# `scripts/update-image.sh`, which prints what moved and dates the change.
#
# Usage: scripts/check-images.sh
set -uo pipefail

here=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
manifest="$here/packaging/images.tsv"

fail=0
checked=0

note_fail() { printf '  ✗ images: %s\n' "$1"; fail=1; }

[ -f "$manifest" ] || { echo "  ✗ images: no manifest at packaging/images.tsv" >&2; exit 1; }

# ── Every line of the manifest is a digest and a date ───────────────────────
pinned_digests=""
while IFS=$'\t' read -r name reference digest pinned purpose; do
  case "$name" in ""|"#"*|"nickname") continue ;; esac
  checked=$((checked + 1))

  case "$digest" in
    sha256:[0-9a-f]*)
      [ ${#digest} -eq 71 ] || note_fail "$name is pinned to \"$digest\", which is not the length of a sha256" ;;
    *) note_fail "$name is pinned to \"$digest\", which is not a digest" ;;
  esac
  case "$pinned" in
    [0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]) ;;
    *) note_fail "$name has no date on its pin, so nobody can tell how old the review is" ;;
  esac
  # A pin with no reason is a pin nobody can judge the next move of.
  [ ${#purpose} -ge 40 ] || note_fail "$name is pinned without saying what it is for"

  pinned_digests="$pinned_digests$digest
"
done < <(grep -v '^#' "$manifest")

# ── Every FROM carries a digest, and it is one of ours ──────────────────────
#
# `git grep` rather than `find`, so that an image reference in an untracked
# scratch file cannot fail a check about what this project ships.
froms=$(git -C "$here" grep -hn "^FROM " -- '*Containerfile*' 2>/dev/null)
lines=0
while IFS= read -r line; do
  [ -n "$line" ] || continue
  lines=$((lines + 1))
  checked=$((checked + 1))

  image=$(printf '%s' "$line" | sed 's/^[0-9]*://' | awk '{print $2}')
  case "$image" in
    *@sha256:*)
      digest=${image#*@}
      if ! printf '%s' "$pinned_digests" | grep -qx "$digest"; then
        note_fail "a Containerfile builds FROM $digest, which packaging/images.tsv does not list.
    The manifest is where an image gets a reason and a date; an image that is
    only in a Containerfile has neither."
      fi ;;
    *)
      note_fail "a Containerfile builds FROM \"$image\", which is a tag and not a digest.
    Whoever owns that tag chooses what this builds." ;;
  esac
done <<< "$froms"

# A floor: this check passes trivially if it found no Containerfiles at all,
# which is exactly what a rename would do to it.
if [ "$lines" -lt 3 ]; then
  echo "  ✗ images: only $lines FROM lines were found, and this tree has more than that" >&2
  exit 1
fi

# ── Nothing reaches for an image outside the harness ────────────────────────
#
# `scripts/in-container.sh` is the one place a container is started, so that
# the manifest is the only way in. A `docker run` elsewhere in a checked-in
# script is a second door, and it is the door that will have a bare tag in it.
# Two files are not evidence and are skipped by name: `in-container.sh`, which
# is the harness itself, and this file, which cannot search for a string
# without containing it. A line that merely mentions the rule in a comment is
# somebody explaining it, which is the opposite of breaking it.
loose=$(git -C "$here" grep -n "docker run" -- 'scripts/*' 2>/dev/null \
  | grep -v -e 'in-container.sh' -e 'check-images.sh' \
  | while IFS= read -r hit; do
      file=${hit%%:*}
      text=${hit#*:}; text=${text#*:}
      case "$text" in
        *[!\ ]*) case "${text#"${text%%[![:space:]]*}"}" in "#"*) ;; *) echo "$file" ;; esac ;;
      esac
    done | sort -u)
checked=$((checked + 1))
if [ -n "$loose" ]; then
  note_fail "these start containers without going through scripts/in-container.sh:
    $(printf '%s' "$loose" | tr '\n' ' ')
    The harness is what applies the pin, drops the capabilities and takes the
    network away. A second way in is a way in without those."
fi

if [ "$fail" -ne 0 ]; then
  echo ""
  echo "  An image that arrives by tag is an image somebody else chose, after"
  echo "  the last person here looked at it."
  exit 1
fi
echo "  ✓ images: $checked checks — every image is pinned by digest, listed with a reason, and started through one harness"
