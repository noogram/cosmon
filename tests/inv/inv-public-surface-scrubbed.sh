#!/usr/bin/env bash
# Witness for INV-PUBLIC-SURFACE-SCRUBBED (ADR-082).
# The audit row for this INV delegates to scripts/publish.sh --check; this test
# delegates to the audit, which delegates to publish.sh. PASS/SKIP/WARN all pass
# the witness; only FAIL fails it. Since BLOCKER B3 (task-20260616-c789) the gate
# is active: on cosmon's live private tree publish.sh --check returns FAIL by
# design (the tree is not a published artefact — see docs/architecture-baseline.md
# W3), so this witness fails locally and passes on a scrubbed scripts/release/
# clone. That is the honest signal, not a regression.
set -uo pipefail

INV="INV-PUBLIC-SURFACE-SCRUBBED"
SCRIPT_DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" && pwd )"
ROOT="$SCRIPT_DIR/../.."
report="$( bash "$ROOT/scripts/architecture-audit.sh" --report /dev/stdout 2>&1 )"
row="$( echo "$report" | grep -E "\| $INV \|" || true )"
if [[ -z "$row" ]]; then echo "test: audit produced no row for $INV" >&2; exit 2; fi
status="$( echo "$row" | awk -F'|' '{ gsub(/ /,"",$2); print $2 }' )"
case "$status" in
  PASS|SKIP|WARN) echo "$INV: $status"; exit 0 ;;
  FAIL) echo "$INV: FAIL — run scripts/publish.sh --check"; exit 1 ;;
  *) echo "$INV: unexpected status '$status'"; exit 2 ;;
esac
