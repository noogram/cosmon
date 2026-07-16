#!/usr/bin/env bash
# audit-emit-sites.sh — list `events.jsonl` emit-sites and flag those
# that have not been migrated to a non-`Unknown` `EmitterKind` header.
#
# Background: cosmon-ward §F1 (delib-20260509-18df, hawking) requires
# that every `events.jsonl` envelope carry an `emitter_kind` so an
# attendant-style consumer can write a *causal filter* excluding its
# own emissions:
#
#   SELECT … FROM molecules
#   WHERE NOT EXISTS (
#     SELECT 1 FROM events e
#     WHERE e.molecule_id = molecules.id
#       AND e.emitter_kind = 'Attendant'
#   )
#
# A naïve attendant that reads every event indiscriminately suffers
# auto-immune dilution. The header is the substrate guard.
#
# This script is best-effort: it greps for emit call-sites in
# `crates/cosmon-*` and reports
#   (a) the count of emit-sites,
#   (b) those still using the bare `emit_one(` / `.emit(` form
#       — i.e. not the `_with_emitter` variant — and therefore relying
#       on the `EventLogWriter` sticky default. Sticky default is OK
#       *only* if the parent process has set a non-`Unknown`
#       `COSMON_EMITTER_KIND` in the environment, which can be
#       confirmed at the process boundary.
#
# Usage:
#   bash scripts/audit-emit-sites.sh           # human-readable report
#   bash scripts/audit-emit-sites.sh --count   # just the totals
#   bash scripts/audit-emit-sites.sh --check   # exit 1 on any unmigrated leak
#
# Out of scope: inspecting *running* `events.jsonl` rows. That is the
# work of the future `cs audit emit` molecule (see substrate-audit
# prologue, cosmon-ward).

set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

mode="${1:-report}"

# Locate emit-sites (production code only; ignore test files).
mapfile -t EMIT_SITES < <(
  grep -rn --include="*.rs" \
    -E '(EventLogWriter::open|emit_one\(|\bemit\()' \
    crates/ \
  | grep -v '/tests/' \
  | grep -vE '\.rs:[0-9]+:.*#\[test\]' \
  | grep -vE '/event_log\.rs:.*tests::' \
  | sort
)

# Sites that explicitly carry an emitter (`emit_one_with_emitter`,
# `emit_with_emitter`, `set_emitter`). These are the migrated ones.
mapfile -t MIGRATED < <(
  grep -rn --include="*.rs" \
    -E '(emit_one_with_emitter|emit_with_emitter|set_emitter)\(' \
    crates/ \
  | sort
)

total="${#EMIT_SITES[@]}"
migrated="${#MIGRATED[@]}"
default_sticky=$((total - migrated))

case "$mode" in
  --count)
    echo "emit_sites_total=${total}"
    echo "emit_sites_migrated=${migrated}"
    echo "emit_sites_default_sticky=${default_sticky}"
    ;;
  --check)
    if [[ ${default_sticky} -gt 0 ]]; then
      echo "WARN: ${default_sticky} emit-site(s) rely on the writer's sticky default emitter."
      echo "      This is acceptable only when COSMON_EMITTER_KIND is set in the parent process."
      echo "      Run 'bash scripts/audit-emit-sites.sh' for the full list."
      exit 1
    fi
    echo "OK: every emit-site explicitly classifies its emitter."
    ;;
  *)
    echo "=== emit-sites in production code (excludes tests) ==="
    printf '%s\n' "${EMIT_SITES[@]}"
    echo
    echo "=== migrated (carry an explicit emitter) ==="
    printf '%s\n' "${MIGRATED[@]}"
    echo
    echo "=== summary ==="
    echo "total            : ${total}"
    echo "migrated         : ${migrated}"
    echo "default-sticky   : ${default_sticky}"
    echo
    echo "Sticky default is acceptable if and only if the parent process"
    echo "has exported COSMON_EMITTER_KIND (e.g. 'cli', 'worker', 'patrol')."
    echo "See cosmon-ward §F1 (delib-20260509-18df) and"
    echo "task-20260509-7210 for the substrate motivation."
    ;;
esac
