# Git Reconciliation — Zone 1 (DO-NOW, non-destructive)

**Molecule:** `task-20260711-c298` · **Parent deliberation:** `delib-20260711-4733` §C1
**Executed:** 2026-07-11 · **Git:** 2.53.0 · **Operator-validated.**

This runbook records **exactly** the Zone-1 commands run against the live,
shared `/srv/cosmon/cosmon/.git` while **6 workers were hammering feat/\* branches**.
Every action here is **additive and reversible**. Nothing rewrote a commit,
pushed history, renamed a remote, or touched a worktree.

> **Why this exists.** The living tree (**lineage A**, root `11b8a15b4…`) and
> `cosmon-private` (**lineage B**, root `7df0613b3…`, twice `filter-repo`'d) are
> **disjoint histories** — no common ancestor. `main` was silently wired to push
> at `origin` (= cosmon-private). One absent-minded `git push` on `main` would
> have shot lineage-A content at the wrong repo. Zone 1 disarms that trip-wire
> and wires the public target — without disturbing the fleet.

---

## What ran, verbatim

### 1a — Disarm the tracking lie

```bash
git branch --unset-upstream main       # main had upstream origin/main (disjoint!) → removed
git config push.default nothing        # a bare `git push` now REFUSES instead of guessing
```

Plus a **narrowly-scoped** `pre-push` hook installed at
`.git/hooks/pre-push` (shared across all worktrees; `core.hooksPath` is default).

**The hook blocks exactly two moves, and nothing else:**

| # | Move | Verdict |
|---|------|---------|
| 1 | `refs/heads/main` → `origin` (cosmon-private) | **REFUSE** (the trip-wire) |
| 2 | *any* ref → `public` (noogram/cosmon) | **REFUSE** (publish is Zone-3, operator-only) |

**Explicitly allowed — the fleet's lifeblood is untouched:**
- `feat/*` → `origin` (the 6 live workers push these continuously) ✅
- archive/provenance **tags** → `origin` (step 1c below) ✅
- anything → `you-legacy` ✅

> **Reviewable copy / reinstall.** The live hook lives at `.git/hooks/pre-push`
> (inside `.git`, not tracked). A verbatim, reviewable copy is committed at
> [`git-reconciliation-zone1-pre-push.sh`](./git-reconciliation-zone1-pre-push.sh).
> Reinstall with: `cp docs/runbooks/git-reconciliation-zone1-pre-push.sh
> .git/hooks/pre-push && chmod +x .git/hooks/pre-push`.
>
> **Scope discipline (operator correction, 2026-07-11).** An earlier design
> blocked *all* lineage-A content → cosmon-private. That would have **broken the
> live fleet**, since workers push lineage-A `feat/*` to `origin` constantly, and
> `.git/hooks` is shared (effect is immediate). The installed hook therefore keys
> on the **pushed ref** (`refs/heads/main` only) and the **destination remote**
> (name `origin`/`public` **or** URL substring `cosmon-private`/`noogram/cosmon`),
> never on commit ancestry. Remove the file to disable: `rm .git/hooks/pre-push`.

**Hook validation (before it was left in place):**
- 8/8 direct-invocation cases pass (feat→origin ALLOW, main→origin REFUSE,
  tag→origin ALLOW, feat→public REFUSE, main→public REFUSE, feat→legacy ALLOW,
  mixed-refs-incl-main→origin REFUSE, public-by-URL-name-mismatch REFUSE).
- Real offline pushes to throwaway bare repos confirm the hook **fires on a real
  push** (not just dry-run): `main`→cosmon-private-URL and `feat/*`→noogram/cosmon-URL
  were both refused by the hook; `feat/*`→cosmon-private-URL succeeded.
- `git push --dry-run` was found **NOT** to invoke the pre-push hook (git 2.53.0),
  so the offline real-push test is the authoritative proof, not the dry-run.

### 1b — Wire the public target (additive)

```bash
git remote add public git@github.com:noogram/cosmon.git
git fetch public          # materialised public/main
```

`public/main` = `76476cab4f66a43fa681d911b6be3542b30a4339` — **matches the
expected `76476cab4`.**

### 1c — Plant the immutable provenance tag (additive, no force)

```bash
git fetch origin
git tag archive/cosmon-private-main-6jul-20260711 origin/main
git push origin archive/cosmon-private-main-6jul-20260711
```

Tag `archive/cosmon-private-main-6jul-20260711` →
`18abd239f87ee6e0e43ecf183dacbb4de5ea56e7` (= `origin/main`, lineage-B tip).
Pushed to `origin` and accepted (the hook allows tag refs to `origin` — only
`refs/heads/main` is blocked there).

---

## Resulting state

### `git remote -v`

```
you-legacy   git@github.com:noogram/cosmon.git (fetch)
you-legacy   git@github.com:noogram/cosmon.git (push)
origin          git@github.com:noogram/cosmon-private.git (fetch)
origin          git@github.com:noogram/cosmon-private.git (push)
public          git@github.com:noogram/cosmon.git (fetch)
public          git@github.com:noogram/cosmon.git (push)
```

`origin` and `you-legacy` are **unchanged** — no rename occurred. `public` is
the only addition.

### `main` no longer shows a false divergence

```
branch.main.remote  → (unset)
branch.main.merge    → (unset)
push.default         → nothing
git rev-parse --abbrev-ref main@{upstream}
  → fatal: no upstream configured for branch 'main'   # trip-wire disarmed ✅
```

Because `main` has **no upstream**, git no longer computes a bogus
`ahead/behind` against the disjoint `origin/main`. The "main is behind its own
photo" lie is gone.

### Reference SHAs (as of execution)

| Ref | SHA | Lineage |
|-----|-----|---------|
| `main` (A tip) | `a1c00de39…` (advances as the fleet merges) | A |
| A-root | `11b8a15b446e8b83a0544d20f40a060f47fe7220` | A |
| `origin/main` (B tip) | `18abd239f87ee6e0e43ecf183dacbb4de5ea56e7` | B |
| B-root | `7df0613b3dfae0d84ab036bca5c1d8a4d5a20b56` | B |
| `public/main` | `76476cab4f66a43fa681d911b6be3542b30a4339` | (public) |
| `archive/cosmon-private-main-6jul-20260711` | `18abd239f…` | B (frozen) |

---

## What was deliberately NOT done (guardrails honoured)

- ❌ **No** `git remote rename` — would edit shared `.git/config` and break the 6
  live workers / `cs done` pushes. (Zone-3, operator-only.)
- ❌ **No** push of `main`/history to any remote.
- ❌ **No** `--force` / `--force-with-lease` anywhere.
- ❌ **No** merge of lineage A into B, or B into A.
- ❌ **No** worktree touched; no `git gc`/prune (attic bundling is C2's job).

## Reversal (if ever needed)

```bash
rm .git/hooks/pre-push                                  # remove the guard
git remote remove public                                # un-wire public
git tag -d archive/cosmon-private-main-6jul-20260711    # local tag
git push origin :refs/tags/archive/cosmon-private-main-6jul-20260711  # remote tag
git branch --set-upstream-to=origin/main main           # (NOT recommended — re-arms trip-wire)
git config --unset push.default
```

---

## Downstream

Feeds **C4** (final reconciliation runbook), which cites the wired `public`
remote and the `archive/*` provenance tag recorded here. Zones 2 and 3
(salvage gate, quiescence, the single irreversible publish) remain
operator-gated per `delib-20260711-4733` outcomes.
