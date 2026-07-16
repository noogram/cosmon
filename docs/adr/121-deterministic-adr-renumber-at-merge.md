# ADR-121 — Deterministic ADR renumber at the merge gate

**Status:** Accepted
**Date:** 2026-06-05
**Decider:** Noogram (auto-pilot remontée)
**Source:** `task-20260605-877d` (🔧 task), filed cosmon-ward from the
2026-06-05 auto-pilot session after a fleet harvest produced two `ADR-117`
files (RPP and LLMPort), hand-renumbered 117 → 118 at merge.

## Context

Cosmon runs N workers in parallel. Each worker lives in an **isolated git
worktree** branched from the same base (usually `main`). When several of
those workers independently decide to file "the next ADR", they each scan
`docs/adr/`, observe the same highest number, and mint the same `ADR-NNN`.

The collision is invisible inside any single worktree: a branch's filesystem —
and its gitignored `state.json` — never sees a peer's allocation. The clash is
a **merge-time fact**, not a creation-time one. On 2026-06-05 the RPP worker
and the LLMPort worker both produced `ADR-117`; the operator noticed only at
`cs done` and renumbered one to 118 by hand. The auto-propel merge loop already
rebases the worker on textual conflicts (it handled `d781` on `tackle.rs`), but
a *number* monotonicity invariant is not a textual conflict — two different
files (`117-rpp.md`, `117-llmport.md`) merge cleanly and silently violate it.

## The structural impossibility of reservation-at-nucleation

The intuitive fix — "reserve the ADR number atomically when the molecule is
nucleated" — **cannot work** in cosmon's branch-per-worker model. There is no
shared mutable surface at nucleation time: each worktree is a disjoint
filesystem until merge, and the state store is per-worktree and gitignored.
A number written into branch A is unobservable from branch B.

This is the same lesson git already teaches: two people can both `touch
feature.txt` on separate branches with no error; the conflict only exists when
the branches converge. Unique-monotonic allocation across isolated branches is
not a property you can establish at creation — it can only be **reconciled at
the single convergence point**, which is the base branch at the merge gate.

## Decision

Resolve ADR-number collisions **deterministically at the merge gate**, exactly
where `cs done` already rewrites colliding `molecule/<name>` workspace
artifacts to disjoint paths before merging (`relocate_workspace_artifacts`).
ADR-number collisions are the same class of problem — parallel branches
adopting an identical convention — and get the same shape of fix.

`cs done`, before merging a worker's branch, now:

1. Reads the ADR numbers the **base branch** already carries
   (`git ls-tree <base> -- docs/adr/`).
2. Reads the ADR files the **branch added** relative to base
   (`git diff --diff-filter=A <base>...HEAD -- docs/adr/`).
3. For each added ADR whose number the base already owns, assigns the next
   free number (`max(used) + 1`, monotonic, order-independent), renames the
   file (`git mv`), rewrites the file's **own title self-reference**
   (`# ADR-NNN` → `# ADR-MMM`), and commits the change on the worker's branch.
4. Merges. The landing is now number-disjoint and needs no manual surgery.

The arithmetic is a pure, I/O-free, unit-tested module
(`crates/cosmon-cli/src/adr.rs`): `parse_adr_number`, `next_free_number`,
`find_collisions`, `plan_renumber`, `rewrite_self_reference`. The merge hook in
`done.rs` is the thin I/O shell around it.

## Properties

- **Deterministic.** `plan_renumber` is a pure function of (base numbers,
  branch-added files); two workers landing in either order get the same
  assignment, and the plan sorts its inputs so the result does not depend on
  the order git happened to list the adds.
- **Non-fatal / defensive.** Mirrors `relocate_workspace_artifacts`: a missing
  worktree, a non-git path, an unreachable base ref, or a repo with no
  `docs/adr/` is a silent no-op. Only an unexpected `git mv`/`commit` failure
  during a real rename surfaces, and the caller downgrades it to a warning —
  the merge hot path is never blocked by ADR bookkeeping. *Propose mechanisms
  of verification, do not impose them* (architectural-invariants §8b).
- **Visible, not silent.** Each renumber is reported in the `cs done` action
  list (`renumbered_adr: … → ADR-NNN`) and recorded as its own
  `chore(done): renumber ADR-NNN → ADR-MMM (fleet collision)` commit on the
  branch. The operator sees exactly what the system did, the same way the
  manual 117 → 118 fix would have read in the log.

## Limits (documented, not hidden)

- **Cross-references are not chased.** Only the renumbered file's own title
  self-reference is rewritten. Citations *to* the renumbered ADR from other
  files are left untouched. This is acceptable because the fleet-collision case
  is, by construction, a *brand-new* ADR that nothing cites yet. A motivated
  future need (renumbering an established, widely-cited ADR) is out of scope
  for the merge gate and should be an explicit operator gesture.
- **Historical intentional duplicates are not touched.** Cosmon's corpus
  contains deliberate same-number ADRs from the early days (e.g. two `006-…`,
  several `032-…`). The merge hook only acts on numbers the *base already owns*
  that a *branch newly adds* — it never rewrites what is already on main.
- **Base divergence (the secondary ask) is deferred.** The remontée also asked
  whether `cs done` should periodically rebase idle-completed workers so
  in-flight branches accumulate less drift as main advances. That is a larger
  change to the dispatch/merge cadence with its own risk surface; it is **not**
  implemented here and is filed as a follow-up (`temp:warm`). The renumber gate
  already removes the specific manual-surgery pain that motivated this task.

## Consequences

- Fleet harvest of parallel ADR-producing workers no longer requires manual
  renumbering. The operator's 117 → 118 gesture is now the machine's default.
- A reusable pure `adr` module exists. If a manual `cs adr next / check /
  renumber` operator command is later wanted (audit + escape hatch), it can be
  built on this module without new logic — filed as a `temp:warm` follow-up
  alongside its ADR-068 UX-parity obligation.
