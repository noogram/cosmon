#!/usr/bin/env bash
# verify-curate-ledger.sh — re-validate a curate-patrol ledger.
#
# The curate-patrol formula produces two append+flush+BLAKE3-sealed
# ledgers inside its molecule directory:
#
#   scan.ndjson       — one DrainVerdict per pending molecule classified
#   decisions.ndjson  — one applied-action line per scan row that actually
#                       triggered a side-effect (cs tag / cs collapse / ...)
#
# This is the curate-patrol analogue of `cs verify <mol_id>` (which
# verifies the briefing.md seal on a regular molecule). It re-computes
# each row's seal and cross-checks that every decision had a matching
# scan row.
#
# Usage:
#   scripts/verify-curate-ledger.sh <molecule-dir>
#
# Exit codes:
#   0 — clean (all seals valid, decisions ⊆ scans)
#   1 — seal mismatch (post-hoc edit, tampering, or writer/verifier seal
#       contract divergence)
#   2 — truncated / corrupt (torn write mid-file, not at EOF) OR usage
#       error
#
# A torn write at the LAST line of either ledger is tolerated and
# reported on stderr; the next patrol night's `resume` will skip it.
#
# See scripts/curate-ledger.py for the seal contract.

set -euo pipefail

if [[ $# -ne 1 ]]; then
    cat >&2 <<'USAGE'
verify-curate-ledger.sh <molecule-dir>

  <molecule-dir>  directory containing scan.ndjson and/or decisions.ndjson
                  (typically .cosmon/state/fleets/default/molecules/<id>/)

Exit: 0 clean | 1 seal mismatch | 2 truncated/corrupt
USAGE
    exit 2
fi

_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
exec python3 "$_DIR/curate-ledger.py" verify "$1"
