# ADR-052: One Ledger, One Writer, One Witness per Field — Retiring the Three-Source Run Plane

**Status:** Accepted
**Date:** 2026-04-19
**Parent task:** `task-20260419-d88a`
**Governing deliberation:** `delib-20260419-d34b` (9-persona panel: godel,
feynman, wheeler, einstein, carnot, torvalds, jobs, shannon, tolnay) —
synthesis at `.cosmon/state/fleets/default/molecules/delib-20260419-d34b/synthesis.md`
**Imports from:** mailroom
ADR-017 — Always-Alive Executor
(Gödel sentence *G*, kill-switch K1–K4) and the lore entry
*Opération Executor — naissance du Mailroom résident*
(verdict-door §4, witness §9, fleet topology §10).
**Related:**
[ADR-016](016-autonomy-regimes-and-resident-runtime.md) (autonomy regimes —
Layer A stateless / Layer B resident),
[ADR-032](032-p-external-witness-axiom.md) (`P_external` axiom — the
constitutional ground of "no system certifies itself"),
[ADR-046](046-p-legibility-axiom.md) (`P_legibility` — every state
decision is human-legible),
[ADR-049](049-cosmon-ward-feedback-flow.md) (cosmon-ward feedback flow —
this ADR is the third binding instance of that rule, after the convoy
cascade and the harness-ignore primitive).

## Context

Two galaxies — cosmon and mailroom — discovered the same pathology
within 48 hours, on independent codebases, written in different languages,
maintained by the same operator under different mental models.

### The 9 ghosts of 18–19 April

| Galaxy | Molecule | Drift shape |
|---|---|---|
| cosmon | `task-20260413-dfd8` | runtime kept emitting `Evolve` against a phantom session (tmux dead, fleet still `Registered`, worker process gone) |
| cosmon | `task-20260416-192a` | mode-mismatch: `mol_status = Completed`, fleet still `Registered`, branch never merged — woke up the next morning as a ghost in `cs ensemble` |
| cosmon | `task-20260413-c1cb` | pilot rebased + force-pushed inline; the merge happened outside the state machine; `cs done` was never called; the ledger has no record of the transition |
| mailroom | `d902`, `93a7`, `af87`, `ffc1`, `b387`, `f2a3` | six `/ask` molecules where the bot harness called `cs nucleate` but never `cs tackle`; the pilot (Claude) opened the tools, read the transcripts, wrote the reply itself, and pushed JSON to the outbox; the molecule sat `Pending` while the user received an "answer" |

Nine ghosts. Two galaxies. One pathology shape: **the state of the
running system is narrated by three witnesses who no longer trust each
other because nobody named who holds the pen.** `fleet.json.desired`
says one thing; `tmux has-session` says another; `molecule.status`
says a third. Whoever read most recently won — until the reader was
the operator's morning eyes on `cs ensemble`, at which point the
reader saw "running" molecules that had been corpses for hours.

The three mailroom principles imported in the operator prompt name
the same disease from different angles:

1. **Verdict-door** *(mailroom lore §4 of the Executor chronicle)* —
   the entry-point of a system is a verdict, not a menu. `cs tackle`
   today dispatches cognition-mode vs runtime-mode through opaque
   heuristics (DAG-root detection, blocker presence). The 78bf convoy
   cascade and the dfd8 ghost are both cases of the door silently
   choosing the wrong side. A door that decides cannot be a menu.
2. **Silence-as-signal** *(mailroom ADR-017 §D3, Gödel sentence
   G)* — no piece of a living system has the right to self-certify
   it lives. A worker whose tmux session was OOM-killed leaves
   `fleet.json.desired = Registered`; the registry never asked tmux,
   and so the registry believes its own past assertion until an
   operator runs `cs kill` by hand. Silence must be *observed* (a
   probe) and *emitted* (a witness event), not assumed-alive.
3. **Evidence-chain** *(mailroom ADR-017 §D6 + lore §11)* — a
   green flag is not a rendered pixel. `cs tackle` today reports
   *"session created"* the moment the tmux pane was spawned, even
   when the pane has already died (`spawn-liveness conflation`,
   chronicle 2026-04-17). A claim of life is honest only if a
   subsequent observation by a different surface confirms it.

The pathology is not a bug fixable by a patch. It is a *structural
absence*: cosmon does not declare which actor owns which field, and
so every actor — pilot, worker, runtime, probe, sibling shell — feels
entitled to write any of them. Drift is not three facts disagreeing.
Drift is *one* fact narrated by an unnamed pen.

### Independent re-discovery is the signal

The deliberation `delib-20260419-d34b` posed the question to a
9-persona panel and asked for the *crystalline* unifying thought.
Six of the nine personas (Einstein, Wheeler, Shannon, Torvalds,
Tolnay, Carnot) independently derived: **the three "truth sources"
are one projection + one probe + one worker assertion;
`fleet.json.desired` is a cached shadow.** Five of the nine (Feynman,
Gödel, Wheeler, Jobs, Einstein) independently derived: **liveness
requires an external witness; nothing in the kitchen certifies
itself.** Five of the nine (Gödel, Jobs, Wheeler, Feynman, Torvalds)
independently derived: **the c1cb + mailroom-6 class is a Gödel
sentence — stateable but not enforceable from inside; detection,
not prevention, is the achievable goal.**

Four-plus-disciplines-same-conclusion is the *silence-rediscovered-
four-times* signature from mailroom 2026-04-17. When it appears,
it marks a *structural dependency of the problem*, not an opinion
of the panel.

The Feynman passage that validated the synthesis (`synthesis.md` §d,
483 words, child-test passes) compressed the entire pathology into
two empirical shapes a child can name:

- *"The ticket says 'lasagne, done.' But the lasagne never reached
  table 4, because nobody carried it. The ticket lies."*
  (I5/I9 — `Completed` without merge, or pilot-merged inline.)
- *"The chef walks away from the kitchen, and the alarm clock keeps
  buzzing on the empty apron, hanging on a hook."*
  (I3/I4 — fleet registered, tmux dead.)

Either shape, named without jargon, is a ghost.

## Decision

### D0. The vision sentence (verbatim from Jobs / Einstein synthesis)

> **Cosmon is a filesystem that remembers which worker owns which
> decision, so no one — not even the pilot — can answer in the
> worker's place.**

Seventeen words. Carried unanimously across the panel — Wheeler,
Feynman, Tolnay, Torvalds, and Einstein all produced restatements
that compress to this. It replaces every prior framing of cosmon
("agent runtime", "state machine library", "DAG orchestrator") in
operator-facing prose. The catalog stays in `THESIS.md`; the
verdict is the door.

### D1. The ten named invariants (I1–I10)

Each invariant carries: a **TLA-ready statement** (the formal
skeleton an external prover can check), a **child analogy** (the
Feynman test — if a smart 8-year-old does not grasp it, the
invariant is not yet ready), the **empirical ghost** it would have
blocked, and an **enforceability classification** (in-band: cosmon
can prove it from inside; out-of-band: cosmon can only *detect*,
not prevent — Gödel territory).

> **Amendment 2026-04-20 — eighth clock (StepClock).** After fixture
> `idea-20260419-2d4e` (the 4-hour silent molecule) the panel
> `delib-20260420-1b02` proved the seven clocks above were
> insufficient — they recorded *worker-process* liveness but not
> *step-emission* liveness. An eighth clock (hawking, `StepClock`)
> and a companion liveness invariant `I_StepProgress` (knuth) were
> added. The new ghost shape is `InferenceStalled` (turing — the
> seventh `GhostKind`). The mechanical proof lives in
> `docs/specs/CosmonRun.tla` (action `MarkStalled`, variables
> `sealLog`/`now`, property `I_StepProgress`) and
> `docs/specs/CosmonRun_StepProgress.cfg`. The Rust-side enum
> extension is tracked separately under the Phase 1 polymer
> workstream.

#### I1 — SingleLedger

**TLA-ready.** The primary observable is `Ledger : List<Event>`,
append-only, single writer per `(molecule_id, seq_no)`. For every
other view `V`, there exists a pure total function `V = f(Ledger)`.
`molecule.status`, `fleet.json.desired`, and surface markdowns are
**materialized views with monotonic watermarks** — never independent
truth.

**Child.** *The tree of pinned tickets never forgets. The tree never
lies. Every new ticket is pinned to the branch of the ticket that
must finish first.*

**Ghost blocked.** All nine. Every ghost is either (a) a projection
written eagerly and then not refreshed, (b) a probe whose reading
disagreed and the disagreement was discarded, or (c) a filesystem
write that bypassed the ledger (c1cb).

**Enforceability.** **In-band** for `molecule.status` and
`fleet.desired` (pure projections via `cs project`, the renamed
`cs reconcile`). **Out-of-band** for external probes — they must be
*emitted* as `*Probed` events before action (see I8).

#### I2 — SingleWriterPerField

**TLA-ready.** Every persisted field has exactly one writer role.

| Field | Writer |
|---|---|
| `Intent` | pilot only (`cs tackle`, `cs freeze`, `cs stop`) |
| `Presence` | probe only (`pane-died` hook, `cs patrol` in pure-observation mode) |
| `Lifecycle` | worker only (`cs evolve`, `cs complete`, `cs stuck`) |
| `BranchMerge` | sibling-shell authority only (`cs done`, `cs harvest`) |

No command writes two fields in two different roles in the same
invocation.

**Child.** *The chef cooks. The apron peg hangs. The boss clears the
ticket from the wall. Nobody sneaks into the boss's job.*

**Ghost blocked.** c1cb (pilot wrote the `BranchMerge` role);
mailroom `/ask` six (pilot wrote the worker `Lifecycle` role).

**Enforceability.** **Partial in-band** (Rust types prevent most
violations via `#[non_exhaustive]` and per-role API entry points).
**Out-of-band for the c1cb class** — a pilot running `git merge` in
a sibling shell is outside the state machine.

#### I3 — FleetMirrorsSession (Gödel G1)

**TLA-ready.** `∀m ∈ Mol : fleet_desired[m] = Registered ⤳ tmux_session[m]`.

The `⤳` (*leads-to*) form is deliberate: I3 is an **eventual safety
property** under weak fairness on `Purge`, not a stepwise safety
invariant. TLC mechanically confirms (see `docs/specs/VALIDATION-REPORT.md`
§Model 4 and §CosmonRun_CrashesI3.cfg) that the moment `TmuxCrash` is
admitted into `Next`, there exists an immediate step where
`fleet_desired = Registered ∧ ¬tmux_session` — the crash happened, the
purge has not yet run. The invariant holds *at the end*, under WF on
`Purge`, not at every step. The reconciliation rate
`r ≥ 7.7 × 10⁻³ Hz` from synthesis §(b) is the operational form of
this latency bound.

**Child.** *If the boss's wall-board says "a chef is cooking", you
had better find an apron peg with a chef on it — or, if the chef just
walked out, a purge sweep that wipes the board within the next beat.*

**Ghost blocked.** cosmon `192a` (fleet registered, tmux absent by
morning) and `dfd8` (runtime kept emitting Evolve against a phantom).

**Enforceability.** **In-band eventual** — `cs project` refuses to
emit a fleet entry whose session fails `has-session`, and a weakly-
fair `Purge` action (patrol / `pane-died`) eventually clears any
registered fleet whose session has died. Stepwise safety is
unattainable once crashes are honestly modelled.

#### I4 — SessionImpliesLiveProcess (Gödel G2)

**TLA-ready.** `∀m ∈ Mol : tmux_session[m] ⤳ worker_pid_alive[m]`.

Like I3, the `⤳` form is deliberate: I4 is an **eventual safety
property** under weak fairness on the future `cs patrol --inspect`
watchdog (and on the `pane-died → cs harvest` pipeline). TLC
mechanically confirms (see `docs/specs/VALIDATION-REPORT.md` §Model 5
and §CosmonRun_CrashesI4.cfg) that once `ProcessCrash` is admitted
into `Next`, a step exists where `tmux_session ∧ ¬worker_pid_alive`
— the worker died but the pane peg still hangs. The stepwise
implication fails by construction; the leads-to form holds under WF
on the harvest action.

**Child.** *An alarm clock buzzing on an empty apron is not a chef.
The buzz is a ghost — and a ghost that the watchdog is weakly-fair-
guaranteed to notice within the next beat.*

**Ghost blocked.** The "session-créée-qui-ment" class — a pane that
logged readiness before the worker could accept input, or whose worker
has since exited without `pane-died` firing.

**Enforceability.** **In-band eventual.** The core cannot detect
process death synchronously; it learns via the tmux `pane-died` hook
(event-driven, cheap) or a probe (polling, wasteful). The invariant
is stateable as leads-to; the *mechanism* — `pane-died` firing
`cs harvest`, plus a weakly-fair patrol sweep — must be wired
out-of-band as **mandatory, not opt-in**.

#### I5 — CompletedEventuallyMerges (Gödel L1)

**TLA-ready.** `∀m ∈ Mol : (mol_status[m] = Completed) ⤳ branch_merged[m]`.

**Child.** *When the lasagne reaches the table, the ticket gets
stamped done and thrown away. Clean. If it doesn't, the kitchen is
broken.*

**Ghost blocked.** `192a` — the morning-after ghost: `status =
Completed`, fleet still registered, nobody called `cs done`.

**Enforceability.** **In-band liveness** — demands weak fairness on
Harvest. At least one of {human `cs done`, `pane-died → cs harvest`
hook, scheduled patrol} must be weakly fair. Without it, a TLC
trace exists where Completed molecules accumulate forever.

#### I6 — NoGhostFleetEntry (Gödel G4)

**TLA-ready.** `∀m ∈ Mol : fleet_desired[m] = Registered ⇒
mol_status[m] ∈ {Running, Frozen}`, **provided `Complete` atomically
clears `fleet_desired` in the same transition that flips `mol_status`
to `Completed`**.

TLC mechanically confirms (see `docs/specs/VALIDATION-REPORT.md`
§Model 3 and §"Per-model results") that I6 holds as a stepwise safety
invariant *only* when this atomicity is enforced. If `Complete`
splits the two writes — first `mol_status = Completed`, then
`fleet_desired = None` — then an intermediate state exists where
`Completed ∧ fleet_desired = Registered`, violating I6. Without the
atomicity, I6 degrades to an eventual safety property of the same
shape as I3 and I4.

**Child.** *A chef's name on the wall-board with no ticket to cook
is not a chef; it's a label forgotten from last week. The boss must
wipe the name and the ticket in one gesture, not two.*

**Ghost blocked.** `192a` (Completed but still registered) and the
analogues of the six mailroom ghosts (the internal executor
registry believed them live while their molecule stayed Pending).

**Enforceability.** **In-band stepwise** — cross-file check at every
`cs` entry point, with the atomicity constraint on `Complete`
expressed directly in the Rust state-store API (one method writes
both fields or neither). Today the check is implicit; the ghost
exists because it is never run.

#### I7 — SingleEventWriter (Gödel G5)

**TLA-ready.** `∀m ∈ Mol : events_writer_lock[m] ∈ {None, Worker}`
plus compare-and-append on `(molecule_id, next_seq_no)`.

**Child.** *Two hands cannot write the same line on the tree at the
same second. The tree uses ink, not pencil.*

**Ghost blocked.** The `events.jsonl` concurrent-write race
identified in the synthesis frame — two writers append and one
overwrites the other.

**Enforceability.** **In-band** — POSIX `flock(2)` + `O_APPEND` +
per-line sequence number. The briefing-seal mechanism in §8b of
`architectural-invariants.md` already *presupposes* a single writer
per invocation; I7 makes the presupposition observable at the
filesystem level.

#### I8 — MeasurementEmission (Einstein L3)

**TLA-ready.** For any external probe `P(m) ∈ {tmux, git, pid, fs}`,
an observer may act on `P(m)` only after appending a corresponding
`*Probed(m, t, result)` event to the ledger.

**Child.** *If maman opens the door and sees you asleep, she has to
tell the clock on the wall first. Otherwise nobody else knows what
she saw.*

**Ghost blocked.** c1cb — the pilot's `git merge` was a write to the
data plane without a corresponding control-plane event. A later `cs
project` has no way to tell "this branch is merged" from "this branch
never existed".

**Enforceability.** **In-band** for cosmon-owned probes (the
`pane-died` hook writes a `WorkerExited` event). **Out-of-band** for
human probes of reality — requires discipline. The remedy is a
git-level oracle: a `pre-merge` hook or CI gate refusing merges
whose commit subject does not derive from a recorded `cs done`.

#### I9 — BranchMergedOnlyIfCompleted (Gödel G6 — *the* Gödel sentence)

**TLA-ready.** `∀m ∈ Mol : branch_merged[m] ⇒ mol_status[m] ∈
{Completed, Collapsed}`.

**Child.** *Somebody else, who isn't the boss, sneaks in and clears
the ticket. A ghost with no name. The kitchen is broken in a way
the kitchen itself cannot see.*

**Ghost blocked.** c1cb (cosmon) and the mailroom `/ask` six.
The pilot stepped into a role no one had named. **All six
mailroom ghosts and c1cb collapse to this single invariant
violation** — a merge or outbox-write performed outside the state
machine.

**Enforceability.** **Mechanically proven out-of-band.** This is
not a design choice — it is a mechanical consequence. TLC produced a
3-state counterexample under `BypassMerge` (see
`docs/specs/VALIDATION-REPORT.md` §Model 6 and
`docs/specs/CosmonRun_I9Counterexample.cfg`): Init → Nucleate(m1) →
BypassMerge(m1) flips `branch_merged` to TRUE in a single step while
`mol_status` is still `Pending`. The counterexample is the first
transition the environment can take; it is not a rare race. The
formal statement is: *I9 is true in every model where
`BypassMerge ∉ Next`, and false in every model where
`BypassMerge ∈ Next`.* Whether `BypassMerge ∈ Next` is a fact about
the environment, not a fact provable from the spec — that last clause
is the formal fingerprint of a Gödel sentence.

Enforcement therefore migrates to out-of-band oracles:

1. A **git pre-merge hook** refusing merges whose subject does not
   derive from a recorded `cs done`.
2. A **CI provenance check** on every merge commit into main.
3. A **pilot refusal register** chronicling the cultural half of the
   discipline.
4. The **`cs done` topology probe** (child task
   `task-20260419-dc10`), which makes any merge-without-completion
   detectable at the next `cs done` or patrol pass.

**This is the single most important finding of the deliberation:**
the refusal of pilot-inline work is a *discipline*, not a mechanism;
cosmon's job is to make the violation **detectable**, not to prevent
it. Detection is the `is_ghost()` function in D2; the mechanical
proof of out-of-band status is the 3-state TLC counterexample cited
above.

#### I10 — SilenceIsSignal (Shannon σ)

**TLA-ready.** Reconciliation fires when

```
σ_tmux(m,t)  > 3 bits
∨ σ_event(m,t) > 4 bits
∨ (fleet_desired(m) = Alive ∧ ¬tmux_session(m))
```

where `σ_X(m,t) = −log P(T_X > t − t_last)` under a stationary
Poisson model fitted to the per-molecule arrival rate `λ_X`.

**Child.** *If maman whispers through the door and nothing answers
for too long, she opens the door. The silence is the signal.*

**Ghost blocked.** The 9 ghosts / 24 h empirical rate
(λ ≈ 1.04 × 10⁻⁴ Hz) gives a reconciliation rate of
**r ≥ 7.7 × 10⁻³ Hz per molecule** for 90 % ghost-catch at 5-min
latency (Shannon §3). Today no periodic X;Y cross-check exists —
channel capacity is fine; the check is simply never run.

**Enforceability.** **In-band** — `cs patrol` becomes an observer
that fires `*Probed` events only when σ thresholds are exceeded,
never on a fixed interval. Polling is retired (Carnot §3:
~120× cost reduction vs event-driven).

### D2. Canonical type — `RunState`

The three "truth sources" collapse into one struct with one writer
per field. Tolnay's signature (`#[non_exhaustive]`, two-field split)
combines with Torvalds's projection function (`is_ghost()` — the
single detection point).

```rust
/// The only authoritative runtime state of a molecule.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunState {
    pub intent: Intent,            // what the operator wrote. Persisted.
    pub witness: Option<Witness>,  // last observation of external reality.
}

#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Intent { Run, Pause, Stop, Terminal(Terminus) }

#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Terminus { Completed, Collapsed, Merged }

#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Witness {
    pub observed_at: DateTime<Utc>,
    pub process: Liveness,         // Alive | Dead | Unknown
    pub branch: BranchState,       // Unmerged | Merged | Absent
}

/// Ghost detection — the single enforcement point.
/// Every one of the 9 ghosts in the 18–19 April log maps to a variant.
impl RunState {
    pub fn ghost(&self) -> Option<GhostKind> { /* pattern-match I3..I9 */ }
}

#[non_exhaustive]
pub enum GhostKind {
    DeadPane,        // I4 — tmux_session ∧ ¬worker_pid_alive (dfd8)
    VanishedWorker,  // I3 — Intent::Run ∧ ¬tmux_session (192a-like)
    UnHarvested,     // I5 — Terminal::Completed ∧ ¬branch_merged (192a)
    StaleProbe,      // I10 — witness.observed_at older than probe_ttl
    UnnamedMerge,    // I9 — branch_merged with no recorded Completed (c1cb)
}

/// Drift becomes a Result::Err at the API boundary.
#[non_exhaustive]
pub enum DriftError {
    IntentWithoutWitness { worker: WorkerId, intent: Intent, last_seen: Option<DateTime<Utc>> },
    TerminalUnmerged    { molecule: MoleculeId, branch: String },
    ConcurrentWitness   { path: PathBuf, writers: u32 },
}
```

**The honest concession.** `witness.observed_at` admits that
`Liveness::Alive` is *never strictly true* — it is "true as of N
seconds ago". A reader seeing `Alive` with `now - observed_at >
probe_ttl` must treat it as `Unknown`. This is Shannon's
silence-as-signal (I10) implemented as a timestamp subtraction. Ugly
field, correct semantics, zero new verbs.

**The semver hazard — act before it compounds.**
`MoleculeStatus::Running` today carries `#[serde(alias = "active")]`.
Every future reader is frozen into accepting both spellings. The fix
— `RunState` as the canonical type, `MoleculeStatus` demoted to a
`#[doc(hidden)]` projection with the alias preserved through the
next major bump — **must land before any new variant is added**, or
the debt compounds.

### D3. CLI delta — 0 add, 4 rename, 11 delete

Torvalds and Tolnay independently derived the same shape: every ghost
is a missing write on an existing verb, not a missing verb. The
invariants belong in the writer-path of existing commands. Adding is
the last resort.

#### 0 add

No new verb.

#### 4 rename / merge

| Today | Becomes | Why |
|---|---|---|
| `cs harvest` | `cs done --if-completed` | The only difference is *"silent no-op when not-Completed"* — that is a flag, not a command. Name makes the perimeter obvious. |
| `cs kill` + `cs purge` | `cs purge` (with `--force` for the SIGKILL path) | Both are infrastructure teardown. One verb. |
| `cs quench` | `cs freeze --reason <str>` | Graceful shutdown with state preservation *is* freeze. |
| `cs reconcile` | `cs project` (alias kept during 1-month deprecation) | `project` reads as *"materialize views from the ledger"*; `reconcile` reads as *"patch something that drifted"*, which is the framing I1 retires. |

#### 11 delete

| Verb | Why it dies |
|---|---|
| `cs touch` | TTL extension masking absence-of-liveness. No invariant. |
| `cs expire` | TTL-as-poll. Liveness derives from the probe (I8). |
| `cs rolling-restart` | A shell loop. Not a primitive. |
| `cs preempt` | `freeze` the incumbent + `tackle` the replacer — composition, not primitive. |
| `cs recover` | `patrol` already scans for stranded molecules; stuckness is a state, not a verb. |
| `cs spawn` | Duplicates `tackle`. |
| `cs dispatch` | Duplicates `nucleate` + `tackle`. |
| `cs deploy` | Overlaps `fleet init`. Templates are the primitive. |
| `cs watch` | A polling daemon in a stateless CLI. §1 of architectural-invariants forbids it. |
| `cs creative` | Prototype stub. Use `deep-think` (already the convention — CLAUDE.md bans `cs creative`). |
| `cs touch-fleet` | TTL-on-fleet. Same shape as `cs touch`; same pathology. |

`cs resume` is **retained** as a convenience alias for
`cs patrol --propel --molecule <id>`, with a tightened
doc-comment perimeter (Torvalds/Tolnay divergence resolved in favor
of muscle memory + ADR-016 alignment).

**Net.** ~40 verbs → ~29. Four renamed, eleven deleted, one
contested verb retained with documentation.

### D4. The five explicit refusals (Jobs)

The unified model **refuses** the following — not as policy, but as
contract. Each refusal is the negation of a pathology already
observed.

1. **The pilot doing inline worker work.** c1cb rebase. Mailroom
   `/ask` inline reply. Same pathology, two galaxies, 48 hours apart.
   Any inline pilot write to a worker-owned artifact is a contract
   breach. *No worker, no answer — only a nucleation waiting for one.*
   *(I2 + I9; enforced out-of-band by git hooks and operator
   discipline, detected in-band by `RunState::ghost()`.)*
2. **A resident daemon inside the `cs` CLI.** `cs` stays git-like.
   One invocation, one decision, one exit. A long-running observer is
   a *separate binary* that is *also a client* of the filesystem —
   never a privileged process hiding mutable state.
   *(§1 of architectural-invariants; preserves Layer A stateless.)*
3. **A pub-sub bus between `cs` commands.** Mailboxes are
   dependencies disguised as convenience. The DAG carries ordering
   (1 bit per edge); the filesystem carries content; nothing else
   may carry signal.
4. **Mutable process memory shared across invocations.** Every `cs`
   starts from disk, writes to disk, exits to disk. Break Markov,
   lose crash-recovery.
5. **Liveness-by-poll as the primary mechanism.** Polling is the
   *fallback*. The primary signal is an event written to disk by the
   one legitimate observer at the moment authority changes hands.
   *(I8 + I10; the ~120× cost advantage of event-driven over
   polling.)*

#### The board demo

A board member watches one terminal. Noogram runs `cs nucleate`,
then `cs tackle`, then — mid-flight — `tmux kill-server`. Without
anyone typing a recovery command, within ten seconds the molecule
returns to `pending` with a stamped witness (`reason:
worker_evaporated`), the registry purged, the artifact trail intact.
The board member asks *"wait, who told it?"* — nobody, because the
filesystem is the only thing that ever knew.

### D5. Out-of-band discipline — the Gödel boundary

Six of the nine ghosts (c1cb + the mailroom `/ask` six) violate
**I9**, which the panel proved is *stateable but not enforceable
from inside cosmon*. The pilot is not a variable of the
specification. Cosmon's only legitimate move is to make the
violation **detectable** at every layer it owns, and to install
**out-of-band oracles** at the layers it does not own.

The mandatory out-of-band gates are:

| Gate | Surface | What it refuses |
|---|---|---|
| Git `pre-merge` hook | every cosmon-tracked galaxy | merge commits whose subject does not match `(evolve\|done\|auto-merge)\(<mol_id>\)` and whose `mol_id` does not have a corresponding `cs done` event in the ledger |
| CI provenance check | GitHub Actions / equivalent | merges into main that lack the `(<mol_id>)` provenance line in the merge commit |
| Mailroom outbox audit gate | `mailroom-bot::ask` | reply JSONs whose `mol_id` is not `Completed` in the cosmon ledger at write time |
| **Pilot refusal register** | an internal register (both galaxies) | the operator-facing record of every time a pilot was tempted to inline-merge or inline-reply, and refused |

The pilot refusal register is the *cultural* half of I9. It does
not prevent the violation — nothing inside the system can — but it
makes the temptation **visible**, so future pilots can see that
*resisting the shortcut is itself a chronicled act*. The first
entries of the register are the c1cb cosmon incident and the
mailroom `/ask` /raccourci-qui-sabote-le-contrat chronicle
(2026-04-19). The register is governed by the syzygie protocol —
both galaxies must keep their copy in sync.

### D6. Cross-galaxy inscription — syzygie

Per the syzygie protocol,
shared vocabulary across cosmon and mailroom must be answered
with `inherit`, `adapt(diff)`, or `refuse(reason)`. **Six of the nine
ghosts are mailroom-side**. The syzygie answer for I9 is therefore
mandated to be `inherit` — mailroom copies I9 verbatim into
its internal chronicle with the cosmon ADR
citation, and files its own `pilot-refusal-register.md`.

The three principles imported in the operator prompt are likewise
inscribed back as `inherit` from mailroom → cosmon:

- **Verdict-door** ← mailroom lore §4 (Executor chronicle), now
  cited in D0 above as the framing for the vision sentence and in
  D3 as the framing for the CLI delta (the door of `cs` is also a
  verdict).
- **Silence-as-signal** ← mailroom ADR-017 §D3 (Gödel sentence
  *G*) and `CLAUDE.md` line *"Liveness needs an external witness"*,
  now I10 (TLA-ready) + the `is_ghost()::StaleProbe` variant
  (operationalized).
- **Evidence-chain** ← mailroom ADR-017 §D6 (stateless-core
  compliance checklist) and lore §11 (the avion-en-papier MVP),
  now I8 MeasurementEmission (the probe must emit before it acts)
  + the git `pre-merge` provenance hook in D5 (every pixel of
  merge-status traces back to a recorded `cs done`).

### D7. The TLA+ skeleton — Gödel's contract

The synthesis includes a minimal TLA+ model (80 non-comment lines)
that names every variable, every action, and every safety / liveness
property of the run plane. It is **not** committed to this ADR — the
committed model lives at
`.cosmon/state/fleets/default/molecules/delib-20260419-d34b/synthesis.md`
§(c). Child molecule **#9** (TLA+ checking gate, see Consequences)
extracts the model into `formal/cosmon-run.tla` + `cosmon-run.cfg`
and wires `tlc` into the test gate.

The skeleton's six safety invariants (I3, I4, I6, I7, I9 + the
no-double-witness lock) and three liveness properties (I5, the
dead-session-eventually-purged property, and the lock-eventually-
released property) are the formal handle by which the rest of this
ADR is verifiable. *Propose mechanisms of verification, do not
impose them* (architectural-invariants §8b) — the TLA+ model is
the proposal; running it on every PR that touches `RunState` is the
discipline.

## Consequences

### Decomposition — the eight implementation children

The synthesis explicitly **defers nucleation** of these children to
this ADR (see `outcomes.md` §"Decomposition plan") so that the ADR
can name them with the precise wording its body requires. After
this ADR merges to main, the following `🔧 task` molecules will be
nucleated `--blocked-by task-20260419-d88a` and tagged `temp:warm`
at creation:

| # | Topic | Invariants | Why first / order |
|---|-------|-----------|-------------------|
| 1 | **`RunState` type + migration in `cosmon-core`.** Replace `MoleculeStatus` + `WorkerStatus` + `fleet.json.desired` with `RunState`. Add `#[doc(hidden)]` projection for `MoleculeStatus` with `serde(alias)` preserved through the next major bump. | I1 + I2 | **Block first.** Canonical type lands before any new ghost variant is added (Tolnay semver hazard). Every later child depends on this. |
| 2 | **`is_ghost()` + `DriftError` + 9-ghost regression test suite.** Pattern-match every `GhostKind` variant against the 18–19 April log. | I3, I4, I5, I6, I9 | The detection surface — the in-band half of the Gödel response. |
| 3 | **`events.jsonl` integrity.** Migrate to `O_APPEND` + per-line monotonic sequence number + `flock(2)` single-writer discipline. | I7 | Retires the events-race ghost. Underpins the seal mechanism in `architectural-invariants.md` §8b. |
| 4 | **Tmux `pane-died` mandatory.** Migrate from opt-in to mandatory: every `cs tackle` installs the hook; the hook's exec target is `cs harvest`; the pane-emitted event carries `worker_exited` to `events.jsonl`. | I4 + I8 + I10 | Event-driven liveness retires polling (Carnot's 120× cost). |
| 5 | **Git `pre-merge` hook + CI provenance gate.** Every merge commit's subject must match `(evolve\|done\|auto-merge)\(<mol_id>\)` and point at a recorded `cs done` transition. | I9 (out-of-band) | What the core cannot prove, git can. |
| 6 | **CLI rename + merge.** `harvest → done --if-completed`, `kill + purge → purge [--force]`, `quench → freeze --reason`, `reconcile → project` (with 1-month deprecation alias). Update `cs help`, `man cs`, generated MCP descriptions, and CLAUDE.md callouts. | surface hygiene | Muscle memory preserved (Torvalds); perimeter clarified (Tolnay). |
| 7 | **CLI delete.** `touch`, `expire`, `rolling-restart`, `preempt`, `recover`, `spawn`, `dispatch`, `deploy`, `watch`, `creative`, `touch-fleet`. Tighten `cs resume` doc-comment. | surface hygiene | Every deletion closes a drift surface. ~40 → ~29 verbs. |
| 8 | **Cross-galaxy inscription.** Copy I9 verbatim into mailroom's internal chronicle per syzygie protocol; file `pilot-refusal-register.md` in both galaxies seeded with c1cb + the `/ask` `/raccourci-qui-sabote-le-contrat` incident. | I9 cross-galactic | Closes the syzygie loop the prompt invoked. |
| 9 | **TLA+ checking gate.** Extract the synthesis §(c) model into `formal/cosmon-run.tla` + `.cfg`. Wire `tlc` into the test gate. Run on every PR that touches `RunState` or `events.jsonl` writers. | I3–I7 + I9 verifiable | The proposal half of "propose mechanisms of verification". |

### Positive

- **Closes the nine-ghost class structurally.** Every shape of
  ghost in the 18–19 April log maps to a `GhostKind` variant
  detected by `RunState::ghost()`. New ghosts of the same shape
  cannot exist in admissible states once child #1 lands; existing
  ghosts become `Result::Err(DriftError::*)` at the API boundary.
- **Verdict-door across the surface.** D0 (vision sentence) is the
  door of `cosmon` the system; D3 (CLI delta) is the door of `cs`
  the binary; both refuse to be menus. Operators (and panels of
  agents acting as operators) read both at first contact and
  internalize them with no second pass.
- **External witness wired in.** I8 + I10 + the mandatory
  `pane-died` hook (child #4) close the "no piece self-certifies
  liveness" loop. Cosmon now carries the same Gödel discipline
  mailroom earned in ADR-017.
- **Evidence-chain across merges.** D5's git `pre-merge` hook +
  CI provenance gate (child #5) make every merge commit traceable
  to a recorded `cs done` transition. The c1cb pathology becomes
  *refused at the gate*, not *detected after the fact*.
- **Cross-galaxy closure.** Six of nine ghosts were mailroom-
  side. The syzygie inscription (D6 + child #8) ensures the
  mailroom reactor inherits I9 verbatim, with provenance back
  to this ADR.
- **Surface shrinks while invariants grow.** ~40 → ~29 CLI verbs;
  3 truth-source enums → 1 `RunState` struct; 0 invariants → 10
  named, formally stated, child-explainable.

### Negative

- **Migration cost.** Child #1 (RunState) is a load-bearing change
  that touches `cosmon-core`, `cosmon-state`, `cosmon-cli`, the
  MCP layer, every formula's worker contract, and the surface
  rendering layer. The `#[serde(alias)]` discipline keeps the file
  format readable across the migration, but the API surface will
  churn. Mitigation: child #1 ships behind a `RunState`-only
  feature flag for one minor cycle, with the legacy projection
  always available; flag flips after the regression suite (#2)
  goes green on every existing fleet's stored state.
- **Out-of-band gates require operator buy-in.** D5's git
  `pre-merge` hook and CI provenance check (#5) cannot be
  installed unilaterally — every cosmon-tracked galaxy must opt
  in. Mitigation: `cs init` in v-next seeds the hook by default
  (per ADR-031 lineage); existing galaxies migrate via a one-
  liner instruction in `docs/handbook.md` and a dedicated
  chronicle entry that walks an operator through it.
- **TLA+ in CI is a heavy gate.** `tlc` on the run-plane model is
  fast (sub-second on the 80-line skeleton), but adding a Java
  toolchain dependency to CI is non-trivial. Mitigation: child
  #9 starts with `tlc` as an opt-in `cargo xtask check-formal`
  job, run on PRs labelled `formal-check`, before considering
  promotion to the default gate.
- **The pilot refusal register is a cultural artefact, not a
  mechanism.** D5's register is honest about what it is — a
  chronicled discipline, not a refusal gate. Operators (and
  agents) can lie to it. Mitigation: there is none in-band; this
  is the I9 / Gödel boundary made explicit. The next strongest
  system (the human operator's own attention, plus syzygie
  cross-galaxy review) is the only enforcement available.

### Neutral

- **No new `cs` verb.** The CLI surface area shrinks net of the
  rename / delete pass.
- **No state-store schema migration on disk.** `RunState` is a
  superset of the existing fields with an additive `Witness`
  payload; the `#[serde(alias)]` discipline keeps the JSON
  readable across rollback windows.
- **No mandatory new dependency.** `tlc` (TLA+ checker) is opt-in
  per child #9. `flock(2)` and `O_APPEND` are POSIX standard.
- **Backwards compatibility on the rename pass.** Old verbs (e.g.
  `cs reconcile`, `cs harvest`) keep a 1-month deprecation alias
  with stderr deprecation notice; CLAUDE.md, MCP descriptions,
  and `man cs` are updated in the same PR per the CLI doc-sync
  feedback rule.

## Non-goals

- **A daemon inside `cs`.** Refused (D4 #2).
- **A pub-sub bus between commands.** Refused (D4 #3).
- **Polling as primary liveness signal.** Refused (D4 #5).
- **Cosmon proving its own consistency.** Forbidden by `P_external`
  (ADR-032). The TLA+ model (D7) is *proposed* as an external
  prover, not an internal one.
- **A user-facing explanation of *G* or the σ-thresholds.** The
  vision sentence (D0) and the Feynman passage (synthesis §d) are
  the operator-facing surface. The math stays in the synthesis and
  in this ADR.

## Mechanical validation

The invariants above were re-checked mechanically by TLC on
2026-04-19, against a 6-model suite derived from the synthesis
skeleton of `delib-20260419-d34b` and committed at
`docs/specs/CosmonRun.tla` + the six `.cfg` companions. The audit
trail of the run — including the captured TLC logs — lives at
`docs/specs/VALIDATION-REPORT.md`.

**Headline.** 9 of 10 invariants hold as stated in the closed
environment; 9 of 9 hold under the expected regime after the three
refinements below are folded into this ADR's prose. The model
checker did **not** refute any invariant; it *sharpened* three
classifications.

**The three refinements, traceable to this file:**

1. **I3 and I4 are eventual-consistency properties, not stepwise
   safety.** The TLA-ready statements above use `⤳` (leads-to)
   rather than `⇒`. Once `TmuxCrash` and `ProcessCrash` are admitted
   into `Next` — which any honest model must do — there exists an
   immediate step where `fleet_desired = Registered ∧ ¬tmux_session`
   (I3) or `tmux_session ∧ ¬worker_pid_alive` (I4). The invariants
   hold under weak fairness on `Purge` (for I3) and on the
   harvest/watchdog action (for I4); they do not hold stepwise. The
   operational latency bound `r ≥ 7.7 × 10⁻³ Hz` from synthesis §(b)
   is the engineering form of this eventual bound.

2. **I6 requires `Complete` to be atomic on two fields.** The
   synthesis skeleton wrote `Complete` without clearing
   `fleet_desired` in the same transition. If the write is split
   into two steps, the intermediate state is
   `mol_status = Completed ∧ fleet_desired = Registered`, which
   violates I6. The ADR prose now requires atomicity explicitly;
   the Rust state-store API (child #1, `RunState` migration)
   expresses the constraint through a single method that writes
   both fields or neither.

3. **I9 is mechanically out-of-band.** The 3-state counterexample
   at `docs/specs/CosmonRun_I9Counterexample.cfg` — Init →
   Nucleate(m1) → BypassMerge(m1) — is the formal twin of the c1cb
   morning gesture. TLC does not "fail" on I9; it correctly reports
   that I9 is contingent on a meta-axiom (the closure of the writer
   set) which the spec cannot discharge. Enforcement migrates to
   the four out-of-band oracles listed under I9 and in §D5.

These three amendments were pushed into this ADR as
`task-20260419-dc54` (the present molecule), after
`task-20260419-af4c` produced the validation report. The ADR now
matches the mechanical verdict — a charter validated by a
9-persona panel still had three grey zones until a machine re-read
it. That is the shape of the Gödel boundary made honest.

## References

- **Governing deliberation.** `delib-20260419-d34b` synthesis at
  `.cosmon/state/fleets/default/molecules/delib-20260419-d34b/synthesis.md`
  — full TLA+ skeleton (§c), Feynman passage (§d), CLI delta (§e),
  refusals (§f), convergences and divergences across 9 personas.
  Outcomes table in
  `.cosmon/state/fleets/default/molecules/delib-20260419-d34b/outcomes.md`
  (decomposition plan, governance trace).
- **Inaugural cosmon ghosts.** Molecule artifacts at
  `.cosmon/state/fleets/default/molecules/task-20260413-dfd8/`,
  `.cosmon/state/fleets/default/molecules/task-20260416-192a/`,
  `.cosmon/state/fleets/default/molecules/task-20260413-c1cb/`.
- **Inaugural cosmon chronicles.** An internal chronicle (entry
  *2026-04-19 — La cuisine, le tablier vide, et la maman qui
  chuchote*); and internal chronicles on spawn-liveness conflation
  and the convoy cascade.
- **Imported mailroom artifacts.**
  `/srv/cosmon/mailroom/docs/adr/017-always-alive-executor.md`
  (D1 resident-runtime client, D3 Gödel sentence *G*, D4 kill-
  switch K1–K4, D6 stateless-core compliance checklist);
  an internal mailroom chronicle
  (§4 verdict-door, §9 external witness, §10 fleet topology, §11
  avion-en-papier);
  an internal mailroom chronicle (entry
  *2026-04-19 — Le raccourci qui sabote le contrat*).
- **Cosmon constitutional axioms invoked.**
  [ADR-016](016-autonomy-regimes-and-resident-runtime.md) (Layer
  A stateless / Layer B resident — D4 #2 cites it directly);
  [ADR-032](032-p-external-witness-axiom.md) (`P_external` — the
  ground of D7 and Non-goal #4);
  [ADR-046](046-p-legibility-axiom.md) (`P_legibility` — the
  ground of D0 and the Feynman child-test of every invariant);
  [ADR-049](049-cosmon-ward-feedback-flow.md) (cosmon-ward
  feedback flow — this ADR is the third binding instance).
- **Syzygie protocol.**
  An internal chronicle
  governs the cross-galaxy inscription in D6 + child #8.
- **Architectural invariants the ADR references.**
  [`docs/architectural-invariants.md`](../architectural-invariants.md)
  §1 (no daemon in `cs` — D4 #2), §7c (Markov boundary — D4 #4),
  §7e (DAG carries 1 bit, filesystem carries content — D4 #3),
  §8b (propose mechanisms of verification, do not impose them —
  D7).

See [ADR-058](058-step-progress-invariant.md) for the 8th clock `StepClock`.
