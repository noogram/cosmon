#!/usr/bin/env bash
# Witness for INV-ADR-OPTIONS-CONSIDERED (ADR-082).
set -uo pipefail

INV="INV-ADR-OPTIONS-CONSIDERED"
SCRIPT_DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" && pwd )"
ROOT="$SCRIPT_DIR/../.."
report="$( bash "$ROOT/scripts/architecture-audit.sh" --report /dev/stdout 2>&1 )"
row="$( echo "$report" | grep -E "\| $INV \|" || true )"
if [[ -z "$row" ]]; then echo "test: audit produced no row for $INV" >&2; exit 2; fi
status="$( echo "$row" | awk -F'|' '{ gsub(/ /,"",$2); print $2 }' )"
case "$status" in
  PASS|SKIP|WARN) echo "$INV: $status"; exit 0 ;;
  FAIL) echo "$INV: FAIL — see scripts/architecture-audit.sh --check"; exit 1 ;;
  *) echo "$INV: unexpected status '$status'"; exit 2 ;;
esac
