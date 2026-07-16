# ADR-114 — Mindguard `surface_visual` attributes surface to the molecule's fork point, not the whole branch

**Status:** Accepted
**Date:** 2026-05-31
**Driver:** Noogram
**Idea source:** `idea-20260531-3fbd` (idea-to-plan, P0 régression du scellage des délibérations)
**Task:** `task-20260531-8478` (structural fix — stays behind P0 child `task-20260531-29c0`)
**Discovery source:** noogram `delib-20260531-40df` — a deep-think deliberation finished all four steps but could not seal; `cs complete` was refused by the `surface_visual` mindguard for a `wiki/**` diff the deliberation never authored.
**Recoupe:** commit `82f1ee052` (`feat(mindguard): câblage surface_visual fail-closed dans cs complete`) · ADR-110 (DAG-aligned branching / single-writer trunk) · the mindguard's janis axiom (`crates/cosmon-cli/src/mindguard/mod.rs`)

---

## 1. Context

The `surface_visual` mindguard (born 2026-05-27, commit `82f1ee052`) refuses
`cs complete <MOL>` when `<MOL>` touched the *visual surface* (`*.html`,
`*.css`, `*.js`, `wiki/**`, `lumen/web/**`) without an independent
`verify-surface` witness landing GREEN inside `T_max`. The intent is sound: no
claim of "done" on a rendered surface without a witness that *looked at the
render*.

The signal it used to compute "surface touched" was:

```
git diff --name-only <merge-base>...HEAD     # <merge-base> = origin/main, then main
```

This is correct **only for a molecule whose branch forked from `main`**. It is
a category error for a molecule whose branch forked from a *blocker's* branch.

## 2. The category error

Cosmon uses **DAG-aligned git branching** (ADR-110, CLAUDE.md "DAG-aligned git
branching"): `cs tackle <MOL>` branches `feat/<MOL>` from the blocker's branch
`feat/<dep>`, not from `main`, so the worker sees its predecessor's committed
output in its own worktree. Content flows through branch topology; the DAG edge
carries one bit (done / not-done).

A consequence: the worker's branch **carries** (*charrie*) every file the
blocker authored. On a knowledge galaxy (noogram), **every** branch carries
`wiki/**`, because the upstream molecules in the pipeline author wiki pages.

So `git diff origin/main...HEAD` on a downstream molecule's branch reports
`wiki/**` files — *inherited from the blocker*, not authored by this molecule.
The merge-base of `origin/main` and a multi-hop downstream `HEAD` is `main`'s
tip, so the diff spans the **entire** lineage, blocker commits included.

Result: a deliberation (`delib-20260531-40df`) that wrote only
`synthesis.md` / `outcomes.md` (no surface) was told it had touched the visual
surface, refused at `cs complete`, and could not seal — its worktree and tmux
session leaked, and `cs done` refused because the molecule was still `running`.
The deliberations earlier the same day (`8b3f`/`0e5d`/`bcc7`/`48b2`) sealed
normally; the regression bit *after* `82f1ee052` wired the gate into the hot
path.

> This is the same class of bug `done.rs::is_branch_merged` was hardened
> against (cf. its doc comment): *"The only source of truth is ancestry
> against the base branch — never against an ambient `HEAD`."* The surface
> gate made the dual mistake — it diffed against the wrong base.

## 3. Decision

**Compute "surface touched" on the molecule's clean diff — the files
attributable to `<MOL>` — not on the whole worktree-vs-main diff.**

The molecule's clean diff is `git diff <fork-point>...HEAD`, where the fork
point is the branch `<MOL>` was created from. Concretely:

1. Load `<MOL>` and resolve its `BlockedBy` predecessors whose `feat/<dep>`
   branch currently exists — the **same** resolution
   `cs tackle::resolve_branch_start_point` uses to pick the worktree's start
   point. These are the *fork-point bases*.
2. Try `git diff --name-only <base>...HEAD` for each fork-point base first.
   Git's **triple-dot** diffs from `merge-base(base, HEAD)`, which is exactly
   the point where `feat/<MOL>` diverged from `feat/<dep>`. The diff is
   therefore precisely what `<MOL>` authored on top of its start point —
   never what the branch inherited.
3. Fall back to `origin/main`, then `main`, then a plain `git diff HEAD`,
   exactly as before, when no fork-point base resolves (root molecule, or a
   blocker that merged and had its branch deleted).

Because `merge-before-dispatch` merges a blocker to `main` *before* dispatching
its dependent, a downstream molecule whose blocker is already on `main` is
correctly handled by the `origin/main` fallback: the blocker's commits are on
`main`, so `origin/main...HEAD` already excludes them. The fork-point base is
needed precisely (and only) when the blocker branch still exists un-merged —
which is exactly the buggy case.

This **changes the semantics of "surface touched"** — it is a structural
decision, hence this successor ADR rather than a silent patch (CLAUDE.md
coherence checklist).

## 4. Why this introduces no false negative

The driving risk (briefing): *a mis-resolved fork point lets a real surface
escape the gate (false negative).* It does not, by construction:

- Every commit `<MOL>` makes is reachable from `HEAD` and is strictly *after*
  `merge-base(blocker, HEAD)` — the worktree is dedicated to `<MOL>` and
  branched at the blocker's tip. So any surface-touching commit by `<MOL>` is
  always inside `<blocker>...HEAD`. The narrowing cannot drop it.
- `merge-base` can only be the true fork or *older* (if the blocker branch were
  rewound), never newer. An older base **over**-captures — a false *positive*,
  which fails **closed** (the safe direction).
- If the blocker branch cannot be resolved, the gate falls back to the *wider*
  `origin/main` base — again over-capturing, fail-closed.

The only commits the narrowing excludes are those *not authored by `<MOL>`*
(ancestors of the merge-base, i.e. the blocker's own work) — which is exactly
the point. A surface authored by the blocker is the blocker's gate to satisfy,
not the dependent's.

## 5. Fail-closed discipline preserved

- Non-git project root → surface untouched (the gate is *for* git-tracked
  surfaces). Unchanged.
- All diff attempts fail on an existing git repo → `MindguardError::Unavailable`
  (operator may `--override-mindguard-down --justification …`, logged
  write-once). Unchanged.
- The change only re-orders the **base candidate list** (fork-point bases
  prepended) and never opens a default-pass on a real surface.

## 6. Consequences

- A deliberation or any downstream molecule that authors no surface seals
  without a spurious `verify-surface` requirement, even on a knowledge galaxy
  where its branch carries `wiki/**`.
- A molecule that *does* author a surface atop its blocker is still gated
  (test `gate_refuses_when_molecule_authors_surface_atop_blocker`).
- The gate's notion of "what this molecule authored" now matches the branch the
  worktree was actually forked from — the same source of truth `cs tackle` and
  `cs done` use.
- Orthogonal to the P0 child (`task-20260531-29c0`), which ships the
  `verify-surface` formula, exempts `MoleculeKind::Deliberation`, and fixes the
  remedy-string chain. Both land independently; this ADR removes the
  *root cause* (the wrong diff base) while the P0 child removes the *acute
  symptom* (no usable witness formula + hanging remedy).

## 7. Implementation

`crates/cosmon-cli/src/mindguard/surface_visual.rs`:

- `molecule_fork_bases(store, mol_id, project_root)` — resolves existing
  `feat/<dep>` blocker branches, mirroring `tackle::resolve_branch_start_point`.
- `branch_exists(project_root, branch)` — `git rev-parse --verify --quiet
  refs/heads/<branch>` (branch heads are shared across linked worktrees, so the
  blocker ref resolves from inside `<MOL>`'s own worktree).
- `surface_touched(project_root, patterns, molecule_bases)` — fork-point bases
  prepended to `["origin/main", "main"]`, plain-`HEAD` last resort unchanged.

Tests: `surface_touched_ignores_blocker_inherited_files` (the regression, both
shapes asserted), `gate_passes_when_only_blocker_authored_surface`,
`gate_refuses_when_molecule_authors_surface_atop_blocker`,
`fork_bases_empty_for_root_molecule`, `fork_bases_skips_deleted_blocker_branch`.
