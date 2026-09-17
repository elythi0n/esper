#!/usr/bin/env bash
# Measure the encode path on a deliberately weak machine.
#
# # Why this exists
#
# v1 encodes in software on every platform, which deletes four platform
# backends and works identically on machines with no usable GPU driver — and
# `capture-and-codec.md` is honest that what it costs is headroom. The open
# question was never whether it holds up on a 20-core workstation. It is
# whether it holds up on the machines this product is actually for: an old
# laptop, a small VM, a headless box somebody has in a cupboard.
#
# Nobody has one of those here, so one is made. `systemd-run --scope` with a
# `CPUQuota` puts the run in a cgroup with a CPU ceiling — and **that is not the
# same as a slower processor**: it is this processor, stopped for part of every
# period. For an encoder with a frame deadline the difference matters, and it is
# the more pessimistic of the two, which is the right direction for a floor.
#
# `esper-bench` reports the limit in its machine description, so every number
# these runs produce is filed under the machine that produced it and none of
# them is gated against this machine's unconstrained baseline.
#
# Usage: scripts/bench-constrained.sh [quota ...]     (default: 400 200 100 50)
set -euo pipefail

command -v systemd-run >/dev/null || {
  echo "  ✗ bench-constrained: systemd-run is not available, so no cgroup can be made" >&2
  exit 1
}

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
QUOTAS=("${@:-}")
[ -z "${QUOTAS[0]:-}" ] && QUOTAS=(400 200 100 50)

# Built once, outside every cgroup: a build inside a 50% quota measures cargo.
echo "  building the benchmark first, outside every limit"
( cd "$ROOT/core" && cargo build --release --quiet -p esper-bench )
BENCH="$ROOT/core/target/release/esper-bench"
[ -x "$BENCH" ] || { echo "  ✗ bench-constrained: no esper-bench binary at $BENCH" >&2; exit 1; }

out="${ESPER_BENCH_OUT:-$ROOT/bench/constrained}"
mkdir -p "$out"

for quota in "${QUOTAS[@]}"; do
  echo
  echo "  ── ${quota}% of a core ──────────────────────────────────────────────"
  began=$(date +%s)
  # `--scope` runs it here rather than as a service, so its output is this
  # terminal's and a failure is this script's exit status.
  systemd-run --user --scope -q -p "CPUQuota=${quota}%" \
    "$BENCH" --json "$out/q${quota}.json" 2>&1 \
    | grep -vE '^\[OpenH264\]' || true
  echo "  (took $(( $(date +%s) - began ))s of wall clock)"
done

echo
echo "  ✓ bench-constrained: ${#QUOTAS[@]} run(s) in $out"
