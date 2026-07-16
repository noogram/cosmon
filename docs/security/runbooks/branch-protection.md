# Runbook — Branch protection on sensitive repos

**Origin**: delib-20260425-39c1 §I.C2 / §V (jobs + niel + forgemaster).
**Posture**: prepared (config and docs landed) → active (toggled in GitHub UI by operator).

This runbook is **not executable from a CI worker** — the `gh` CLI used here
requires interactive login with the operator's GitHub account, with hardware
token. Workers prepare the config; the human flips the switch.

## Why this runbook exists

The maillon faible 2026-04-25 is a stolen GitHub PAT pushed direct to `main`.
Branch protection raises the bar from *any token holder can ship* to *any token
holder must open a PR, pass status checks, and produce a signed merge commit*.
Coupled with secret-scanning push protection (separate, org-wide), the catastrophic
class of "credential exfil → main → CI auto-deploy" closes.

## Sensitive repos in scope

Seven repos. Apply the same policy to each:

1. `cosmon`
2. `mailroom`
3. `tenant-demo`
4. `addl`
5. `accord`
6. `chancery`
7. `noogram-labs` *(activate at creation time — pre-commit on the empty repo)*

## Required policy on `main` (per repo)

| Setting                                              | Value                  |
|------------------------------------------------------|------------------------|
| Require pull request before merging                  | enabled, 0 approvers (solo for now) |
| Require status checks to pass                        | enabled, strict mode   |
| Required status checks                               | `cargo deny` · `cargo audit` · `cargo vet` (cosmon) · `Format` · `Clippy` · `Test` (per repo) |
| Require signed commits                               | enabled                |
| Require linear history                               | enabled                |
| Block force-pushes                                   | enabled                |
| Block deletions                                      | enabled                |
| Restrict who can push                                | empty (PR-only)        |
| Apply rules to administrators                        | enabled                |

## Apply via `gh` CLI (operator session, with hardware token)

For each repo, after authenticating `gh` with the operator account:

```sh
REPO="Noogram/cosmon"   # adjust per repo

gh api -X PUT "repos/$REPO/branches/main/protection" \
  -F "required_pull_request_reviews[required_approving_review_count]=0" \
  -F "required_pull_request_reviews[dismiss_stale_reviews]=true" \
  -F "required_status_checks[strict]=true" \
  -f "required_status_checks[contexts][]=cargo deny" \
  -f "required_status_checks[contexts][]=cargo audit" \
  -f "required_status_checks[contexts][]=cargo vet" \
  -f "required_status_checks[contexts][]=Format" \
  -f "required_status_checks[contexts][]=Clippy" \
  -f "required_status_checks[contexts][]=Test" \
  -F "enforce_admins=true" \
  -F "required_linear_history=true" \
  -F "allow_force_pushes=false" \
  -F "allow_deletions=false" \
  -F "required_conversation_resolution=true" \
  -F "required_signatures=true" \
  -F "restrictions=null"
```

For repos other than cosmon, keep only the status-check contexts that exist in
their CI matrix.

## Org-wide secret scanning push protection

Settings → Code security and analysis → enable for all repositories:

- Secret scanning: enabled
- Push protection: enabled
- Push protection bypass: only specific people (operator)

This rejects pushes that contain credentials *server-side* before they land on
GitHub. It is the single highest-leverage 5-minute action in this sprint
(niel §2.3, forgemaster §1).

## Verification (per repo, after enabling)

```sh
gh api "repos/$REPO/branches/main/protection" --jq '{
  required_reviews: .required_pull_request_reviews.required_approving_review_count,
  signed: .required_signatures.enabled,
  linear: .required_linear_history.enabled,
  force_push_blocked: (.allow_force_pushes.enabled | not),
  contexts: .required_status_checks.contexts
}'
```

Expected output (cosmon):

```json
{
  "required_reviews": 0,
  "signed": true,
  "linear": true,
  "force_push_blocked": true,
  "contexts": ["cargo deny", "cargo audit", "cargo vet", "Format", "Clippy", "Test"]
}
```

## Worker / pilot signing model (delib §III.D / forgemaster option b)

- Workers commit on `feat/task-XXXX` worktree branches **without** signing.
- Pilot (Noogram) signs the merge commit on `main` via PR merge (hardware-bound
  GPG/SSH-FIDO2 once YubiKey-postée arrives 30/04).
- Branch protection enforces signed commits *only on main*. Worker branches
  remain unconstrained — the discipline is at the merge boundary.

## Rollback (if a status check is broken)

Branch protection is a UI toggle. To temporarily lower a single context:

```sh
gh api -X PATCH "repos/$REPO/branches/main/protection/required_status_checks" \
  -f "contexts=[\"Format\",\"Clippy\",\"Test\"]"  # remove the broken one
```

Re-add it once green. Never disable signed-commits or force-push blocking
without a written reason in CHRONICLES.md.

## Cross-references

- `delib-20260425-39c1/synthesis.md` §I.C2, §III.D, §V (cost/gain table)
- `delib-20260425-39c1/responses/forgemaster.md` §3 (signing model)
- `delib-20260425-39c1/responses/niel.md` §2 (Saturday actions)
- `.github/workflows/{ci,deny,archive-verify,readme-quickstart,tla-verify}.yml` (status check definitions)
