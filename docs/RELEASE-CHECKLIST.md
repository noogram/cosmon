# Cosmon Pre-Launch Checklist — a projection, not a tick-list

> **Source:** ported from oxymake's release model (oxymake délib-20260529-13a7,
> Q-REL-4 enforcement + Q-REL-5 checklist), adapted to Cosmon's isolated
> public-projection release boundary. Governed by janis's
> locked discipline: *A discipline whose referee, player, and clock-keeper are
> the same single operator is decoration.*

This document is **not** a list of boxes a human ticks. janis's
most-dangerous-gate finding is that a self-ticked checklist is *pre-waived by
design* and *produces no corpse when it silently certifies* — it manufactures
the illusion of completeness.

So the checklist is a **command-backed projection** of exogenous referees. The
single source of truth is the script:

```bash
scripts/release-checklist.sh            # pre-flip:  GATE items, protection gates pending
scripts/release-checklist.sh --post-flip  # post-flip: protection gates become hard
```

It exits **non-zero** if any GATE fails. **Do not flip the repo public while it
exits non-zero.** The corpse is the exit code, not an operator's opinion.

## The public projection boundary

Cosmon releases a clean public tree from a confidential development repository
through `scripts/release/cosmon-release-resync.sh`: clone into an isolated object
store, purge private paths from every publishable commit, genericize retained
content, rewrite author/committer/message channels to Noogram, then audit the
exact projection. The development repository is never pushed or rewritten.

## The fixed sequence (janis keystone — do not reorder)

The repo is **private**, so branch protection is HTTP 403 and **every CI job is
a radar, not a gate** until the flip. The exogeneity of every gate is
*downstream of the flip itself*. Therefore:

```
   project  →  audit  →  publish one ref  →  protect
```

1. **project** — create a fresh isolated clone with the release-resync chain.
2. **audit** — require `scripts/publish.sh --check` and the projection audit to pass.
3. **publish one ref** — an operator-only gesture; never use `--all` or `--mirror`.
4. **protect** — `scripts/apply-branch-protection.sh` (wires required checks +
   push-protection; only settable once public).

## The two membranes (ADR-133) — whole-file vs inside-the-file

| Membrane | Question it answers | Referee |
|----------|--------------------|---------|
| **Residence** | Is this whole *file* allowed on the public surface? | `scripts/artifact-map-audit.py` over `.cosmon/artifact-map.toml` (RED on any tracked `solo` path) |
| **Content** | Does a confidential *string* hide inside a public file? | gitleaks (secrets) + `release-checklist.sh` gate 4 (client/domain/infra denylist) + the D7 publish gate in `cs done` ([ADR-128](adr/128-d7-attribution-vacuum-and-publish-gate.md)) |

The confidential-string denylist is sourced **externally** (`$COSMON_FORBID_PATTERN`,
a gitignored `scripts/.release-denylist.local`, or the now-untracked
`.cosmon/release-rules.toml` on disk) — never inlined into the shipping script,
so the detector cannot re-leak what it forbids (ADR-127 §6, carried into ADR-133).

## The two bins (janis (e)) — nothing lives between

Every item is **exactly one** of:

- **[GATE]** — exogenous: a command's non-zero exit blocks the flip / fails CI.
- **[ADVISORY]** — explicitly non-gating: reported, never blocks. Honest demotion.

A **[GATE PEND]** is a GATE whose referee cannot run here yet (branch protection
while still private); it is reported and counted, never a silent pass.

| # | Item | Bin | Exogenous referee (the command / service) |
|---|------|-----|-------------------------------------------|
| 1 | gitleaks full-history detect exits 0 | GATE | `gitleaks detect --log-opts=--all` |
| 2 | 0 non-public (`solo`) paths tracked | GATE | `scripts/artifact-map-audit.py` + CI job *Artifact-map residence gate* |
| 3 | *Artifact-map residence gate* in `required_status_checks` | GATE (post-flip) | `gh api …/required_status_checks` |
| 4 | confidential client/domain/infra strings absent from the tree | GATE | `git grep` over an externally-sourced denylist (mirrors the D7 `cs done` gate) |
| 5 | `CLAUDE.local.md` untracked + gitignored | GATE | `git ls-files` + `git check-ignore` |
| 6 | `.cosmon/state/` absent from the tracked tree (ADR-055) | GATE | `git ls-files` grep |
| 7 | retired allowlist machinery (`release-allowlist.toml` / `release-rules.toml`) untracked | GATE | `git ls-files` grep |
| 8 | `LICENSE` + `LICENSE-APACHE` present | GATE | file existence |
| 9 | `deny.toml` present; `cargo deny check` clean | GATE | `cargo deny` + CI job *Deny* |
| 10 | all `github.com` cosmon URLs are `noogram/cosmon` | GATE | `git grep` |
| 11 | branch protection on `main` → 200 with ≥1 required check | GATE (post-flip) | `gh api …/branches/main/protection` |
| 12 | GitHub secret-scanning + push-protection enabled | GATE (post-flip) | `gh api … security_and_analysis` |
| 13 | second independent referee named | **ADVISORY** | see below — honestly ABSENT |

## Item 13 — the honest ABSENT record (janis (d) #5)

`noogram` has one org member and one repo admin: the operator himself. A
CODEOWNERS entry pointing at the operator's own account is *a self-appointed
mindguard in a reviewer's hat* — theater, not a gate. The *second independent
referee* requirement is therefore **UNMET**, recorded as **ADVISORY**, not faked
as a gate. Consequence stated plainly: *"main stays public"* is **MONITORED**
(if a topology-guard cron is added) but **not ENFORCED** — the same operator who
could flip it back can disable the monitor. When a second `noogram` org-admin
exists, promote item 13 from ADVISORY to GATE and update `.github/CODEOWNERS`.

## What the exogenous CI gates are (Q-REL-4)

| Workflow | Job name (the `required_status_checks` context) | Referee for |
|----------|--------------------------------------------------|-------------|
| `.github/workflows/artifact-map.yml` | `Artifact-map residence gate` | residence (whole-file leaks) |
| `.github/workflows/ci.yml` | `Format` / `Clippy` / `Test` / `Documentation` | the engineering Definition-of-Done floor |
| `.github/workflows/deny.yml` | `Deny` (bans/licenses/sources) | supply chain |

None is a `.git/hooks/pre-commit` (those are `--no-verify`-bypassable and
self-refereed). None carries `continue-on-error` or a skip-env. They become
**gates** only once their job name is listed in `required_status_checks` —
which `scripts/apply-branch-protection.sh` does post-flip.

> **Follow-ups (file as `temp:warm` beads, not blockers here):** a CI *Secret
> scan* job (gitleaks) and a *Forbid confidential strings* job would make gates 1
> and 4 exogenous CI referees rather than local-only checks. Until they exist,
> those gates run locally in `release-checklist.sh` and are honest about it.

## Engineering Definition-of-Done floor (the `ci.yml` gates)

Distinct from the public-prep referees above, the standard code-quality gates
from `CLAUDE.md` must pass before any release. CI (`.github/workflows/ci.yml`)
enforces them:

| Gate | Command |
|------|---------|
| Build | `cargo check --workspace` |
| Test | `cargo test --workspace` |
| Lint | `cargo clippy --workspace -- -D warnings` |
| Format | `cargo fmt --all -- --check` |
