# ADR-174 — Per-worker ref and object storage is deferred until a named use case

**Status:** Deferred (2026-08-07) — not refused, not scheduled.
**Date:** 2026-08-07.
**Decider:** Emmanuel (operator).
**Entry artefacts.** [ADR-165](165-the-nonroot-pilot-and-its-shared-uid.md) and
[ADR-166](166-the-root-to-uid-demote-path-is-refused.md); GitHub issue #28,
which tracked the deferral and is closed by this record;
`task-20260727-7f01`, the deliberation that examined four topologies.

**A third status.** This repository's ADRs carry `Proposed` and `Accepted`.
Neither fits a decision to *not build something yet, for stated reasons, with
written conditions that would reopen it*. `Deferred` is that status. An issue
was the wrong home: an issue is actionable work with a definition of done, and
this has neither — it has a bar.

---

## Context

A worker commits from a linked `git worktree`. Its refs and its objects live in
the dispatcher's repository, shared with every sibling. Git cannot subdivide
either, so the grant that lets a worker commit is the same grant that lets it
rewrite a sibling's branch and delete a shared object. #20 established this
empirically; ADR-166 turned it into a typed refusal of root dispatch rather
than a narrower `chown`, because narrower is not available.

The deliberation examined four topologies. The one it recommended (C) gives
each worker a private repository — its own refs, index, logs and objects —
reaching the shared store read-only through `objects/info/alternates`, with
`cs done` **fetching** the molecule's ref into a controlled integration ref
instead of merging in place.

## Decision

**Not now, and not by default ever.**

1. The nominal path stays `git worktree`, the native mechanism. Where the simple
   mode suffices, cosmon uses the simple mode.
2. A private-repository topology, if it is ever built, is a *second* topology
   with an explicit named activation — never inferred from the presence of
   files, and never a silent upgrade of the harvest path everyone takes.
3. Implementation is not authorised until the admission bar below is met.

## The admission bar

1. **A named situation.** Someone blocked today: who, doing what. A ceremony of
   rights is not a use case.
2. **Evidence the simple mode was tried** and does not suffice.
3. **A named activation surface**, persisted as a topology type. The two paths —
   `LinkedShared` and `PrivateAlternate` — have different creation, crash-recovery
   and teardown procedures; inferring one from the other would tear down a
   private repository as if it were a linked worktree, and lose work.
4. **The falsifiable trigger**, already stated in #28: when per-worker storage
   lands, `the_grant_still_permits_a_sibling_ref_rewrite_and_a_shared_object_delete`
   goes red. That red is the signal, not a review.

## Costs weighed

**Destabilisation.** `cs done` merges in place today. Fetch-then-merge changes
the path every molecule takes, on code that produced three measured failure
modes this month: a detached HEAD that would have merged into nothing, gate
contention that collapsed a healthy molecule, and a purge that terminated live
work. A second topology is a second place to lose a molecule.

**Disk, measured on this repository rather than assumed.** `.git/objects` is
468 MB; a linked worktree is 67 MB–2 GB of checked-out files, already duplicated
per worker. With `objects/info/alternates`, a private repository *borrows* the
468 MB read-only and stores only the objects it creates — its own overhead is
refs, index and logs: kilobytes to a few MB. Option C is therefore close to free
on disk.

**Option D is refused on this ground.** The deliberation's fallback — a full
independent copy without alternates — would multiply the 468 MB per worker.
Fourteen worktrees on this machine would become roughly seven gigabytes of
duplicated history to avoid one `chown`. If read-only alternates cannot be made
portable, the answer is to keep the refusal, not to copy the store.

## Not established

Whether any real use case exists. One candidate is named without endorsement:
dissociating a galaxy from its target repository, where a cosmon galaxy pilots
work whose deliverable belongs to a third party (`task-20260728-7d49`). That
candidate must itself pass bar item 2 — it may be answerable without touching
the storage topology at all.

## Consequences

- The shared-uid pilot remains the documented default; ADR-165 and ADR-166 stand.
- `RootSpawnDecision::Demote` and the transport-side provisioning port stay
  dormant and unreachable from policy, as ADR-166 left them: they are the
  substrate this would re-enable, and the tests that characterise the grant must
  still be able to build it.
- Issue #28 is closed as recorded-here, **not** as `wontfix`. Nobody should
  read an open ticket where there is a deferred decision.
- Reopening does not need permission: it needs bar items 1 and 2, in writing.
