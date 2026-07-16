# ADR-145 — Model-affinity ordering of the runtime frontier drain

**Status:** Accepted (implemented — `task-20260707-9833`).
**Date:** 2026-07-07.
**Decider:** Noogram.
**Parent deliberation:** `delib-20260705-7288`
— C3, *"model-affinity batching for the single-resident-model `ollama-g5`
oracle"*. This ADR ratifies the **scheduler half** of that verdict: the
runtime, not just a library primitive, must reorder the ready frontier by
bound model.
**Kind:** ADR-grade because it changes the **DAG-dispatch order** of the
resident runtime — a behaviour of the L3 scheduler, not a local refactor. Per
CLAUDE.md — *"Do not backdoor architectural changes through individual PRs …
DAG-dispatch-order behaviour change, not a back-doorable PR"* — the doctrine is
filed as a decision.

**Binds / cites / complies with:**

- [ADR-142](142-incarnation-launch-time-decision.md) — the `Incarnation`
  (adapter · model · effort as one launch-time decision). The frontier reorder
  needs each molecule's **model** slot; ADR-142 fixes that this slot is chosen
  **once, at spawn**, which is the whole reason a *pre-resolution* is required
  (see §The pre-resolution problem).
- [ADR-043 (parallel-limit-per-step)](043-parallel-limit-per-step.md) — the
  per-`(formula, step_order)` concurrency caps. The affinity reorder runs
  **after** the cap admission in `DagPolicy::next_actions`, on the already-
  admitted batch, and reuses the same `(formula_id, current_step)` keying and
  the same `load_*` formula-scan shape (`load_step_models` mirrors
  `load_parallel_limits`).
- [ADR-016](016-autonomy-regimes-and-resident-runtime.md) /
  [ADR-138](138-autonomous-runtime-two-loop-client-of-core.md) — the resident
  runtime is a **client** of the transactional core. This ADR touches only the
  scheduling *policy* (`DagPolicy`), not the core state machine; the reorder is
  a pure function of the snapshot.
- [`docs/architectural-invariants.md`](../architectural-invariants.md) §Purity
  — `DagPolicy` performs no I/O. The model **pre-resolution** that *does* read
  the filesystem is injected from the CLI layer as a closure
  ([`ModelResolver`]); the policy stays filesystem-free.

**Merged primitives wired by this ADR** (`task-20260705-c843`,
commit `465101087`):

- `cosmon_graph::affinity_order` — clusters a ready frontier by bound model,
  resident-first. Permutation invariant.
- `cosmon_graph::model_switch_count` — the executable spec of `affinity_order`'s
  optimality; counts VRAM model loads for a given order.
- `cosmon_provider::OllamaProvider::with_keep_alive` — the provider half
  (holds a model warm across the batch).

These three had **zero callers outside `cosmon-graph` / `cosmon-provider`** — a
breach of the *"merged primitive = wired primitive"* invariant. This ADR closes
the breach for the two scheduler primitives and records why the third
(`keep_alive`) stays off the dispatch path.

---

## Context

On a single-GPU local oracle (`ollama-g5`, C3 of `delib-20260705-7288`) the box
holds **exactly one** ~120 B model resident in VRAM (48 GB ≈ one 120 B; a
second model forces a ~40 GB swap through disk). Ollama keeps the last-used
model loaded for `keep_alive` (5 min default), so two consecutive dispatches
that ask for the **same** model pay the load once, whereas an alternating
frontier (`A, B, A, B, …`) reloads on **every** turn. The local path dies on
latency long before it dies on quality.

`task-20260705-c843` merged the two halves of the fix as pure, tested library
primitives — but wired them to **nothing**. The scheduler (`cs run`'s
`DagPolicy`) still emitted the ready batch in pure critical-path / id order,
model-blind. `affinity_order` was dead code. This is exactly the pathology the
federation names *"a merged primitive that is not a wired primitive"* — the
capability exists in the crate graph but no live caller reaches it, so the
system behaves as if it were never written.

Two beads were noted at c843 merge time: *(a)* a per-step model-swap guard, and
*(b) wiring `affinity_order` into `cs run`'s drain*. This ADR is bead **(b)**.

## The pre-resolution problem (why this needs an ADR, not a PR)

`affinity_order(frontier, model_of, resident)` needs, for **every molecule in
the frontier**, the model it is bound to. But the frontier is made of
**pending** molecules — molecules that have **not been tackled yet**. And
ADR-142 fixes the `Incarnation` (adapter · model · effort) **once, at launch
time**. Therefore:

> At frontier-ordering time there is **no `ModelSelected` event** to read. The
> molecule has no model *yet* — it will get one when `cs tackle` runs.

So the scheduler cannot *observe* each molecule's model; it must **pre-resolve**
it — re-derive the model `cs tackle` *will* pick, from the molecule's persisted
state, before the tackle happens. That is a genuine new behaviour of the
scheduler: it now reasons about a launch-time decision **ahead of** the launch.
Two consequences make it a decision, not a mechanical wiring:

1. **Which sources are consulted.** The full tackle-time chain is
   `--model > formula-step pin > $COSMON_DEFAULT_MODEL > per-galaxy config >
   global config > None` (`resolve_model_selection`, ADR-142 C1). The scheduler
   pre-resolution consults **only the formula-step `model =` pin**. Rationale:
   - `--model` is a *human, tackle-time* input; it does not exist for a
     still-pending frontier molecule.
   - config / env defaults are **galaxy-global** — they collapse every molecule
     into a single model bucket, where affinity ordering is a provable no-op
     (one bucket ⇒ zero swaps to save). Reading them would add I/O and a
     precedence surface for zero behavioural gain.
   - the formula-step pin is therefore the **sole source of per-molecule model
     variation** — the only thing affinity ordering exists to exploit (ADR-142:
     a tiered model is reachable only from `--model` or a formula-step pin;
     strong is never inherited from a default — tackle's C4 safe-default guard).

   A molecule with no formula-step pin pre-resolves to `None` (unbound) and is
   dispatched **last**, on whatever model is resident — it never *causes* a
   swap.

2. **Tracking the resident model.** Optimality (draining the resident bucket
   first, saving its reload) requires the scheduler to **track which model is
   loaded**. Since there is no daemon telling it, the policy tracks it
   structurally: `resident` is seeded from `cs run --resident-model` (the model
   already warm at start, if known) and, each tick, updated to the model of the
   **last** molecule it dispatched — that is what will be loaded when the next
   tick's batch begins.

## Decision

1. **`DagPolicy` gains an optional affinity reorder.** When enabled, after the
   critical-path sort **and** the ADR-043 concurrency-cap admission, the
   already-admitted eligible batch is passed through `affinity_order`. Because
   `affinity_order` is a **stable partition**, critical-path order survives as
   the **intra-bucket tie-break**: model-affinity is the primary key, critical
   path the secondary. On a serial single-GPU box, avoiding a 40 GB reload
   dominates any critical-path gain, so model-first is correct there; and the
   reorder only ever permutes a batch the DAG already declared ready.

2. **The reorder is a permutation — DAG semantics are untouched.** Every
   molecule the frontier admitted is still dispatched exactly once; only the
   *order within a ready batch* changes. This is the load-bearing invariant of
   `affinity_order` (rule 1) and is asserted directly by
   `affinity_permutation_never_drops_a_molecule`.

3. **Off by default.** Affinity is enabled only by `cs run --affinity`. Absent
   the flag, `DagPolicy` returns the eligible batch untouched — cloud dispatch
   (many models, no single-resident constraint) is **byte-identical** to the
   pre-ADR-145 path. The flag is legacy-DAG-policy-mode only; the ADR-095
   `--resident` loop is out of scope for V0 (see §Scope).

4. **Model pre-resolution is injected, not performed in the policy.** The pure
   `DagPolicy` never reads the filesystem. `cs run` builds a `ModelResolver`
   closure from the DAG's formula-step pins (`load_step_models`, mirroring
   `load_parallel_limits`) and injects it via `DagPolicy::with_affinity`. This
   preserves the policy's purity contract.

5. **`model_switch_count` is wired as the live optimality meter.** Each tick,
   when affinity is on, the policy measures the naive (pre-reorder) and
   clustered (post-reorder) switch counts and folds the saved delta into
   `affinity_switches_saved` (exposed for observability and tests), emitting a
   one-line stderr note when a swap is saved. This gives `model_switch_count` a
   real production caller — it is no longer only its own doctest.

6. **`keep_alive` stays off the dispatch path — interim mitigation instead.**
   `OllamaProvider::with_keep_alive` is the *provider* half. But the floor
   dispatch path runs the **`OpenAIProvider`** against Ollama's OpenAI-compat
   `/v1` endpoint, not `OllamaProvider` (which targets the native `/api/chat`).
   Wiring `keep_alive` would require either routing the floor through the native
   adapter or teaching the `/v1` path an Ollama-specific extension — a larger
   provider-selection change out of scope here. Until then the resident-model
   guarantee is obtained **daemon-side**, not per-request:

   > **Interim mitigation.** Set `OLLAMA_KEEP_ALIVE=-1` in the `ollama-g5`
   > daemon environment (never unload) **and** pin **one model per fleet** so
   > the frontier is single-bucket by construction. Under a one-model pin,
   > `affinity_order` is a no-op (correctly) and `keep_alive` is moot — the
   > model is loaded once and never evicted. The scheduler reorder (this ADR)
   > is what makes a *multi-model* fleet on one GPU viable later, once the
   > provider half lands.

   Wiring `keep_alive` into a native-adapter dispatch path is filed as
   follow-up (`temp:warm`).

## Consequences

- **Positive.** The two scheduler primitives are now wired; the *"merged =
  wired"* invariant holds again. A multi-model frontier on a single-GPU oracle
  reorders to minimise VRAM swaps, provably optimal for the single-resident
  machine (`affinity_order`'s switch-count floor). Cloud dispatch is unchanged.
- **Neutral / bounded.** The reorder is `O(|batch|)` per tick; the formula scan
  is one read per distinct formula, once at `cs run` bootstrap.
- **Negative / accepted.** On a *multi-GPU* box (more than one model resident at
  once) the single-resident assumption behind `resident`-tracking is too
  conservative — it will still cluster by one "resident" model. That is the
  wrong model of the hardware, but it never breaks correctness (still a
  permutation) and the flag is opt-in; a multi-resident policy is future work.
- **Debt.** `keep_alive` remains unwired (provider-path mismatch); the interim
  daemon-side mitigation covers the one-model-per-fleet case that matters today.

## Alternatives considered

- **Reorder before the concurrency-cap admission.** Rejected: the caps decide
  *which* molecules dispatch this tick; affinity only decides their *order*.
  Reordering first would let a low-priority resident-model molecule displace a
  critical-path molecule from a scarce slot. Affinity is a within-batch
  concern, so it runs on the admitted batch.
- **Persist the resolved model on the molecule at nucleation.** Rejected for
  V0: it would duplicate the ADR-142 resolution chain into a second write-site
  and risk drift with what `cs tackle` actually picks. Re-deriving from the
  formula-step pin at ordering time keeps a single source of truth.
- **Make affinity always-on.** Rejected: it would change cloud dispatch order
  for no benefit and re-key the critical-path scheduler on a hardware
  assumption most fleets don't have. Opt-in via `--affinity`.

## Scope

- **In:** `DagPolicy` legacy mode (`cs run`, `cs run --policy dag`). The
  `--affinity` and `--resident-model` flags. `load_step_models`,
  `ModelResolver`, and the `model_switch_count` meter.
- **Out (future):** the ADR-095 `--resident` RuntimeLoop; wiring `keep_alive`
  through a native-adapter dispatch path; a multi-resident (multi-GPU) affinity
  policy; the per-step model-swap guard (bead *(a)* from c843).

[`ModelResolver`]: ../../crates/cosmon-runtime/src/dag_policy.rs
