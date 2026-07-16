---
title: cell-models — seed skeleton (recap opérateur)
status: seed-private
last_updated: 2026-04-24
source_molecule: task-20260423-e99b
parent_delib: delib-20260423-f4da §5
visibility: PRIVATE — do not publish the org nor invite external contributors until the gating in §5 of delib-f4da holds.
---

# cell-models — seed skeleton

## What exists locally

- **Repo**: `/srv/cosmon/cell-models/` — git initialized, commit `b5e34d6`, branch `main`.
- **Licence**: MIT.
- **Remote**: **not yet pushed**. GitHub org not yet created (see gating below).

## Items delivered (8 of 8 functional, 2 contingent on remote)

| # | Item | Status | Path |
|---|---|---|---|
| 1 | GitHub org private | ⏳ pending — create manually (see §Remote setup) | — |
| 2 | Repo seed with code that runs | ✅ | `/srv/cosmon/cell-models/` |
| 3 | `MAINTAINERS.md` | ✅ | 2 slots, Noogram + TBD |
| 4 | `LICENSE` (MIT) | ✅ | `LICENSE` |
| 5 | `CODE_OF_CONDUCT.md` | ✅ | Contributor Covenant v2.1 |
| 6 | `CONTRIBUTING.md` | ✅ | DCO sign-off, 1 PR 1 review |
| 7 | `GOVERNANCE.md` | ✅ | Lazy consensus, 2/3 override, BDFL transition |
| 8 | Branch protection on `main` | ⏳ pending — set on GitHub after push | — |

## Code health

Three smoke tests pass in <2 s on CPU (pytest). Benchmark command `lcm-bench`
converges on a synthetic dataset in under a minute.

```
src/lcm/
  __init__.py
  data.py    — synthetic dataset + loaders
  model.py   — tiny transformer encoder (CellTransformer)
  bench.py   — train_and_evaluate() with CLI entry point
examples/
  train_toy.py
tests/
  test_smoke.py  — 3 passing tests, all public entry points wired
data/
  README.md  — synthetic recipe, no bytes checked in
```

## 2nd maintainer slot (TBD, independent of Noogram)

`MAINTAINERS.md` has an explicit placeholder. Per `delib-20260423-f4da` §B
gating, this slot must be filled by someone **independent of Noogram** before
the org can go public. Candidate pressenti: Étienne Lempereur — to propose
only after Ivan café (regret bayésien if proposed before commit).

## Remote setup (2 commands, do NOT run yet)

```bash
# Step 1 — create private org on GitHub (UI) or via gh CLI:
gh org create cell-models --visibility=private

# Step 2 — push seed to the new org:
cd /srv/cosmon/cell-models
git remote add origin git@github.com:cell-models/cell-models.git
git push -u origin main

# Step 3 — enable branch protection on main via GitHub UI:
#   Settings → Branches → Branch protection rules → add rule:
#     - Branch name pattern: main
#     - Require pull request before merging (1 approval)
#     - Require status checks (placeholder, add CI later)
#     - Do not allow bypassing the above settings
```

**Do not execute until the four conditions in §B of `delib-20260423-f4da` hold**:

1. Café Ivan done + Jérémie commit verbal to a form of collaboration.
2. Jérémie consulted an MA IP/employment lawyer.
3. Written Prior Inventions Exhibit obtained from Boston, OR Jérémie
   decided clearly against Boston.
4. 2nd maintainer identified and willing to be listed.

## Naming

`cell-models` — chosen per delib-f4da §5:

- Close to Jérémie's PhD framing ("Large Cell Models") without naming him.
- Describes what the code does, not who writes it.
- Rejected alternatives: `Noogram` (operator secret), `openivan` (inverse of
  the point), `opencell` (likely taken), `flotte` (cryptic).
- Fallback: `cell-foundation-models`.

## What we do NOT do before T+6 months

Per delib-f4da §5:

- No legal foundation (Linux Foundation / SFC / Apache)
- No fiscal host (Open Collective)
- No formal CLA via an entity
- No trademark
- No public website
- No Slack / Discord
- No PMC
- No bylaws
- No press announcement

We add an element only when a real problem requires it.

## Gap vs. worker prompt

The worker (task-20260423-e99b) completed 5 of 8 items before hitting an
Anthropic API content-filter. The operator closed the loop manually:

- Added `CODE_OF_CONDUCT.md` (worker had not reached it).
- Added `README.md` (not in the original list but referenced by `pyproject.toml`).
- `git init` + initial commit `b5e34d6`.
- Smoke-tested the code (`pytest -q` → 3 passed).
- Wrote this recap.

The worker's partial commit history was preserved in-place — all files
produced by the worker are now in `main` as of commit `b5e34d6`.
