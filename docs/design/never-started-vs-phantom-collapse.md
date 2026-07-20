# Is `Collapsed` the right terminal state when the runtime never started you?

Re-examination requested by `task-20260719-f45b` (ASK 3). This is a
findings note, not a decision: it maps the mechanism, corrects the
premise, and names what is actually broken. Choosing a fix is an ADR.

## The premise needs a correction

The brief assumes cosmon *chooses* collapse for a worker that never
started. It doesn't — not by default.

`auto_freeze_orphans` (`crates/cosmon-cli/src/cmd/patrol.rs:1139`)
targets **`Frozen`**, and only reaches for `Collapsed` when the caller
passes `--auto-collapse`:

```rust
let target_status = if auto_collapse {
    MoleculeStatus::Collapsed
} else {
    MoleculeStatus::Frozen
};
```

`Frozen` is non-terminal (`is_terminal` is `Completed | Collapsed`
only, `molecule.rs:184`) and `Frozen → Running` is legal
(`molecule.rs:215`). The recoverable state the brief asks for already
exists and is already the default.

The collapse came from the flag. `crates/cosmon-runtime/src/resident.rs:1172`
shells `cs patrol --auto-collapse --json` on every phantom-reap sweep,
unconditionally.

## The obvious defence of that flag does not hold

The reap exists to break the *flotte aveugle* deadlock (ADR-116 Part
B): a molecule stuck `Running` behind a dead worker blocks its
dependents, so the DAG can neither advance nor drain. The natural
reading is that the reap needs `Collapsed` **because terminality is
what releases successors**, and that freezing would trade a lost
molecule for a stalled DAG.

That reading is wrong. Readiness is not gated on terminality. The
authoritative reducer (`crates/cosmon-state/src/frontier.rs:181-220`)
decides per-status:

```rust
MoleculeStatus::Collapsed => true,              // releases unconditionally
MoleculeStatus::Frozen    => m.stuck_at.is_none(),
MoleculeStatus::Completed => m.merged_at.is_some(),
_ => false,
```

`Frozen` splits into two species. A **stuck-freeze**
(`stuck_at.is_some()`, set by `cs stuck`) keeps dependents blocked. A
**delivered-freeze** (`stuck_at == None`) **releases them**.

And `auto_freeze_orphans` never writes `stuck_at` — it sets `status`
and `updated_at`, nothing more (`patrol.rs:1193-1199`). So a
patrol-frozen orphan carries `stuck_at == None` and lands in the
releasing species.

**Therefore the default freeze already unblocks the DAG.** The
`--auto-collapse` in the resident buys terminality the frontier
reducer does not need in order to release. On the liveness argument
alone, the resident could stop passing the flag and get drainage *and*
recoverability at once.

## What is actually broken

Removing the flag would be the wrong lesson to draw, because the
freeze path has its own defect hiding underneath it.

A delivered-freeze means, per the reducer's own comment, that the
molecule "has *delivered* its work and is parked for visibility" — the
canonical case being a mission that finished decomposing into a child
DAG. An orphan has delivered nothing. Its worker died. Yet because
`auto_freeze_orphans` leaves `stuck_at` unset, the orphan is filed as
delivered and its dependents are released to run on top of work that
never happened.

So both branches of the flag are wrong, in opposite directions:

| | `--auto-collapse` (resident today) | default freeze |
|---|---|---|
| molecule | terminal — brief lost, id lost | recoverable ✓ |
| dependents | released | released |
| correctness of release | wrong (nothing delivered) | wrong (nothing delivered) |

The real gap is in the vocabulary, not the choice between the two
states: the delivered/stuck dichotomy has **no slot for "died without
delivering"**. Every orphan must be mislabelled as one or the other.
`Starved` (`molecule.rs:125`) is the precedent for carving out exactly
this kind of blameless-environment case — "external authority refused
service … invites a wait or a rotation; **never a re-prompt**" — and
it is correctly non-releasing (`_ => false`).

## The two populations the reap cannot tell apart

|  | worker ran, then died | worker never started |
|---|---|---|
| cause | crash, OOM, API loss mid-run | backend unreachable / model unservable |
| worktree | possibly dirty, partial edits | pristine |
| brief | may be partly consumed | untouched |
| repair | inspect, then re-nucleate | fix the environment, retry as-is |
| collapse destroys | ambiguous state (defensible) | a recoverable molecule (not defensible) |

A discriminator already exists in the data: the collapse path records
`collapsed_step` (`patrol.rs:1198`). `collapsed_step == 0` with no
artifacts is a strong never-started signal, available today without a
schema change.

## What already changed

The preflight that landed in this molecule removes the largest observed
source of the never-started population: a dispatch to a backend that
cannot serve the resolved model is now refused before the worktree
exists, leaving the molecule `pending`. It never reaches the reaper.
Both molecules cited in the brief (`task-20260719-059b`,
`task-20260719-e02c`) would be refused today.

That lowers the urgency but does not close the gap — tmux failure, OS
kill between spawn and first turn, or an adapter binary vanishing all
still land in the same conflated bucket.

## Options

1. **Stamp the reason, keep the state.** Record a machine-readable
   `never_started` marker (derivable today from `collapsed_step == 0`
   plus an empty artifact set) so an operator or a later sweep can
   revive with confidence. Smallest diff; no enum or transition
   change; it is also the input options 2 and 3 both need.

2. **Stop releasing dependents on a non-delivered freeze.** Have
   `auto_freeze_orphans` set `stuck_at` when it freezes an orphan,
   moving it into the stuck-freeze species so children stop
   dispatching on undelivered work. This is the correctness fix, and
   it is independent of the never-started question — but it will
   surface DAGs that currently drain by accident, so it needs its own
   blast-radius review.

3. **A distinct status** (e.g. `Stranded`): non-releasing, semantically
   "never started, brief intact, retry once the environment is
   repaired". The enum doc at `molecule.rs:86` explicitly anticipates
   additive variants, and `_ => false` in the reducer means a new
   variant defaults to non-releasing — the safe direction. Cleanest
   semantics, largest blast radius.

Recommendation: **1 and 2 are separable and 2 is the more important
finding**, though it is also the one most likely to change observed
DAG behaviour. 1 is safe to land immediately. 3 only pays for itself
if 2 proves insufficient.

## What would falsify this note

The load-bearing claim is that `auto_freeze_orphans` leaves `stuck_at`
unset, putting patrol-frozen orphans in the *releasing* species. It was
checked by reading `patrol.rs:1184-1226`, where only `status`,
`updated_at`, and (on the collapse branch) `collapse_reason` /
`collapsed_step` are written. If some earlier stage stamps `stuck_at`
on a dispatched molecule, orphan freezes are stuck-freezes instead,
dependents already stay blocked, and the second half of this note
collapses to "the flag is simply unnecessary."

An earlier draft of this note asserted the opposite of the frontier
finding — that terminality gates release, and that freezing would
stall the DAG. Reading `frontier.rs` refuted it. The claim above is
stated so the next reader can do the same to it.
