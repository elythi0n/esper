#!/usr/bin/env bash
# The workspace still type-checks for Windows and macOS.
#
# # Why this exists
#
# The CI matrix named `windows-latest` and `macos-latest` from the first commit,
# with a comment saying all three are real targets and tested from the start.
# Neither had ever compiled: the repository has no remote, so no CI run has ever
# happened, and the matrix was a statement of intent that read like a statement
# of fact. Both targets were then made clean by hand — and checked nowhere. No
# `--target` in the Makefile, no cross job, `GOOS` only inside `build-all`. Which
# is the same defect one level up.
#
# # What it checks without any cross toolchain, and why that is the useful part
#
# `cargo check` does not link, so no cross linker is needed. What *is* needed is
# a C compiler for the target, because three dependencies build C — `aws-lc-sys`,
# `openh264-sys2` and `opusic-sys` — and their build scripts run whatever target
# is asked for.
#
# But **twenty-seven of the forty-one crates here have no C anywhere in their
# graph — including dev-dependencies**, and those need nothing installed. That is not a consolation
# prize: the two crates that were actually broken when this was last checked
# were `esper-capture-x11`, which used `rustix` and `std::os::fd` unconditionally
# while its own manifest gated `rustix` to `cfg(unix)`, and `esper-pty`, which
# pulled a dependency that does not build on Windows. Both are in the no-C set.
# A Linux-only `use` is overwhelmingly likely to land in a crate like those
# rather than in the media pipeline.
#
# The set is derived from `cargo tree` on every run rather than listed here, so
# a new crate is covered the day it exists and one that gains a C dependency
# drops out with a line saying so. **Dev-dependencies count**, because this
# runs `--all-targets`: leaving them out of the query put crates in the no-C
# set whose test binaries build C, and the check then failed on the very
# dependency it was arranged to avoid.
#
# # What needs a toolchain, and what needs a machine
#
# With `mingw-w64` installed the whole workspace type-checks for Windows and
# this script does that too. macOS cannot be linked here at all — the same C
# needs Apple's SDK, whose licence restricts it to Apple hardware — so the
# twelve media crates are checked for Windows only, and for macOS by a Mac.
#
# **None of this is a build**, and none of it says the tests pass there. It
# catches one thing: a Linux-only `use` in a crate nobody thought was
# Linux-only.
set -uo pipefail
cd "$(dirname "$0")/.."

CORE=core
# The C-building dependencies. A crate whose graph contains one of these cannot
# be checked for a foreign target without that target's C compiler.
C_DEPS='aws-lc-sys|openh264-sys2|opusic-sys'
TARGETS="x86_64-pc-windows-gnu aarch64-apple-darwin"

fail=0
installed=$(rustup target list --installed 2>/dev/null || echo "")

# Split the workspace by whether its dependency graph builds C.
pure=""
impure=""
for crate in $(ls "$CORE/crates"); do
  if cargo tree --manifest-path "$CORE/Cargo.toml" -p "$crate" \
       --edges normal,build,dev 2>/dev/null | grep -qE "$C_DEPS"; then
    impure="$impure $crate"
  else
    pure="$pure -p $crate"
  fi
done
pure_count=$(printf '%s' "$pure" | grep -o ' -p ' | wc -l)
impure_count=$(printf '%s' "$impure" | wc -w)
printf '  %s crates have no C to build; %s do and need that target'"'"'s compiler\n' \
  "$pure_count" "$impure_count"

for target in $TARGETS; do
  if ! printf '%s\n' "$installed" | grep -qx "$target"; then
    printf '  · %s is not installed — run: rustup target add %s\n' "$target" "$target"
    continue
  fi

  # The part that always runs.
  if cargo clippy --manifest-path "$CORE/Cargo.toml" $pure --all-targets \
       --target "$target" --quiet -- -D warnings 2>&1 | sed 's/^/      /'; then
    printf '  ✓ %s: %s crates type-check, clippy clean\n' "$target" "$pure_count"
  else
    printf '  ✗ %s: a crate with no C stopped type-checking\n' "$target"
    fail=1
  fi

  # The part that needs a real cross compiler.
  case "$target" in
    *windows-gnu) probe=x86_64-w64-mingw32-gcc ;;
    *) probe="" ;;
  esac
  if [ -n "$probe" ] && command -v "$probe" >/dev/null 2>&1; then
    if cargo clippy --manifest-path "$CORE/Cargo.toml" --workspace --all-targets \
         --target "$target" --quiet -- -D warnings 2>&1 | sed 's/^/      /'; then
      printf '  ✓ %s: the whole workspace type-checks, using %s\n' "$target" "$probe"
    else
      printf '  ✗ %s: the whole workspace does not type-check\n' "$target"
      fail=1
    fi
  elif [ -n "$probe" ]; then
    printf '  · %s: the %s crates that build C are unchecked — install mingw-w64 for those\n' \
      "$target" "$impure_count"
  else
    printf '  · %s: the %s crates that build C need Apple'"'"'s SDK, so they need a Mac\n' \
      "$target" "$impure_count"
  fi
done

# The Go half needs no toolchain and no targets installed.
for pair in windows/amd64 darwin/amd64 darwin/arm64; do
  os=${pair%/*}; arch=${pair#*/}
  if GOOS="$os" GOARCH="$arch" CGO_ENABLED=0 go build ./... 2>&1 | sed 's/^/      /'; then
    printf '  ✓ go builds for %s\n' "$pair"
  else
    printf '  ✗ go does not build for %s\n' "$pair"
    fail=1
  fi
done

if [ "$fail" -ne 0 ]; then
  printf '\n  a target stopped type-checking. See the header of this script for what\n'
  printf '  this does and does not prove.\n'
  exit 1
fi
printf '  ✓ Windows and macOS still type-check (a type-check, not a build)\n'
