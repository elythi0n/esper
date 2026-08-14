#!/usr/bin/env bash
# Install one package's files into a staging directory.
#
#     package-files.sh <package> <destdir> <format> <builddir>
#
# **One implementation, three package formats.** The Arch `package()`, the
# Debian builder and the RPM `%install` all call this, so a file added to
# `packaging/contents.tsv` appears in every package at once and cannot be
# forgotten in two of them. What differs between distributions is where a
# licence goes, and that is the only thing this file branches on.
#
# It is deliberately not the thing that *checks* the packages. A checker that
# compared this script's output against the manifest this script reads would
# agree with itself for ever — see `check-packages.sh`, which compares against
# what the build actually produced.
set -euo pipefail

pkg=${1:?package name}
dest=${2:?destination directory}
format=${3:?arch, deb or rpm}
built=${4:?directory holding the built binaries}

here=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
manifest=$here/packaging/contents.tsv

[ -f "$manifest" ] || { echo "no manifest at $manifest" >&2; exit 1; }

# Where a unit file lives in the tree.
#
# The host's user unit is under `internal/service/` because the `esper` binary
# **embeds** it — one copy, so the file `esper host enable` writes and the file
# a package installs cannot say different things. The broker's is an ordinary
# packaging file, because nothing embeds it.
unit_source() {
  case $1 in
    esper.service)        printf '%s\n' "$here/internal/service/esper.service" ;;
    esper-broker.service) printf '%s\n' "$here/packaging/systemd/esper-broker.service" ;;
    *) echo "no source known for unit '$1'" >&2; return 1 ;;
  esac
}

placed=0
while IFS=$'\t' read -r package kind name; do
  case $package in ''|\#*) continue ;; esac
  [ "$package" = "$pkg" ] || continue

  case $kind in
    bin)
      src=$built/$name
      [ -f "$src" ] || { echo "not built: $src" >&2; exit 1; }
      install -Dm755 "$src" "$dest/usr/bin/$name"
      ;;
    unit-user)
      install -Dm644 "$(unit_source "$name")" "$dest/usr/lib/systemd/user/$name"
      ;;
    unit-system)
      install -Dm644 "$(unit_source "$name")" "$dest/usr/lib/systemd/system/$name"
      ;;
    licence)
      # The one thing distributions genuinely disagree about.
      case $format in
        deb) install -Dm644 "$here/$name" "$dest/usr/share/doc/$pkg/copyright" ;;
        *)   install -Dm644 "$here/$name" "$dest/usr/share/licenses/$pkg/$name" ;;
      esac
      ;;
    *)
      echo "unknown kind '$kind' for $package/$name" >&2
      exit 1
      ;;
  esac
  placed=$((placed + 1))
done < "$manifest"

# **A package with nothing in it installs cleanly.** A mistyped package name
# would otherwise produce an empty staging directory and a package that looks
# fine until somebody installs it.
if [ "$placed" -eq 0 ]; then
  echo "no entries for package '$pkg' in $manifest" >&2
  exit 1
fi
