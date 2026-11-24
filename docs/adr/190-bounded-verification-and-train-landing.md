<!-- SPDX-License-Identifier: AGPL-3.0-only -->
# ADR-190 — Bounded verification and train landing: gate slots, one writer of main, `cs done` the only closer

**Status:** Accepted (2026-11-17), decided by the operator on review of pull
request #222, with two amendments (see "Amendments on acceptance" below).
Proposed 2026-11-16.
**Date:** 2026-11-16 (proposed); 2026-11-17 (accepted).
**Decider:** Noogram.
**Authoring molecule:** `task-20261116-dcd7`.
**Parent deliberation:** `delib-20261116-d0fc` (panel: architect, torvalds,
adversary).
**Answers:** GitHub issues #214 (project-wide harvest queue, admission) and
#213 (harvest in trains).

**Amends:** the lock order of
[ADR-110](110-single-writer-trunk-and-coordination-invariants.md) I1 (adds
the gate slot ahead of trunk). **Proposed an amendment to:**
[ADR-158](158-post-merge-integrity-delegation-cascade.md), in §8 below. It was
applied to ADR-158 by pull request #225 (amendment of 2026-11-17).

**Binds:**
[ADR-052](052-one-ledger-one-writer-one-witness.md) §I9 (a merged branch
belongs to a Completed or Collapsed molecule),
[ADR-082](082-architecture-baseline.md) (I/O-free core behind ports),
[ADR-172](172-done-authority-is-an-operator-sealed-capability.md) (harvest
authority, `[harvest_authority] required`),
[ADR-176](176-remote-harvest-authority-is-a-sealed-capability.md) §11 (the
trunk flock binds at the effect boundary).

**Scope.** This ADR decides three mechanisms (gate slots and named pools,
admission at `cs tackle`, train landing) and the invariants each must keep.
It adds no code. The increments that implement it are separate molecules;
their refutation tests are this ADR's acceptance criteria (§4).

**Amendments on acceptance (2026-11-17).** The operator accepted this ADR on
review of pull request #222 with two amendments, written into the text below
and marked with their date:

1. A run waiting for a gate slot is distinguished from a run holding one, and
   the patrol's background-work ceiling applies from slot acquisition (§3
   Slots, §4 B1).
2. The status of the increments is recorded, since several landed between
   proposal and acceptance (§1.1, §7, §8, §9, §10, and the table below).
   Statements that were true on 2026-11-16 are kept and annotated, not
   rewritten.

**Increment status (2026-11-17).**

| Increment | What | Status |
|---|---|---|
| A1 | Compare-and-swap rollback in `cs done` | Pending (pull request #227 or later); not on main |
| A2 | Closure of a landed branch without re-gating; ADR-158 amendment (§8) | Landed, pull request #225 |
| A3 | `empty_branch` only when the branch never had a commit | Landed, pull request #225 |
| A4 | Queue hint and first-parent landed-evidence scan | Landed, pull request #225 |
| C1 | `cs done --expect-head <oid>` | Landed, pull request #225 |
| C2 | `cs done --print-merge-message` | Landed, pull request #225 |
| — | Runtime stamps `merged_at` only when the branch landed (§9 question 1) | Landed, pull request #226 |
| B1, B2 | Gate slots and named pools | Not started |
| B3 | Admission at `cs tackle` | Not started |
| D1 | Landing verb | Not started |

**Increment status (2026-11-20).** The table above is the status at
acceptance and is kept as written. Since then the following landed on main,
as recorded in the CHANGELOG (release 0.10.0 and [Unreleased]):

| Increment | What | Status |
|---|---|---|
| A1 | Compare-and-swap rollback in `cs done` | Landed, pull request #227 (0.10.0) |
| B1, B2 | Gate slots and named pools (`[gate_slots]`, `cs gates slot`, `scripts/run-gates-bg.sh` through it, `gates.queued`) | Landed, pull request #233 (0.10.0) |
| B3 | Admission at `cs tackle` (`[admission].max_active`, `min_free_bytes`) | Landed, pull request #231 (0.10.0) |
| D1 | Landing verb `cs land-train` | Landed, pull request #232 (0.10.0) |
| — | `cs land-train --exclude <id>=<head>`: refuses a member whose pinned head contains the exact excluded head | Landed on main, molecule `task-20261120-29c3`; unreleased |

Statements elsewhere in this ADR that call A1 pending (§1.1, §10) were true
on 2026-11-17 and are superseded by this table. A proposed amendment to D3,
[ADR-192](192-a-train-is-a-landing-mode-that-refuses-carriers.md), would
replace the exact-head exclusion test and add a base-first precondition to
ejection; until it is accepted, D3 below is unchanged.

---

## 1 · Context

Issue #214 asks for a project-wide harvest queue and admission control.
Issue #213 describes harvesting completed molecules in trains: several
branches merged and gated once, then landed together. The reporter's
operator script measured a large speed-up and changed shape five times in
one night.

### 1.1 What the code does today (read, not run)

Each line below was re-read in the tree at the time of writing (2026-11-16);
the line numbers are those of that tree and have since moved. Paths are
repository-relative. Rows that changed before acceptance carry a dated note
naming the change; the original statement is kept.

| Fact | Where |
|---|---|
| `cs done` takes the trunk lock before the merge and holds it through the merge, the `merged_at` stamp, the post-merge hook and the archive write; it drops it before the fleet purge. `cs stitch` takes the trunk lock alone. | `crates/cosmon-harvest/src/transaction.rs:2789-2813`; `crates/cosmon-cli/src/cmd/stitch.rs:265` |
| Nothing bounds the worker-side full gate. The formula tells workers to run it through `[gates].background_runner_command`, which is `./scripts/run-gates-bg.sh`; that script takes no lock. | `.cosmon/formulas/task-work.formula.toml:66-90`; `.cosmon/config.toml:95`; `scripts/run-gates-bg.sh` |
| `cs tackle` has no admission refusal. Its pressure check runs only with `--reclaim-derived`, reclaims derived output, reports and swallows failures, and never refuses a spawn. | `crates/cosmon-cli/src/cmd/tackle.rs:1669`, `:4175-4205` |
| The post-merge gate cascade has no empty-diff short-circuit when a gate command is declared. Rung 1 (`integrity_command`) always runs; rung 2 (cargo) returns `NothingToVerify` on a diff with no build-relevant file only when no `build_command` is declared, and otherwise declines to rung 3 (`build_command`). *2026-11-17 (A2, #225):* when `cs done` merged nothing (`AlreadyMerged`, HEAD unchanged), no rung runs. | `transaction.rs:6945-6987`, `:7092-7098` |
| A delegated gate command calls `ensure_trusted` first and returns `Unverified{expected:true}` on stale trust. | `transaction.rs:7004-7020` |
| On a branch already reachable from base (`AlreadyMerged`), `cs done` sets `merge_succeeded` and the gate runs on the empty diff `pre_merge_head..HEAD`. *2026-11-17 (A2, #225):* no gate runs on that path; the landing commit is recorded as `landed_outside_cs_done: <oid>`. | `transaction.rs:3221-3241`, `:3022-3026`, `:3493`; diff range at `:7180-7200` |
| Cosmon itself declares `build_command = "cargo check --workspace"`. | `.cosmon/config.toml:76` |
| With `[hooks].pre_done` configured, `cs done` calls `ensure_trusted` before the merge block and refuses on stale trust. *2026-11-17 (A2, #225):* unchanged in substance; the refusal now names the hook and does not say the merge was rolled back. | `transaction.rs:2692-2702` |
| The `post_merge` hook block is guarded only by `merge_succeeded && !resuming_integrated`, so it also runs on the `AlreadyMerged` path when the hook is configured, the base is the trunk (or `--deploy-off-trunk` is given) and trust holds; on stale trust it is skipped with a warning. *2026-11-17 (A2, #225):* the hook no longer runs when `cs done` merged nothing; the skip is recorded as a `PostMergeHook` `Skipped` event with a warning. | `transaction.rs:3728`, `:3791`, `:3855` |
| The #109 shell-surface check compares the files changed over `pre_merge_head..HEAD`. On the `AlreadyMerged` path that range is empty. | `transaction.rs:3410-3420`, `:6638-6647`, `:7180-7200` |
| A post-merge gate failure rolls main back with `git reset --hard <pre_merge_head>`, without checking that main still points at this invocation's merge. *2026-11-17:* still true on main; the compare-and-swap rollback (A1) is pending. | `transaction.rs:6798-6836`, `:7666-7680` |
| `empty_branch` is reported when the branch is zero commits ahead of base and its tip is on base's first-parent chain. A branch landed by a `--no-ff` merge is labelled `already_merged`; a fast-forwarded branch is labelled `empty_branch`. *2026-11-17 (A3, #225):* `empty_branch` now also requires the tip to equal the oldest entry of the branch reflog; without a reflog the branch is not claimed empty, and a fast-forwarded branch is labelled `already_merged`. | `transaction.rs:947-955`, `:979-998`, `:1030-1064`, `:3230-3235` |
| `cs done` writes the merge subject from the project's `[project] merge_subject` template when one is set, otherwise `Merge branch '<branch>'`, followed by one trailer paragraph: `Co-Authored-By` attribution first, then `Mol-Id`, `Mission-Id` and one `Depends-On` per `blocked_by` parent. *2026-11-17 (C2, #225):* `cs done --print-merge-message` prints that message without merging, built from the same paragraphs as the merge. | `transaction.rs:3151-3166`, `:5385-5402`; `crates/cosmon-harvest/src/lineage.rs:117-128`; `crates/cosmon-core/src/config.rs:1537` |
| `cs done` refuses `--strategy ff-only` whenever trailers exist, because a fast-forward has no commit to carry them. | `transaction.rs:474-492` |
| CI provenance accepts `Merge branch 'feat/<mol_id>'` (pattern not end-anchored), a base-sync subject only of the form `Merge branch 'main' into feat/<mol_id>`, a `Base-Sync: <base>..feat/<mol_id>` trailer subject to a structural trunk check, and integration shapes for `feat/issue-N` only. `Merge branch 'train' into feat/<id>` matches none of them. | `scripts/check-provenance.sh:160`, `:192`, `:202`, `:228-240` |
| `cs stitch` writes `stitch(<mol>): merge into <base>`, which matches none of the provenance patterns. | `stitch.rs:450` |
| The harvest queue's landed-evidence scan reads `git log main --merges` (not first-parent) and counts any subject containing `feat/<id>`, for molecules whose branch no longer exists. For landed work the queue's hint is `cs done <id> --no-merge`, which records `MergeSkipped`. *2026-11-17 (A4, #225):* the scan reads `git log main --first-parent --merges` and only the merged-branch side of the subject; the hint is plain `cs done <id>`. | `crates/cosmon-cli/src/cmd/harvest_queue.rs:147-187`; `crates/cosmon-state/src/work_location.rs:76-84`; `transaction.rs:3388` |
| The harvest door already bounds the closed-but-unintegrated backlog by an operator-sealed ceiling. | `crates/cosmon-filestore/src/harvest_door.rs:356-367` |
| *Added 2026-11-17 (#216).* Patrol does not nudge a worker whose background work is in flight and younger than `PROPEL_BACKGROUND_CEILING` (45 minutes). A detached gate run counts as in flight while `gates.pid` names a live process and `gates.exit` is absent in the molecule directory; its age is the modification time of `gates.pid`, which `scripts/run-gates-bg.sh` writes when it detaches. | `crates/cosmon-core/src/propel.rs` (`decide_nudge`, rule 3b); `crates/cosmon-cli/src/cmd/patrol_background.rs` (`gate_run_age`) |
| `[harvest_authority] required` is a boolean field of the project config. | `crates/cosmon-core/src/config.rs:678-684` |
| `cs done` is not the only production writer of `merged_at`. The resident runtime stamps it after its executor's `on_complete` returns `Ok`, and the production executor returns `Ok` even when the `cs done` child exits non-zero. *2026-11-17 (#226):* the executor now returns an error when `cs done` exits non-zero, and the runtime stamps `merged_at` only when `cs done` did not and `Executor::branch_landed` reports the branch reachable from its base. | `crates/cosmon-runtime/src/lib.rs:557-594`, `:1528-1538` |

### 1.2 What follows from it

- The trunk lock already serializes merges and the post-merge compile gate.
  A queue for merges would duplicate it. The gap #214 names is the
  worker-side full gate, which no lock bounds.
- Closing k members of a train that landed outside `cs done` runs k build
  commands under the trunk lock in any galaxy that declares
  `integrity_command` or `build_command` (cosmon included), and stale trust
  refuses each closure. A galaxy with neither declared gets
  `NothingToVerify` on the cargo rung.
- A train that changes the trusted shell surface is invisible to the #109
  check at closure, because the closure range is empty.

## 2 · Decision

**D1 — Gate slots.** Full gates and project-declared heavy commands run while
holding a slot from an N-token pool. A slot is a kernel `flock` on a file
under the main checkout's state directory
(`<state dir>/gate-slots/<pool>/<k>.lock`), held by the process that runs the
gate for the life of that run. Cosmon owns the primitive and the state-dir
resolution, because resolving the main checkout's state directory from inside
a worktree is cosmon knowledge; the project config owns N and the pool names.
An unset count keeps today's behaviour. `cs done` never takes a slot. The
only permitted nesting is slot → trunk; the full order of acquisition is
slot → trunk → fleet.

**D2 — Admission at `cs tackle`.** A project may set `max_active` and
`min_free_bytes` (on the worktree and build volume). When either bound is
exceeded, `cs tackle` refuses the spawn with a named reason before the
worktree exists. The decision is a pure function in the core; the facts
(molecule counts, free bytes) arrive through ports. No load-average or other
machine-load signal is an input.

**D3 — Train landing.** A train is built and gated off-trunk from `--no-ff`
merges of pinned member heads, each carrying exactly the subject and trailers
`cs done` would write. It lands only by a compare-and-swap fast-forward of
main under the trunk lock, inside a cosmon verb, after the #109 surface
predicate has passed over the landing range. Each member is then closed by
`cs done --expect-head <pinned head>`, which stays the writer of `merged_at`
for train members. A red train is rebuilt from the current main tip without
the member found by prefix bisection. At landing, repeat `--exclude <id>=<head>`
for every removed member so `cs land-train` can refuse a member whose pinned
head contains an excluded head.

D3 constrains the *shape* of a train build and of a red-train rebuild,
whichever tool performs it. Building trains, gating them and bisecting a red
one inside cosmon is a non-goal of this ADR (§6); in the first increments the
operator's tool builds and gates, and cosmon lands and closes.

## 3 · Invariants

Each mechanism must keep the following. A change that breaks one of them
needs a successor ADR.

**Lock order.**
- Order of acquisition: gate slot, then trunk, then fleet. No process
  acquires a lock earlier in this order while holding a later one.
- `cs done` never acquires a gate slot, on any path. The reason: if `cs done`
  waited for a slot while holding trunk, and a train process holding a slot
  waited for trunk, the two would deadlock.
- The train gate runs under one slot and never under the trunk lock. The
  trunk lock covers only the landing's checks and the fast-forward.
- A landing process releases the trunk lock before it runs any member's
  `cs done`. `flock` taken through separate open file descriptions does not
  nest, so a `cs done` child under a parent holding trunk blocks forever.

**Slots.**
- At most N holders of a pool at once, across all worktrees of the galaxy.
- A crashed holder frees its slot (the kernel releases the `flock`).
- A run waiting for a slot is queued, not failed (#112).
- Holders of pool X never block holders of pool Y.

**Queued is not a stall (amendment of 2026-11-17).** With N slots and more
than N workers, a run can wait for a slot longer than
`PROPEL_BACKGROUND_CEILING` (45 minutes, issue #216). If that wait counted as
"gate run in flight", the ceiling would lapse and patrol would nudge a worker
that is legitimately queued.
- The gate runner records the wait and the run separately, in the molecule
  directory: `gates.queued` is written when the runner detaches, before it
  requests a slot, and names the waiting process; `gates.pid` is written only
  once the slot is held, and `gates.queued` is then removed. `gates.exit`
  keeps its meaning.
- Patrol skips a worker whose run is queued, without applying the ceiling,
  only when all of the following hold: the `gates.queued` process is alive,
  neither `gates.pid` nor `gates.exit` exists, and the pool has at least one
  live holder. Any other combination (dead queued process, no live holder,
  unreadable files) is unknown and is decided as before the rule existed,
  so a stale `gates.queued` cannot silence patrol.
- The ceiling applies from slot acquisition: the age of a run in flight is
  the age of `gates.pid`, and time spent queued is not counted in it.
- Rationale for exempting the wait from the ceiling: the wait ends when a
  holder releases its slot, and each holder's run is itself under the
  ceiling. This bounds the wait by the holders' runs; it is not a bound of
  45 minutes. Past the ceiling, patrol resumes nudging the holder; it does
  not free the slot, which only the holder's exit releases. With several
  runs queued, a run's wait is the sum of the runs ahead of it.

**Admission.**
- A refusal happens before any worktree, branch or session exists, and names
  the bound that was exceeded and the observed value.
- The counting rule for `max_active` is documented with the config field
  (open question 2, §9).

**Main.**
- In the train path, main moves only inside a cosmon verb holding the trunk
  lock, by a compare-and-swap fast-forward (`update-ref` against the
  recorded train base), to a history whose first-parent chain has the shape
  of k serial `cs done` merges. This invariant is scoped to train landing:
  `cs done` continues to move main by its own merge, and by its rollback.
- The landed tree equals the gated tree.
- Every first-parent commit of the landing range is a `--no-ff` merge whose
  second parent is the pinned tip of a Completed member (ADR-052 §I9).

**Closure.**
- No train mechanism writes `merged_at`. For train members it is written
  only by `cs done`, after git shows the member's tip reachable from base.
- Git ancestry plus `merged_at` stays the only record of "merged"; no train
  journal or aggregate asserts it.
- `cs done --expect-head <oid>` refuses, on the merge path and the
  already-merged path, when the branch tip is not `<oid>`: what lands or is
  closed as landed is exactly what was gated.

**Merge shape.**
- Each member's merge carries the subject `cs done` would write
  (`Merge branch 'feat/<mol_id>'` by default, or the project's
  `merge_subject` template) and the same trailer paragraph, byte-identical,
  produced by one exported renderer.
- No train-level merge commit reaches main.

## 4 · Acceptance criteria

The refutation tests below are the acceptance criteria of the increments
that implement this ADR. Each states the observation that refutes the
implementation.

**B1 — Gate slots.**
- N+1 concurrent holders of a pool of N: refuted if the maximum observed
  overlap exceeds N.
- `kill -9` the holder while another run waits: refuted if the waiter does
  not start.
- Two worktrees of one galaxy with N=1: refuted if both gates run at once.
  This catches a per-worktree state-dir resolution.
- Refuted if `gates.exit` appears before the bundle has run.
- Static check: refuted if any `cs done` path acquires a slot.
- Queued versus running (amendment of 2026-11-17). N=1, two workers; the
  second run stays queued longer than `PROPEL_BACKGROUND_CEILING` while the
  first holds the slot. Refuted if patrol nudges the second worker while it
  is queued. Then `kill -9` the first holder's process: refuted if the second
  run does not acquire the slot, or if it is nudged on acquisition on account
  of its time in the queue (its age restarts at `gates.pid`).
- Refuted if a `gates.queued` whose process is dead, or whose pool has no
  live holder, still exempts the worker from nudging.

**B2 — Named pools.**
- Refuted if holders of pool X block holders of pool Y.

**B3 — Admission at `cs tackle`.**
- Cap 2 with two Running molecules that have live sessions: refuted if the
  third spawn succeeds, or exits 0 without spawning.
- Refuted if a Completed-but-unharvested or an awaiting-human molecule is
  counted differently from the documented rule.

**D1 — Landing verb.** Each test is refuted if main moves or a member is
closed twice.
- A member fast-forwarded into the train instead of merged `--no-ff`.
- A first-parent commit in the landing range that is not a member merge.
- A member whose branch moved after it was pinned.
- Main moved after the train was gated (the CAS base no longer matches).
- A change to the trusted shell surface with no trust decision.
- A kill between the fast-forward and the closures, followed by a rerun.

Before landing, the verb also checks that every member is Completed and
that `[harvest_authority] required` is false; this increment refuses a train
when authority is required.

## 5 · Alternatives considered

- **The trunk lock spanning the full gate.** It stalls every `cs done` and
  `cs stitch` for the length of a gate (8–15 min in the reporter's runs).
- **A resident harvester holding a queue lock, or a Train aggregate with its
  own merged state.** It is a second source of truth for "merged", and no
  need beyond the operator loop has been shown. A persistent queue file is
  rejected for the same reason: it duplicates the ledger. The queue stays the
  derived view `harvest_queue::assess`.
- **`cs done --batch`, or `cs done` into a train base.** The first mixes a
  batch transaction into the per-molecule `cs done`. The second stamps
  `merged_at`, archives and deletes the branch before the train is accepted,
  so an ejected member would be "merged" while absent from main.
- **Extending `cs stitch`.** It is mission-scoped, gates per merge on main,
  runs no full gate per batch, and its subject fails provenance
  (`stitch.rs:450`).
- **A single train merge commit on main with a recursive provenance walk.**
  It needs a new provenance shape walked recursively, as `feat/issue-N`
  integration branches are; it puts members on a second parent; and
  `git bisect --first-parent` would step one train at a time instead of one
  molecule at a time.
- **Squash or rebase landing, or fast-forwarding members into the train.**
  These lose the merge commit that carries `Mol-Id` and `Mission-Id` (the
  contract `cs done` enforces at `transaction.rs:474-492`), put worker
  commits on main's first parent, produce a false `empty_branch`, and break
  ancestry-based closure.
- **Replay through k separate `cs done` merges plus a tree-equality check.**
  Other writers can interleave between the merges, and a partial rollback
  leaves `merged_at` set on molecules whose merge was undone.
- **Train-sync with carrier exclusion from the start (the reporter's
  practice).** A raw `git merge train` writes a subject that fails
  provenance. `cs sync --base train` writes an accepted trailer, but it
  passes CI only if the exact train state the member merged ends up on
  main's first parent, so a rebuilt train turns every train-synced member
  into a permanent provenance failure. It also creates carriers. Members
  sync from landed main through the existing `cs sync` instead; a
  conflicting member is ejected and returns after the next landing. Cost:
  one extra train cycle for that member, estimated and not measured.
- **Blame-based attribution in core.** Extracting `file:line` from gate
  output is language-specific, and blame falls back to "last one in" when it
  fails, which the reporter showed to be wrong. Bisection over the train's
  first-parent states is deterministic for a reproducible failure. Blame may
  remain an operator-side first probe.
- **Load-average or other machine-signal admission.** The reporter observed
  load averages of 4–15 for hours, then 600–960 within minutes; the signal
  misled in both directions. Memory pressure is rejected until someone gives
  a measurable cross-OS definition. `mkdir`+pid locks are rejected because
  `flock` already releases on process death.
- **Leaving trains wholly operator-side, with only the defect fixes and
  `--expect-head`/the exported message in cosmon.** An operator script that
  fast-forwards main itself bypasses the trunk lock, the harvest-authority
  check and the #109 predicate. The landing is the one point where main
  moves, so it is cosmon's; train build stays operator-side for now. One
  panelist (torvalds) held that the landing should enter cosmon only once a
  second operator script shows the same shape; the deliberation recommended
  otherwise for landing and kept that condition for train build.

## 6 · Non-goals

- Train build, the single train gate and bisection inside cosmon. They enter
  cosmon only when operator use shows a stable shape; a successor ADR or an
  amendment records it.
- Train-sync (`cs sync --base train`) with carrier detection. It is
  reconsidered only if the ejection rate measured under the no-train-sync
  rule justifies it.
- Load-average admission, and any admission input that is a machine-load
  signal.
- Batch size and wait time, busy detection, whisper wording, a built-in
  docs-only skip of the re-gate, build-directory placement, and pushing.
  These are operator or project policy; pushing from automated sessions is
  forbidden.
- A bound on scoped test runs a worker starts outside the runner. The slot
  bounds only what goes through it.

**Note of 2026-11-24 (non-goal re-examined, kept).** After deliberation
`delib-20261124-00b9`, the operator re-examined the first non-goal (train
build, the train gate and bisection inside cosmon) against the data the
reporter of issue #213 published, and kept it (decision A). No
`cs land-train --assemble` is planned. The panel's reasons:

- The entry criterion of this section, "operator use shows a stable shape",
  is not met. Between consecutive reports from the same operator, the
  train-merge step came back, blame came back after `git log -S` had replaced
  it, and practices the reporter had already described were missing from the
  list of steps.
- The second-script condition of §5 is not met: no second train-building
  tool exists.
- The two incidents the reporter described were process-supervision faults
  (a wait inside the gate command, an orphan acting on the operator's own
  exclusion list), not gaps in what this section keeps out of cosmon.

One change follows from the same decisions and is not a non-goal: a gate-slot
holder may hand its slot descriptor to the command, **opt-in only**, for a
pool declared for a controller (decision B). A controller pool of size 1 then
keeps its slot while any process of the run lives, so a second controller
waits instead of racing an orphan. The `full_gate` pool keeps the
close-on-exec descriptor (`crates/cosmon-cli/src/cmd/gates.rs`, module comment), and the default behaviour of
`cs gates slot` is unchanged. This is not implemented yet. The decisions on
`ADR-192` that came with these two are recorded in its amendment of the same
date.

## 7 · Where this meets shell trust (#215)

This section locates the intersections; it does not redesign trust.

- **Landing.** The closure range `pre_merge_head..HEAD` is empty for a
  train-landed member, so `cs done` never sees a surface change made by a
  train. The landing verb therefore runs the surface predicate itself, once,
  over `base..tip`, and refuses a surface change that carries no trust
  decision.
- **Train gate.** The train gate executes repository-supplied shell from
  branches that have not landed, in a shared worktree. This is the same
  exposure class as worker-side gates; the train adds no new class.
- **Closure under stale trust.** Increment A2 (§8) removes the trust
  dependency of the gate rung only. When `[hooks].pre_done` is configured,
  its own `ensure_trusted` call (`transaction.rs:2702`) still refuses closure
  under stale trust; whether it should be skipped on an empty diff is left
  to the A2 increment to decide. The `post_merge` hook on the same path
  skips with a warning under stale trust (`:3855`).
  *2026-11-17 (A2, #225):* decided. The `pre_done` hook still needs trust,
  because it is configured shell that would run; under stale trust the
  refusal names the hook and gives `cs trust` then a rerun. The `post_merge`
  hook no longer runs when `cs done` merged nothing.
- **Interim review gap opened by A2.** Today a stale-trust refusal at closure
  is the only stop, and an accidental one, for a shell-surface change that an
  operator train landed by fast-forward. A2 removes that stop on the gate
  rung. Until the landing verb (D1) runs the surface predicate over
  `base..tip`, nothing in cosmon reviews such a change. The A2 increment must
  therefore warn, naming every trusted-surface path changed between the
  molecule's fork point and its landing commit, or the operator accepts the
  gap explicitly when accepting A2.
  *2026-11-17 (A2, #225):* the warning exists. A closure that skipped the
  gate names every trusted-surface path changed between the branch's fork
  point (oldest branch reflog entry, else a merge base) and the landing
  commit, and says the changes are unknown when that range cannot be
  established. Its limits are recorded in ADR-158's amendment of 2026-11-17.
  The warning reviews nothing; the gap closes only with the landing verb.

## 8 · Amendment to ADR-158 (increment A2)

> **Applied.** Proposed here on 2026-11-16 as "proposed, not applied". The
> operator accepted increment A2, and the paragraph was written into
> ADR-158 as its amendment of 2026-11-17 by pull request #225 (merged),
> together with the warning that replaces the accidental stop (§7). The
> text below is the paragraph as proposed; ADR-158 holds the applied text,
> which also records what the increment implemented beyond it (the
> `post_merge` hook, the `pre_done` refusal wording, the warning) and its
> falsifiers.

> **No merge, nothing to verify.** When `cs done` finds the branch already
> reachable from base (`AlreadyMerged`) and `pre_merge_head..HEAD` is empty,
> the post-merge cascade runs no rung: no `integrity_command`, no cargo
> check, no `build_command`, and no `ensure_trusted` call on the gate's
> behalf. `cs done` stamps `merged_at` from ancestry and records the landing
> commit as evidence. The gate exists to verify a tree that `cs done`
> produced; on this path `cs done` produced none, and the tree on main was
> verified, or not, by whoever moved main. ADR-052 §I9 is unchanged: the
> molecule must still be Completed. The `pre_done` trust call and the #109
> surface check are not changed by this paragraph (see ADR-190 §7).

Acceptance test for A2: land `feat/A` with a `--no-ff` merge outside
`cs done`, with shell trust stale and a `build_command` that would fail.
`cs done A` must exit 0, report `already_merged`, stamp `merged_at` and run
no gate command.

## 9 · Open questions

1. **Second writer of `merged_at`.** The resident runtime stamps `merged_at`
   after its executor's `on_complete` returns `Ok`, and that executor returns
   `Ok` even when `cs done` exits non-zero (`crates/cosmon-runtime/src/lib.rs:557-594`,
   `:1528-1538`). This ADR adds no writer and does not change that path.
   Whether it contradicts the closure invariant of §3 needs its own
   decision.
   *Answered 2026-11-17 by pull request #226.* The runtime remains a writer
   of `merged_at`, but only for a branch that landed: the executor reports
   an error when `cs done` exits non-zero, and the runtime stamps
   `merged_at` only when `cs done` did not and `Executor::branch_landed`
   finds the branch reachable from its base
   (`crates/cosmon-runtime/tests/merged_at_landed.rs`). The record of
   "merged" stays git ancestry plus `merged_at`, as §3 requires.
2. **Counting rule for `max_active`.** Whether Completed-but-unharvested and
   awaiting-human molecules count toward the cap.
3. **Lifecycle path for a red-train culprit's fix.** Today it goes through
   `cs whisper` to a Completed molecule, which keeps changing its branch;
   whether Completed → working should be a lifecycle transition is unknown.
4. **Tackle-time base oid.** Whether the molecule records the oid it forked
   from, which the `empty_branch` fix (A3) needs to report `empty_branch`
   only when the tip equals the fork point.
   *Answered 2026-11-17 by the A3 change (#225).* The oid is not stored on
   the molecule, which records only the base branch name. The fork point is
   read from the branch reflog (its oldest entry); without a usable reflog
   the branch is not claimed empty.
5. **`cs stitch` provenance subject.** `stitch(<mol>): merge into <base>`
   fails CI provenance. It is a separate defect to be filed on its own.
6. **`post_merge` hook per member.** Read, not run: the hook runs on the
   `AlreadyMerged` path (§1.1), so a train of k members runs it k times.
   Whether a landing should run it once instead is open.
   *Premise changed 2026-11-17 (A2, #225):* the hook no longer runs when
   `cs done` merged nothing, so closing k train members runs it zero times.
   Whether a landing should run it once is still open.
7. **Operator ergonomics.** The deliberation had no operator-ergonomics seat.
   The cost of the no-train-sync rule against the reporter's 76-minute median
   is estimated, not measured.

## 10 · Consequences

- ADR-110's lock order gains a lock ahead of trunk. The existing rule
  (trunk before fleet, trunk dropped before the fleet purge) is unchanged.
- `cs done` gains `--expect-head` and an exported merge-message renderer
  (CLI changes; help, man page and docs are updated with them).
  *Landed 2026-11-17 (C1, C2, #225).*
- An operator train is safe to run with the defect fixes and
  `--expect-head` alone, landing under its own lock; the landing verb moves
  that last step under the trunk lock.
- The defect fixes the deliberation found (CAS rollback in `cs done`,
  closure of a landed branch without re-gating, the `empty_branch` label, the
  queue hint and landed-evidence scan) are independent of this ADR except
  for the ADR-158 amendment in §8.
  *2026-11-17:* all landed by #225 except the compare-and-swap rollback
  (A1), which is pending.
