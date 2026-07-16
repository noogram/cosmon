#!/usr/bin/env bash
# license-table.sh — emit & validate the per-crate license partition.
#
# Source of truth: this script walks every `crates/*/Cargo.toml`, resolves
# each crate's effective license (workspace inheritance or per-package
# override), and looks up the (tier, license, rationale) triple in
# `scripts/license-rationales.tsv`. It then renders a markdown table
# grouped by tier — the same shape as ADR-092 §3, kept alive against
# workspace drift.
#
# Why this exists: ADR-092 §3 inscribed the partition at 60 crates on
# 2026-05-09. Three weeks later the workspace had 63, and the three new
# almanac-* crates had silently inherited the AGPL workspace default
# without anyone applying the placement test. The living table closes
# that loop (chronicled 2026-05-17).
#
# Usage:
#   bash scripts/license-table.sh              # emit the block to stdout
#   bash scripts/license-table.sh --check      # CI gate: exit 1 on drift
#   bash scripts/license-table.sh --write      # rewrite the block in
#                                              # docs/license/INDEX.md
#
# Failure modes (all fail-closed):
#   - A Cargo.toml declares a license that is neither AGPL-3.0-only nor
#     Apache-2.0 (someone invented a third licence).
#   - A crate has no row in license-rationales.tsv (new crate added
#     without curating its placement → row marked NEEDS-RATIONALE).
#   - The mapping says Apache-2.0 but the Cargo.toml inherits AGPL (or
#     vice versa) — doc/code divergence.
#   - --check: the rendered block does not match what is currently
#     between the markers in docs/license/INDEX.md.
#
# Doctrine: ADR-092 (license bascule MPL-2.0 → AGPL-3.0 + Apache-2.0).
# The living table is tooling, not a new architectural decision — no
# successor ADR. If the pipeline grows beyond emit + check + write,
# raise the question.

set -uo pipefail

# ---------------------------------------------------------------------------
# Locate galaxy root (walk up from the script until CLAUDE.md is found).
# ---------------------------------------------------------------------------
SCRIPT_DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" && pwd )"
GALAXY_ROOT="$SCRIPT_DIR"
while [[ "$GALAXY_ROOT" != "/" && ! -f "$GALAXY_ROOT/CLAUDE.md" ]]; do
  GALAXY_ROOT="$( dirname "$GALAXY_ROOT" )"
done
if [[ ! -f "$GALAXY_ROOT/CLAUDE.md" ]]; then
  echo "license-table: cannot locate galaxy root (no CLAUDE.md ancestor of $SCRIPT_DIR)" >&2
  exit 2
fi
cd "$GALAXY_ROOT"

MAPPING_FILE="scripts/license-rationales.tsv"
INDEX_FILE="docs/license/INDEX.md"
BEGIN_MARKER="<!-- BEGIN LICENSE TABLE -->"
END_MARKER="<!-- END LICENSE TABLE -->"
ALLOWED_LICENSES=("AGPL-3.0-only" "Apache-2.0")

if [[ ! -f "$MAPPING_FILE" ]]; then
  echo "license-table: missing mapping file $MAPPING_FILE" >&2
  exit 2
fi

# ---------------------------------------------------------------------------
# Argument parsing
# ---------------------------------------------------------------------------
MODE="emit"
while [[ $# -gt 0 ]]; do
  case "$1" in
    --check) MODE="check"; shift ;;
    --write) MODE="write"; shift ;;
    --help|-h)
      sed -n '1,/^set -uo pipefail/p' "$0" | sed 's/^# \{0,1\}//'
      exit 0
      ;;
    *)
      echo "license-table: unknown argument '$1' (try --help)" >&2
      exit 2
      ;;
  esac
done

# ---------------------------------------------------------------------------
# Parse the workspace default license from the root Cargo.toml.
# ---------------------------------------------------------------------------
WORKSPACE_LICENSE="$(
  awk '
    /^\[workspace\.package\]/ { in_pkg = 1; next }
    /^\[/ && !/^\[workspace\.package\]/ { in_pkg = 0 }
    in_pkg && /^license[[:space:]]*=/ {
      line = $0
      sub(/^license[[:space:]]*=[[:space:]]*"/, "", line)
      sub(/"[[:space:]]*$/, "", line)
      print line
      exit
    }
  ' Cargo.toml
)"
if [[ -z "$WORKSPACE_LICENSE" ]]; then
  echo "license-table: failed to parse [workspace.package].license from Cargo.toml" >&2
  exit 2
fi

# ---------------------------------------------------------------------------
# Load the hand-curated mapping (TSV: crate \t tier \t license \t rationale).
# ---------------------------------------------------------------------------
declare -A MAP_TIER MAP_LICENSE MAP_RATIONALE
while IFS=$'\t' read -r crate tier license rationale; do
  [[ -z "$crate" || "${crate:0:1}" == "#" ]] && continue
  MAP_TIER[$crate]="$tier"
  MAP_LICENSE[$crate]="$license"
  MAP_RATIONALE[$crate]="$rationale"
done < "$MAPPING_FILE"

# ---------------------------------------------------------------------------
# For each crate dir, resolve effective license from its Cargo.toml.
# Returns: empty string if no license line found.
#          "AGPL-3.0-only" (or whatever) if explicit override.
#          $WORKSPACE_LICENSE if `license.workspace = true`.
# ---------------------------------------------------------------------------
crate_license() {
  local toml="$1"
  local line
  # Look for top-level [package] license line (workspace inheritance or
  # explicit string). Stop at the next section so we don't pick up a
  # [dependencies] table by accident.
  line="$(
    awk '
      /^\[package\]/ { in_pkg = 1; next }
      /^\[/ && !/^\[package\]/ { in_pkg = 0 }
      in_pkg && /^license[[:space:]]*[\.=]/ {
        print
        exit
      }
    ' "$toml"
  )"
  if [[ -z "$line" ]]; then
    echo ""
    return
  fi
  if [[ "$line" == *"workspace"* && "$line" == *"true"* ]]; then
    echo "$WORKSPACE_LICENSE"
    return
  fi
  # Explicit string: license = "Apache-2.0"
  echo "$line" | sed -E 's/.*=[[:space:]]*"([^"]+)".*/\1/'
}

is_allowed_license() {
  local lic="$1"
  for allowed in "${ALLOWED_LICENSES[@]}"; do
    [[ "$lic" == "$allowed" ]] && return 0
  done
  return 1
}

# ---------------------------------------------------------------------------
# Walk every crate, build the table in memory.
# ---------------------------------------------------------------------------
ERRORS=()
CORE_ROWS=()
FRONTIER_ROWS=()
NEEDS_RATIONALE_ROWS=()
TOTAL=0

# Sort crate dirs alphabetically for determinism.
while IFS= read -r dir; do
  TOTAL=$((TOTAL + 1))
  crate="$(basename "$dir")"
  toml="$dir/Cargo.toml"
  declared="$(crate_license "$toml")"

  if [[ -z "$declared" ]]; then
    ERRORS+=("crate '$crate' has no license line in $toml")
    continue
  fi
  if ! is_allowed_license "$declared"; then
    ERRORS+=("crate '$crate' declares unknown license '$declared' in $toml (allowed: ${ALLOWED_LICENSES[*]})")
    continue
  fi

  mapped_tier="${MAP_TIER[$crate]:-}"
  mapped_license="${MAP_LICENSE[$crate]:-}"
  mapped_rationale="${MAP_RATIONALE[$crate]:-}"

  if [[ -z "$mapped_tier" ]]; then
    NEEDS_RATIONALE_ROWS+=("| \`$crate\` | NEEDS-RATIONALE | $declared | add a row in scripts/license-rationales.tsv |")
    ERRORS+=("crate '$crate' is in the workspace but absent from $MAPPING_FILE — add a row (tier + license + rationale)")
    continue
  fi
  if [[ "$mapped_license" != "$declared" ]]; then
    ERRORS+=("crate '$crate' Cargo.toml says '$declared' but mapping says '$mapped_license' — either update the Cargo.toml or the mapping (whichever is wrong)")
    continue
  fi

  row="| \`$crate\` | $mapped_tier | $mapped_license | $mapped_rationale |"
  if [[ "$mapped_tier" == "core" ]]; then
    CORE_ROWS+=("$row")
  elif [[ "$mapped_tier" == "frontier" ]]; then
    FRONTIER_ROWS+=("$row")
  else
    ERRORS+=("crate '$crate' has unknown tier '$mapped_tier' in mapping (allowed: core, frontier)")
  fi
done < <(find crates -mindepth 1 -maxdepth 1 -type d | sort)

# Sort rows alphabetically (find already gave us sorted dirs, but re-sort
# for paranoia — the rationale text may shift order if find ever changes).
IFS=$'\n' CORE_ROWS_SORTED=($(printf '%s\n' "${CORE_ROWS[@]}" | sort))
IFS=$'\n' FRONTIER_ROWS_SORTED=($(printf '%s\n' "${FRONTIER_ROWS[@]}" | sort))
unset IFS

CORE_COUNT=${#CORE_ROWS_SORTED[@]}
FRONTIER_COUNT=${#FRONTIER_ROWS_SORTED[@]}

# ---------------------------------------------------------------------------
# Render the block content (everything that goes between the markers).
# DATE_TODAY can be overridden via env for deterministic tests.
# ---------------------------------------------------------------------------
: "${LICENSE_TABLE_DATE:=$(date +%Y-%m-%d)}"

render_block() {
  echo "## Per-crate partition (machine-generated — do not edit by hand)"
  echo
  echo "_Last generated: ${LICENSE_TABLE_DATE}, ${TOTAL} crates._"
  echo
  echo "_Run \`bash scripts/license-table.sh --write\` to refresh._"
  echo
  echo "### Core (AGPL-3.0-only) — ${CORE_COUNT} crates"
  echo
  echo "| Crate | Tier | Licence | Rationale |"
  echo "|-------|------|---------|-----------|"
  if [[ $CORE_COUNT -gt 0 ]]; then
    printf '%s\n' "${CORE_ROWS_SORTED[@]}"
  fi
  echo
  echo "### Frontier (Apache-2.0) — ${FRONTIER_COUNT} crates"
  echo
  echo "| Crate | Tier | Licence | Rationale |"
  echo "|-------|------|---------|-----------|"
  if [[ $FRONTIER_COUNT -gt 0 ]]; then
    printf '%s\n' "${FRONTIER_ROWS_SORTED[@]}"
  fi
  if [[ ${#NEEDS_RATIONALE_ROWS[@]} -gt 0 ]]; then
    echo
    echo "### Needs curation"
    echo
    echo "| Crate | Tier | Licence | Rationale |"
    echo "|-------|------|---------|-----------|"
    printf '%s\n' "${NEEDS_RATIONALE_ROWS[@]}"
  fi
}

BLOCK_CONTENT="$(render_block)"

# ---------------------------------------------------------------------------
# Report any structural errors (unknown license, missing rationale, drift).
# ---------------------------------------------------------------------------
report_errors() {
  if [[ ${#ERRORS[@]} -gt 0 ]]; then
    echo "license-table: ${#ERRORS[@]} error(s):" >&2
    for err in "${ERRORS[@]}"; do
      echo "  - $err" >&2
    done
    return 1
  fi
  return 0
}

# ---------------------------------------------------------------------------
# Extract the current block from INDEX.md (between markers), or empty if absent.
# Strips the date line so doc/code comparison is content-only.
# ---------------------------------------------------------------------------
extract_current_block() {
  if [[ ! -f "$INDEX_FILE" ]]; then
    return
  fi
  awk -v begin="$BEGIN_MARKER" -v end="$END_MARKER" '
    $0 == begin { capture = 1; next }
    $0 == end   { capture = 0 }
    capture     { print }
  ' "$INDEX_FILE"
}

strip_date_line() {
  grep -v '^_Last generated:' || true
}

# ---------------------------------------------------------------------------
# Modes
# ---------------------------------------------------------------------------
case "$MODE" in
  emit)
    report_errors || true   # show warnings but still emit table
    echo "$BLOCK_CONTENT"
    # Non-zero exit if there were structural errors, so `> file` callers
    # notice before committing a stale block.
    [[ ${#ERRORS[@]} -eq 0 ]] || exit 1
    ;;

  check)
    fail=0
    if ! report_errors; then
      fail=1
    fi
    current="$(extract_current_block)"
    if [[ -z "$current" ]]; then
      echo "license-table: $INDEX_FILE has no $BEGIN_MARKER / $END_MARKER block — run scripts/license-table.sh --write" >&2
      fail=1
    else
      rendered_nodate="$(echo "$BLOCK_CONTENT" | strip_date_line)"
      current_nodate="$(echo "$current"        | strip_date_line)"
      if [[ "$rendered_nodate" != "$current_nodate" ]]; then
        echo "license-table: $INDEX_FILE is stale — content drift between rendered table and on-disk block." >&2
        echo "  Fix: bash scripts/license-table.sh --write" >&2
        echo >&2
        echo "--- on-disk block (current)" >&2
        echo "+++ rendered block (expected)" >&2
        diff <(echo "$current_nodate") <(echo "$rendered_nodate") >&2 || true
        fail=1
      fi
    fi
    exit "$fail"
    ;;

  write)
    if ! report_errors; then
      echo "license-table: refusing to --write while structural errors are unresolved" >&2
      exit 1
    fi
    if [[ ! -f "$INDEX_FILE" ]]; then
      echo "license-table: $INDEX_FILE does not exist" >&2
      exit 2
    fi
    if ! grep -qF "$BEGIN_MARKER" "$INDEX_FILE" || ! grep -qF "$END_MARKER" "$INDEX_FILE"; then
      echo "license-table: $INDEX_FILE is missing the marker pair ($BEGIN_MARKER / $END_MARKER)" >&2
      echo "  Add them where the block should live, then re-run --write." >&2
      exit 2
    fi
    # Use a temp file for the body — passing a multi-line string via
    # `awk -v body=...` trips BSD awk's `newline in string` warning and
    # truncates output silently on some implementations. File-based read
    # is portable across GNU awk, BSD awk (macOS), and mawk.
    body_tmp="$(mktemp)"
    out_tmp="$(mktemp)"
    printf '%s\n' "$BLOCK_CONTENT" > "$body_tmp"
    awk -v begin="$BEGIN_MARKER" -v end="$END_MARKER" -v body_file="$body_tmp" '
      {
        if ($0 == begin) {
          print
          while ((getline line < body_file) > 0) print line
          close(body_file)
          skipping = 1
          next
        }
        if ($0 == end) {
          skipping = 0
          print
          next
        }
        if (!skipping) print
      }
    ' "$INDEX_FILE" > "$out_tmp"
    mv "$out_tmp" "$INDEX_FILE"
    rm -f "$body_tmp"
    echo "license-table: wrote ${TOTAL}-crate block to $INDEX_FILE"
    ;;
esac
