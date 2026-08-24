#!/usr/bin/env bash
# Run one command in a pinned image, with nothing it was not given.
#
# # Why this exists
#
# Several things this project claims are true of *other people's machines* and
# cannot be checked on the machine that develops it. The .deb is assembled here
# by hand from `ar` and `tar`, and Arch has no `dpkg` to open it with. The RPM
# spec has been written since Phase 16 opened and has never been built, for
# want of `rpmbuild`. "Follow the README on a clean machine" has no clean
# machine. A container supplies the missing half — a different distribution's
# userland — and nothing else, which is exactly the half that was missing.
#
# # Why it is not just `docker run`
#
# Because the defaults are the wrong way round. `docker run` gives a container
# the network, full capabilities in its namespace, and whatever image is behind
# a mutable tag at the moment it is typed. Every one of those is something this
# project would rather grant deliberately:
#
#   - **No network.** A verification step that can reach the internet can pass
#     by downloading a thing it was supposed to find in the package. `--net` is
#     for the two steps that genuinely install from a distribution's archive,
#     and it is visible at those call sites.
#   - **No capabilities**, unless asked for. `dpkg` and `rpm` need a few to set
#     ownership on the files they unpack; nothing else here needs any, and the
#     ones that are granted are named in this file with what they are for.
#   - **No mutable tags.** Images come from `packaging/images.tsv` by nickname
#     and are run by digest. An image not written down there cannot be run from
#     here at all.
#   - **Nothing writable by default.** Mounts are read-only unless `--rw` says
#     otherwise, so a verification cannot quietly edit the tree it is verifying.
#
# # Usage
#
#   scripts/in-container.sh [options] <nickname> -- <command> [args...]
#
#   --ro <src>:<dst>   mount a host path read-only
#   --rw <src>:<dst>   mount a host path writable — for output directories
#   --net              allow network access, for installing from an archive
#   --caps <a,b,c>     add capabilities, lowercase, without the CAP_ prefix
#   --tmpfs <dst>      a writable scratch directory that never touches the host
#   --user <spec>      run as this user (default: the image's own)
#   --built <tag>      an image this repository builds, in place of a nickname
#
# Everything after `--` is the command, passed to the image's shell.
set -uo pipefail

here=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
manifest="$here/packaging/images.tsv"

die() { echo "  ✗ in-container: $*" >&2; exit 1; }

mounts=()
network="none"
caps=()
tmpfs=()
user=""

while [ $# -gt 0 ]; do
  case "$1" in
    --ro) mounts+=("--volume" "${2}:ro"); shift 2 ;;
    --rw) mounts+=("--volume" "$2"); shift 2 ;;
    --net) network="bridge"; shift ;;
    --caps) IFS=',' read -ra want <<< "$2"; for c in "${want[@]}"; do caps+=("--cap-add=$c"); done; shift 2 ;;
    --tmpfs) tmpfs+=("--tmpfs" "$2:rw,exec,nosuid,size=512m"); shift 2 ;;
    # An image this repository builds itself, named by tag rather than by
    # digest because it does not exist until something here builds it. It is
    # not unpinned: its own `FROM` is a digest from the manifest, and
    # scripts/check-images.sh holds it to that.
    --built) built="$2"; shift 2 ;;
    --user) user="$2"; shift 2 ;;
    --) shift; break ;;
    -*) die "unknown option \"$1\"" ;;
    *) nickname="$1"; shift ;;
  esac
done

[ $# -gt 0 ] || die "no command to run; everything after \`--\` is the command"

if [ -n "${built:-}" ]; then
  # Built here, from a base the manifest pins. `docker image inspect` rather
  # than building on demand: a check that quietly builds its own tooling is a
  # check that can take twenty minutes for reasons the caller did not ask for.
  docker image inspect "$built" >/dev/null 2>&1 \
    || die "no image called \"$built\" has been built here yet"
  image="$built"
else
  [ -n "${nickname:-}" ] || die "no image nickname — see packaging/images.tsv for the names"
  [ -f "$manifest" ] || die "no image manifest at $manifest"

  # The manifest decides. A nickname that is not in it cannot be run, which
  # is what stops a new step reaching for an unreviewed image in a hurry.
  line=$(grep -v '^#' "$manifest" | awk -F'\t' -v n="$nickname" '$1 == n { print; exit }')
  [ -n "$line" ] || die "no image called \"$nickname\" in packaging/images.tsv"

  reference=$(printf '%s' "$line" | cut -f2)
  digest=$(printf '%s' "$line" | cut -f3)

  case "$digest" in
    sha256:*) ;;
    *) die "the pin for \"$nickname\" is \"$digest\", which is not a digest" ;;
  esac

  # Tag and digest together: the tag so a person reading the logs knows what
  # it is, the digest so the tag has no say in what runs.
  image="${reference%%:*}@${digest}"
  case "$reference" in
    *@*) image="$reference" ;;
  esac
fi

# `--rm` because none of these are meant to outlive the check that started
# them, and a stopped container holding a half-installed package is a thing
# somebody debugs for twenty minutes before noticing the date on it.
exec docker run --rm \
  --network "$network" \
  --cap-drop=ALL \
  "${caps[@]+"${caps[@]}"}" \
  --security-opt=no-new-privileges \
  ${user:+--user "$user"} \
  "${tmpfs[@]+"${tmpfs[@]}"}" \
  "${mounts[@]+"${mounts[@]}"}" \
  "$image" "$@"
