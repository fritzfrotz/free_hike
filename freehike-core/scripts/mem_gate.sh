#!/bin/bash
# SPDX-License-Identifier: Apache-2.0
#
# mem_gate.sh — the L3a memory-footprint gate (operating manual §Level 3).
#
# Samples DIRTY-ANONYMOUS memory (the metric Jetsam/LMKD actually enforce),
# NOT total RSS: a total-RSS gate would fail spuriously the moment the
# engine mmaps a multi-GB PBF, because clean file-backed pages inflate RSS
# while costing nothing against the kill ceiling.
#   - macOS:  `vmmap --summary` "Physical footprint" (dirty + swapped)
#   - Linux:  /proc/<pid>/status RssAnon
# The whole process TREE of the spawned command is sampled each tick and
# the largest single process gates (cargo wrappers spawn the real test
# binary as a child).
#
# Usage:
#   scripts/mem_gate.sh [LIMIT_MB] [-- command...]
# Defaults: LIMIT_MB=50 (the P3 pillar ceiling); command = the release-mode
# ignored Innsbruck end-to-end (the current host-side compile driver).
#
# Measured baseline 2026-07-20 (Innsbruck test-binary driver, host macOS):
# peak 61MB physical footprint vs the 50MB ceiling — the driver carries
# libtest/cargo harness overhead the budget was never meant to include, so
# the default invocation currently FAILS LOUDLY by design; see D009 for the
# clean-driver work. The ceiling itself is a HITL threshold — never widen
# it to make this script pass.
#
# DEBT(D009): mem gate needs a harness-free CLI driver plus the in-process allocator peak counter, an Austria-scale on-device run, and the iOS increased-memory entitlement — platforms: ios,core
#
# Exit codes: 0 = command succeeded and peak under limit; 1 = peak breached
# the limit; the command's own failure code otherwise.

set -u

LIMIT_MB=50
if [ "${1:-}" != "" ] && [ "${1:-}" != "--" ]; then
  LIMIT_MB="$1"
  shift
fi
[ "${1:-}" = "--" ] && shift

if [ $# -gt 0 ]; then
  CMD=("$@")
else
  CMD=(cargo test -p compiler --release -- --ignored --nocapture real_innsbruck)
fi

cd "$(dirname "$0")/.." || exit 2

descendants() {
  echo "$1"
  for c in $(pgrep -P "$1" 2>/dev/null); do
    descendants "$c"
  done
}

# Prints the dirty-anon figure for one pid in KB (empty if unreadable).
sample_kb() {
  if [ "$(uname)" = "Darwin" ]; then
    vmmap --summary "$1" 2>/dev/null | awk '
      /Physical footprint:/ {
        v = $3
        if (v ~ /G$/)      { sub(/G$/, "", v); print v * 1024 * 1024 }
        else if (v ~ /M$/) { sub(/M$/, "", v); print v * 1024 }
        else if (v ~ /K$/) { sub(/K$/, "", v); print v }
        else               { print v / 1024 }
        exit
      }'
  else
    awk '/^RssAnon:/ {print $2; exit}' "/proc/$1/status" 2>/dev/null
  fi
}

# Warm the build UNSAMPLED when the default cargo driver is used — a cold
# `cargo test --release` spawns rustc processes whose footprint would
# otherwise pollute the tree max (rustc is not the thing under gate).
if [ "${CMD[0]}" = "cargo" ]; then
  cargo test -p compiler --release --no-run >/dev/null 2>&1
fi

"${CMD[@]}" &
ROOT_PID=$!
PEAK_KB=0

while kill -0 "$ROOT_PID" 2>/dev/null; do
  for pid in $(descendants "$ROOT_PID"); do
    kb=$(sample_kb "$pid")
    if [ -n "${kb:-}" ]; then
      kb=${kb%.*}
      [ "$kb" -gt "$PEAK_KB" ] 2>/dev/null && PEAK_KB=$kb
    fi
  done
  sleep 0.5
done

wait "$ROOT_PID"
CMD_STATUS=$?

LIMIT_KB=$((LIMIT_MB * 1024))
PEAK_MB=$((PEAK_KB / 1024))
echo "mem_gate: peak dirty-anon ${PEAK_MB}MB (${PEAK_KB}KB) — limit ${LIMIT_MB}MB"

if [ "$CMD_STATUS" -ne 0 ]; then
  echo "mem_gate: command failed with status $CMD_STATUS"
  exit "$CMD_STATUS"
fi
if [ "$PEAK_KB" -gt "$LIMIT_KB" ]; then
  echo "mem_gate: FAIL — dirty-anon peak breached the ${LIMIT_MB}MB ceiling (P3 pillar)"
  exit 1
fi
echo "mem_gate: PASS"
