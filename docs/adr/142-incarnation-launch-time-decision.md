# ADR-142 — The `Incarnation`: adapter · model · effort are one launch-time decision

**Status:** Proposed (filed for operator decision — `task-20260705-590a`, C5 of
`delib-20260704-b476`).
**Date:** 2026-07-05.
**Decider:** Noogram.
**Parent deliberation:** `delib-20260704-b476`
— *"MODEL-ROUTING intra-flotte — give `cs` per-molecule model routing,
isolated, without mutating the shared session default."* This ADR ratifies
**wheeler's Q4 verdict** (name the abstraction: the three launch dials are one
decision) and **von-neumann's Q2 verdict** (`(adapter, model)` is a dependent
bundle, validated opaquely).
**Kind:** ADR-grade because it reserves a federation-wide *noun* for the
worker-spawn boundary and fixes the composition rule between two per-Adapter
axes. Per CLAUDE.md — *"Do not backdoor architectural changes through
individual PRs"* — the doctrine is filed as a decision. **This ADR ships no
code**; it documents an abstraction whose narrow first slot (`model`) already
landed in C1 and whose remaining slots and validator are reserved, not built.

**Binds / cites / complies with:**

- [ADR-079](079-worker-spawn-port-and-adapter-contract.md) — the Worker-Spawn
  Port and the per-Adapter typed axes in `cosmon_core::spawn_seam`
  ([`ValidatedAdapterName`](../../crates/cosmon-core/src/spawn_seam.rs)). The
  `Incarnation` is the *bundle* that crosses this port; it does not add a new
  port.
- [ADR-106](106-adapter-naming-canonicalisation.md) — adapter naming
  canonicalisation. The `adapter` slot of an `Incarnation` carries a canonical
  adapter name; aliases are resolved at the CLI seam, never persisted.
- [ADR-118](118-llmport-doctrine-and-degradation-matrix.md) — the LLMPort
  doctrine (which abstraction is the LLM boundary, where adapters live). The
  `Incarnation` sits *above* an adapter: it selects which adapter, and within
  it which model — it does not itself talk to a provider.
- [ADR-119](119-adapter-exit-code-contract.md) — the structured adapter
  exit-code contract (the fifth per-Adapter obligation). Composition validation
  (below) is the same shape of per-Adapter typed refusal, one axis up.
- [ADR-108](108-cosmon-incarne-v0-baseline.md) — the *incarné* lineage. The
  noun chosen here (`Incarnation`) continues the physics verb already in the
  vocabulary: nucleate → tackle → **incarne**. A bodiless molecule takes on a
  body; that body's parameters are the `Incarnation`.
- **C1 — `task-20260705-c255`**
  (merged `5ec2c77`): the model-pin resolution chain, the `cs tackle --model`
  flag, the formula-step `model =` pin, and
  [`ModelSelectionSource`](../../crates/cosmon-core/src/event_v2.rs). This ADR
  names the abstraction C1 is the first slot of.

**Architectural invariants:** `docs/architectural-invariants.md` §8j (every
Port is a typed ingress binding) — the composition validator this ADR reserves
is a per-Adapter typed refusal at the spawn seam.

---

## Context

To put one molecule on a strong model (Fable 5) instead of an economical one
(Opus / Sonnet), the operator used to type `/model` **inside the worker's
Claude Code pane**. That mutates the *shared* session default: every subsequent
molecule silently inherits the strong model, credits burn, and the surface
shows the molecule but not the stickiness (a WYSIATI attribution trap —
kahneman). There was no per-molecule model isolation, even though the
`--adapter` axis already had exactly that shape (`cs tackle --adapter`, a
formula-step `adapter =` pin, and a config resolution chain).

C1 (`task-20260705-c255`) closed the mechanical gap for the model axis: a
`cs tackle --model <id>` flag, a formula-step `model = "<id>"` pin, and
`resolve_model_selection` — a verbatim shape-clone of `resolve_adapter_selection`
with the precedence `--model > formula-pin > $COSMON_DEFAULT_MODEL >
per-galaxy config > global config > floor None`. The pin rides the *already
built* per-session `ANTHROPIC_MODEL` closure-shadow (dependency injection at
spawn, never `std::env::set_var`), so two sibling workers get independent
command strings with zero cross-contamination.

C1 shipped the **mechanism**. This ADR fixes the **noun** and the
**composition rule** — the two things that keep the model axis from being read
as an unrelated bolt-on, and that make the still-missing effort axis a
zero-migration future rather than a second bolt-on.

## The observation that forces a name

There is not one launch-time dial. There are three, and they are turned at the
**same instant, at the same boundary, by the same shape of resolver**:

| Dial | Question it answers | Status |
|------|---------------------|--------|
| **adapter** | *which backend runs this molecule?* (claude / openai / anthropic / aider / local) | shipped (ADR-079, ADR-106) |
| **model** | *within that backend, which model?* (Fable 5 / Opus / Sonnet) | shipped by C1 |
| **effort** | *at what reasoning depth / token budget?* | not yet needed — **reserved** |

Every one of these:
1. is chosen at exactly one side-effect boundary — the worker spawn (`cs tackle`);
2. is resolved by the same six-tier precedence
   (`Flag > FormulaPin > EnvVar > Config > GlobalConfig > Default`) — see
   [`AdapterSelectionSource`](../../crates/cosmon-core/src/event_v2.rs) and its
   sibling [`ModelSelectionSource`](../../crates/cosmon-core/src/event_v2.rs);
3. is **scoped to the adapter above it** — a model id only has meaning inside
   its adapter (Fable is a *claude* model); an effort tier will only have
   meaning inside its adapter too.

Three dials, one decision, one frontier, one resolution shape, one
adapter-scoping. If they are named separately the reader sees three features;
if they are named together the reader sees one — and every future dial (a
fourth, fifth axis) slots into the same struct instead of arriving as another
special case.

## Decision

**Reserve the noun `Incarnation` for the resolved launch-time bundle, and
model it as a dependent triple with two dormant slots:**

```
Incarnation {
    adapter: AdapterId,             // which body (always present)
    model:   Option<ModelId>,      // which model within that body  (C1: live)
    effort:  Option<EffortTier>,   // at what depth                 (reserved, dormant)
}
```

Read as prose: *an `Incarnation` is the answer to "how does this molecule take
on a body?" — which backend, which model inside it, at what effort.* The
physics lineage is deliberate: a molecule is bodiless until `cs tackle`
**incarne**s it; the `Incarnation` is the parameter set of that embodiment.

The following are consequences of the naming, not new machinery:

### 1 — `Incarnation` is a *dependent bundle*, not a flat product

`model` and `effort` are **adapter-scoped**: their legal values depend on which
`adapter` was chosen. Formally, the valid pair space is a dependent sum
`Σ(a : Adapter). Model(a)`, not the flat product `Adapter × Model` (von-neumann
Q2). A `ModelId` alone is meaningless; only `(adapter, model)` is a coordinate.
This is why config for the model axis nests under `[adapters.<name>].default_model`
and **never** as a flat top-level `default_model` key — a flat key would assert
a model exists independent of its adapter, which is false.

### 2 — Per-molecule model routing is a *special case* of the `Incarnation`

- adapter-routing (shipped) = the `Incarnation` with only the `adapter` slot
  chosen by the operator, `model`/`effort` = `None`.
- model-routing (C1) = the `Incarnation` with the `adapter` at its default and
  the `model` slot filled.
- The general "task → `(adapter, model, effort)`" router (year-two, if ever)
  = the `Incarnation` with all three slots chosen by a policy.

The narrow pin C1 shipped is a **strict subset by construction** of the general
abstraction. This is the whole point of naming it now: the narrow feature and
the general feature are the *same struct at different fill levels*, so the
general one — should its pain ever be real — is filled in, not rebuilt.

### 3 — The resolution chain is adapter-scoped and shared in shape

Each slot resolves by the same six-tier fold, and the model/effort folds read
config **inside the resolved adapter's table**. Precedence, verbatim from C1's
`resolve_model_selection`:

```
--flag  >  formula-step pin  >  $COSMON_DEFAULT_*  >  per-galaxy [adapters.<a>]  >  global [adapters.<a>]  >  floor
```

The **floor differs by axis and this is load-bearing** (von-neumann's minimax):

- `adapter` floor = the configured default adapter (a body is mandatory).
- `model` floor = **`None`** — cosmon pins nothing and the adapter's own
  default applies. A strong-model floor's worst case is a silent frontier
  dispatch with zero operator intent; `None`'s worst case is byte-identical to
  today's no-pin behaviour. **Silence must resolve to the cheapest safe option,
  never a strong model.** Strong is reachable only from a positive per-molecule
  act (`--flag` or `formula-pin`), never from a config/env *default* — the guard
  itself is C4's job, but the doctrine is fixed here.
- `effort` floor (when it ships) = `None`, same reasoning.

### 4 — Composition is validated opaquely, at one gate, before side effects

Because `(adapter, model)` is a dependent sum, an invalid pair (a model id that
does not exist in the chosen adapter) is a **typed refusal**, the analogue of
`spawn_seam`'s `UnknownAdapter` / `AdapterNotFound`, fired **after both axes
resolve and before any worktree/tmux side effect**. The reserved refusal is
`ModelNotValidForAdapter { model, adapter, known_models }`.

Where the set of valid models lives is decided here as **verdict C — opaque
pass-through** (von-neumann): cosmon carries the model id opaquely and the
**backend is the authority on its own fiber** — an invalid id is rejected by the
adapter at launch (surfaced via the ADR-119 exit-code contract). Rationale: a
source-literal model table (verdict A) has an unbounded maintenance derivative
and *rots toward false negatives* — it starts refusing valid new models the day
a provider ships one. An **optional, fail-open, adapter-declared `known_models`
hint** (verdict B) may be carried purely for good error messages; absent the
hint, composition is opaque and the backend judges. **Never a hard-coded table
(A).** An unknown id is passed through, not pre-rejected.

> This ADR *reserves* `ModelNotValidForAdapter` and the opaque-validation rule
> as doctrine. The validator's code, and the `Incarnation` struct itself, are a
> follow-up beside `ValidatedAdapterName` in `cosmon-core::spawn_seam` — **not
> shipped by this molecule** (C5 is doc-only). Until then, composition is
> already opaque-by-default: C1 carries the model id opaquely and the backend
> rejects an invalid pair at launch. The reservation prevents a future author
> from inventing a divergent name or a hard-coded table.

### 5 — Cross-provider extensibility falls out of the carrier discipline

The resolver produces **one adapter-uniform `Option<String>`** per axis. How
that value reaches the backend — an env var (`ANTHROPIC_MODEL` for the claude
adapter), a `--model` CLI arg, a config default for a direct-API arm — is each
`build_*_command`'s **private business**, not the resolver's. This is why the
model axis scales to N adapters with no `match` in the resolver, and why a
future cross-provider router (route *this* task to *that* adapter+model) is a
policy filling the `Incarnation` slots, not a new mechanism. The abstraction is
provider-agnostic by construction; only the carrier is provider-specific, and
the carrier is already encapsulated per adapter.

## What is reserved vs. what is built

| Element | State after this ADR |
|---------|----------------------|
| `adapter` slot | **built** (ADR-079, ADR-106) |
| `model` slot + resolution chain + `--model` flag + formula pin | **built** (C1 / `c255`) |
| `ModelSelectionSource` attribution enum | **built** (C1) |
| The noun `Incarnation` + the "three dials, one decision" doctrine | **reserved by this ADR** |
| `Incarnation { adapter, model, effort }` struct in `cosmon-core` | reserved — code is a follow-up, effort as a **dormant `Option`** |
| `ModelNotValidForAdapter` typed refusal + opaque-validation rule | reserved by this ADR — code is a follow-up |
| `effort` slot / `--effort` flag / `EffortTier` | reserved — **no flag, no wiring**; ships the day effort has cited pain, by filling an `Option` (zero migration) |
| Auto-routing (`task → (adapter, model, effort)` by difficulty) | deferred `temp:cold` — a policy that fills the slots; unbuildable until `ModelSelected` history exists to validate a classifier (carnot) |

The discipline (jobs): the `Incarnation` is a **struct + one resolver-per-slot +
one validator**, *not a runtime router*. Reserving the general shape is cheap
forward-compat insurance; building the policy factory now would re-import the
exact leak this feature exists to remove (a false-`hard` classification silently
escalates a molecule to a strong model).

## Consequences

**Positive.**
- The model axis reads as *the second slot of a named bundle*, not an unrelated
  feature. A reviewer encountering `--model` finds the doctrine that governs it.
- The effort axis, when its pain lands, is an `Option` fill — no new struct, no
  migration, no second "how do we isolate this per molecule?" deliberation.
- Composition failures have one canonical name and one gate, matching the
  adapter axis's `UnknownAdapter` — no divergent per-axis error vocabulary.
- The opaque-validation verdict prevents a maintenance sink (a model table that
  rots toward refusing valid new provider models).

**Costs / tensions carried forward.**
- **T1 (the deepest panel tension) is *not* fully resolved by this ADR.**
  Opaque model ids (verdict C, above) and the "strong is rare, guard against a
  strong default" safety property (kahneman/carnot) are in genuine friction:
  you cannot both "know nothing about model ids" and "refuse a strong default."
  The proposed resolution — a thin, optional, operator-declared, **fail-open**
  `strong = [ids]` *cost-class* annotation per adapter (a cost annotation, **not**
  a validity table; an unlisted id is treated as cheap/safe) — is C4's to
  implement and ratify. This ADR only fixes that legality stays opaque; the
  cost-class axis is orthogonal and lands with the ceiling.
- The `known_models` hint, if ever added, must stay **fail-open** — an empty or
  stale hint must never reject a real model. It is for error messages only.

**Neutral.**
- `Incarnation` is **domain vocabulary, not CLI surface.** The operator-facing
  verb stays `cs tackle --model` (and, later, `--effort`). No `cs incarnation`
  command is introduced; the noun lives in `cosmon-core`, the events, and this
  ADR.

## Coherence checklist (per CLAUDE.md)

1. **Stateless?** Yes — an `Incarnation` is resolved fresh per `cs tackle`
   invocation; nothing persists between dispatches (this *is* the amnesia
   invariant: strong is never inherited).
2. **Idempotent?** Yes — resolution is a pure function of (flags, formula,
   env, config); twice = once.
3. **Regime-aware?** Yes — the `Incarnation` is resolved in the Propelled
   regime at spawn, by the same command (`cs tackle`) that already resolves the
   adapter. No new regime, no daemon.
4. **Single perimeter?** Yes — it names an attribute of an existing command's
   role; it adds no command.
5. **Symmetric undo?** N/A — a spawn attribute, torn down with the worker at
   `cs done`.
6. **Runtime-compatible?** Yes — a future resident runtime fills the same slots
   from a policy; the struct is the seam.
7. **Worker/human boundary?** Preserved — resolution happens in the
   human-callable `cs tackle`; workers never self-select a model.
8. **Write-read asymmetry?** Preserved — resolution reads config and writes the
   spawn; attribution (`ModelSelected`, C2) is a separate emission.
9. **Merge-before-dispatch?** N/A.
10. **CLI-first for workers?** N/A — this is a spawn-time attribute of *how* a
    worker is launched, chosen by the pilot.

## Deferred (named so they are not silently dropped)

- **`Incarnation` struct + `ModelNotValidForAdapter` validator code** — a
  follow-up beside `ValidatedAdapterName` in `cosmon-core::spawn_seam` (this ADR
  is doc-only).
- **`effort` axis** — dormant `Option<EffortTier>` slot; no flag, no wiring.
- **Strong-cost-class annotation + fail-closed ceiling + safe-default guards** —
  C4 of `delib-20260704-b476`; resolves tension T1's cost half.
- **Auto-routing by difficulty** — `temp:cold`; a policy filling the slots,
  gated on accumulated `ModelSelected` history (carnot: unbuildable before
  manual-pin volume exists to validate a classifier).
- **Cross-provider task router** — year-two; the `Incarnation` noun is reserved
  for it, the machinery is built only when a second concrete instance exists.
