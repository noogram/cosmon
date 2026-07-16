#!/usr/bin/env bash
# Witness for INV-NAMED-INVARIANT-HAS-TEST (ADR-082) — the closure meta-rule.
#
# This test does double duty:
#   (1) The audit itself contains the closure check; this file delegates to that row.
#   (2) The closure check is *self-applied* — this very file is the witness for
#       its own INV. If the rule passes, the rule has at least one witness, which
#       is itself the proof obligation.
set -uo pipefail

INV="INV-NAMED-INVARIANT-HAS-TEST"
SCRIPT_DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" && pwd )"
ROOT="$SCRIPT_DIR/../.."
report="$( bash "$ROOT/scripts/architecture-audit.sh" --report /dev/stdout 2>&1 )"
row="$( echo "$report" | grep -E "\| $INV \|" || true )"
if [[ -z "$row" ]]; then echo "test: audit produced no row for $INV" >&2; exit 2; fi
status="$( echo "$row" | awk -F'|' '{ gsub(/ /,"",$2); print $2 }' )"
case "$status" in
  PASS|SKIP|WARN) echo "$INV: $status (closure: this file is the witness for itself)"; exit 0 ;;
  FAIL) echo "$INV: FAIL — see scripts/architecture-audit.sh --check"; exit 1 ;;
  *) echo "$INV: unexpected status '$status'"; exit 2 ;;
esac
