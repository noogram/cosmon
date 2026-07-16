#!/usr/bin/env bash
# Curate-patrol ledger — Bash library, sourced by the curate-patrol formula.
#
# Implements the append-and-flush-and-BLAKE3-seal discipline; delegates to
# scripts/curate-ledger.py for atomic I/O (Python is the only easy way to
# do O_APPEND + os.fsync from a shell context without race conditions).
#
# Usage from a formula step:
#   source scripts/curate-ledger.sh
#   curate_ledger_append "$MOLECULE_DIR/scan.ndjson" "$row_json"   # MUST succeed before action
#   cs collapse "$mol_id" --reason "..."                            # only NOW the side-effect fires
#
# See scripts/curate-ledger.py module docstring for the seal contract.

# Locate the Python helper relative to this file's dir, regardless of CWD.
_CURATE_LEDGER_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
_CURATE_LEDGER_PY="$_CURATE_LEDGER_DIR/curate-ledger.py"

# Append one sealed row to a ledger file. fsync is performed before return.
# The caller MUST check the exit code: nonzero ⇒ DO NOT fire the action.
#
# Args:
#   $1 — ledger file path (will be created if missing)
#   $2 — row JSON without `sealed` field; must contain mol_id, action, decided_at
# Stdout:
#   the BLAKE3 seal hex (so the caller can log it inline)
# Exit:
#   0 = sealed + flushed, safe to fire the action
#   2 = corrupt input (missing fields, invalid JSON) — do NOT fire
curate_ledger_append() {
    local ledger_path="$1"
    local row_json="$2"
    if [[ -z "$ledger_path" || -z "$row_json" ]]; then
        echo "curate_ledger_append: usage: <ledger-path> <row-json>" >&2
        return 2
    fi
    python3 "$_CURATE_LEDGER_PY" append "$ledger_path" "$row_json"
}

# Print already-decided mol_ids to stdout for resume-after-crash.
# The scan step pipes this through `comm -23` against the live pending set
# to skip molecules already classified in this patrol night.
#
# Args:
#   $1 — ledger file path (may or may not exist; missing = empty resume set)
# Exit:
#   0 = resume set printed (possibly empty)
#   1 = seal mismatch (corrupted/tampered ledger; abort the patrol)
#   2 = mid-file torn write (unrecoverable; operator must investigate)
curate_ledger_resume_set() {
    local ledger_path="$1"
    if [[ -z "$ledger_path" ]]; then
        echo "curate_ledger_resume_set: usage: <ledger-path>" >&2
        return 2
    fi
    python3 "$_CURATE_LEDGER_PY" resume "$ledger_path"
}

# Compose the standard `decided_at` ISO 8601 UTC timestamp. The seal
# binds to this exact string, so writer + verifier MUST use the same
# format. RFC 3339 / ISO 8601 with Z suffix, second precision.
curate_ledger_now() {
    date -u +"%Y-%m-%dT%H:%M:%SZ"
}
