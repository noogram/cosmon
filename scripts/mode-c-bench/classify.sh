#!/usr/bin/env bash
# classify.sh — verdict for ONE mode-C replay: RECOVERED | DIED | INCONCLUSIVE.
#
# The whole scientific value of the bench lives here: it refuses to score a
# run PASS unless the 500 actually FIRED (the typed re-inject event is on
# disk). A worker that self-chunked and never tripped the parser gets
# INCONCLUSIVE, not a false victory.
#
# Usage:
#   classify.sh <molecule_dir> [--state completed|stuck|...] [--json]
#   classify.sh --selftest        # offline, deterministic, no ollama/cs needed
#
# Resolution of the two derived signals:
#   completed  : from `cs observe <id> --json | jq -r .state == "completed"`,
#                unless --state overrides (fixtures / offline).
#   artefacts  : non-empty synthesis.md / responses/* / frame.md in the dir.
#
# Provenance: delib-20260707-df9b §M-BENCH.
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=lib.sh
source "$HERE/lib.sh"

selftest() {
  local fail=0
  # Each case: fixture, forced-state, forced-artefacts(0/1), expected verdict.
  # The triples exercise every branch of classify_verdict independently of
  # the filesystem-derived signals.
  check() { # $1=name $2=events $3=completed $4=artefacts $5=expected
    local got; got=$(classify_verdict "$2" "$3" "$4")
    if [[ "$got" == "$5" ]]; then
      printf '  ok   %-28s → %s\n' "$1" "$got"
    else
      printf '  FAIL %-28s → %s (expected %s)\n' "$1" "$got" "$5"; fail=1
    fi
  }
  echo "classify.sh --selftest (fixtures under $HERE/fixtures)"
  # RECOVERED: fired, completed, artefacts, no death.
  check "recovered"            "$HERE/fixtures/recovered.events.jsonl"    1 1 RECOVERED
  # DIED: fired, stuck, no artefacts.
  check "died"                 "$HERE/fixtures/died.events.jsonl"         0 0 DIED
  # INCONCLUSIVE (no fire): self-chunked, completed WITH artefacts — the
  # load-bearing false-PASS guard.
  check "inconclusive-no-fire" "$HERE/fixtures/inconclusive.events.jsonl" 1 1 INCONCLUSIVE
  # AMBIGUOUS: fired, death marker AND completed/artefacts — never upgraded.
  check "ambiguous"            "$HERE/fixtures/ambiguous.events.jsonl"     1 1 AMBIGUOUS
  # Guard: a RECOVERED-shaped events file but NO artefacts is NOT recovered.
  check "recovered-no-artefact" "$HERE/fixtures/recovered.events.jsonl"   1 0 AMBIGUOUS
  # Guard: DIED events but marked completed is ambiguous, not DIED.
  check "died-but-completed"   "$HERE/fixtures/died.events.jsonl"         1 1 AMBIGUOUS

  echo "--- batch predicate ---"
  checkb() { # $1=name $2=stdin-verdicts $3=expected
    local got; got=$(printf '%s\n' $2 | batch_predicate 2>/dev/null)
    if [[ "$got" == "$3" ]]; then printf '  ok   %-28s → %s\n' "$1" "$got"
    else printf '  FAIL %-28s → %s (expected %s)\n' "$1" "$got" "$3"; fail=1; fi
  }
  checkb "1rec-0died"       "RECOVERED INCONCLUSIVE INCONCLUSIVE" PASS
  checkb "any-died-fails"   "RECOVERED DIED INCONCLUSIVE"         FAIL
  checkb "all-inconclusive" "INCONCLUSIVE INCONCLUSIVE"          INCONCLUSIVE
  checkb "rec-with-amb"     "RECOVERED AMBIGUOUS"                PASS-WITH-AMBIGUITY

  if (( fail )); then echo "SELFTEST: FAIL"; return 1; fi
  echo "SELFTEST: PASS"
}

main() {
  if [[ "${1:-}" == "--selftest" ]]; then selftest; exit $?; fi

  local dir="" state="" json=0
  while [[ $# -gt 0 ]]; do
    case "$1" in
      --state) state="$2"; shift 2 ;;
      --json)  json=1; shift ;;
      *)       dir="$1"; shift ;;
    esac
  done
  [[ -n "$dir" ]] || { echo "usage: classify.sh <molecule_dir> [--state S] [--json]" >&2; exit 2; }

  local events="$dir/events.jsonl"
  local mol_id; mol_id="$(basename "$dir")"

  # completed?
  local completed=0
  if [[ -z "$state" ]]; then
    if command -v cs >/dev/null 2>&1; then
      state="$(cs observe "$mol_id" --json 2>/dev/null | jq -r '.state // empty' 2>/dev/null || true)"
    fi
  fi
  [[ "$state" == "completed" ]] && completed=1

  # artefacts?
  local artefacts=0
  has_artefacts "$dir" && artefacts=1

  local verdict; verdict=$(classify_verdict "$events" "$completed" "$artefacts")
  local fired died retry5xx
  fired=$(count_marker "$MARKER_FIRED" "$events")
  died=$(count_marker "$MARKER_DEATH" "$events")
  retry5xx=$(count_marker "$MARKER_RETRY_5XX" "$events")

  if (( json )); then
    printf '{"molecule":"%s","verdict":"%s","fired":%s,"death_markers":%s,"retry_5xx":%s,"completed":%s,"artefacts":%s,"state":"%s"}\n' \
      "$mol_id" "$verdict" "$fired" "$died" "$retry5xx" "$completed" "$artefacts" "${state:-unknown}"
  else
    printf '%-22s verdict=%-12s fired=%s death=%s retry5xx=%s completed=%s artefacts=%s\n' \
      "$mol_id" "$verdict" "$fired" "$died" "$retry5xx" "$completed" "$artefacts"
  fi
}
main "$@"
