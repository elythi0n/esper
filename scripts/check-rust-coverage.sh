#!/usr/bin/env bash
# What the Rust tests have never run, measured rather than guessed at.
#
# # Why this exists, and why it installs nothing
#
# The Go half has had a coverage number since `make check-coverage`; the Rust
# half — 43 crates and about 1,800 tests — had never been measured at all, so
# "which code has nothing run it" was unanswerable on the larger of the two.
# The first run answered it: `esper-engine`'s `paths.rs` sat at **0.00%**, and
# it holds the rule that refuses an empty `ESPER_HOME` rather than quietly
# writing this machine's identity key into the working directory.
#
# `cargo-llvm-cov` is the usual way and it is a `cargo install`. It is not
# needed: rustc's own LLVM version and this machine's `llvm-profdata` are the
# same, so `-C instrument-coverage` plus the system tools do the whole job.
# The version check below is the part that matters — profiles written by one
# LLVM are not readable by a very different one, and the failure is a merge
# that reports nothing rather than an error anybody would read.
#
# # Why it is not in `ci`
#
# Instrumented, it is a full rebuild of the workspace and about fifteen
# minutes. It is also not a pass/fail gate: coverage is a poor measure of
# whether code is *tested* — every defect found in this tree this week was in a
# line already counted as covered. It is good at one thing, which is naming
# code nothing has run, and that is a question worth asking deliberately rather
# than on every push.
#
# Usage: scripts/check-rust-coverage.sh [output.txt]
set -uo pipefail

here=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
out="${1:-$here/rust-coverage.txt}"

for tool in llvm-profdata llvm-cov; do
  command -v "$tool" > /dev/null || {
    echo "  ✗ rust coverage: no $tool on this machine — install LLVM, or use cargo-llvm-cov" >&2
    exit 1
  }
done

rustc_llvm=$(rustc --version --verbose | awk '/^LLVM version:/ { print $3 }' | cut -d. -f1)
tool_llvm=$(llvm-profdata --version | awk '/LLVM version/ { print $3 }' | cut -d. -f1)
if [ "$rustc_llvm" != "$tool_llvm" ]; then
  echo "  ✗ rust coverage: rustc's LLVM is $rustc_llvm and llvm-profdata is $tool_llvm." >&2
  echo "    Profiles from one are not readable by the other, and the failure is silent." >&2
  exit 1
fi

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
prof="$work/prof"; mkdir -p "$prof"

export RUSTFLAGS="-C instrument-coverage"
export LLVM_PROFILE_FILE="$prof/esper-%p-%m.profraw"

cd "$here/core"
echo "  building the workspace with coverage instrumentation…"
cargo test --workspace --no-run --target-dir "$work/target" --message-format=json 2>/dev/null \
  | python3 -c '
import sys, json
for line in sys.stdin:
    try: m = json.loads(line)
    except Exception: continue
    if m.get("profile", {}).get("test") and m.get("executable"):
        print(m["executable"])
' > "$work/binaries.txt"

count=$(wc -l < "$work/binaries.txt")
if [ "$count" -lt 20 ]; then
  echo "  ✗ rust coverage: only $count test binaries were built, which is not this workspace" >&2
  exit 1
fi
echo "  running $count test binaries…"

# `--no-fail-fast`, and the result is not the point: a test that fails under
# instrumentation still records what it reached, and two of them do fail —
# the decoder's sandbox kills the child for writing a profile, which is the
# sandbox working, and one damage threshold depends on how fast the machine is.
cargo test --workspace --no-fail-fast --target-dir "$work/target" > "$work/tests.log" 2>&1
echo "  $(grep -cE '^test result: ok' "$work/tests.log") suite(s) passed, \
$(grep -cE '^test result: FAILED' "$work/tests.log") failed under instrumentation"

llvm-profdata merge -sparse "$prof"/*.profraw -o "$work/all.profdata" || {
  echo "  ✗ rust coverage: the profiles would not merge" >&2
  exit 1
}

objects=()
while read -r b; do [ -x "$b" ] && objects+=(--object "$b"); done < "$work/binaries.txt"
llvm-cov report --instr-profile="$work/all.profdata" "${objects[@]}" \
  --ignore-filename-regex='(/\.cargo/|/rustc/|/target/)' > "$out" || {
  echo "  ✗ rust coverage: the report would not build" >&2
  exit 1
}

total=$(awk '/^TOTAL/ { print $(NF-3) }' "$out")
echo "  ✓ rust coverage: $total of lines across $count test binaries — written to $out"
echo ""
echo "  the files nothing has run:"
awk '$0 ~ /\/src\// {
  n = split($0, f, /[ \t]+/);
  lines = f[8]; pct = f[10]; gsub("%", "", pct);
  if (lines + 0 > 40 && pct + 0 < 1) printf "    %6s  %s\n", pct "%", f[1];
}' "$out" | sed 's|.*/crates/|crates/|' | head -20
