#!/usr/bin/env bash
# smoke-dispatch.sh (worktree ROOT) — prove ONE real dispatch through the
# container real-mission producer's production path, and record what it
# produced beneath $MOLECULE_DIR/dispatch-output/ (producer-work,
# task-20260726-1f8e).
#
# THE PRODUCER
#   scripts/container-real-mission-bench.sh — the missing arm of the
#   container bench. It builds the external tester's published environment
#   (docker/container-real-mission/Dockerfile, issue #20), installs `cs`
#   from THIS tree, and drives a real molecule through the real
#   `cs tackle --adapter claude` dispatch path inside it.
#
# THE PRODUCTION DISPATCH PATH (what this script runs)
#   Nothing is doubled, stubbed or fabricated: a real `cs init`, a real
#   `cs nucleate`, and a real `cs tackle --adapter claude` run inside the
#   container as uid 10001, and the records copied out are the ones that
#   run emitted.
#
# THE HONEST MINIMAL UNIT
#   The dispatch necessarily halts at door 3, the credential gate, because
#   the container holds no credential and this harness must never give it
#   one. So the measured outcome asserted here is precisely: the gate
#   REFUSED, for the expected reason, with its own words captured verbatim.
#   A refusal for the right reason is a real measured outcome — not a pass
#   fabricated, and not a failure. The one remaining step (the login) is a
#   human's; the harness prints the exact command for it.
#
# VERDICT SEMANTICS (bench/README.md)
#   green (exit 0) the gate refused for the expected reason
#   red   (exit 1) it did not refuse, or refused for a different reason
#   INCONCLUSIVE   the discriminating step could not run here — e.g. no
#   (exit 2)       docker engine. NEVER reported as green.
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$ROOT"

# The fleet injects $MOLECULE_DIR on tackle, and there is no dead default. A
# fallback that names one operator's home directory is wrong on every other
# checkout; a fallback that names one MOLECULE that has since been harvested is
# worse, because it resolves — it turns a missing-env error into a mkdir
# somewhere nobody looks. So the by-hand path is opt-in and DERIVED, never
# assumed: name the molecule with $MOLECULE_ID and the galaxy root is found by
# walking up out of .worktrees/, which is right on every checkout and hardcodes
# no machine. Give neither and the script refuses, naming who injects it.
if [ -z "${MOLECULE_DIR:-}" ]; then
  : "${MOLECULE_ID:?set MOLECULE_DIR to the molecule directory (the fleet injects it on tackle), or MOLECULE_ID to resolve it under this galaxy}"
  GALAXY_ROOT="$ROOT"
  case "$ROOT" in
    */.worktrees/*) GALAXY_ROOT="${ROOT%%/.worktrees/*}" ;;
  esac
  MOLECULE_DIR="$GALAXY_ROOT/.cosmon/state/fleets/default/molecules/$MOLECULE_ID"
fi
DISPATCH_OUT="$MOLECULE_DIR/dispatch-output"
rm -rf "$DISPATCH_OUT"
mkdir -p "$DISPATCH_OUT"

say()   { printf '\033[1;34m▸ %s\033[0m\n' "$*"; }
die()   { printf '\033[1;31m[smoke][red] %s\033[0m\n' "$*" >&2; exit 1; }
incon() { printf '\033[1;33m[smoke][INCONCLUSIVE] %s\033[0m\n' "$*" >&2; exit 2; }

command -v jq >/dev/null 2>&1 || incon "jq is not on PATH; the produced record cannot be read"

# The producer clears its own out dir, so it gets a SUBdirectory: writing
# bench.log into a directory the producer is about to `rm -rf` would unlink
# the log while it is still being appended to, and the evidence would
# vanish exactly when it is needed.
MISSION_OUT_DIR="$DISPATCH_OUT/mission"

say "running the producer: scripts/container-real-mission-bench.sh"
MISSION_OUT_DIR="$MISSION_OUT_DIR" \
  bash "$ROOT/scripts/container-real-mission-bench.sh" \
  >"$DISPATCH_OUT/bench.log" 2>&1
RC=$?
# Not read through a pipe: the redirection above leaves $? as the bench's own.
tail -n 40 "$DISPATCH_OUT/bench.log"

case "$RC" in
  2) incon "the producer could not run its discriminating step; reason in $DISPATCH_OUT/bench.log" ;;
  0) : ;;
  *) die "the producer reported a finding (rc=$RC); read $DISPATCH_OUT/bench.log" ;;
esac

RECORD="$MISSION_OUT_DIR/mission-record.json"
[ -f "$RECORD" ] || incon "no mission-record.json produced under $DISPATCH_OUT"

VERDICT="$(jq -r '.verdict // "?"'  "$RECORD")"
MOL="$(jq -r '.molecule // "?"'     "$RECORD")"
TRC="$(jq -r '.tackle_rc // "?"'    "$RECORD")"
PROV="$(jq -r '.provenance_ok'      "$RECORD")"
NAMES="$(jq -r '.post_conditions.refusal_names_credential' "$RECORD")"
NONZERO="$(jq -r '.post_conditions.tackle_exited_non_zero' "$RECORD")"
NOTRUN="$(jq -r '.post_conditions.molecule_not_left_running' "$RECORD")"
LINE="$(jq -r '.refusal_line // ""' "$RECORD")"

[ "$VERDICT" = "REFUSED-AT-CREDENTIAL-GATE" ] \
  || die "verdict is '$VERDICT', not REFUSED-AT-CREDENTIAL-GATE"
[ "$PROV"    = "true" ] || die "provenance failed: the shipped cs is not the fixed branch"
[ "$NONZERO" = "true" ] || die "cs tackle did not exit non-zero — the gate did not hold"
[ "$NAMES"   = "true" ] || die "the refusal did not name the credential — a different door stopped it"
[ "$NOTRUN"  = "true" ] || die "the molecule was left 'running' after the refusal"
[ -n "$LINE" ]          || die "the refusal line was not captured verbatim"

say "OK: molecule=$MOL tackle_rc=$TRC verdict=$VERDICT"
say "the gate's own words, captured verbatim:"
printf '  %s\n' "$LINE"
say "records written under: $DISPATCH_OUT"
ls -1R "$DISPATCH_OUT"
