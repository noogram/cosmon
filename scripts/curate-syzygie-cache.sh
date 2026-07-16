#!/usr/bin/env bash
# curate-syzygie-cache.sh — build the cross-galaxy citation cache used by
# the curate-patrol classifier (row 4: syzygie_cited == true → Surface).
#
# Greps every peer galaxy's `docs/lore/CHRONICLES.md` and `docs/adr/`
# directory for cosmon molecule IDs (kind-YYYYMMDD-xxxx pattern) and
# writes a sorted, deduped list of cited IDs — one per line — to the
# cache path. The cache is meant to be generated ONCE per patrol night,
# then re-read per-molecule by `curate-classify.sh`, satisfying carnot's
# "don't grep per-molecule" budget constraint.
#
# Usage:
#   scripts/curate-syzygie-cache.sh [--galaxies-root <dir>] [--out <path>]
#
# Defaults:
#   --galaxies-root  ~/galaxies
#   --out            stdout
#
# Exit codes:
#   0  cache written (or empty — that's fine, no citations is valid)
#   2  galaxies-root does not exist

set -euo pipefail

GALAXIES_ROOT="${HOME}/galaxies"
OUT_PATH=""

while [[ $# -gt 0 ]]; do
  case "$1" in
    --galaxies-root) GALAXIES_ROOT="$2"; shift 2 ;;
    --out)           OUT_PATH="$2"; shift 2 ;;
    -h|--help)
      sed -n '2,/^$/p' "$0" | sed 's/^# \{0,1\}//'
      exit 0
      ;;
    *) echo "unknown arg: $1" >&2; exit 64 ;;
  esac
done

if [[ ! -d "$GALAXIES_ROOT" ]]; then
  echo "galaxies root not found: $GALAXIES_ROOT" >&2
  exit 2
fi

# Pattern: cosmon short ID = <kind>-YYYYMMDD-<4 hex>
# Kinds whitelisted: task, idea, decision, deliberation, issue, signal,
# spark, delib, mol, mission, patrol, vg, retro, sess, drift.
# The whitelist is permissive on purpose — false positives in the
# syzygie set are cheap (one extra Surface) but false negatives leak.
PATTERN='\b(task|idea|decision|deliberation|delib|issue|signal|spark|mol|mission|patrol|vg|retro|sess|drift)-[0-9]{8}-[0-9a-f]{4}\b'

emit() {
  # shellcheck disable=SC2010
  for galaxy_dir in "$GALAXIES_ROOT"/*/; do
    local g_name
    g_name="$(basename "$galaxy_dir")"
    # Defensive: skip if neither target path exists.
    local chron="${galaxy_dir}docs/lore/CHRONICLES.md"
    local adr_dir="${galaxy_dir}docs/adr"
    if [[ -f "$chron" ]]; then
      grep -hEo "$PATTERN" "$chron" 2>/dev/null || true
    fi
    if [[ -d "$adr_dir" ]]; then
      grep -rhEo "$PATTERN" "$adr_dir" 2>/dev/null || true
    fi
    # Reference the galaxy in a comment-channel so downstream debug
    # tooling can attribute a citation; the body of stdout stays
    # mol-id-per-line for the classifier.
    : "$g_name"
  done | LC_ALL=C sort -u
}

if [[ -n "$OUT_PATH" ]]; then
  emit > "$OUT_PATH"
else
  emit
fi
