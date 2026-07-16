#!/usr/bin/env bash
# Test the cs run diamond DAG end-to-end.
#
# Creates 4 molecules (A→B+C→D), runs cs run on the root,
# verifies all complete, reports results.
#
# Usage: ./examples/test-diamond-dag.sh

set -euo pipefail

echo "=== Creating diamond DAG ==="
A=$(cs --json nucleate task-work --var "topic=diamond-test-$(date +%s) A root" | jq -r .id)
B=$(cs --json nucleate task-work --var "topic=diamond-test B branch1" --blocked-by "$A" | jq -r .id)
C=$(cs --json nucleate task-work --var "topic=diamond-test C branch2" --blocked-by "$A" | jq -r .id)
D=$(cs --json nucleate task-work --var "topic=diamond-test D sink" --blocked-by "$B" --blocked-by "$C" | jq -r .id)

echo "  A=$A"
echo "  B=$B (blocked-by A)"
echo "  C=$C (blocked-by A)"
echo "  D=$D (blocked-by B+C)"
echo ""

echo "=== DAG dependencies ==="
cs deps "$A" --transitive
echo ""

echo "=== Running cs run (no timeout, poll every 10s) ==="
cs run "$A" --poll-interval 10

echo ""
echo "=== Final state ==="
for m in "$A" "$B" "$C" "$D"; do
  status=$(cs --json observe "$m" | jq -r .status)
  echo "  $m: $status"
done

echo ""
echo "=== Verifying all completed ==="
all_done=true
for m in "$A" "$B" "$C" "$D"; do
  status=$(cs --json observe "$m" | jq -r .status)
  if [ "$status" != "completed" ]; then
    echo "FAIL: $m is $status (expected completed)"
    all_done=false
  fi
done

if $all_done; then
  echo "✅ Diamond DAG test PASSED — all 4 molecules completed"
else
  echo "❌ Diamond DAG test FAILED"
  exit 1
fi
