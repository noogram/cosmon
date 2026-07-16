# ADR-111 — Mission = Molecule + DecayProduct + Blocks (convention on existing primitives)

**Status:** Accepted (2026-05-24).
**Date:** 2026-05-24.
**Decider:** Noogram.
**Empirical motive:** sibling deliberation
`delib-20260523-a682`
— *« faut-il un framework MISSION en cosmon, un mode runtime parallèle au mode
linéaire ? »* — panel five personas (wheeler / von-neumann / godel / jobs /
torvalds). The synthesis converged unanimously: **the pattern is already
implemented; it lacks a name, not a runtime.**
**Authoring task:** smithy `task-20260523-c465` (cosmon-ward branch
`feat/task-20260524-c465-adr-mission-convention`).
**Authoring discipline:** torvalds subtraction + wheeler invariant-first.
Doc-only — no code lands in this commit.

**Binds (sibling ADRs landing on the same trunk):**
- smithy ADR-0019
  — Option D' synthesis (« data structure first, invariants named, framework
  deferred »). This ADR is the cosmon-side §5 Phase-2 #2 inscription.
- companion cosmon ADR *« Single-writer-trunk et invariants de coordination »*
  (Phase 2 #1) — inscribes I1–I5 (WRITER-UNIQUE, ISOLATION, ADDITIVE-COUNTERS,
  PROGRESS, OBSERVATION-NEUTRE).

**Refers to:**
- [ADR-016](016-autonomy-regimes-and-resident-runtime.md) — resident runtime,
  `--resident` flag, mission-like predecessor that was retired by
  ADR-054. The pattern named below
  is the *survived doctrine* of ADR-016 once the runtime mode was removed.
- [ADR-022](022-native-dag-scheduler.md) — `DagPolicy`, the scheduler that
  consumes `Blocks` edges and splices `DecayProduct` children mid-run.
- ADR-004 — Molecule as the
  unit of work.

---

## Context

The 2026-05-23 deliberation `delib-20260523-a682` asked whether cosmon should
adopt a **MISSION framework**: a new runtime mode, typed crew roles, a TLA+
spec on coordination, and a *polymerisation* primitive that would chain
sub-missions automatically. The proposal came in response to five empirical
breaks observed during the v1.4 drain (cross-worker contamination of the
cosmon main checkout, naïve 30-minute conflict merges, non-additive
`surface_freeze` counters, spawn timeouts under load, artisanal human
stitching of multi-endpoint drains).

The panel rejected the framing on a different ground than expected.

**The observation that surfaced in synthesis §C1–C7 is retroactive:** the
operator had been *using* the pattern for months — speaking of « la mission
v1.5 = drain endpoints 4+5+6+7, bake, smoke, deliver Pierre » — without
having the word. When the panel went looking for what a MISSION runtime
would need to add to cosmon, every primitive was already there:

| MISSION concept | Existing cosmon primitive | Reference |
|---|---|---|
| *Root work-unit with sub-tasks* | `Molecule` with `MoleculeLink::Blocks` / `BlockedBy` edges | `crates/cosmon-core/src/interaction.rs:236-258` |
| *Sub-task emerges from progress* | `MoleculeLink::DecayProduct` / `DecayedFrom` (splice mid-run) | `crates/cosmon-core/src/interaction.rs:211-230` |
| *Execution order across the tree* | `DagPolicy::compile_plan` → `next_actions` (critical-path-weighted) | `crates/cosmon-runtime/src/dag_policy.rs` |
| *Single coordinator while crew works* | `with_fleet_lock` over `cs run --resident <root>` | `crates/cosmon-filestore/src/lib.rs:147`, `crates/cosmon-cli/src/cmd/run.rs:520` |
| *Crew-in-mission with roles* | Worker fleet + per-molecule worktrees | `crates/cosmon-cli/src/cmd/tackle.rs:418` |

There is **no missing primitive.** A MISSION framework would mean either
(a) reskinning these into a parallel mode (rejected by jobs D1 — doubles the
surface for a minority of cases) or (b) adding a TLA+-validated coordination
engine on top (rejected 5/5 panel for cosmon's current maturity — 1-2 engs,
weekly deliverables; possible Phase 3 conditional under smithy ADR-0019
§5).

What the panel asked for instead is exactly what this ADR delivers:
**a name for the convention, so the pattern stops being invisible to its
own users.**

---

## Decision

### D1 — Convention naming

A **MISSION** in cosmon is, by convention:

1. **A root molecule** of any kind (typically `task` or `galaxy`) that names
   a deliverable-shaped goal (« v1.5 endpoints 4+5+6+7 », « ship guest-slice
   MVP », « onboard tenant Pierre »).

2. **Child molecules linked by `MoleculeLink::Blocks` / `BlockedBy`** — the
   DAG that the root depends on. These children may exist at nucleation time
   (operator pre-planned) **or** appear during execution as
   `MoleculeLink::DecayProduct` of a previously-completed molecule (mid-run
   discovery — the dynamic DAG hook of ADR-016 §5 / ADR-022).

3. **`DecayProduct` as the polymerisation contract.** When a worker
   completes a molecule and the work surfaces a follow-up that belongs to
   the *same* mission, the follow-up is nucleated as a `DecayProduct` of the
   completed molecule (not as a free-floating molecule). `DagPolicy` splices
   it into the running plan via `cosmon_graph::insert_subgraph`, and the
   mission auto-extends without operator intervention.

No new types. No new state. No new runtime mode. The convention is a way of
**reading** what already exists.

### D2 — Optional CLI ergonomic alias (deferred, opt-in)

A future alias

```
cs mission <name> tackle <root-id>
```

would be the ergonomic equivalent of

```
cs run <root-id> --resident --poll-interval 5
```

with the difference being **lexical only**: it documents that the operator
considers `<root-id>` a mission rather than a leaf task. The alias is
**not gating** — every cosmon feature must work whether the operator calls
`cs run --resident` or `cs mission … tackle`. The alias may land in a future
release of cosmon-cli; it is explicitly **not part of this ADR's contract**
and may be dropped if usage stays at zero.

Rationale: per jobs (synthesis §D1), maintaining two parallel runtimes
doubles the surface for a minority of cases. An alias is one extra line in
`cosmon-cli`'s command table — that line is cheap and rollbackable. A mode
is not.

### D3 — Recognition of the OTP supervision-tree family

The pattern described in D1 is a known one in the distributed-systems
literature. The closest external analogue is the **OTP supervision tree**
(Armstrong, *Making Reliable Distributed Systems in the Presence of
Software Errors*, KTH thesis 2003):

| OTP supervision tree | cosmon mission convention |
|---|---|
| Supervisor process | Root molecule + `DagPolicy` running under `--resident` |
| Child specification | `MoleculeLink::Blocks` (pre-planned) or `DecayProduct` (mid-run) |
| Restart strategy | Currently: explicit retry via `cs evolve` on failed molecule. Future Phase 3 of smithy ADR-0019 §5 may type the strategy. |
| Crash recovery | File-on-disk state (ADR-047) + `with_fleet_lock` — no in-memory supervisor state to lose. |

The point of citing OTP is **not** to import its full vocabulary
(*one_for_one*, *rest_for_one*, *strategy*), but to inscribe the lineage:
this is not a novel pattern; cosmon discovered it from below, then noticed
the family it belongs to. Future doctrine refinements should consult OTP
literature rather than re-invent, per the wheeler subtraction discipline
(« nommer ce qui existe ailleurs avant de l'appeler nôtre »).

### D4 — What this ADR explicitly does NOT introduce

To make the subtraction visible (and prevent doctrinal scope-creep):

- **No new molecule kind.** A mission is a *use of* `Molecule`, not a new
  kind alongside `task`, `idea`, `delib`, etc.
- **No new link type.** `MoleculeLink::Blocks`/`BlockedBy`/`DecayProduct`
  cover the structure; no `MissionMember`, no `CrewRole`, no `Phase`.
- **No new CLI command (beyond the optional alias D2).** Operators tackle
  missions today with `cs run <root> --resident` — that path keeps working.
- **No TLA+ spec.** The coordination invariants (I1–I5 of smithy ADR-0019
  §3) belong to the *trunk-and-stitch* ADR (companion), not to this one.
  TLA+ on `cs stitch` is Phase 3 conditional under smithy ADR-0019 §5.
- **No vocabulary `polymerisation`.** Rejected by smithy ADR-0019 §4.
  Replacement: *auto-chaining* via `DecayProduct`, the mechanism named
  above.

### D5 — Doctrinal status of pre-existing "mission" usage

Operator notes, deliberation frames, and chronicles that already used the
word *mission* informally (notably the v1.4/v1.5 drain narratives and
`DIAGNOSIS-mission-collapse.md`) are **retroactively legitimised** by this
ADR. They were not metaphorical — they were observing the convention before
it was named.

---

## Consequences

### What changes immediately (zero code)

- The chronicle entry « v1.5 = mission ; les sous-tâches sont ses
  *children-by-Blocks* » becomes a precise statement, not a metaphor.
- New chronicles can use *mission*, *crew-in-mission*, *auto-chaining* as
  defined terms without each entry re-defining them.
- Future deliberations facing a similar « do we need a framework for X? »
  question have a precedent: **read the existing primitives twice before
  proposing a new mode.**

### What stays at the human frontier

Per godel D2 of the parent delib (synthesis §D2), the mission pattern does
*not* absorb the operator — it relocates them to the frontier:

- Naming a mission (« v1.6 = drain endpoints 8+9 + bake ») is an operator
  act; cosmon does not auto-recognise a deliverable boundary.
- Approving `DecayProduct` splices that surface unexpected work *outside*
  the mission's named scope (e.g. *« le worker a découvert qu'il faut
  d'abord rewrire l'auth »*) is an operator decision — the system can splice
  the child structurally, but the operator chooses whether the new edge
  belongs to *this* mission or earns a fresh root.
- Calling `cs land` / `cs done` on the root (the « mission est livrée »
  moment) remains operator-gated.

### Risk accepted

If, six months from now, missions become genuinely large-scale (50+
workers, multi-galaxy, typed inter-worker contracts), the convention may
no longer be enough — typed crew roles (von-Neumann étage 2 of smithy
ADR-0019 §5) and a TLA+ spec on the coordination engine (Phase 3) become
warranted. **That risk is accepted as the explicit cost of subtraction
today.** Phase 3 absorbs it if/when it materialises, without paying its
cost now.

### Rollback path

This ADR is doc-only. Rollback = `git revert` of the introducing commit.
The cited primitives (`MoleculeLink`, `DagPolicy`, `with_fleet_lock`) are
untouched and remain available regardless of whether the *mission* word is
in vogue.

---

## References

### Internal (paths relative to `/srv/cosmon/cosmon/`)

- `crates/cosmon-core/src/interaction.rs:211-258` —
  `MoleculeLink::{DecayedFrom, DecayProduct, Blocks, BlockedBy}`.
- `crates/cosmon-runtime/src/dag_policy.rs` —
  `DagPolicy`, `compile_plan`, `next_actions`,
  `cosmon_graph::insert_subgraph` (mid-run splice).
- `crates/cosmon-cli/src/cmd/run.rs:520` — `cs run --resident`.
- `crates/cosmon-cli/src/cmd/tackle.rs:418` — per-molecule worktree.
- `crates/cosmon-filestore/src/lib.rs:147` — `with_fleet_lock`.
- `docs/adr/004-bead-formula-molecule-distinction.md` — Molecule unit.
- `docs/adr/016-autonomy-regimes-and-resident-runtime.md` — resident
  runtime, retired by ADR-054 but the doctrine survives.
- `docs/adr/022-native-dag-scheduler.md` — native DAG scheduler.
- `docs/adr/047-event-log-protocol-v0.md` — file-on-disk source of truth.
- `docs/adr/054-retire-adr-016-resident-runtime.md` — what was retired.
- `DIAGNOSIS-mission-collapse.md` — pre-existing diagnostic that
  observed the pattern before it was named.

### Smithy cross-galaxy (paths relative to `/srv/cosmon/smithy/`)

- `docs/adr/0019-orchestration-single-writer-trunk-mission-deferred.md` —
  Option D' synthesis. This ADR implements its §5 Phase-2 #2.
- `.cosmon/state/fleets/default/molecules/delib-20260523-a682/synthesis.md`
  — five-persona panel synthesis (lives in the **smithy** fleet state,
  not cosmon's; the delib was nucleated smithy-side because the framing
  question was a doctrinal smithy decision).

### External

- `[@armstrong2003erlang]` — Armstrong, *Making Reliable Distributed
  Systems in the Presence of Software Errors*, KTH thesis 2003. OTP
  supervision-tree family.
- `[@erdweg2015buildsystems]` — Erdweg et al., *A Sound and Optimal
  Incremental Build System with Dynamic Dependencies*, OOPSLA 2015. The
  formal model behind dynamic-DAG splice (the `DecayProduct` mechanism is
  a build-system *dynamic dependency*).
