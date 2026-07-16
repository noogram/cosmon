# ADR-045: Fleet Metabolism — Periodic Self-Observation of Fleet Configuration Fitness

## Status

Accepted (2026-04-16).

Derived from deliberation `delib-20260416-8c50` (10-persona panel: wheeler,
einstein, shannon, feynman, godel, hawking, turing, tolnay, jobs, niel). See
`synthesis.md` in the deliberation molecule for the full convergence trail.

## Context

The pilot currently has no systematic way to evaluate whether `fleet.toml`
configuration is fit for the workloads the fleet actually executes. Four
self-observation channels already exist:

| Channel | Observes | Time horizon |
|---------|----------|-------------|
| `cs patrol` | Worker liveness, stale molecules | Real-time |
| `temp-review` formula | Backlog hygiene, tag staleness | Periodic (~weekly) |
| `claudion` probes | Token spend, session energy | Per-session |
| `cs reconcile` | Surface drift from state | On-demand |

Each channel terminates in a separate action. None projects its observations
onto fleet configuration fitness. The question the pilot cannot answer today:
"Given the last N cycles of execution, is my `fleet.toml` well-tuned?"

IFBDD (If It's Big, Don't Do It) is a real-time discipline exercised by the
pilot during work. It cannot be automated — the act of saying it IS the
intervention. But fleet configuration review is a *different* loop: periodic,
retrospective, statistical. The two are complementary, not substitutable.

### The composition insight (Wheeler)

The metabolism is not a fifth observation channel. It is a **funnel** that
composes outputs from the four existing channels into a projection onto
`fleet.toml` suggestions. The new thing is the projection function, not the
observation. This matters: adding a new observer increases system complexity;
adding a composition over existing observers increases leverage without
complexity.

### The decidability hierarchy (Turing)

Not all fleet configuration parameters are equally amenable to evidence-based
tuning:

| Parameter class | Decidability | Examples |
|----------------|-------------|----------|
| **Parametric** | Quasi-decidable from metrics | `parallel_limit`, token budgets, step timeouts |
| **Topology** | Undecidable — requires counterfactual reasoning | Add/remove agents, change roles, restructure fleet |

"Optimal fleet.toml" is a halting-problem reduction. The metabolism must
restrict its scope to the decidable end of the spectrum and explicitly flag
any heuristic suggestions that cross into undecidable territory.

## Decision

**Implement fleet self-observation as a formula (`fleet-review`), with the
concept name "metabolism" in thesis and documentation.**

### 1. Implementation: formula only

Zero new types in `cosmon-core`. Zero new CLI commands. The formula lives at
`.cosmon/formulas/fleet-review.formula.toml`, is executed by `cs evolve`,
reads existing artifacts (`events.jsonl`, `state.json`, claudion probes), and
writes a markdown report. This is the composability principle in action: if
the formula fails, delete the `.toml` file — zero semver impact, zero
deprecation ceremony.

### 2. Naming

| Layer | Name | Rationale |
|-------|------|-----------|
| Formula file | `fleet-review` | Operational, descriptive, `cs nucleate fleet-review` reads naturally |
| Concept (thesis, docs) | metabolism | Physics vocabulary — continuous transformation of the system's own substrate. Rejected alternatives: CMB (passive fossil ≠ active observer), Homeostasis (too narrow), Conscience (moral vocabulary), Pulse (generic) |

Wheeler's decisive argument: the physical CMB is a passive residue; the
proposal is an active observer. "Metabolism" captures the mechanism precisely.

### 3. Scope restriction by decidability

The formula restricts to **parametric observations only** at v0-v1:

- **Collapse rate per formula** — highest mutual information, cheaply
  observable from `state.json`.
- **Duration/step ratio** — medium-high MI, reveals formula step bottlenecks.
- **Energy utilization** — medium MI, available from claudion probes.

Dropped at v0: synthesis entropy (expensive, noisy), commit frequency
(confounded by formula structure). Topology suggestions (add/remove agents)
are deferred to v2+ and must be explicitly flagged as heuristic when
introduced.

### 4. P_external gate — mandatory and load-bearing

No auto-application of suggestions to `fleet.toml`. Ever. The gate serves
three structural purposes:

1. **Breaks Godel circularity** — the metabolism cannot prove consistency of
   its own observation apparatus from within. A suggestion that removes a
   claudion probe or reduces logging literally blinds the metabolism, and the
   metabolism cannot detect this from its own metrics.
2. **Prevents oscillation** — without external damping, the feedback loop can
   enter limit cycles (suggest A → apply → metrics shift → suggest ¬A).
3. **Keeps the mechanism in Propelled regime** — autonomous fleet modification
   requires a separate ADR with a damping formula (Hawking's chronology
   protection conjecture, applied to configuration space).

P_external degenerates over time (rubber-stamping, legibility decay,
reflexive capture). Mitigations: falsifiable predictions attached to each
suggestion (Godel), bounded step size per cycle (Shannon), hysteresis
cooldown between contradictory suggestions (Godel).

### 5. The 7 red lines

The metabolism must NOT:

1. **Auto-apply suggestions** — P_external gate is structural, not optional.
2. **Resolve contradictions** — present tradeoffs, let the pilot hold the
   utility function. Resolving contradictions internally = building a planner
   = crossing into the Autonomous regime.
3. **Suggest topology changes at v0-v1** — parametric observations only;
   topology is undecidable without an LLM oracle.
4. **Emit more than 3-5 observations per cycle** — the pilot's channel
   capacity is ~10-15 bits/week (Shannon). Rate-limit to the receiver, not
   the observation rate.
5. **Produce output below minimum observation threshold** — empty galaxy =
   silence, not defaults. The formula must distinguish: active suggestion /
   explicit quiescence / insufficient data.
6. **Optimize for pilot approval rate** — Godel's reflexive capture trap.
   Success metric is fleet fitness improvement, not suggestion acceptance.
7. **Introduce new types or CLI commands at v0** — formula only, deletable
   if it fails. Types and commands earn their place by demonstrated value.

### 6. Evolution path with explicit gates

```
v0 ──[gate A]──> v1 ──[gate B]──> v2 ──[gate C]──> v3

Gate A: 5+ scans confirm metrics are meaningful (Feynman's evidence test)
Gate B: 10+ accepted suggestions demonstrate signal (not noise)
Gate C: Separate ADR with damping formula (Hawking's chronology protection)
```

| Version | Scope | Trigger | Output | Gate to next |
|---------|-------|---------|--------|-------------|
| **v0** | Pure scan — observations only, no suggestions | Manual (`cs nucleate fleet-review`) | `fleet-review.md` with metrics snapshot | 5+ scans where pilot finds signal |
| **v1** | Parametric suggestions with falsifiable predictions | Manual | `fleet-review.md` with suggestions + predictions | 10+ accepted suggestions |
| **v2** | Patrol-triggered invocation, EMA damping | `cs patrol --propel` can invoke | Same + trend analysis | Explicit ADR for autonomous regime |
| **v3** | Autonomous parametric adjustment within pre-set bounds | Runtime policy | Bounded adjustments + audit log | Never without ADR + damping formula |

The v0 formula IS the measurement instrument. Its first output is not
"suggestions" but "observations." The pilot reads the scan, notices patterns
(or doesn't), and the very act of reading is the evidence-gathering that
justifies v1.

### 7. Composition principle

The metabolism is a funnel, not a sensor:

```
patrol ──────┐
temp-review ─┤
claudion ────┤──> fleet-review (projection function) ──> fleet-review.md
reconcile ───┘
```

It reads artifacts produced by the four existing channels and projects them
onto fleet.toml fitness observations. No new data collection, no new
instrumentation, no new daemon. The value is in the **cross-molecule
statistical aggregates** that are invisible to per-molecule tools (Shannon's
unique contribution argument).

### 8. Relationship to smart-limits (ADR-044)

**Defer unification.** The two mechanisms share the same observation substrate
(`events.jsonl`, claudion probes) but target different layers:

| Mechanism | Layer | Scope | Time scale |
|-----------|-------|-------|------------|
| smart-limits (ADR-044) | Execution | Per-step concurrency caps, token throttles | Real-time |
| fleet-review (this ADR) | Configuration | Fleet-wide parameter fitness | Periodic (days/weeks) |

Unification makes sense when both mechanisms exist and their suggestions
overlap. Premature now. When the time comes, the shared observation substrate
is the natural join point.

### 9. Complementary: `cs peek` vital signs

Jobs' counter-proposal (rejected as replacement, adopted as complement):
a status line in `cs peek` showing real-time fleet vital signs — pending >48h
count, temp distribution, collapsed/completed ratio. Different time horizon
(real-time passive monitoring vs. periodic active analysis), same underlying
function. Not part of this ADR's scope but should be a sibling molecule.

## Consequences

- **Fleet configuration becomes observation-driven** rather than
  convention-driven. This is IT FROM BIT applied to fleet tuning:
  configuration is a consequence of observation, not of convention (Einstein).
- **The formula is deletable with zero semver impact.** If the metabolism
  concept fails in practice (Feynman's evidence test), remove the `.toml`
  file and the markdown report. No types to deprecate, no commands to remove,
  no API to version.
- **Extends Principle 0's self-reference.** Cosmon already observes its own
  molecules (patrol), its own backlog (temp-review), its own energy
  (claudion), and its own surfaces (reconcile). The metabolism extends
  self-reference to fleet configuration fitness — the system observes whether
  its own operational parameters are well-tuned.
- **The Godel sentence is load-bearing.** "This configuration change will not
  degrade the metabolism's own observational capacity" is unfalsifiable from
  within. The P_external gate exists precisely because this sentence cannot be
  resolved by the mechanism itself. Any future relaxation of the gate (v3)
  must address this with an explicit damping formula.
- **Future autonomous regime (v3) requires its own ADR.** This ADR does not
  authorize autonomous fleet modification. The gate between v2 and v3 is an
  architectural boundary (Propelled → Autonomous regime transition per
  ADR-016), not a parameter to tune.

## References

- `delib-20260416-8c50` — parent deliberation (10-persona panel synthesis)
- [ADR-016](016-autonomy-regimes-and-resident-runtime.md) — autonomy regimes
  and the resident runtime (Inert / Propelled / Autonomous)
- [ADR-044](044-smart-resource-limits-roadmap.md) — smart resource limits
  roadmap (related, separate layer)
- [ADR-032](032-p-external-witness-axiom.md) — P_external witness axiom
