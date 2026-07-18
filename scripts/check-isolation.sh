#!/usr/bin/env bash
# Verify this repository is self-contained.
#
# Esper is developed inside a larger planning tree that holds design
# documents, decision records and a build tracker. None of that is published,
# and nothing here may reference it: a public repository linking into a private
# folder is a broken link at best and a leak at worst.
#
# The rule: no file in this repository may reference any path outside it.
#
# This runs as part of the default `make` target. A rule that depends on
# remembering is not a rule.
set -euo pipefail

cd "$(dirname "$0")/.."
fail=0

note() { printf '  %s\n' "$*"; }
bad()  { fail=1; printf '\n  ✗ %s\n' "$*"; }

# Files to search: everything tracked by git. Build output is full of absolute
# paths from the toolchain and is not shipped, so it must never be scanned —
# .gitignore already covers it, which is why `git ls-files` is the primary path.
#
# The fallback exists only before `git init`. It excludes every build directory
# by pattern rather than by name, because a missed one produces thousands of
# false positives from cargo's dependency files.
files() {
  if git rev-parse --git-dir >/dev/null 2>&1; then
    git ls-files
  else
    find . -type f \
      -not -path '*/.git/*' \
      -not -path '*/target/*' \
      -not -path '*/node_modules/*' \
      -not -path './build/*' \
      -not -path './dist/*' \
      | sed 's|^\./||'
  fi
}

# 1. Relative paths that escape the repository root.
#    `../` inside a subdirectory is fine; what matters is whether it resolves
#    outside, so each hit is resolved rather than pattern-matched.
while IFS= read -r f; do
  [ -f "$f" ] || continue
  case "$f" in *.png|*.jpg|*.ico|*.cbor|*.pdf|*.woff*) continue ;; esac
  dir=$(dirname "$f")
  while IFS= read -r ref; do
    [ -n "$ref" ] || continue
    resolved=$(cd "$dir" 2>/dev/null && realpath -m "$ref" 2>/dev/null) || continue
    case "$resolved" in
      "$PWD"|"$PWD"/*) ;;
      *) bad "$f references \"$ref\", which resolves outside the repository"
         note "  → $resolved" ;;
    esac
  done < <(grep -oE '(\.\./)+[A-Za-z0-9_./-]*' "$f" 2>/dev/null | sort -u)
done < <(files)

# 2. Filenames that only exist in the planning tree. Mentioning one means a
#    reader is being sent somewhere they cannot go.
# Names that only exist in the planning tree. A path is the obvious case; a
# bare "ADR 0012" is the same failure with no slash in it — the reader is still
# being sent somewhere they cannot go.
planning_only='TRACKER\.md|docs/decisions/|docs/design/|docs/findings/|tracker-sync|ADR [0-9]{4}'
while IFS= read -r f; do
  [ -f "$f" ] || continue
  case "$f" in scripts/check-isolation.sh) continue ;; esac
  case "$f" in *.png|*.jpg|*.ico|*.cbor|*.pdf|*.woff*) continue ;; esac
  if hits=$(grep -nE "$planning_only" "$f" 2>/dev/null); then
    bad "$f references the planning tree:"
    printf '%s\n' "$hits" | sed 's/^/      /'
  fi
done < <(files)

# 3. Absolute paths into a developer's machine.
while IFS= read -r f; do
  [ -f "$f" ] || continue
  case "$f" in scripts/check-isolation.sh) continue ;; esac
  case "$f" in *.png|*.jpg|*.ico|*.cbor|*.pdf|*.woff*) continue ;; esac
  if hits=$(grep -nE '/home/[a-z]|/Users/[a-z]|C:\\Users' "$f" 2>/dev/null); then
    bad "$f contains an absolute developer path:"
    printf '%s\n' "$hits" | sed 's/^/      /'
  fi
done < <(files)

if [ "$fail" -ne 0 ]; then
  cat <<'MSG'

  This repository must be self-contained. Rewrite the reference for readers who
  only have this repository — do not copy the planning document across.
MSG
  exit 1
fi

echo "  ✓ repository is self-contained"
