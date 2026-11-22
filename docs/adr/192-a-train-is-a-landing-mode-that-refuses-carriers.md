<!-- SPDX-License-Identifier: AGPL-3.0-only -->
# ADR-192 — A train is a landing mode that refuses carriers: no unlanded import, exclusion by first own commit, ejection only over a green base

**Status:** Proposed (2026-11-20).
**Date:** 2026-11-20.
**Decider:** Noogram.
**Authoring molecule:** `task-20261120-83c1`.
**Parent deliberation:** `delib-20261120-9b6f` (a panel of four).
**Answers:** operating data that the reporter of GitHub issue #213 published
on that issue after running trains under
[ADR-190](190-bounded-verification-and-train-landing.md) D3.

**Amends:** [ADR-190](190-bounded-verification-and-train-landing.md) D3, by
the text in §3 below, which is proposed and not applied: ADR-190's D3 is not
edited while this ADR is Proposed. ADR-190 §6 (non-goals) is kept unchanged.
**Binds:**
[ADR-052](052-one-ledger-one-writer-one-witness.md) §I9 (a merged branch
belongs to a Completed or Collapsed molecule),
[ADR-110](110-single-writer-trunk-and-coordination-invariants.md) (one
writer of the trunk),
[ADR-082](082-architecture-baseline.md) (I/O-free core behind ports).

**Why an amendment and not a successor.** ADR-190 §3 requires a successor ADR
for a change that breaks one of its invariants. This ADR breaks none: it
keeps every §3 invariant, narrows how D3's `--exclude` decides a carrier,
adds a precondition to D3's ejection step and adds invariants. The
repository's practice for that case is a new numbered ADR that names the
section it amends ([ADR-022](022-native-dag-scheduler.md) amends ADR-016 §4;
ADR-190 amends ADR-110 I1). ADR-190 is Accepted, so its text is annotated
rather than rewritten, as ADR-190 did for ADR-158 (its §8).

**Scope.** This ADR decides what a train is in relation to a polymer, who
builds it, how a red train is reduced, and which carrier rule the landing
enforces. It adds no code. The increments that would implement it are
listed in §8; none is started by this ADR.

**What the operator has decided, and what is open.** The operator chose the
carrier rule: no unlanded import (I3) together with exclusion by the
excluded molecule's first own commit (I4). Four further questions are open
and recorded in §5 with a recommended option and its alternative: a typed
"could not run" gate verdict (O2), gate evidence at landing (O3), reopening
a Completed culprit (O4), and a red prefix under a green speculative
extension (O5). No text in this ADR accepts any of them.

---

## 1 · Context

### 1.1 The questions

An operator ran trains under ADR-190 D3 and reported red trains whose
reduction, by removing members one at a time, repeatedly removed the wrong
member, and members that carried a removed member's commits into the
rebuilt train. Four questions follow.

- **Q1.** Is a train a distinct object in cosmon, or a mode of landing? And
  how does it relate to a polymer, the DAG of molecules linked by `Blocks`
  edges (THESIS, "Polymer")?
- **Q2.** Who builds a train, where does that rule live, and is there a
  write-set invariant between members?
- **Q3.** When a train's gate is red, what is ejected, how is it found, and
  what happens to the state of the molecules downstream of an ejected one?
- **Q4.** May a train be gated speculatively on the base it is expected to
  land on, while an earlier train is still in flight?

### 1.2 What the code does today (read, not run)

Paths are repository-relative; line numbers are those of the tree at the
time of writing.

| Fact | Where |
|---|---|
| `cs land-train` takes `--train-base`, `--train-tip`, one `--member <id>=<head>` per member and repeatable `--exclude <id>=<head>`. It has no check-only mode. | `crates/cosmon-cli/src/cmd/land_train.rs:41-57` |
| Under the trunk lock, the base must equal `--train-base`, or already have the tip on its first-parent chain (a rerun); otherwise "moved after the gate". | `crates/cosmon-harvest/src/train.rs:408-424` |
| The first-parent chain of `train_base..tip` must be exactly one two-parent merge per member, in order, each with the member's pinned head as second parent. Its message must start with the member's rendered `cs done` message, byte for byte, and may continue only with trailer lines (what a sign-off hook appends). | `train.rs:200-268` (`check_train_chain`), called at `:426-429`; message rule `:270-296` (`merge_message_matches`) |
| Each member must be Completed, integrate into this base (a member with another base branch is refused), and, unless already closed, have its branch at the pinned head. | `train.rs:431-489`; the base-branch refusal at `:443-453` |
| An exclusion is refused when its id is also a member; otherwise its head must resolve, and a member is refused when `git merge-base --is-ancestor <excluded head> <member head>` succeeds. Only the exact head the operator passes is tested. | `train.rs:510-550`; the ancestry test at `:529-531`; called at `:491` |
| The exclusion check runs inside `land`, after the trunk lock is taken, so after the operator has paid for the gate. | `train.rs:408-410`, `:491` |
| An integration test asserts that a member carrying an earlier commit of the excluded molecule lands when the operator names a later head of that molecule. | `crates/cosmon-cli/tests/land_train.rs:494-502` (`exclusion_allows_landing_when_no_member_contains_the_pinned_head`) |

*Annotation (2026-11-22):* the table above records the code as it stood when this ADR was written. Since then, `--exclude` tests the excluded member's first own commit, the oldest entry of `git rev-list --first-parent <excluded head> ^<train base>`, so a member carrying any own commit of the excluded member is refused, and an excluded head already reachable from the train base refuses no member. This is I4 below. It still misses a rebased culprit. See `docs/guides/train-landing.md`. The ADR's status and decisions are unchanged.
| The base moves by `git update-ref <base> <tip> <train_base>`, a compare-and-swap. | `train.rs:827-836` |
| `land` reads no gate evidence: nothing checks that the tip it moves the base to was gated. | `train.rs:342-508` (no gate input in `LandTrainRequest`, `:112-123`) |
| `land` does not read `blocked_by`. A dependent enters the frontier only when its blocker is Completed and has `merged_at`. | `train.rs:342-508`; `crates/cosmon-state/src/frontier.rs:168`; `crates/cosmon-core/src/molecule.rs:194-196` |
| Members are closed after the lock is released, each by a `cs done <id> --expect-head <head>` child. | `land_train.rs:151-200` |
| The resident loop never emits `Done` for a molecule tagged `no-auto-harvest` or `harvest_to:<branch>`. | `crates/cosmon-runtime/src/resident.rs:390-395`; test at `:2995` |
| The non-resident runtime calls `on_complete` (which runs `cs done`) for each newly Completed molecule of the DAG, and `cs run`'s teardown runs `cs done` on every Completed molecule, selected by status alone. No `no-auto-harvest` check was found on either path. | `crates/cosmon-runtime/src/lib.rs:1555-1590`; `crates/cosmon-cli/src/cmd/run.rs:589-612`, `:1085-1087` |

### 1.3 Where the code and ADR-190 disagree

- **Landed tree and gated tree.** ADR-190 §3 ("Main") states that the landed
  tree equals the gated tree. The code cannot enforce this: `land` takes no
  gate evidence (§1.2). The invariant holds today by operator assertion
  only. This ADR states it as such (I10) and leaves the check open (O3).
- **Base-first.** The train guide (`docs/guides/train-landing.md`, "Rebuilding
  a red train", step 1) tells the operator to gate the base alone before
  dropping any member. ADR-190 D3 does not say it. §3 below adds it to D3.
- **`--exclude` and the §6 non-goal.** ADR-190 D3 now carries `--exclude`
  (added after acceptance, CHANGELOG [Unreleased]), while §6 keeps
  "train-sync with carrier detection" as a non-goal. The two are compatible
  only if `--exclude` is read as a landing backstop; §3 says so.
- **Merge message.** ADR-190 D3 and §3 ("Merge shape") require each member
  merge to carry exactly, byte-identically, the message `cs done` would
  write. The check accepts that message followed by trailer lines
  (`train.rs:270-296`), because a `prepare-commit-msg` sign-off hook appends
  one to `cs done`'s own merge as well. The rendered text itself stays
  byte-exact; this ADR does not change the rule.
- **Increment status.** ADR-190's increment table said A1, B1–B3 and D1 had
  not landed although they had. Its table is refreshed with a dated
  annotation in the same change as this ADR.

## 2 · Decision (proposed)

### Q1 — A train is a landing mode

A train is a chain of `--no-ff` merges of the pinned heads of Completed,
unmerged molecules on one base, landed by one compare-and-swap. It has no
identity, status or journal in cosmon; git ancestry plus `merged_at` stays
the only record of "merged" (ADR-190 §3, "Closure").

A polymer layer is one admissible source of members among others: several
polymers and loose molecules can share a train. The two differ in when
membership is known. A polymer layer's membership is declared before the
work, by `Blocks` edges. A train's membership is observed after the work,
from the ancestry of pinned heads.

For a polymer layer to ride a train today, its members must be held from
per-molecule harvest with the `no-auto-harvest` tag; otherwise the resident
loop lands each one as it completes. Their dependents wait on `merged_at`
and are released when the train's closures stamp it. The non-resident
runtime and `cs run`'s teardown do not consult the tag (§1.2, read, not
run); I9 states the invariant and its test settles it.

Rejected: a Train aggregate or a train status, which is a second source of
truth for "merged" (ADR-190 §5); and "a train is a polymer layer", because a
layer is not a correctness boundary, and molecules with no `Blocks` edge
between them can still conflict.

### Q2 — The operator builds the train; ADR-190 §6 stays

Train build stays in the operator's tool. Under the resident runtime a
polymer never accumulates a batch of Completed, unmerged molecules: each is
landed as it completes, before its dependents are dispatched. A train built
by a polymer would race that sweep. The conditions ADR-190 set for moving
train build into cosmon are not met: operator use showing a stable shape
(§6), and a second operator script converging on the same shape (the
condition §5's last alternative keeps for train build).

What cosmon owes the operator's tool is on the landing side:

1. `cs land-train --check`: every landing check, without the trunk lock,
   the compare-and-swap or any write, so a malformed train or a carrier is
   refused before a gate is paid for (I5).
2. A carrier rule that does not depend on the operator remembering a head
   (I3, I4).
3. Optionally, a JSON listing of harvest candidates with their current
   heads.

**Write sets.** Cosmon adds no file-overlap invariant between members. The
one write-order invariant is I11: a train never holds a member whose blocker
is neither merged nor earlier in the same train. Today only the frontier
keeps it (§1.2). Between independent members, the operator's tool may use
the observed write set,
`git diff --name-only $(git merge-base <base> <head>) <head>`, as a
scheduling hint: an overlapping candidate waits for the next train.
Disjoint observed write sets make the tip's tree independent of member
order. Semantic overlap is not decidable statically; the gate on the exact
tip is its only witness.

Rejected: a formula step (a formula drives one molecule, a train spans
several); a rule in `cs run` (it races the resident sweep); a `cs train`
build verb (it lifts §6 on one operator's evidence); refusing members whose
files overlap (it refuses gated green work); declared write sets (no such
field exists, and it replaces an observation by a forecast).

### Q3 — Red train: base first, then prefix bisection; no new state

In order:

1. Refuse carriers before gating (I3, I4, through `--check`), so bisection
   never runs on a train that holds one.
2. On a red tip, gate the base alone (prefix 0) in the same environment. If
   the base is red, refuse the batch and eject nobody (I6).
3. Otherwise bisect over the train's first-parent states. Eject the member
   whose merge turns the first prefix red, rebuild the train from the
   current base, and gate it again.

For one culprit this costs ⌈log₂ k⌉ + 2 gate runs (5 to 6 for k = 16).
Bisection is the verdict. Searching the failure's quoted location in each
member's history is an operator probe, and its suspect must be confirmed by
a red/green pair of adjacent prefixes. None of it enters cosmon's core
(ADR-190 §5, blame-based attribution).

**Downstream state.** No new state. The ejected member stays Completed with
`merged_at` unset. Its dependents stay Pending, because a `Blocks` edge
clears only on Completed with `merged_at` set (`frontier.rs:168`). They are
released when the member lands in a later train. A refused batch moves
nothing.

Rejected: an "Ejected" status (a train journal by another name); collapsing
the culprit (a Collapsed blocker never clears, so its dependents are
stranded); refusing the whole batch on any red; ejection by blame or by
train order; detecting carriers by repeated removal.

**Can `cs land-train` today refuse a member whose pinned head reaches
commits of a member removed from an earlier train?** Partly. `--exclude <id>=<head>` refuses a
member that contains that exact head, only at landing, and only if the
operator passes the head the removed member had in the earlier train
(§1.2). A carrier of an earlier commit lands, and a test asserts it. I4
closes that gap (implemented since: see the annotation under §1.2), `--check` moves the refusal before the gate, and I3 refuses
a carrier with no `--exclude` at all.

### Q4 — Speculative gating already works through one compare-and-swap

Build T′ as T plus the next members on T's tip, gate it, and land it as one
call from T's base naming all members. Landing T and then T′'s suffix from
T's tip is refused in a galaxy whose closures commit state to the base,
because the base no longer equals T's tip: the refusal is safe, and only
the speculation is lost.

A speculative green is landable only if the gate ran on that exact tip, the
pinned heads are unchanged, the base is still at the recorded train base,
and its members synced from the landed base. What to do when a contained
prefix gated red while the extension gated green is open (O5). A green
obtained in a cloned build directory is the project's concern; treat it as
provisional until shown equivalent to a clean build.

Rejected: relaxing the compare-and-swap for bookkeeping commits, and
rebasing a green tip onto a moved base. Both land a tree nobody gated.

## 3 · Amendment to ADR-190 D3 (proposed, not applied)

> **Proposed.** The paragraphs below would replace the last two sentences of
> the first paragraph of ADR-190 D3 if this ADR is accepted. Until then ADR-190's D3 is unchanged.

> **Red train.** No member is ejected without a green gate verdict on the
> exact train base, obtained in the same environment as the red verdict on
> the tip. A red base refuses the batch and ejects nobody. Over a green
> base, a red train is rebuilt from the current base without the member
> found by prefix bisection over the train's first-parent states.
>
> **Carriers.** Once a survey of legitimate flows has shown that none fails
> it, a member whose first-parent chain above the train base contains a
> merge of a commit that has not landed (I3) is refused, with or without
> `--exclude`. At landing, repeat `--exclude <id>=<head>` for every
> removed member; `cs land-train` refuses a member whose pinned head contains
> the first own commit of an excluded molecule (I4). `--exclude` is a
> landing backstop. It does not lift the ADR-190 §6 non-goal "train-sync
> (`cs sync --base train`) with carrier detection": members still sync from
> the landed base, and a train-synced member is refused, not repaired.

**First own commit.** For an excluded molecule `e` with head `h`, its first
own commit is the oldest entry of `git rev-list --first-parent h
^<train_base>`. Any member that carries any commit of `e`'s own first-parent
history above the train base also carries that commit, so testing it covers
every later head of `e`. Two limits:

- A culprit that was rebased after the earlier train has new commit ids and
  is not recognised. I3 does not catch it either if the carrier rebased
  onto it (see I3's limit test).
- When `h` is already an ancestor of the train base, the list is empty and
  `e` excludes nothing. Today's exact-head test refuses, in that case, every
  member that contains `h` through the landed base, although those commits
  are already on the base (read, not run).

**Precondition for enforcing I3.** Before I3 refuses anything, a read-only
survey must establish that no legitimate flow puts an unlanded merge on a
member's first-parent chain above the base: `cs sync` from the base,
`Base-Sync` trailers, stacked molecules (one tackled on another's branch)
and integration bases. A `cs sync` merge is expected to pass, because its
second parent is an older base commit; this is unverified. Integration bases
are already refused (`train.rs:443-453`). If the survey finds a legitimate
flow that fails I3, I3 is not enforced and the operator decides again.

## 4 · Invariants

Each invariant comes with the observation that refutes it. **[T]** marks an
invariant that an existing test already exercises.

| # | Invariant | Refuted if |
|---|---|---|
| I1 [T] | Git ancestry plus `merged_at` is the only record of "merged". No train mechanism writes `merged_at`, and no train-level commit reaches the base. | A member has `merged_at` set after a landing was killed before its closures. Existing test: `a_rerun_after_a_kill_before_any_closure_closes_each_member_once` (`land_train.rs:1079`). |
| I2 [T] | The base moves only by a compare-and-swap from `--train-base`, under the trunk lock, to a tip whose first-parent chain is exactly the named member merges, each pinned head unchanged. | A landing succeeds after the base moved, or after a member branch moved. Existing tests: `the_base_moving_after_the_gate_is_refused` (`:818`), `a_base_move_at_the_compare_and_swap_preserves_the_ref_and_checkout` (`:833`), `a_member_whose_branch_moved_after_the_pin_is_refused` (`:788`). |
| I3 | No unlanded import. On every member's first-parent chain above the train base, each merge's second parent is an ancestor of the train base. The rule needs no `--exclude`. **Conditional:** enforced only after the survey of legitimate flows in §3 finds none that fails it. | A member that merged a train holding another member lands with no `--exclude` given. A companion test records the known limit: a member rebased onto the train passes. |
| I4 | Exclusion by first own commit. No member contains the first own commit of an excluded molecule (§3). It replaces the exact-head test at `train.rs:529-531`. | The fixture of `land_train.rs:494-502` lands. It lands today by design, so under I4 its expectation flips. A second test: an excluded head already reachable from the train base refuses no member. |
| I5 | `--check` equals `land` minus the lock and the move. | A ref or state file changes under `--check`; or an input passes `--check` and `land` then refuses it for a reason other than the base or a member branch having moved. |
| I6 | Base first. No ejection without a green verdict on the exact base oid, obtained in the same environment. A red base refuses the batch. | In the reference operator tool's test, with a red base, any member is ejected. |
| I7 | "Could not run" is never red. A gate that did not run (build directory unreachable or unwritable, toolchain missing) yields a verdict that bisection and ejection never consume. Where it is produced is open (O2). | With the build directory unmounted, the verdict reads as a code failure, or an ejection follows. |
| I8 | Ejection releases nothing. The dependents of a Completed member with `merged_at` unset stay Pending. | Such a dependent enters the frontier. |
| I9 | One landing path per molecule. A molecule pinned to a train is never landed by the resident `Done` sweep, by the runtime's `on_complete`, or by `cs run`'s teardown while it is in a train. | A pinned member without the `no-auto-harvest` tag is landed by `cs done` from the runtime while it is in a train. §1.2 suggests the non-resident paths do not consult the tag; this test settles it. |
| I10 | The landed tree equals the gated tree, stated as scope. Today it holds by operator assertion, because `land` takes no gate evidence (§1.3). Whether `land` should require evidence is open (O3). | Documentation-level until O3 is decided. |
| I11 | A train never holds a member whose blocker is neither merged nor earlier in the same train. **Conditional:** needed only if a manual `cs tackle` can produce a Completed member whose blocker is unmerged, which is unverified. | A train `[B]`, with B blocked by an unmerged A, lands. |

## 5 · Open decisions

O1 is decided by the operator. O2 to O5 are open: each is recorded with
the recommended option and the alternative, in the order the operator is
expected to take them.

- **O1 — Form of the carrier rule. Decided by the operator:** I3 together
  with I4, I3 subject to the survey in §3. Alternatives that were weighed: a
  scan for merges carrying another molecule's `Mol-Id` trailer (needs the
  same survey); a pairwise predicate over the named members only (catches
  carriers among named members only).
- **O2 — Typed "could not run" verdict (I7).** Recommended (a 3–1 majority of
  the panel): in cosmon's runner `scripts/run-gates-bg.sh`, decided by
  preflight probes (build directory reachable, writable, with free space)
  and never by parsing compiler output; the runner writes a distinct verdict
  next to `gates.exit`. Alternative: a project-side probe only, on the
  ground that a missing `gates.exit` already reads as incomplete.
- **O3 — Gate evidence at landing (I10).** Recommended: state I10 as scope
  and add no check yet. Alternative: `land` requires a recorded green
  verdict on the tip oid.
- **O4 — Reopening a Completed culprit for its fix (ADR-190 §9 Q3).** One
  panelist proposed a Completed → Running transition allowed only while
  `merged_at` is unset, with a recorded reason. Recommended: keep it out of
  this ADR and leave ADR-190 §9 Q3 open, or give it its own deliberation;
  one proposal is not a convergence. Alternative: adopt that transition
  here.
- **O5 — Red prefix, green speculative extension (Q4).** Recommended: land
  nothing until the contradiction is resolved, because a gate verdict that
  is not a function of the tree is a determinism failure. Alternative: land
  the green extension, since the gate judges the tree that lands, and treat
  the red prefix as a flakiness signal for the operator.

## 6 · Operator-cost flags

The panel had no operator-ergonomics seat. Two costs are carried here as
flags, not as measurements:

- **Base-first** costs one full gate per red train. The saving it buys rests
  on the cascades the operator measured, where removals continued past a
  red base; it is not an estimate.
- **Refusing carriers** forbids the train-sync practice outright. Its cost,
  one extra train cycle for a conflicting member that must wait for the
  landed base, is still the estimate ADR-190 §9 Q7 records. No measurement
  exists, and data gathered under train-sync cannot provide one, because it
  does not measure the ejection rate under the no-train-sync rule.

## 7 · Alternatives considered

Rejections specific to each question are listed under it in §2. Two
alternatives span the whole decision:

- **Moving train build, gate and bisection into cosmon now.** It lifts
  ADR-190 §6 on the evidence of one operator's tool, and under the resident
  runtime it races the per-molecule landing sweep (Q2).
- **Leaving `--exclude` as an exact-head test and documenting the duty to
  pass the right head.** This is today's behaviour. It depends on a head
  that is stored nowhere but in the operator's memory or tool, and the
  reported carrier scenario (a culprit that received a fix, then was
  excluded with its new head) lands under it.

## 8 · Consequences

If this ADR is accepted:

- ADR-190 D3 takes the text of §3, and ADR-190 §6 is unchanged.
- The following increments become possible. Each is a separate molecule,
  and the first three touch `crates/cosmon-harvest/src/train.rs` and
  `crates/cosmon-cli/tests/land_train.rs`, so they are serialized in this
  order:
  1. I4: flip the test at `land_train.rs:494-502` first, then replace the
     exact-head test with the first-own-commit test as a pure function plus
     a thin git reader, add the reachable-from-base test, and update step 3
     of `docs/guides/train-landing.md`.
  2. I5: `cs land-train --check`, with the equivalence test, regenerated
     help, man page and reference docs, and the CLI/UI parity audit.
  3. I3: the survey of §3 first, then the predicate only if the survey
     allows it, with its refutation and limit tests.
  4. I9 and I11: a failing test that establishes whether a train member can
     be landed by the runtime or by `cs run`'s teardown, then the smallest
     guard; and whether a manual `cs tackle` can produce a Completed member
     with an unmerged blocker.
  5. I7: only if O2 is decided in favour of cosmon's runner.
- Unrelated to the decision, the comment at
  `crates/cosmon-runtime/src/dag_policy.rs:163-176` says a Collapsed blocker
  releases its dependents; the frontier does not (`frontier.rs:168`). It is
  a separate documentation fix.

Whatever is decided, a falsifier remains available with the reporting
operator's logs: for each red train, gate prefix 0 first, then bisect, and
compare the result with the member whose removal turned the train green.
Q3's recommendation is wrong if bisection names the wrong member in a train
with a green base, no carriers and a deterministic failure.
