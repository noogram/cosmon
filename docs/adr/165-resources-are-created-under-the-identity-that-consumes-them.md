# ADR-165 — Resources are created under the identity that consumes them

**Status:** Accepted (2026-07-28).
**Date:** 2026-07-28.
**Decider:** Noogram.
**Authoring task:** `task-20260727-ee8f`.

**Entry artefact.** Four hand-over defects in two days, all in the same
mechanism: cosmon creating a molecule's filesystem resources as root and
handing them to a non-root worker afterwards. Three times the hand-over was too
short — the worktree, the config-home consent files, the linked worktree's
gitdir. Once it was too long: the repair chowned the repository's whole common
dir recursively, handing the worker `refs`, `config` and `hooks`.

**Architectural invariants:** demotes
[§8y — What cosmon repairs and what cosmon judges are one list](../architectural-invariants.md#8y-what-cosmon-repairs-and-what-cosmon-judges-are-one-list)
from doctrine to the safety net of a degraded path, and replaces it as the
general rule with the title of this ADR.

**Related ADRs:**
[ADR-162](162-dispatch-boundary-ready-is-earned.md) and
[ADR-163](163-a-question-may-only-be-asked-where-an-answer-can-arrive.md) (the
other doors of noogram/cosmon#20).

**Evidence:** [`docs/benches/nonroot-pilot-2026-07-28.md`](../benches/nonroot-pilot-2026-07-28.md).

---

## Context

### Nobody chose root

This is worth stating plainly, because the decision reads as a reversal and it
is not one. There was never a moment at which running the pilot as root was
weighed and preferred.

`docker exec` without `-u` **is** root. That is the whole provenance. Our own
container guide piloted that way — `cs` running as root with
`COSMON_WORKER_UID=10001` — after having, one step earlier, given the entire
project tree away with `chown -R 10001:10001 /srv/mission`. We handed the
workshop to the worker and then brought root back in to build every new bench
inside it. The same guide already used `-u 10001` for the read-only doors and
abandoned it the moment anything had to be written.

A default flag in somebody else's CLI became our privilege model, and then four
defects in two days became evidence about *ownership transfer* rather than
evidence about *the transfer existing at all*.

### Why the class does not close by making the hand-over reliable

Each of the four fixes was correct. None of them reduces the size of the next
one. The mechanism has an enumeration at its heart — *which resources does a
worker touch?* — and that enumeration is over somebody else's internals:
`.claude.json` is a Claude Code implementation detail a release can rename, and
git's layout moved once inside this very issue (worktree → gitdir → common dir)
and moves again with `--separate-git-dir`, submodule gitdirs, and the reftable
backend.

The fourth defect is the one that settles it, because it points the other way.
The first three were the list being too short. The fourth was the repair being
too generous: a recursive chown of `<repo>/.git` hands the worker `hooks/`,
which is code the **pilot** executes at `cs done` time. A mechanism that fails
in both directions is not one bad list away from correct.

There is no hand-over to get right if there is no hand-over.

## Decision

### 1. Nominal — one identity

**The cosmon pilot and its workers run under the same non-root uid.**

`RootSpawnDecision::SpawnAsIs` is already structurally reachable for a non-root
dispatcher
([`crates/cosmon-core/src/root_spawn_policy.rs`](../../crates/cosmon-core/src/root_spawn_policy.rs)).
This path is not new and not written for this ADR; it is the entire non-root
fleet, and it has simply never been named nominal. On it there is no demotion,
no chown, no repair set, and nothing to enumerate — the worktree, the state
dirs, the config home and the git plumbing are created by the identity that
will consume them.

In a container that is the full invocation, and it is full on purpose:

```sh
docker exec -it -u 10001:10001 \
  -e HOME=/home/cosmon-worker \
  -e CLAUDE_CONFIG_DIR=/home/cosmon-worker/.claude-mission \
  -e LC_ALL=C.UTF-8 \
  -w /srv/mission <container> bash
```

`HOME` is load-bearing, not decoration. Changing only `-u` while leaving
`HOME=/root` puts the worker back behind a 0700 directory it cannot read —
which is defect #2 of the four, reintroduced by a partial command.

### 2. Compatibility — root → uid stays, and stays fail-closed

> **Superseded on 2026-07-28 by
> [ADR-166](166-the-root-to-uid-demote-path-is-refused.md).** The residue this
> section states as tolerable — "a worker can still rewrite a sibling branch
> ref" — was then reproduced twice, on two benches and two uids, together with
> deletion of a shared object. It does not yield to a fourth narrowing, so the
> path is now **refused** rather than kept as compatibility. Everything below
> remains an accurate description of the mechanism; nothing live enters it.
> §1 of this ADR (the nominal one-identity path) is unaffected and is what the
> refusal points operators at.

The demote path is not removed. A root dispatcher still demotes, still
provisions, still refuses with a typed `UnprovisionedTarget` when the target
cannot reach what it needs, and is still tested. What changes is its status: it
is a **compatibility path**, not the documented default and not a recommended
one.

Its repair set is also narrowed, by `converge-20260727-a302` in parallel with
this decision and landed as `18b820d`. The worktree's own gitdir belongs to one
dispatch and still moves whole; the repository's **common dir** is *entered*
rather than transferred, and only the three entries a commit actually writes go
across — `objects`, `refs/heads`, `logs/refs/heads`. `config` and `hooks/` no
longer change hands, which is the part that matters most: `hooks/` is executed
by whoever runs git next, and the dispatcher runs git next, as root, at
`cs done`.

That work also states the residue this ADR would otherwise have to: `refs/heads`
moves as a *directory*, because a loose-ref store creates `<branch>.lock` beside
`<branch>`, and a cosmon branch is `feat/task-…`, so the directory holding it
holds the sibling molecules' branches too. A worker can still rewrite a sibling
branch ref. Ownership cannot separate them; a per-worker repository over a
shared object store can. That is a different design, not a tighter `chown`.

### 3. §8y is demoted, and what replaces it

§8y — *what cosmon repairs and what cosmon judges are one list* — remains true
and remains enforced. It is demoted from doctrine to **the safety net of the
degraded path**: it governs the compatibility mechanism, which is the only
place a repair set exists.

The general rule that replaces it:

> **Resources are created under the identity that will consume them.**

A repair set exists only where creation-under-consumer is impossible, and each
such place must say why.

## The boundary we give up

This has to be named rather than discovered later.

With a shared uid, POSIX no longer prevents a worker from calling `cs done` and
merging to trunk. There is no ownership difference left to lean on, because
there is no ownership difference left at all.

Two things must be said alongside it, or the sentence is misleading.

**The boundary was already weak.** A worker that owns its worktree, its gitdir
and the branch it commits on is one `git push`/`cs done` away from trunk in any
case; what stood in the way was a `chown` the dispatcher had performed on
itself, not an access-control decision anybody designed.

**And a947's common-dir chown destroyed it outright on the root path too.** A
worker handed `<repo>/.git` recursively owns `hooks/`, and a `post-merge` hook
is code the pilot runs. The root path was therefore *not* the one that kept the
boundary. The shared-uid model is not what loses it.

The conclusion to record: if `cs done` must remain a human gesture, that has to
become a **typed cosmon authorisation** — never a side effect of Unix ownership.
[ADR-172](172-done-authority-is-an-operator-sealed-capability.md) settles the
opened question: ordinary harvest is delegable, human-reserved thresholds are
not, and both travel as an operator-sealed capability. A broker is custody, not
the authorisation.

## Consequences

- The container guide is rewritten onto the non-root pilot, and the
  chown-then-pilot-as-root loop is deleted from it rather than annotated.
- The compatibility path keeps a fail-closed root → uid test that does **not**
  recursively chown the common dir — superseded by
  [ADR-166](166-the-root-to-uid-demote-path-is-refused.md), which refuses the
  path outright; the test survives as the evidence for that refusal (`18b820d`, whose hermetic test freezes every
  common-dir path outside the granted set and makes a real commit, and whose
  root-gated test provisions as root and acts as uid 10001 through `setpriv`).
- The repair path is instrumented: every *entry* into it is counted, before any
  precondition, into an optional per-dispatch journal
  (`COSMON_OWNERSHIP_TRANSFER_JOURNAL`). This exists because final-state
  ownership cannot distinguish *the repair never ran* from *the repair ran and
  changed nothing* — a chown onto the owner a path already had is invisible to
  `stat`. The claim the evidence makes is the first sentence, so the instrument
  has to observe the call.
- `cs done` authorisation is settled by ADR-172.
