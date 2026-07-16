#!/usr/bin/env bash
# Witness for INV-DOMAIN-PURE-NO-IO (ADR-082).
# Delegates to scripts/architecture-audit.sh and asserts this INV's row is not FAIL.
set -uo pipefail

INV="INV-DOMAIN-PURE-NO-IO"
SCRIPT_DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" && pwd )"
ROOT="$SCRIPT_DIR/../.."
report="$( bash "$ROOT/scripts/architecture-audit.sh" --report /dev/stdout 2>&1 )"
row="$( echo "$report" | grep -E "\| $INV \|" || true )"
if [[ -z "$row" ]]; then
  echo "test: audit produced no row for $INV" >&2
  exit 2
fi
status="$( echo "$row" | awk -F'|' '{ gsub(/ /,"",$2); print $2 }' )"
case "$status" in
  PASS|SKIP|WARN) echo "$INV: $status"; exit 0 ;;
  FAIL) echo "$INV: FAIL — see scripts/architecture-audit.sh --check"; exit 1 ;;
  *) echo "$INV: unexpected status '$status'"; exit 2 ;;
esac
