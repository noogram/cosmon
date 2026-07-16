#!/usr/bin/env bash
# moussage-e2e.sh — B1 moussage resident, drain gate (task-20260610-e5f6).
#
# Proves, against the REAL `cs` binary and a throwaway tenant-shaped
# galaxy, that:
#
#   1. a root + 3 children DAG (children --blocked-by root) drains to
#      completion via `cs run <root>` WITHOUT human intervention —
#      design (a): the loop runs on the same filesystem as the
#      StateStore and trunk.lock;
#   2. the proof is read from PRE-EXISTING signals — `cs ensemble
#      --json` statuses + the events ledger written by the verbs
#      themselves — never from a PASS the harness writes;
#   3. the B3 budget floor is a NAMED exit (code 90), not a stall;
#   4. the B1 depth bound REFUSES too-deep plans before starting
#      (code 92) and B2 refuses too-wide plans (code 91).
#
# Workers are formula `command` gates (`true`) — the drain mechanics,
# not the LLM, are under test. No Claude session, no API key, no cost.
#
# Usage: scripts/moussage-e2e.sh [path-to-cs]
set -euo pipefail

CS="${1:-$(cd "$(dirname "$0")/.." && pwd)/target/debug/cs}"
[ -x "$CS" ] || { echo "✗ cs binary not found at $CS (cargo build -p cosmon-cli)"; exit 1; }

say() { printf '\033[1;34m▸ %s\033[0m\n' "$*"; }
ok()  { printf '\033[1;32m✓ %s\033[0m\n' "$*"; }
die() { printf '\033[1;31m✗ %s\033[0m\n' "$*" >&2; exit 1; }

command -v jq >/dev/null || die "jq required"

TMP="$(mktemp -d /tmp/moussage-e2e.XXXXXX)"
trap 'rm -rf "$TMP"' EXIT
say "throwaway tenant galaxy: $TMP"

# Tenant-shaped galaxy: git repo + .cosmon/{state,formulas}. The drain
# loop, the StateStore and trunk.lock all live HERE — co-location is
# what makes the advisory flock binding (I1 validity condition).
git -C "$TMP" init -q -b main
git -C "$TMP" -c user.email=e2e@moussage -c user.name=e2e commit -q --allow-empty -m init
# Galaxy marker: walk-up discovery requires .cosmon/config.toml, and the
# gate executor requires a project_id — `cs init --upgrade` mints it.
mkdir -p "$TMP/.cosmon/state" "$TMP/.cosmon/formulas"
printf '# moussage-e2e throwaway galaxy\n' > "$TMP/.cosmon/config.toml"
( cd "$TMP" && "$CS" init --upgrade >/dev/null 2>&1 ) || die "cs init --upgrade failed"

cat > "$TMP/.cosmon/formulas/gate-noop.formula.toml" << 'EOF'
formula = "gate-noop"
version = 1
description = "single shell-gate step — drains without any worker"
id_prefix = "task"

[[steps]]
id = "gate"
title = "No-op gate"
description = "Shell gate: exits 0 immediately."
command = "true"
acceptance = "exit 0"
EOF

# Strip any inherited molecule/state context so the walk-up finds $TMP.
unset COSMON_STATE_DIR COSMON_MOL_DIR COSMON_PARENT_MOL_ID COSMON_ARTIFACT_DIR || true
cd "$TMP"

nucleate() { # $* = extra args; echoes molecule id
  "$CS" --json nucleate gate-noop --kind task "$@" | jq -r '.id // .molecule_id // .[0].id'
}

# ── Scenario 1: root + 3 children drains without intervention ────────
say "scenario 1 — root + 3 children, cs run drains it"
ROOT="$(nucleate --var topic=root --no-parent)"
[ -n "$ROOT" ] && [ "$ROOT" != "null" ] || die "root nucleation failed"
K1="$(nucleate --var topic=k1 --blocked-by "$ROOT")"
K2="$(nucleate --var topic=k2 --blocked-by "$ROOT")"
K3="$(nucleate --var topic=k3 --blocked-by "$ROOT")"
say "DAG: $ROOT ⊃ {$K1, $K2, $K3}"

"$CS" --json run "$ROOT" --no-teardown --timeout 120 \
    --max-actions 16 --max-depth 4 --max-molecules 32 \
    > "$TMP/run.json" 2> "$TMP/run.err" \
  || die "cs run exited $? — $(tail -3 "$TMP/run.err")"

REASON="$(jq -r '.reason' "$TMP/run.json")"
[ "$REASON" = "PolicyDrained" ] || die "expected PolicyDrained, got $REASON"

# Proof (pre-existing signal #1): ensemble statuses — all four done.
ENS="$("$CS" --json ensemble)"
for id in "$ROOT" "$K1" "$K2" "$K3"; do
  STATUS="$(echo "$ENS" | jq -r --arg id "$id" \
    '(.molecule_states // .molecules) | map(select(.id==$id)) | .[0].status')"
  [ "$STATUS" = "completed" ] || die "molecule $id is '$STATUS', expected completed"
done
ok "ensemble: 4/4 molecules completed (root + 3 children)"

# Proof (pre-existing signal #2): the events ledger written by the
# verbs themselves carries the lifecycle of every molecule.
LEDGER="$(find .cosmon/state -name 'events.jsonl' | head -1)"
[ -n "$LEDGER" ] || die "no events.jsonl ledger found"
for id in "$ROOT" "$K1" "$K2" "$K3"; do
  grep -q "$id" "$LEDGER" || die "ledger carries no event for $id"
done
EVENTS="$(wc -l < "$LEDGER" | tr -d ' ')"
ok "events ledger: $EVENTS events, all 4 molecule ids present ($LEDGER)"

# ── Scenario 2: B3 budget floor = NAMED exit 90, never a stall ───────
say "scenario 2 — B3 budget floor is a named exit (code 90)"
R2="$(nucleate --var topic=root2 --no-parent)"
C1="$(nucleate --var topic=c1 --blocked-by "$R2")"
C2="$(nucleate --var topic=c2 --blocked-by "$R2")"
C3="$(nucleate --var topic=c3 --blocked-by "$R2")"
set +e
"$CS" --json run "$R2" --no-teardown --timeout 120 --max-actions 2 \
    > "$TMP/run2.json" 2>/dev/null
RC=$?
set -e
[ "$RC" -eq 90 ] || die "expected exit 90 (budget_exhausted), got $RC"
[ "$(jq -r '.reason' "$TMP/run2.json")" = "BudgetExhausted" ] \
  || die "expected reason BudgetExhausted, got $(jq -r '.reason' "$TMP/run2.json")"
ok "budget floor → exit 90, reason BudgetExhausted (terminated, named, no stall)"

# ── Scenario 3: B1 depth bound refuses before starting (code 92) ─────
say "scenario 3 — B1 depth refusal (code 92)"
set +e
"$CS" --json run "$R2" --no-teardown --timeout 30 --max-depth 1 \
    > "$TMP/run3.json" 2>/dev/null
RC=$?
set -e
[ "$RC" -eq 92 ] || die "expected exit 92 (max_depth_exceeded), got $RC"
[ "$(jq -r '.error' "$TMP/run3.json")" = "max_depth_exceeded" ] \
  || die "expected error max_depth_exceeded"
ok "depth 2 > bound 1 → refused with exit 92 max_depth_exceeded"

# ── Scenario 4: B2 width bound refuses before starting (code 91) ─────
say "scenario 4 — B2 molecule-quota refusal (code 91)"
set +e
"$CS" --json run "$R2" --no-teardown --timeout 30 --max-molecules 2 \
    > "$TMP/run4.json" 2>/dev/null
RC=$?
set -e
[ "$RC" -eq 91 ] || die "expected exit 91 (molecule_quota_exceeded), got $RC"
[ "$(jq -r '.error' "$TMP/run4.json")" = "molecule_quota_exceeded" ] \
  || die "expected error molecule_quota_exceeded"
ok "4 molecules > bound 2 → refused with exit 91 molecule_quota_exceeded"

echo
ok "moussage E2E PASS — drain, named budget floor, named depth/width refusals"
