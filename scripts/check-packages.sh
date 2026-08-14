#!/usr/bin/env bash
# Check that the packages install everything that was built, in every format.
#
# **The build decides and the packaging reports.** The three package
# definitions all install from one manifest, so they cannot disagree with each
# other — but they can all be wrong together, and the way that happens is a new
# binary nobody adds to the manifest. It builds, it ships in nothing, and every
# package is internally consistent about not containing it.
#
# So the question asked here is not "do the packages match the manifest" — they
# do by construction, and asking would be asking the manifest about itself. It
# is **"is everything the build produced in a package"**, which is answered
# from `build/`, and "does each format put it in the right place", which is
# answered by staging each package and looking.
set -euo pipefail

here=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
cd "$here"

BIN=${BIN:-build}
fail=0
checks=0

ok()   { printf '  ✓ %s\n' "$1"; checks=$((checks + 1)); }
bad()  { printf '  ✗ %s\n' "$1"; checks=$((checks + 1)); fail=1; }

echo "  — checking every built binary is in a package"

# What the build produced. Executables only: `build/` also holds `dist/` and
# `win/` subdirectories from the other build verbs.
built=$(find "$BIN" -maxdepth 1 -type f -executable -printf '%f\n' | LC_ALL=C sort)
if [ -z "$built" ]; then
  bad "nothing in $BIN/ — run make build first"
  echo "  $checks checks"
  exit 1
fi

packaged=$(awk -F'\t' '$1 !~ /^#/ && $2 == "bin" { print $3 }' packaging/contents.tsv \
  | LC_ALL=C sort -u)

missing=$(comm -23 <(printf '%s\n' "$built") <(printf '%s\n' "$packaged") || true)
if [ -z "$missing" ]; then
  ok "every binary in $BIN/ is named by a package ($(printf '%s\n' "$built" | wc -l) of them)"
else
  bad "built and packaged by nothing: $(printf '%s' "$missing" | tr '\n' ' ')"
fi

# And the other way: a manifest naming a binary that is not built would produce
# a package that fails at build time rather than shipping something wrong, but
# it would fail confusingly.
phantom=$(comm -13 <(printf '%s\n' "$built") <(printf '%s\n' "$packaged") || true)
if [ -z "$phantom" ]; then
  ok "and every binary a package names is one the build produces"
else
  bad "named by a package and never built: $(printf '%s' "$phantom" | tr '\n' ' ')"
fi

echo "  — checking each format puts them where that distribution expects"

stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT

# What each format is expected to lay down. Written out here rather than
# derived, because deriving it from the script under test is how a path bug
# agrees with itself.
expect_arch_esper='usr/bin/esper
usr/bin/esper-engine
usr/lib/systemd/user/esper.service
usr/share/licenses/esper/LICENSE'
expect_deb_esper='usr/bin/esper
usr/bin/esper-engine
usr/lib/systemd/user/esper.service
usr/share/doc/esper/copyright'
expect_arch_broker='usr/bin/esper-broker
usr/lib/systemd/system/esper-broker.service
usr/share/licenses/esper-broker/LICENSE'

staged() {
  local pkg=$1 format=$2 dir="$stage/$1-$2"
  ./scripts/package-files.sh "$pkg" "$dir" "$format" "$BIN" >/dev/null
  find "$dir" -type f -printf '%P\n' | LC_ALL=C sort
}

compare() {
  local what=$1 got=$2 want=$3
  if [ "$got" = "$(printf '%s' "$want" | LC_ALL=C sort)" ]; then
    ok "$what"
  else
    bad "$what"
    diff <(printf '%s\n' "$want" | LC_ALL=C sort) <(printf '%s\n' "$got") \
      | sed 's/^/      /' || true
  fi
}

compare "esper, as Arch lays it out"   "$(staged esper arch)"        "$expect_arch_esper"
compare "esper, as Debian lays it out" "$(staged esper deb)"         "$expect_deb_esper"
compare "the broker, packaged apart"   "$(staged esper-broker arch)" "$expect_arch_broker"

# **The broker package is installable on its own.** That is the whole reason it
# is a separate package: an operator putting an introducer on a server does not
# want the engine, and a package that dragged it in would make "install only
# the broker" impossible.
if printf '%s' "$(staged esper-broker arch)" | grep -qE 'usr/bin/(esper|esper-engine)$'; then
  bad "the broker package carries the engine, so it is not independently installable"
else
  ok "and carries neither esper nor the engine"
fi

echo "  — checking the RPM spec's file list, which RPM makes it repeat"

# `%files` is the one place a format forces the manifest to be written twice.
spec=packaging/rpm/esper.spec
spec_files=$(sed -n '/^%files/,/^%changelog/p' "$spec" \
  | grep -E '^%\{_bindir\}|^%\{_prefix\}' \
  | sed -e 's|%{_bindir}|usr/bin|' -e 's|%{_prefix}|usr|' -e 's|^/||' \
  | LC_ALL=C sort)
want_files=$(printf '%s\n%s\n' "$expect_arch_esper" "$expect_arch_broker" \
  | grep -v 'share/licenses' | LC_ALL=C sort)
compare "the spec lists exactly what the other formats install" "$spec_files" "$want_files"

echo "  $checks checks"
exit $fail
