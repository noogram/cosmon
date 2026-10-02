#!/usr/bin/env bash
# check-collaboration-contract.test.sh — fixtures for
# scripts/check-collaboration-contract.py.
#
# A minimal valid contract must pass; each deliberately incomplete variant
# (one omitted fact at a time) must fail with a finding that names the omission.
# Finally the real contract must pass. Hermetic: fixtures live in a tmpdir.
set -uo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$HERE/.." && pwd)"
CHECK="$HERE/check-collaboration-contract.py"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

pass=0 fail=0
ok()  { echo "PASS: $*"; pass=$((pass+1)); }
bad() { echo "FAIL: $*" >&2; fail=$((fail+1)); }

SURFACE="$WORK/surface.txt"
echo 'GET /v1/workers | task-x | 2026-01-01 | tenant | cosmon:worker:read | adapter-only | x' > "$SURFACE"

cat > "$WORK/valid.md" <<'MD'
| Scope | Grants | Implies |
|---|---|---|
| `cosmon:work:read` | read | - |
| `cosmon:work:write` | write | `cosmon:work:read` |

| Writer | Record class | Location |
|---|---|---|
| `owner-store` | envelopes | owner dir |

| ID | Verb | Route | Scope | CLI counterpart | Writer | Limit | Retry | Effect perimeter | Phase |
|---|---|---|---|---|---|---|---|---|---|
| R1 | send | `POST /v1/work/{owner}/messages` | `cosmon:work:write` | `cs work send` | `owner-store` | budget.max_payload_bytes | idempotent-key | advisory-write | W4 |
| R2 | list | `GET /v1/work/{owner}` | `cosmon:work:read` | `cs work list` | `owner-store` | page 100 | safe-repeat | read-only | W4 |

| ID | Operation | Disposition | Reason |
|---|---|---|---|
| X1 | remote evolve | refused | lifecycle |
| X2 | remote complete | refused | lifecycle |
| X3 | grant signing | refused | host-local |
| X4 | shell execution | refused | no exec |
| X5 | transcript sync | refused | out of scope |
| X6 | automatic wake-up | refused | no scheduler |
| X7 | remote nucleate | refused | lifecycle |
| X8 | remote tackle | refused | lifecycle |

| Code | HTTP | Meaning |
|---|---|---|
| stale_scope_revision | 409 | stale |

| ID | Decision | Status |
|---|---|---|
| O1 | a | adopted-default |
| O2 | a | adopted-default |
| O3 | a | adopted-default |
| O4 | a | adopted-default |
| O5 | a | adopted-default |
| O6 | a | adopted-default |
| O7 | a | adopted-default |
| O8 | a | adopted-default |
MD

run() { python3 "$CHECK" --spec "$1" --surface "$SURFACE" 2>&1; }

# expect_fail <name> <sed-expr> <needle>: mutate the valid fixture, require exit 1
# and a finding containing <needle>.
expect_fail() {
  local name="$1" expr="$2" needle="$3" out
  sed -E "$expr" "$WORK/valid.md" > "$WORK/case.md"
  if cmp -s "$WORK/valid.md" "$WORK/case.md"; then bad "$name: mutation changed nothing"; return; fi
  out="$(run "$WORK/case.md")"; local rc=$?
  if [ $rc -ne 0 ] && grep -qF -- "$needle" <<<"$out"; then ok "$name"
  else bad "$name (rc=$rc, wanted '$needle'): $out"; fi
}

out="$(run "$WORK/valid.md")" && ok "valid fixture passes" || bad "valid fixture should pass: $out"

# One omitted fact per route column.
expect_fail "route without scope"        's#^(\| R1 .*write` \| `cs )#\1#; s#\| `cosmon:work:write` \| `cs work send`#| | `cs work send`#' "route R1: empty Scope"
expect_fail "route without CLI"          's#\| `cs work send` \|#| |#' "route R1: empty CLI counterpart"
expect_fail "route without writer"       's#\| `owner-store` \| budget#| | budget#' "route R1: empty Writer"
expect_fail "route without limit"        's#\| budget.max_payload_bytes \|#| |#' "route R1: empty Limit"
expect_fail "route without retry"        's#\| idempotent-key \|#| |#' "route R1: empty Retry"
expect_fail "route without effect"       's#\| advisory-write \| W4#| | W4#' "route R1: empty Effect perimeter"
expect_fail "route without phase"        's#\| read-only \| W4#| read-only | #' "route R2: empty Phase"
expect_fail "undeclared scope"           's#cosmon:work:write` \| `cs work send#cosmon:work:other` | `cs work send#' "not in the scope table"
expect_fail "undeclared writer"          's#\| `owner-store` \| budget#| `ghost-store` | budget#' "not in the custody table"
expect_fail "lifecycle effect refused"   's#\| advisory-write \| W4#| lifecycle-write | W4#' "Effect perimeter"
expect_fail "unbounded limit"            's#\| budget.max_payload_bytes \|#| unbounded |#' "Limit must be"
expect_fail "unknown retry rule"         's#\| idempotent-key \|#| try-again |#' "Retry"
expect_fail "duplicate route"            's#`GET /v1/work/\{owner\}`#`POST /v1/work/{owner}/messages`#' "duplicate route"
expect_fail "already mounted route"      's#`GET /v1/work/\{owner\}`#`GET /v1/workers`#' "already mounted"
expect_fail "missing refusal"            '/transcript sync/d' "does not name 'transcript'"
expect_fail "missing decision"           '/^\| O5 /d' "omits O5"
expect_fail "decision without status"    's#^\| O3 \| a \| adopted-default#| O3 | a | maybe#' "decision O3"
expect_fail "bad error status"           's#\| 409 \|#| 200 |#' "HTTP must be"

# No route table at all.
grep -v -E '^\| (R1|R2) |^\| ID \| Verb' "$WORK/valid.md" > "$WORK/noroutes.md"
out="$(run "$WORK/noroutes.md")"; rc=$?
if [ $rc -ne 0 ] && grep -qF "missing table: route table" <<<"$out"; then ok "missing route table"
else bad "missing route table (rc=$rc): $out"; fi

# The real contract.
if out="$(python3 "$CHECK" 2>&1)"; then ok "real contract passes"; else bad "real contract: $out"; fi

echo "$pass passed, $fail failed"
[ "$fail" -eq 0 ]
