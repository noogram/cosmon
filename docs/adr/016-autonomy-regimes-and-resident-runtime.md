# ADR-016: Autonomy Regimes and the Resident Runtime

## Status

**Phases 3+ un-retired by [ADR-095](095-resident-runtime-ifbdd-path.md)**
(2026-05-17). ADR-095 reverses ADR-054 §1 (Phase 3+ retirement) and §4
(`cs run` "dies") under the IFBDD lens — the empirical-retirement basis
of ADR-054 is invalidated because the absence of a forensic instrument
in the eleven-month skeleton is not evidence of the absence of demand.
The Resident Runtime is ratified as a **constrained client of the
transactional core** under five named structural invariants (ADR-095
§2 — RR-1 through RR-5) and the bedrock test
`docs/architectural-invariants.md` §14 (*karpathy's tatouage — "you
can `cat` cosmon's state"*). The two-layer architecture diagram in §1
is **restored** with the Resident Runtime layer redrawn as a
constrained client per ADR-095 §2.

**ADR-054 inheritance preserved.** ADR-054 §2 (future planner work
needs a fresh ADR — this ADR-095 *is* that fresh ADR) and §3 (three-
regime vocabulary survives; Autonomous redefined as tenant-owned, not
solely cosmon-lab) are inherited verbatim. The Resident Runtime
is *one* tenant in the Autonomous regime; claude-code via MCP is
another; a future planner crate would be a third. Wheeler's framing
(*liveness as observation-delegation*) is unchanged.

**Earlier history:** Superseded in part by
ADR-054 (2026-04-20) — the
empirical-feature-pull retirement — partially reversed by ADR-095 as
above.

Original status: Proposed (2026-04-09).

## Context

Cosmon today is a git-like stateless CLI. Every command reads the state store,
mutates it, and exits. Workers run in tmux sessions launched by `cs tackle`,
and supervision happens out-of-band via `cs patrol` (now with `--propel`, see
recent commit `cefd7db`).

This model is powerful — it composes with anything (shells, cron, launchd,
humans) and has no daemon to own. But it leaves a genuine gap: **nothing in
cosmon reacts to events it did not cause itself**. When a worker finishes a
molecule, no process can observe that completion and decide "launch the
next one in the DAG". The human (or an external shell loop) must poll and
dispatch.

Noogram asked whether we should build a complementary "live" runtime — a
process with its own event loop, analogous to `claude-code`. He proposed a
four-level autonomy ladder:

1. **L1** — `cs tackle`: one molecule at a time, human-driven.
2. **L2** — `cs convoy`: batch execution of related molecules.
3. **L3** — `cs run <dag>`: execute a DAG of molecules, dynamically
   incorporating children spawned by decays, terminate when drained.
4. **L4** — `cs plan "do X"`: goal-driven planning + execution, claude-code-like.

A panel review (feynman, jobs, wheeler) converged on a simpler model:

- **feynman**: "push/pull" is cargo-culted from HTTP. The real axis is
  *does a process own a control loop that outlives a single decision?*
  That collapses the ladder to **two modes**: transactional (no loop) and
  resident (loop). The planner (L4) is not a new mode — it is a different
  *policy* plugged into the same resident loop.
- **jobs**: Kill L2. It is a shell `for` loop masquerading as a feature. Do
  not build L4 — claude-code already solves that problem. Cosmon's unique
  shape is L3. Ship L1 + L3.
- **wheeler**: The deeper invariant is **temporal locus** — where the clock
  that advances the system lives. Three physics-consistent regimes fall out:
  **Inert** (clock external to cosmon), **Propelled** (trajectory fixed at
  nucleation, progresses on fuel), **Autonomous** (internal clock + deliberation
  function). L3 and L4 are both *autonomous*; they differ only in the
  deliberation function.

The three voices converged on the same structure:
- **One runtime, two policies** (feynman)
- **Cosmon is substrate, planners are tenants** (jobs)
- **Delegation of the right to observe** (wheeler)

## Decision

### 1. Two layers, not four levels

Cosmon is organized as two layers sharing one state store.

```
┌──────────────────────────────────────────────────────┐
│  POLICY LAYER (pluggable deliberation functions)     │
│                                                      │
│  ┌──────────┐  ┌──────────────┐  ┌─────────────┐     │
│  │ DagPolicy│  │ DynamicDag   │  │ (external)  │     │
│  │  static  │  │  decay-aware │  │ LLM planner │     │
│  └──────────┘  └──────────────┘  └─────────────┘     │
├──────────────────────────────────────────────────────┤
│  RESIDENT RUNTIME (single event loop)                │
│                                                      │
│  • observe events (worker done, decay fired, ...)    │
│  • consult policy → next action                      │
│  • emit transactional commands                       │
│  • shared state store with the CLI                   │
├──────────────────────────────────────────────────────┤
│  TRANSACTIONAL CORE (cs CLI — current)               │
│                                                      │
│  • stateless, git-like invocations                   │
│  • cs tackle, cs evolve, cs nucleate, cs patrol, ... │
│  • state in .cosmon/state/ JSON files                │
│  • used by humans AND by the resident runtime above  │
└──────────────────────────────────────────────────────┘
```

**Invariants:**

- The transactional core is the source of truth. Every mutation goes through
  it. The resident runtime is a *client* of the core, not a replacement.
- A human can `cs observe` or `cs freeze` a molecule while the resident
  runtime is working on it. They share the state store.
- The resident runtime is optional. Cosmon is fully usable without it —
  the test suite, the CLI, and the MCP server all run against the core alone.
- LLM-based planners (the "L4" idea) are **not built into cosmon**. They are
  external tenants that either call `cs` commands or speak to cosmon via MCP.
  claude-code is the reference L4.

### 2. Three autonomy regimes (physics vocabulary)

The state of a molecule-and-its-observer relationship sits in one of three
regimes. These replace ambiguous "alive/semi-alive/dead" terminology.

| Regime | Clock locus | Trajectory | Human intervention |
|--------|-------------|------------|--------------------|
| **Inert** | External (human invokes `cs`) | N/A — no motion | Every transition |
| **Propelled** | External or runtime (fuel → decay) | Fixed at nucleation (formula steps) | Start, stalls, completion |
| **Autonomous** | Internal (resident runtime) | Computed at runtime (DAG or goal) | Policy definition, escalations |

- **Inert** molecules have state but no clock. They evolve only when something
  external invokes a cosmon command against them. A newly-created `pending`
  molecule with no tackle yet is inert.
- **Propelled** molecules carry momentum: a tackle or a resume has given
  them forward motion along a predetermined trajectory (the formula steps).
  They progress until the fuel is exhausted (steps complete) or the motion
  dies (stall). `cs patrol --propel` is literally the watchdog that
  re-applies force to stalled propelled molecules.
- **Autonomous** molecules live inside the resident runtime. Their next step
  is not predetermined — it is computed by a policy (a DAG scheduler, a
  dynamic DAG with decay-aware re-planning, or an external LLM planner).
  From the molecule's point of view the difference is the source of the
  next observation; from the runtime's point of view the difference is the
  deliberation function.

"Liveness" is not a property of the system but a **statement about the
delegation of the right to observe** (wheeler). `cs tackle` delegates to
the human. `cs patrol --propel` delegates to cron. `cs run <dag>` delegates
to the resident runtime's DAG policy. An external claude-code session
delegates to itself via MCP.

### 3. What we build, what we refuse

| Command | Status | Rationale |
|---------|--------|-----------|
| `cs tackle` | **Ship** (done) | Inert → Propelled for a single molecule. L1. |
| `cs patrol --propel` | **Ship** (done, `cefd7db`) | Transport safety net for Propelled molecules. |
| `cs convoy` | **Refused** | A shell `for` loop with extra config. Skip the stepping stone. |
| `cs run <dag>` | **Build next** | L3, the resident runtime. The only cosmon-shaped hole in the world. |
| `cs plan "do X"` | **Refused** | claude-code already solves goal-driven planning. Expose via MCP instead. |

### 4. Scheduler reuse: `ox-sched` extraction

> **⚠️ Successor decision (2026-04-09): superseded by [ADR-022](022-native-dag-scheduler.md).**
> The `ox-sched` extraction described below is **no longer the planned
> path**. A five-persona deliberation
> (`delib-20260409-c422`)
> voted 4–1 in favor of growing `cosmon-graph` natively instead. The
> resident runtime's `DagPolicy` will use a native `cosmon-graph::Plan`
> reducer — not a wrapper over OxyMake's scheduler. Key mechanical
> reasons: OxyMake's scheduler is tokio-driven and assumes a frozen
> `JobGraph` (incompatible with §5 decay-aware re-planning and §6
> multi-context composition). See [ADR-022](022-native-dag-scheduler.md)
> for the full rationale, the three flip-conditions that would reopen
> the question, and the new implementation sequence.
>
> Phase 1 (`MoleculeLink::Blocks`) of `idea-20260408-589f` remains
> unchanged and is still the prerequisite for any DAG scheduling.

~~The resident runtime's DAG policy is not a new scheduler — it is a thin~~
~~adapter over OxyMake's proven scheduler. See an internal note~~
~~for the detailed feasibility study. This ADR adopts that study's recommendation:~~

1. ~~Extract a small `ox-sched` crate from OxyMake containing `SchedulableJob`~~
   ~~trait + toposort + ready-frontier + critical-path pass.~~
2. ~~Cosmon and OxyMake both implement `SchedulableJob` for their own node types.~~
3. ~~The resident runtime's `DagPolicy` wraps `ox-sched` and emits `cs evolve` /~~
   ~~`cs nucleate` commands based on the plan it computes.~~

The `MoleculeLink::Blocks` variant (Phase 1 of the oxymake idea) remains a
prerequisite: without first-class blocking edges, there is no DAG to schedule.

### 5. Dynamic DAG: decay-aware re-planning

A key property of cosmon molecules that OxyMake does not model: a running
molecule can spawn children via **decay**. The resident runtime's DAG policy
must handle this:

- On worker completion, check whether the molecule emitted any `decayed_to`
  links.
- If yes, load the new children, compile them into the existing plan,
  recompute ready-frontier, and continue.
- Terminate only when the full transitive closure is drained.

This is the *dynamic DAG* extension. A static `DagPolicy` handles convoys
with no decay. A `DynamicDagPolicy` handles decay-aware re-planning.
Both policies plug into the same resident runtime loop.

### 6. Multi-graph / multi-dag strategy

Cosmon can have multiple concurrent DAGs in flight (e.g., research convoy A
+ infra convoy B). The resident runtime treats each DAG as a separate
*execution context* with its own policy instance. Contexts share the state
store and can interact via cross-DAG blocking links, but they are scheduled
independently. The runtime is a single process supervising N policies.

This avoids a second daemon per DAG while still isolating scheduling
decisions per context.

## Consequences

**Positive:**

- The transactional core stays git-like. It remains the lingua franca.
  Every agent, every script, every human uses the same vocabulary.
- The resident runtime is built from a single loop + pluggable policies.
  Adding a new policy is a module, not a new subsystem.
- Scheduler logic is not rewritten. OxyMake's mature scheduler is reused
  via the `ox-sched` extraction, keeping both projects in lockstep.
- The 3-regime vocabulary (Inert / Propelled / Autonomous) aligns with
  the physics metaphor and clarifies where humans need to intervene at
  each regime.
- `cs patrol --propel` is retroactively recognized as the transport-layer
  mechanism for re-propelling stalled Propelled molecules. Its name is
  now load-bearing, not decorative.

**Negative:**

- Refusing `cs convoy` and `cs plan` requires saying "no" to features
  users may ask for. Substitutes (shell loops, claude-code via MCP) exist
  but need documentation so users know the escape hatch.
- The `ox-sched` extraction is a cross-repo change that temporarily couples
  cosmon and oxymake development timelines. Phase 1 (`MoleculeLink::Blocks`)
  is independently valuable and can be shipped first to de-risk.
- "Autonomous" is now an overloaded term in the codebase — it refers to
  the regime above, not to individual worker autonomy in the psychological
  sense. The thesis's "Propulsion Principle" language applies to Propelled
  molecules, not Autonomous ones.

**Neutral:**

- The "resident runtime" does not yet exist. This ADR is a north star,
  not a shipping milestone. `cs tackle` + `cs patrol --propel` delivers
  the Inert→Propelled half today; `cs run <dag>` with DagPolicy is the
  next major build.

## Implementation Sequence

> **Phase 3+ update (2026-05-17, [ADR-095](095-resident-runtime-ifbdd-path.md)).**
> Phases 3, 4, and 5 below are **un-retired** under ADR-095 with five
> named structural invariants (ADR-095 §2 — RR-1 through RR-5) and an
> IFBDD construction order (forensic hooks ship *before* the
> behaviour they instrument). The phases are gated on the karpathy
> bedrock test (`docs/architectural-invariants.md` §14 — *"you can
> `cat` cosmon's state"*); any phase PR that breaks the test is a
> structural breach. ADR-095 §4 names the 90-day forensic
> falsification window — if the build is falsified, RR-3 (deletable
> as one PR by construction) is the excision path, with a fresh
> ADR ratifying the retirement on forensic evidence rather than on
> the absence of evidence ADR-054 cited.

1. **Phase 0 (done)**: `cs tackle` propulsion prompt + `cs patrol --propel`
   + fleet registration in tackle (`cefd7db`).
2. **Phase 1**: `MoleculeLink::Blocks` variant, `cs deps <id>` CLI,
   MCP exposure of blocking links.
3. **Phase 2**: Extract `ox-sched` from OxyMake. Publish as separate crate.
   *(Superseded by [ADR-022](022-native-dag-scheduler.md) — native
   `cosmon-graph::Plan` reducer instead of `ox-sched` wrapping.)*
4. **Phase 3 — un-retired ([ADR-095](095-resident-runtime-ifbdd-path.md)):**
   `cosmon-runtime` crate as a *constrained client* of the transactional
   core (RR-1), owning no state (RR-2), deletable as a single Cargo
   target (RR-3), with `.cosmon/state/` JSON remaining authoritative
   (RR-4) and the four silent-failure-mode forensic hooks baked in
   from day one (RR-5). `cs run <dag>` consumes the loop. *Construction
   order: forensic hooks first, behaviour second.*
5. **Phase 4 — un-retired ([ADR-095](095-resident-runtime-ifbdd-path.md)):**
   `DynamicDagPolicy` — decay-aware re-planning. All five invariants
   apply; the policy is a strategy plugged into the constrained loop,
   never a new state owner.
6. **Phase 5 — un-retired ([ADR-095](095-resident-runtime-ifbdd-path.md)):**
   Multi-context supervisor — N policies sharing one runtime. The
   bedrock test (`docs/architectural-invariants.md` §14) remains
   load-bearing: every supervisor decision must be re-derivable from
   `cat`-able state.
7. **Not scheduled**: `cs plan "do X"`. External tenants may implement this
   via MCP; cosmon provides no first-party planner. (Inheritance from
   ADR-054 §3 — *Autonomous is tenant-owned, not solely cosmon-lab*.)

## Amendment (2026-05-31, delib-20260531-c761): Q2b — L3 is bounded-ephemeral, never reloads in place

**Context.** A long-lived Resident Runtime is the textbook case for a
config-reload reflex (SIGHUP → re-read config). The panel (architect,
torvalds, turing, godel, carnot — unanimous) **rejected** that reflex and
ratified its opposite. The motivating incident: a resident runtime launched
before a `just install` redeploy kept dispatching workers using its
*launch-time RAM snapshot* of config + binary — silently billing the wrong
oracle, ignoring an edited `[adapters.default]`, and making stale dispatch
decisions. All three symptoms are the **same root defect**: a long-lived
process trusting its launch-time snapshot over the authoritative on-disk
state.

**Decision (amends Phase 3+).** The Resident Runtime (L3) **re-derives
binary + config + env at each spawn boundary; it never reloads in place.**

1. **Witness obligation.** At launch the runtime seals
   `H = BLAKE3(resolved_config ⊕ binary-image-id)` — the same seal-as-trace
   BLAKE3 primitive as `prompt_seal` / `briefing_seals`
   (`docs/architectural-invariants.md` §8b), not a new mechanism.
2. **Pre-dispatch re-check.** Before *every* dispatch the runtime recomputes
   `H'` from current on-disk config + binary. `H' == H` → it has *witnessed
   its own freshness* for this act → dispatch. `H' != H` → it can no longer
   witness freshness → **halt fail-closed**: refuse the dispatch, emit
   `EventV2::ConfigDriftDetected`, exit non-zero so a supervisor relaunches a
   fresh process.
3. **Never self-repair mid-flight.** The runtime MUST NOT reload/merge config
   in place. The only sound move on drift is to *stop* and let a fresh launch
   re-derive from disk.

**Why bounded-ephemeral, not reload-on-change.**

- **architect (systems).** A self-reloading runtime becomes a stateful
  config-cache with every failure mode statelessness was invented to abolish
  — it can drift (reload half-applies), race its own reload, and lie about
  which config it is on. It cannot hot-swap the running binary image anyway
  (no SIGHUP reloads `argv[0]` into a new ELF). Bounded-ephemeral = a fast
  loop of stateless spawns; config-freshness comes *for free* from dying and
  being reborn, the same gesture as every L1 `cs` invocation.
- **godel (formal).** A process that reloads itself must reason about its own
  state mid-flight and races its own reload (TOCTOU: dispatch begins under
  config-vN, reload commits vN+1 between engine-resolution and worker-mint).
  *"A running process cannot prove 'I am currently fresh' while still
  running"* (operational shadow of the Second Theorem). Reload **promotes** a
  *visible* stale-snapshot bug into an *invisible* race. The fix makes
  `config-fresh` a witness obligation the runtime carries and checks, and
  *halts* when it can no longer witness it.
- **carnot (irreversibility).** Billing is an irreversibility boundary: once
  the HTTP request to the oracle is accepted, the exergy is destroyed.
  Config-honoring dispatch refuses to *form* the request (changes the
  boundary condition itself) — primary by irreversibility ordering. Egress
  fail-closed (`delib-20260530-0877`) is the recoverable backstop, ranked
  *below* this fix: *"the seatbelt, not the brakes."*

**Consistency framing (godel, Q2a — why this is mandatory).** The runtime
*performs dispatch* (same engine-resolution path, same key): it is "an L1
actor wearing an L3 lifetime", so cosmon's `config-fresh` axiom binds it and
was previously *false* of it. Cosmon is a `substrate`-tier galaxy under the
ADR-082 Gödel substrate-galaxy obligation — it must obey the rules it imposes
on dependents. L1/L3 stratification would have been legitimate *only* if L3
were a pure scheduler reading the 1-bit DAG signal and shelling a fresh
`cs tackle` per dispatch (which re-derives config in the child). The defect
was that L3 internalised an L1 act and froze it; this amendment dissolves it.

**Implementation.** `crates/cosmon-runtime/src/resident.rs` —
`config_binary_seal`, the `launch_seal` field on `RuntimeLoop`, the
pre-dispatch re-check, and `ExitReason::ConfigDrift`;
`crates/cosmon-cli/src/cmd/run.rs::run_resident` maps `ConfigDrift` to
`exit(75)` (EX_TEMPFAIL); `EventV2::ConfigDriftDetected` in
`crates/cosmon-core/src/event_v2.rs`. The retroactive acceptance test
(`crates/cosmon-runtime/tests/resident_config_drift_halt.rs`) reproduces the
May-25-launch / May-31-reinstall scenario and asserts the halt.

**Optional, not built (future).** A *deploy-generation token* on disk that
each spawn records and `cs peek` / `cs observe` surface, so drift is
one-glance observable. Deferred to keep this change minimal; the
`runtime-trace.jsonl` `launch` + `config-drift-halt` lines already make the
seal observable.

## References

- an internal ADR/idea — full feasibility study for `ox-sched`
  extraction (supersedes this ADR's Phase 2 details).
- ADR-007: `cosmon-graph` extraction — precedent for factoring graph ops
  into dedicated crates.
- ADR-014: Gas Town bridge — external tenants as a pattern; the L4 policy
  follows the same model.
- `THESIS.md` Part V (Vocabulary) — physics naming. This ADR adds three
  regime terms to the lexicon.
- `crates/cosmon-cli/src/cmd/patrol.rs` — `find_stale_running_molecules`
  and `propel_stale_molecules`, the existing Propelled-regime watchdog.
- Panel deliberation: feynman (first principles), jobs (subtraction),
  wheeler (vocabulary). Conducted 2026-04-09 in session
  `c2912623-5ab0-4182-95f8-08512babc9af`.
