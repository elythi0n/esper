#!/usr/bin/env bash
# Build a binary .deb.
#
#     build-deb.sh <package> <outdir> <builddir>
#
# **Why this and not `dpkg-deb`.** A `.deb` is an `ar` archive holding three
# members in a fixed order, and building it from `ar` and `tar` means the
# package can be built — and therefore *checked* — on a machine that is not
# Debian. A packaging file nobody can build is a packaging file nobody knows is
# broken, and this project is developed on Arch.
#
# It follows binary-package format 2.0. What it deliberately does not do is
# replace Debian's own tooling for an upload to the archive: there is no
# `debian/rules`, no source package and no `lintian` here. It builds the thing
# a person downloads and runs `dpkg -i` on.
#
# Everything is deterministic: the same inputs produce the same bytes. Ordinary
# `tar` output is not — it carries mtimes, uids and directory order — so each
# of those is pinned rather than left to whatever the filesystem says.
set -euo pipefail

pkg=${1:?package name}
outdir=${2:?output directory}
built=${3:?directory holding the built binaries}

here=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
version=$(tr -d 'v \n' < "$here/VERSION")
: "${SOURCE_DATE_EPOCH:=$(date -u +%s)}"

case $(uname -m) in
  x86_64)  debarch=amd64 ;;
  aarch64) debarch=arm64 ;;
  *) echo "no Debian architecture name known for $(uname -m)" >&2; exit 1 ;;
esac

case $pkg in
  esper)
    summary='Secure remote desktop and terminal'
    description=' Esper gives you your own machine from somewhere else: its screen, its
 keyboard, its shell and its ports. Sessions are end-to-end encrypted between
 two devices that have been paired with each other, and no part of it depends
 on a server the project runs.'
    ;;
  esper-broker)
    summary='Introduces two Esper peers to each other'
    description=' A broker helps two machines find each other when neither can be dialled
 directly. It holds no session keys and decrypts nothing: what it introduces
 is already encrypted between the two ends.
 .
 Install this only on a server you want to act as an introducer. It is not
 needed to use Esper.'
    ;;
  *) echo "no Debian metadata for package '$pkg'" >&2; exit 1 ;;
esac

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
root=$work/root
mkdir -p "$root/DEBIAN"

"$here/scripts/package-files.sh" "$pkg" "$root" deb "$built"

# Installed-Size is in kibibytes and is what apt shows before installing.
size=$(du -ks --exclude=DEBIAN "$root" | cut -f1)

cat > "$root/DEBIAN/control" <<EOF
Package: $pkg
Version: ${version}-1
Section: net
Priority: optional
Architecture: $debarch
Maintainer: elythi0n <https://github.com/elythi0n/esper>
Installed-Size: $size
Depends: libc6
Homepage: https://github.com/elythi0n/esper
Description: $summary
$description
EOF

# md5sums, which `dpkg --verify` and `debsums` read. Paths are relative to /
# and the list is sorted, because unsorted is one more thing that would differ
# between two builds of the same input.
(
  cd "$root"
  find . -type f -not -path './DEBIAN/*' -printf '%P\n' | LC_ALL=C sort | while read -r f; do
    printf '%s  %s\n' "$(md5sum "$f" | cut -d' ' -f1)" "$f"
  done
) > "$root/DEBIAN/md5sums"

chmod 0644 "$root/DEBIAN/control" "$root/DEBIAN/md5sums"

# Two tarballs, both pinned: no mtimes from the filesystem, no uid or gid from
# whoever is building, and a stable member order.
tar_reproducibly() {
  local out=$1 dir=$2
  shift 2
  tar --create --file "$out" --directory "$dir" \
    --format=gnu \
    --sort=name \
    --mtime="@$SOURCE_DATE_EPOCH" \
    --owner=root --group=root --numeric-owner \
    "$@"
}

tar_reproducibly "$work/control.tar" "$root/DEBIAN" .
tar_reproducibly "$work/data.tar" "$root" --exclude=./DEBIAN .
gzip -9n "$work/control.tar" "$work/data.tar"

printf '2.0\n' > "$work/debian-binary"

mkdir -p "$outdir"
# Absolute, because the `ar` below runs from the staging directory and a
# relative path would land there instead.
outdir=$(cd -- "$outdir" && pwd)
deb=$outdir/${pkg}_${version}-1_${debarch}.deb
rm -f "$deb"
# `D` is deterministic mode: zeroed mtime, uid and gid in the ar headers. The
# member order is required by the format, not a preference.
(cd "$work" && ar rcD "$deb" debian-binary control.tar.gz data.tar.gz)

echo "$deb"
