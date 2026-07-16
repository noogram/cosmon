#!/usr/bin/env bash
# run_all.sh — run the full lifecycle smoke suite (happy path + fault
# injection variants) and aggregate the verdict.
#
# This is what CI invokes and what an operator runs before merging a
# change to tackle.rs / done.rs / evolve.rs / wait.rs. Target wall-clock:
# <2 minutes total (the individual scripts aim for <15s each).
#
# Exit codes:
#   0  every script passed
#   1  at least one script failed
#   2  harness setup error

set -u

HERE="$(cd "$(dirname "$0")" && pwd)"
REPO="$(cd "$HERE/../../.." && pwd)"

SCRIPTS=(
    "$HERE/../full-lifecycle-smoke.sh"
    "$HERE/fault-exit-42.sh"
    "$HERE/fault-hang.sh"
    "$HERE/fault-segfault.sh"
)

total=0
fail=0
start="$(date +%s)"

echo "smoke-suite: running ${#SCRIPTS[@]} scripts"
echo "═══════════════════════════════════════════════════════════════════════"

for script in "${SCRIPTS[@]}"; do
    if [ ! -x "$script" ]; then
        echo "smoke-suite: $script not executable" >&2
        exit 2
    fi
    total=$((total + 1))
    echo
    echo "──── $(basename "$script") ────"
    if bash "$script"; then
        :
    else
        rc=$?
        if [ "$rc" = "2" ]; then
            echo "smoke-suite: setup error in $(basename "$script")" >&2
            exit 2
        fi
        fail=$((fail + 1))
    fi
done

elapsed=$(($(date +%s) - start))
echo
echo "═══════════════════════════════════════════════════════════════════════"
printf 'smoke-suite: %d/%d scripts passed  (%ds)\n' \
    "$((total - fail))" "$total" "$elapsed"

if [ "$fail" -gt 0 ]; then
    exit 1
fi
exit 0
