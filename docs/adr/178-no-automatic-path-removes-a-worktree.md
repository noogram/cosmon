<!-- SPDX-License-Identifier: AGPL-3.0-only -->
# ADR-178 — No automatic path removes a worktree; durable eligibility is advisory

**Status:** Accepted (2026-09-11).
**Date:** 2026-09-11.
**Decider:** Noogram.
**Authoring molecule:** `task-20260911-2224` — P3 of issue 61.
**Answers:** GitHub issue #61, and open question 1 of `task-20260911-f9b6`.

**Scope.** This ADR ratifies a commitment that has been load-bearing in code
since `48889e5a` and is now reachable from a CLI verb. It decides *what a
reclamation surface may remove*. It adds no mechanism: the predicates, the
adapter and the `cs purge --worktrees` surface are the molecules around it.

**Binds:**
[ADR-082](082-architecture-baseline.md) (the I/O-free domain core behind
injectable ports),
[ADR-052](052-one-ledger-one-writer-one-witness.md) §D3 (`cs purge` is the one
infrastructure-teardown verb),
the working contract at
[`docs/design/worktree-reclaim/CONTRACT.md`](../design/worktree-reclaim/CONTRACT.md).

---

## 1 · Context

A cosmon galaxy accumulates `.worktrees/<molecule-id>/` directories. Each one
holds a Git worktree and, in a Rust galaxy, a `target/` build directory that is
usually the overwhelming majority of its bytes. On the repository where issue
61 was reported, fourteen such directories existed, five of which had no
molecule at all, and the worker roster — the only mechanism that had ever
looked at them — could see two.

So there is real pressure to build something that deletes worktrees. The
question this ADR answers is whether cosmon may ever do that *by itself*.

The reason the question is not obvious is that the two kinds of bytes in a
worktree are not alike:

* **Derived** bytes are rebuildable by definition. Losing `target/` costs
  compile time and nothing else.
* **Durable** bytes are the work. A commit not yet reachable from the trunk, an
  uncommitted edit, an ignored operator note — each may be the only copy.

A single "is this worktree reclaimable?" boolean cannot carry both, which is
the contradiction the working contract split into two predicates.

## 2 · Decision

**D1.** No automatic path in this workspace removes a whole worktree. Every
mechanism that reclaims disk reclaims *validated derived roots* and returns the
worktree itself.

**D2.** `durable_eligibility` is **advisory**. Its `Eligible` verdict is a
reachability claim — "every byte here is also somewhere else" — and it
authorises no removal, in any caller, under any flag. Callers may print it.

**D3.** The existing owners of whole-worktree removal are unchanged: `cs done`
tears down the worktree it harvested, and an operator runs `git worktree
remove` by hand. Both are moments a human asked for the removal of one named
tree. Neither is widened by this ADR, and no new one is created.

**D4.** Any future mechanism that would remove a worktree automatically
requires a successor ADR. Reaching `durable_eligibility` from a removal path is
the thing that is forbidden; adding a new advisory reader is not.

## 3 · Why this and not the obvious alternative

The obvious alternative is that `Eligible` should mean *removable* — that is
what the word means, and the predicate already demands registration, proven
zero-ahead ancestry, a clean status and an empty ignored-content inventory.
Four conjuncts is a strong conjunction.

It is strong and it is not sufficient, for a reason that has nothing to do with
how many conjuncts there are:

**Every conjunct is an observation, and an observation is a claim about the
past.** `git status` was clean when it ran. Between that moment and the
`remove_dir_all`, an agent can write a file; on this fleet, agents write files
in worktrees continuously and by design. Derived reclamation survives this race
because it holds Cargo's build lock across the interval, which excludes the
producer that writes into `target/`. There is no equivalent lock over an
operator's editor, a detached `cs tackle` pane, or a half-finished `git
rebase`. Exclusion over durable content is not something cosmon can establish,
and eligibility without exclusion is a snapshot, not a guarantee.

The asymmetry of the two mistakes settles the rest. A derived root wrongly
reclaimed costs a rebuild. A worktree wrongly removed costs work that existed
nowhere else — the 2026-08-02 incident, where four molecules went
`running → collapsed` and their commits were recovered by hand, is the same
loss reached by a different route.

## 4 · What this ADR does not say

It does not say worktrees should accumulate. `cs purge --worktrees` names every
directory it finds, reports the bytes each holds, and states per candidate why
it is withheld. An operator reading that register can remove any of them with
one command. The decision here is about *who* decides, not about whether
anything is ever removed.

It does not introduce a byte threshold, an age-based escape, or a background
timer. A destructive action that runs unobserved is outside the perimeter of
every decision above.

## 5 · Consequences

* `durable_eligibility` has exactly one class of caller — reporting — and a
  test asserts the derived selection never contains a candidate's own path.
* Disk pressure is relieved by the derived tier alone. On a Rust galaxy that
  is most of the bytes; on a galaxy where it is not, the remedy is to state
  the additional derived roots in `[worktree_reclaim].evict` and to establish
  an exclusion protocol for them, not to widen this ADR.
* A galaxy that wants automatic whole-worktree removal must write ADR-N and
  say how it excludes a concurrent writer. That is the bar, and it is the bar
  because it is the thing that actually protects the bytes.
