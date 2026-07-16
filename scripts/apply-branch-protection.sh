#!/usr/bin/env bash
# apply-branch-protection.sh — cosmon's load-bearing post-flip wiring (Q-REL-4)
# ============================================================================
# Ported from oxymake (délib-20260529-13a7). "A CI job not in
# required_status_checks lets the operator merge red — it is theater." This
# script is the one act that converts every CI radar into an exogenous GATE. It
# is UNRUNNABLE while the repo is private (branch protection → HTTP 403 without
# Pro); run it IMMEDIATELY after the public flip, in the fixed sequence:
#   clean → flip → PROTECT → enforce.
#
# It is idempotent: re-running re-asserts the same protection.
#
# USAGE:  scripts/apply-branch-protection.sh
# ============================================================================
set -euo pipefail

REPO="noogram/cosmon"
BRANCH="main"

# The EXACT job `name:` of every check that must be green to merge. These are the
# check-run names GitHub matches against — they must equal the `name:` field in
# each workflow job, verbatim. Keep this list in sync when a gate is added.
# The Definition-of-Done floor (Format/Clippy/Test) comes from .github/workflows/
# ci.yml; 'Artifact-map residence gate' is the one-repo membrane (ADR-133).
REQUIRED_CHECKS=(
  "Format"
  "Clippy"
  "Test"
  "Documentation"
  "Artifact-map residence gate"
)

echo "==> Verifying ${REPO} is public (protection is unavailable while private)…"
priv=$(gh api "repos/${REPO}" --jq .private)
if [ "$priv" = "true" ]; then
  echo "ERROR: ${REPO} is still PRIVATE. Flip it public first:" >&2
  echo "       gh api -X PATCH repos/${REPO} -f private=false" >&2
  exit 1
fi
echo "    OK: ${REPO} is public."

# Build the JSON contexts array from REQUIRED_CHECKS.
contexts_json=$(printf '%s\n' "${REQUIRED_CHECKS[@]}" | python3 -c \
  'import sys,json; print(json.dumps([l.rstrip("\n") for l in sys.stdin if l.strip()]))')

echo "==> Applying branch protection on ${REPO}@${BRANCH} with $(echo "$contexts_json" | python3 -c 'import sys,json;print(len(json.load(sys.stdin)))') required checks…"

# Full protection payload. `enforce_admins=true` is what makes the gate bind the
# OPERATOR too — without it, an admin (the only role here) can merge red, which
# is precisely the self-referee pathology. require a PR review so a (future)
# second CODEOWNER's approval can block.
payload=$(python3 - "$contexts_json" <<'PY'
import json, sys
contexts = json.loads(sys.argv[1])
print(json.dumps({
    "required_status_checks": {"strict": True, "contexts": contexts},
    "enforce_admins": True,
    "required_pull_request_reviews": {
        "required_approving_review_count": 1,
        "require_code_owner_reviews": True,
    },
    "restrictions": None,
    "required_linear_history": True,
    "allow_force_pushes": False,
    "allow_deletions": False,
}))
PY
)

echo "$payload" | gh api -X PUT "repos/${REPO}/branches/${BRANCH}/protection" \
  -H "Accept: application/vnd.github+json" --input - >/dev/null
echo "    OK: branch protection applied."

echo "==> Enabling GitHub secret-scanning + push-protection…"
gh api -X PATCH "repos/${REPO}" \
  -H "Accept: application/vnd.github+json" \
  --input - >/dev/null <<'PY'
{"security_and_analysis":{"secret_scanning":{"status":"enabled"},"secret_scanning_push_protection":{"status":"enabled"}}}
PY
echo "    OK: secret-scanning + push-protection enabled."

echo "==> Verifying with the post-flip checklist…"
exec "$(dirname "${BASH_SOURCE[0]}")/release-checklist.sh" --post-flip
