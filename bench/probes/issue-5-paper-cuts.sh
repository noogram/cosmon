#!/usr/bin/env bash
# Probe #5 — PAPER CUTS (static, headless), scoped to worker-FACING surfaces.
#
# The tester's first-contact defects live in the *generated worker prompt /
# persona* text a worker actually reads: a mangled choosealicense.com URL,
# hard-coded `/srv/cosmon` persona paths emitted INTO a prompt, and a raw
# `git diff` usage dump.
#
# v1-bench BUG (fixed here): the old probe grepped ALL of crates/ (including
# doc-comments `///` `//!`, plain `//` comments, tests, examples, and testkit
# crates) for the bare substring `/srv/cosmon/`. That flags every legitimate
# mention of the daemon's galaxies-root convention and every doc comment — 37
# noise hits — and reported a spurious RED. It also NEVER emitted GREEN, so a
# genuinely-fixed paper cut read as INCONCLUSIVE.
#
# This probe (a) scans only worker-facing code (excludes comment-only lines,
# tests, examples, fixtures, *-testkit, and the cosmon-daemon crate whose job is
# literally to serve `/srv/cosmon/`), and (b) VALIDATES each pattern against the
# v0.2.1 baseline so absence-on-HEAD is meaningful:
#   RED           a paper-cut pattern still matches worker-facing HEAD source.
#   GREEN         the pattern matched the v0.2.1 baseline (worker-facing) and is
#                 GONE on HEAD — a validated, measured fix.
#   INCONCLUSIVE  the pattern matches neither baseline nor HEAD (it shifted; a
#                 human must re-derive it) — never a silent GREEN.

source "$(dirname "${BASH_SOURCE[0]}")/../lib/common.sh"

ID="issue-5-paper-cuts"
NAME="Paper cuts (worker-facing): mangled license URL, /srv/cosmon persona paths, git diff dump"
ADAPTER="static"

SRC="${1:-}"
CLEANUP=0
if [[ -z "$SRC" ]]; then
  SRC="$(mktemp -d)"; CLEANUP=1
  checkout_uut "$SRC"
fi

EVIDENCE="$EVIDENCE_OUT/$ID.txt"
: > "$EVIDENCE"

# Each entry: LABEL::REGEX. Patterns target worker-FACING strings.
# NB the "mangled-license-url" pattern also catches choosetenant_auditornse.com,
# a garbled/nonexistent canonical-source domain emitted into the GENERATED worker
# prompt (crates/cosmon-cli/src/cmd/tackle.rs build_prompt) as a place to fetch
# licence/canonical text — the exact first-contact bad-URL paper cut. The v1
# probe missed it; the null-context judge surfaced it.
PATTERNS=(
  "mangled-license-url::choosealicense\.com|chooselicense|choose-a-license|choosalicense|choosetenant_auditornse|choosetenant"
  "srv-cosmon-persona-in-prompt::/srv/cosmon/[A-Za-z0-9_./-]*persona"
  "git-diff-usage-dump::git diff --help|usage: git diff|git-diff\(1\)"
)

# worker_facing_hits REGEX ROOT
# Grep ROOT/crates for REGEX in worker-facing source only: drop comment-only
# lines, tests, examples, fixtures, testkit crates, and the daemon crate.
worker_facing_hits() {
  local regex="$1" root="$2"
  rg -n --no-heading "$regex" "$root/crates" -t rust 2>/dev/null \
    | rg -v "/tests/|_test\.rs|#\[cfg\(test\)\]|/target/|/examples\.rs|/fixtures/|-testkit/|/cosmon-daemon/" \
    | rg -v ':[0-9]+:\s*(///|//!|//)' \
    || true
}

{
  echo "# Probe #5 paper-cuts (worker-facing scan, baseline-validated)"
  echo "# unit-under-test: $COSMON_TAG   baseline: v0.2.1"
  echo
} >> "$EVIDENCE"

# Materialise the v0.2.1 baseline once for validation (best-effort).
BASE="$(mktemp -d)"
BASE_OK=0
if git -C "$REPO_ROOT" rev-parse v0.2.1 >/dev/null 2>&1; then
  git -C "$REPO_ROOT" archive v0.2.1 | tar -x -C "$BASE" && BASE_OK=1
fi

declare -a SIG_PARTS=()
RED_COUNT=0
GREEN_COUNT=0
INCONCLUSIVE_COUNT=0

for entry in "${PATTERNS[@]}"; do
  label="${entry%%::*}"
  regex="${entry#*::}"
  head_hits="$(worker_facing_hits "$regex" "$SRC")"
  head_n=0; [[ -n "$head_hits" ]] && head_n="$(printf '%s\n' "$head_hits" | grep -c .)"
  base_n=0
  if [[ "$BASE_OK" -eq 1 ]]; then
    base_hits="$(worker_facing_hits "$regex" "$BASE")"
    [[ -n "$base_hits" ]] && base_n="$(printf '%s\n' "$base_hits" | grep -c .)"
  fi
  echo "## pattern: $label   (/$regex/)   baseline=$base_n head=$head_n" >> "$EVIDENCE"
  if [[ "$head_n" -gt 0 ]]; then
    printf '%s\n' "$head_hits" | sed "s#$SRC/##g" >> "$EVIDENCE"
    state="RED"; RED_COUNT=$((RED_COUNT+1))
  elif [[ "$base_n" -gt 0 ]]; then
    echo "  GONE on HEAD (present in v0.2.1 baseline) — validated fix." >> "$EVIDENCE"
    state="GREEN"; GREEN_COUNT=$((GREEN_COUNT+1))
  else
    echo "  no match on baseline OR HEAD — pattern shifted; human must re-derive." >> "$EVIDENCE"
    state="INCONCLUSIVE"; INCONCLUSIVE_COUNT=$((INCONCLUSIVE_COUNT+1))
  fi
  SIG_PARTS+=("$label=b$base_n/h$head_n:$state")
  echo >> "$EVIDENCE"
done

rm -rf "$BASE"

SIG="$(IFS=' '; echo "${SIG_PARTS[*]}")"

# Overall: any live worker-facing hit is RED. Else, if at least one validated
# fix and no unresolved shifts, GREEN. Else INCONCLUSIVE.
if [[ "$RED_COUNT" -gt 0 ]]; then
  VERDICT="RED"
  NOTE="$RED_COUNT paper-cut pattern(s) still present in worker-facing source on this tree; see evidence for file:line. (baseline-validated scan, comments/tests/daemon excluded)."
elif [[ "$GREEN_COUNT" -gt 0 && "$INCONCLUSIVE_COUNT" -eq 0 ]]; then
  VERDICT="GREEN"
  NOTE="All $GREEN_COUNT validated paper-cut pattern(s) present in v0.2.1 are GONE from worker-facing HEAD source — measured fixes. The v1 bench's 37-hit RED was noise from doc-comments, tests, and the daemon's legitimate /srv/cosmon galaxies-root convention."
else
  VERDICT="INCONCLUSIVE"
  NOTE="No worker-facing hit on HEAD, but $INCONCLUSIVE_COUNT pattern(s) also absent from the v0.2.1 baseline (shifted strings) and $GREEN_COUNT validated-fixed — a human must re-derive the shifted patterns before claiming fully fixed."
fi

emit_probe "$ID" "$NAME" "$ADAPTER" "$VERDICT" "$SIG" "$EVIDENCE" "$NOTE"

[[ "$CLEANUP" -eq 1 ]] && rm -rf "$SRC"
exit 0
