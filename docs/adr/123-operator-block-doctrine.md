# ADR-123 — Operator-Block Doctrine: Irreversibility-Class Blocking, Typed Capability, and the "Waiting-on-Operator" Encoding

**Status:** proposed
**Date:** 2026-06-08
**Decider:** Noogram
**Authoring task:** `task-20260608-9f2c`
**Parent deliberation:** `delib-20260608-6a5f` (panel: architect, torvalds, turing, kahneman)
**Source incident:** 2026-06-07/08 — a worker on a signable-instrument rewrite
(`cc4a`) raised a Claude Code `AskUserQuestion` modal inside its tmux session,
blocked **invisibly**, and the polymer (DAG drainage) stalled all night. The
resident runtime (ADR-095) was independently dead since 23:50.

**Binds:**
- [ADR-052](052-one-ledger-one-writer-one-witness.md) — `RunState { intent, witness }`,
  the canonical run-state type that demoted `MoleculeStatus` to a legacy projection
  (this ADR's Q4 turns on that demotion).
- [ADR-016](016-autonomy-regimes-and-resident-runtime.md) — the Inert/Propelled/Autonomous
  regimes (this doctrine governs worker behaviour inside `Propelled`).
- [ADR-062](062-quotaclock-9th-clock.md) — the `Starved` cause taxonomy
  (the precedent against minting a fourth "alive-but-not-progressing" lifecycle state).
- [ADR-053](053-cosmon-daemon-supervisor.md) / [ADR-095](095-resident-runtime-ifbdd-path.md)
  — the supervisor + runtime layer that carries the external backstop (sibling
  children C1/C2 of this deliberation, not this ADR).

**Architectural invariants:** `docs/architectural-invariants.md` §8b (verification
mechanisms are proposed, not imposed), the command-perimeter table (`cs done` is the
human-only irreversibility gate), and the Control-plane/Data-plane separation (no
mailboxes; the block is a typed event on the ledger, not a message).

**Downstream:** **blocks `task-20260608-c210` (C4)** — the harness enforcement of this
doctrine cannot be implemented until the encoding (Q4) and the capability shape (Q5)
are fixed here. C4 is wired `--blocked-by` this molecule.

---

## Context

A worker faced two briefing imperatives that read as a contradiction:

- generic: **"DO NOT wait for user input between steps"**;
- task-discipline: **"don't edit the signable act without an operator decision."**

It resolved the conflict by guessing, honoured the discipline, and blocked — through
Claude Code's in-session `AskUserQuestion` modal, a surface **external to cosmon's
state machine**. The molecule sat in `Running`, byte-indistinguishable from a healthy
worker. The only liveness oracle cosmon owns — `tmux has-session` — returns one bit
(`true`) whether the cognition inside is thinking, waiting, or wedged dead. Three
behavioural states collapsed to one observable. Nothing fired; the pipeline froze.

The deliberation (`delib-20260608-6a5f`) answered five questions. This ADR records the
**durable policy** (Q1, Q2, Q5) and **adjudicates the one open design divergence** (Q4 —
how a worker encodes "waiting on operator"). The sibling fixes are out of scope here:
the always-on patrol wiring + event-age detector (C1, `task` TBD), the runtime
seal-self-poisoning fix (C2, `task` TBD), and the harness enforcement (C4,
`task-20260608-c210`, blocked by this ADR).

### The two-axis decision matrix (turing)

The legitimate-block class is not "irreversible" loosely — it is the **intersection of
two axes**: *undecidable by the worker* (the answer needs information or authority not
on its tape) **AND** *irreversible*. A worker that pauses to query a human is a Turing
**oracle machine**: querying a black box it cannot compute is the *correct* response to
a locally-undecidable question, not a fault.

| | Decidable by worker (safe default on tape) | Undecidable by worker (needs operator oracle) |
|---|---|---|
| **Reversible** (cheap undo) | **JUST ACT** — normal log only | **SURFACE-AND-CONTINUE** — pick a default, act, flag loudly; one revert is the worst case |
| **Irreversible** (no cheap undo) | **AUTO-DEFAULT + ALERT** — *forbidden in practice; see Q2* | **ALERT + observable BLOCK** ← the incident lives here |

### What "irreversible" means (architect)

The irreversibility line is **not** "the artifact is valuable." Almost everything a
worker does is reversible *inside* cosmon — git worktrees, per-step commits, and
`cs done` as the sole merge gate exist precisely so that drafting, rewriting, and
proposing options on an **unmerged worktree** are reversible by `git` + `cs`. The line
is crossed only when an action produces **an effect cosmon cannot revert with `git` +
`cs`**: a signature transmitted, `git push` to a shared remote, an email/publish sent,
or an authoritative value written that downstream consumers act on before a human
reviews it.

In the incident the worker mistook *"this artifact is important"* for *"this action is
irreversible."* Editing the unmerged draft was reversible; only *transmit/sign* would
have crossed the line. **`cs done` is already the human-only irreversibility gate** —
the worker must trust it and not invent a second blocking gate inside `Running`.

---

## Decision

### Q1 — Blocking on an operator is NOT always a fault

It is **legitimate for exactly the undecidable-AND-irreversible class, and a fault
everywhere else.**

> **The line, in one sentence:** a worker may pause for an operator only at the boundary
> where its next action would produce an effect cosmon cannot revert with `git` + `cs`
> (signature transmitted, push to a shared remote, email/publish, an authoritative value
> downstream consumers act on); everything upstream of that boundary — drafting,
> rewriting, proposing options on an unmerged worktree — is reversible and **must
> surface-and-continue, never block.**

In the incident the worker **correctly identified** an undecidable+irreversible decision
but **blocked invisibly**. The bug is the invisibility, not the block (turing's
load-bearing principle: *blocking on a human oracle is legitimate; blocking invisibly is
the bug*).

### Q2 — Mechanism: surface-and-continue by default; ALERT + observable-BLOCK for the irreversible cell; AUTO-DEFAULT forbidden for that cell

The regime is **a combination keyed to the matrix**, never one global mechanism:

| Cell | Mechanism | Worker emits |
|---|---|---|
| Reversible + decidable | Just act | Normal step log |
| Reversible + undecidable | **Surface-and-continue** (global default) | A `needs-review` artifact in the molecule dir + acts; molecule stays `Running` |
| Irreversible + decidable | Act on the safe default + ALERT | An `auto-defaulted` record (what/why) + `cs notify` |
| **Irreversible + undecidable** | **ALERT + observable BLOCK** | The typed block signal (Q4) **before** yielding |

**AUTO-DEFAULT — auto-applying a value when a timer expires — is FORBIDDEN for the
irreversible class** (CV-4). turing's worst-case ranking is the binding argument: the
worst case of AUTO-DEFAULT on an irreversible act (a wrong value gets signed/pushed) is
the **only unrecoverable outcome** of the four mechanisms — strictly worse than the
silent block, which was *fully recoverable*. torvalds: *"a clock must never manufacture
consent a human refused to give."* kahneman: auto-default breeds automation complacency
and trains the operator to stop trusting that blocks are real.

**DV-2 reconciliation (by cell).** architect's "the polymer must never hang a full
night" requirement is honoured by the **reversible-cell surface-and-continue default**,
not by auto-applying anything irreversible. For the irreversible cell, the timeout may
only **escalate the ALERT louder over time** (and may *park / requeue* the molecule), but
it **never silently applies a value**. So "no night-long hang" and "no auto-default for
irreversible" both hold: the polymer keeps draining on the reversible default, and the
one genuinely irreversible decision waits — loudly, observably, bounded by escalation,
never by silent consent.

### Q5 — Typed capability granted at nucleation, not prose precedence

The contradiction is resolved by a **typed guard on the molecule**, not by a precedence
rule between two strings of prose. A precedence rule alone (kahneman, turing) merely
re-confirms the worker's silent choice and changes nothing observable.

```rust
/// May this molecule pause for an operator decision, and at which boundary?
///
/// Absent ⇒ the worker MUST surface-and-continue (the safe default). There is
/// no contradiction to arbitrate: the generic "DO NOT wait" wins by construction
/// because the capability simply is not present, so the worker reads exactly one
/// instruction.
///
/// Present ⇒ the worker MAY block, but ONLY at `boundary`, and ONLY after emitting
/// the typed block signal (Q4). Blocking without emitting is a protocol violation.
pub struct OperatorBlockCapability {
    /// The irreversibility boundary that authorises a pause (Q1 line).
    boundary: IrreversibleBoundary, // Signature | ExternalSend | Publish | AuthoritativeValue
    // NOTE: no `timeout_to_default` field that auto-APPLIES a value — forbidden by Q2/CV-4.
    // A bound, if any, escalates the ALERT or parks the molecule; it never applies a value.
}
```

Granted at `cs nucleate` (flag or formula TOML), visible in `state.json`. The
task-discipline stops being a paragraph competing with another paragraph and becomes a
single routing rule over the matrix. **Mandatory surfacing (kahneman):** a
capability-bearing worker that yields **MUST** emit the typed signal **before** blocking
— *blocking without emitting is a protocol violation, not a judgment call.*

**`AskUserQuestion`-in-tmux is forbidden as a blocking primitive.** It is invisible to
cosmon by construction (it lives in Claude Code, external to the state machine), so it
can never satisfy the emitted-not-inferred requirement. The sanctioned path is: write
the proposal/options to the molecule dir, emit the typed block event, then yield.

**Alerts are tiered by irreversibility (CV-6, kahneman).** Operational stalls
(heartbeat-stale, config-drift) self-heal silently and escalate only after self-heal
fails; only the irreversible-class block interrupts the operator. Loudness is
proportional to irreversibility, not to the system's anxiety — otherwise the one
load-bearing alert dies in the flood (alert fatigue = availability bias weaponised).

### Q4 — Encoding decision: **(b) event + tag on a still-`Running` molecule**

The worker emits the "waiting on operator" signal as:

- **`EventV2::WorkerBlockedOnOperator { molecule_id, boundary, since }`** — a new
  append-only, `#[non_exhaustive]`-friendly event variant in `cosmon-core/src/event_v2.rs`,
  peer to the existing `WorkerSilenceDetected`. This is the worker-emitted half (CV-2 (i)).
- **tag `temp:awaiting-op`** — the derived surface marker, projected by the patrol and
  read by `cs peek`, `cs ensemble --tag`, and `STATUS.md`. The molecule **stays
  `Running`.**

"Waiting on a human" is a **transient annotation on a Running molecule** — identical in
kind to `temp:frozen` — not a lifecycle position. The event ledger and the tag set are
both first-class typed surfaces the patrol already reads, so this is **not** the
"state outside the type" sin (an `EventV2` on `events.jsonl` is the authoritative typed
ledger, not a stray sentinel file).

**Non-negotiable backstop (independent of the encoding):** an **external event-age /
heartbeat patrol** (sibling child C1) must catch the *un-emitting* case — the Claude Code
`AskUserQuestion` modal, or a future free-text guard nobody typed — because no
self-reported state can ever see a block that happened *outside* cosmon. CV-2 (ii). The
key: age of the last `events.jsonl` append while intent = running, threshold ≈ 15 min,
action = **ALERT only** (never kill, never auto-decide). The doctrine deliberately does
**not** rely solely on workers remembering to declare capability (kahneman's pre-mortem).

---

## Options Considered

The policy questions (Q1, Q2, Q5) were unanimous on the panel (CV-1, CV-3, CV-4); the
live divergence was **Q4 — the encoding** (DV-1), with three named positions. Per
ADR-082 `INV-ADR-OPTIONS-CONSIDERED`, the Q4 alternatives are enumerated below, plus the
one rejected Q2 mechanism (AUTO-DEFAULT).

### Q4 — Option (a): `MoleculeStatus::WaitingOnOperator` variant (turing)

Add a seventh lifecycle variant to `MoleculeStatus`, an honest cause peer to
`Starved`/`Frozen`; the enum is already `#[non_exhaustive]`.

- **Pros:** "Blocked on oracle" is a genuine, distinct cause of non-progress; the
  taxonomy already distinguishes *who* stopped the worker (`Frozen` = self-suspend,
  `Starved` = authority refused compute). A variant the patrol reads directly cannot be
  bypassed by state-outside-the-type. Lowest conceptual surprise for a reader scanning
  statuses.
- **Cons (rejected on these):** **`MoleculeStatus` is `#[doc(hidden)]` and explicitly
  demoted to a legacy projection (verified: `crates/cosmon-core/src/molecule.rs:94`,
  per ADR-052).** Adding a variant to a type the codebase is actively migrating *away
  from* moves against the grain. It incurs the full typestate cost (a `Waiting`
  zero-size state + `MoleculeState` impl + typed transition methods, or an asymmetry
  that breaks the compile-time-safety thesis), new `can_transition_to` arms, reconcile +
  surface render mappings, and breaks every **in-crate** exhaustive `match`
  (`#[non_exhaustive]` shields *other* crates, not `cosmon-core`'s own `emoji()`,
  `Display`, `FromStr`, `is_terminal`, …). That is a substrate-tier change to encode one
  transient bit, and it adds a *fourth* "alive-but-not-progressing" state next to
  `Frozen`, `Starved`, and Running-but-idle — the exact muddle ADR-062 fought.

### Q4 — Option (c): `RunState` witness shape, projected to `Frozen` (torvalds)

Add the blocked state to the canonical `RunState { intent, witness }` (the type ADR-052
introduced to *replace* `MoleculeStatus`), and project it to `Frozen` for legacy
consumers via `molecule_status_from_run_state`.

- **Pros:** `RunState` is the type *designed* for observability — it splits pilot intent
  from probe witness and is the migration target. Putting the new state in the type we
  are migrating *toward* is the clean-data-structure move. **Verified to exist:**
  `crates/cosmon-core/src/run_state.rs:230` (`RunState`), `:550`
  (`molecule_status_from_run_state`). torvalds' core claim is real.
- **Cons (rejected on these):** the **migration is incomplete** — `RunState` is today a
  *display/projection* layer, **not yet the persisted storage truth** (verified:
  `run_state.rs:560-564`, "*without persisting a `RunState` on disk yet — the migration
  of `MoleculeStatus` → `RunState` on the storage side is the [future work]*"). So
  there is no reliable place to *persist* the blocked state there yet. Worse, "waiting on
  operator" fits **neither** field: `Intent` is *pilot-written* (Run/Pause/Stop/Terminal)
  and `Witness` is *probe-observed* `Liveness {Alive, Dead, Unknown}` — but the block is
  a **worker self-assertion**, a third writer role the two-field shape does not model.
  Carrying it would require extending `RunState`'s shape *and* the projection. And the
  proposed projection-to-`Frozen` re-introduces the `Frozen`-vs-real-cause muddle
  (operator-suspend vs awaiting-operator are different things). **Verified but premature.**

### Q4 — Option (b): event + tag, molecule stays `Running` — **CHOSEN** (architect)

Emit `EventV2::WorkerBlockedOnOperator` + tag `temp:awaiting-op`; molecule stays `Running`.

- **Pros:** append-only and `#[non_exhaustive]`-friendly (`event_v2.rs`); **zero** new
  typestate, **zero** in-crate `match` breakage, **zero** reconcile schema change, **zero**
  transition-table edits. It does not add weight to either a *dying* type (option a) or a
  *not-yet-canonical* one (option c). It models "waiting on a human" as what it actually
  is — a transient annotation peer to `temp:frozen`, carried on the authoritative event
  ledger + tag set (both first-class typed surfaces the patrol already reads, so the
  "state outside the type" objection does not apply). Satisfies CV-2 (i) (worker-emitted)
  and composes directly with the C1 external backstop for CV-2 (ii).
- **Cons (accepted):** a reader scanning `MoleculeStatus` alone will not *see* the block
  in the status field — they must read the tag or the event. Mitigated because `cs peek`,
  `cs ensemble --tag`, and `STATUS.md` all project the tag; and because the load-bearing
  detector is the external event-age patrol (C1), not the status field. If the
  `RunState` storage migration later completes, the blocked annotation can be lifted into
  a witness/intent shape **without** invalidating the event + tag — they remain the
  worker-emitted source the projection reads. So (b) is also the lowest-regret step
  toward (c)'s end-state.

### Q2 — Rejected mechanism: AUTO-DEFAULT (timeout → apply a value) for the irreversible class

- **Pros:** bounds the wait so the polymer never hangs a full night (architect's
  original motivation).
- **Cons (rejected on these):** its worst case — a wrong value auto-applied to an
  irreversible act that then gets signed/pushed — is the **only unrecoverable outcome**
  of the four mechanisms (turing's rank #1). It manufactures consent a human refused to
  give (torvalds) and trains automation complacency (kahneman). The "never hang a night"
  goal is met instead by the **reversible-cell surface-and-continue default** (DV-2
  reconciliation), so AUTO-DEFAULT buys nothing it does not also endanger.

## Decision Outcome

**Q4 = Option (b)** — `EventV2::WorkerBlockedOnOperator` + tag `temp:awaiting-op`,
molecule stays `Running`. **Option (a)** (`MoleculeStatus::WaitingOnOperator`) is
**rejected** because it adds a lifecycle variant to a `#[doc(hidden)]` legacy type being
migrated away (ADR-052) at substrate-tier cost for one transient bit. **Option (c)**
(`RunState` witness, project to `Frozen`) is **rejected as premature**: `RunState` exists
but is not yet the storage truth, "waiting on operator" fits neither its pilot-`Intent`
nor its probe-`Witness` field (it is a worker self-assertion — a third writer role), and
projecting to `Frozen` re-introduces a cause-muddle. Option (b) is the smallest mechanism
that satisfies emitted-not-inferred today and is the lowest-regret step toward (c)'s
eventual end-state.

On the policy axes: **Q1** = blocking is legitimate only for the
undecidable-AND-irreversible class (the git+cs revert boundary), a fault everywhere else.
**Q2** = surface-and-continue by default, ALERT + observable-BLOCK for the irreversible
cell, **AUTO-DEFAULT rejected by name** for that class. **Q5** = typed
`OperatorBlockCapability` granted at nucleation with mandatory pre-block surfacing;
prose precedence rejected; `AskUserQuestion`-in-tmux forbidden as a blocking primitive.

## Consequences

- **(risk — load-bearing)** The typed capability + worker-emitted event is the *belt*;
  it depends on a worker honestly emitting before it blocks. kahneman's pre-mortem: in
  three months a *new* free-text guard nobody bothered to type recreates the incident
  (planning-fallacy on typing each guard). **Therefore the external event-age patrol (C1)
  is the load-bearing suspenders, and this ADR explicitly does NOT rely on workers
  remembering to declare capability.** If C1 does not ship, the doctrine is only half
  installed and the incident can recur via the un-emitting path.
- **(risk)** Encoding "waiting on operator" as a tag rather than a status means any
  consumer that reasons over `MoleculeStatus` alone (legacy code, an external dashboard)
  will see a plain `Running` molecule and may mis-read it as healthy. Mitigation: the
  surfaces operators actually use (`cs peek`, `cs ensemble --tag`, `STATUS.md`) project
  the tag, and the C1 patrol fires the alert regardless of who reads the status.
- **(risk — alert fatigue)** Every fix here is a *new notification*. If operational
  signals (heartbeat-stale, config-drift) share the channel with irreversible-class
  blocks, the one load-bearing alert drowns. Mitigation is the CV-6 tiering: only the
  irreversible class interrupts; operational classes self-heal and escalate only after
  failure.
- **(positive)** The briefing contradiction is dissolved at the source: a worker reads
  **one** typed capability, not two competing prose strings. Absent capability ⇒
  surface-and-continue by construction.
- **(positive)** Zero changes to the typestate machine, the status enum, the
  transition table, or reconcile/surface schemas — the encoding is purely additive
  (one event variant + one tag string), so this ADR is shippable without a substrate-tier
  re-audit of dependent galaxies.
- **(follow-on)** C4 (`task-20260608-c210`) implements the harness enforcement: the
  capability check in the worker yield-path, mandatory emit-before-block, and the
  forbidding of `AskUserQuestion` as a blocking primitive. C1 implements the external
  backstop. C2 fixes the runtime seal self-poisoning. This ADR fixes only the *doctrine*
  and the *encoding* those depend on.
