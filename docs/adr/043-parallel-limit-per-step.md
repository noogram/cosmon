# ADR-043: Per-Step `parallel_limit` — Opt-in Concurrency Cap

## Status

Accepted (2026-04-15). Implemented in `cosmon-core` (schema + parsing),
`cosmon-runtime` (DagPolicy enforcement), and `cosmon-cli` (`cs run`
wiring).

## Context

The resident runtime (`cs run`, ADR-016 Phase 3) dispatches every ready
molecule on every tick. For a well-behaved DAG this is exactly what we
want: the DAG edges already model the ordering constraint, the frontier
reducer (ADR-041) already gates on merged-ness, and the policy sorts by
critical path. Parallelism is a property of the DAG's shape.

But cosmon is now running non-trivial fan-outs: mission-controller
decompositions, deep-think panels, fleet audits with many sibling
targets. A decomposition step that nucleates 12 siblings makes the
runtime dispatch 12 `cs tackle` subprocesses at once — each spawning a
tmux pane, a worktree, and a Claude worker. On the operator's laptop
that is not twelve times faster; it is twelve times more contention on
tmux, git, and the token budget.

A related signal from the pilot (2026-04-15):

> *Le parallel limit fait complètement sens, surtout que cosmon devenant
> de plus en plus robuste, j'ai tendance à lancer des grosses sessions
> en parallèle. Par contre je préférerais que cela soit désactivé par
> default (limite infinie) et que cela soit un mécanisme que l'on active
> uniquement sur demande.*

Dust (a related orchestration system) ships a hard-coded
`concurrency_for_block()` with per-block-type caps (LLM=32, Data=64,
Browser=8). That is too opinionated for cosmon: our "blocks" are
formula steps defined by users, not a closed vocabulary owned by the
runtime.

## Decision

Add an **optional** `parallel_limit` declaration to every formula step:

```toml
[[steps]]
id = "decompose"
title = "Decompose"
description = "Nucleate siblings."
parallel_limit = { max = 4, mode = "static" }
```

### Semantics

- **Scope.** The cap applies to the `(formula_id, step_order)` pair. The
  runtime refuses to dispatch a new `Evolve` if doing so would push the
  count of `Running` molecules sharing that pair above `max`.
- **Opt-in.** Absence of `parallel_limit` means unbounded (the
  pre-ADR-043 default). Every existing formula parses and behaves
  exactly as before.
- **Static mode only, today.** `mode = "static"` is the only enforced
  mode. `mode = "smart"` parses but is a no-op — see ADR-044.
- **`cs run`-only.** The limit is honored by the resident runtime
  (`cs run`). Manual `cs tackle` dispatches are not capped — tackle is
  the operator's explicit "I want this now" lever and is already a
  human-pace action. This is consistent with the command-perimeter
  discipline (architectural-invariants.md).
- **Not a retry gate.** A molecule dropped by the cap stays in the ready
  frontier; the next tick re-evaluates as running molecules complete.
  No backoff, no queue — the DAG already encodes ordering.

### Validation

- `max = 0` is a parse error (use `1` for serial, or omit the field for
  unbounded).
- Unknown `mode` values are rejected at parse time so typos fail loudly.

### Wire-up

- `cosmon-core::formula::Step` gains `parallel_limit: Option<ParallelLimit>`.
- `cosmon-runtime::DagPolicy::with_limits()` accepts a
  `HashMap<(FormulaId, usize), u32>` and applies the cap inside
  `next_actions` after critical-path ordering.
- `cs run` builds that map via `load_parallel_limits()` by scanning the
  formulas referenced in the compiled DAG.

## Consequences

**Positive:**
- Operators can self-throttle heavy decompositions without rewriting the
  formula or manually staggering `cs tackle`.
- Existing formulas are unchanged by default — the feature is inert
  until a formula opts in.
- The `(formula, step)` key aligns with the DAG policy's existing view
  of molecules and requires no new coordinate system.

**Trade-offs:**
- The runtime now loads formulas at `cs run` startup to collect limits.
  This adds one file read per unique formula in the DAG; unparseable or
  missing formulas are skipped silently (consistent with the
  best-effort posture elsewhere).
- Counting `Running` molecules per `(formula, step)` is O(N) per tick,
  where N is the snapshot size. Negligible in practice; not worth a
  cached index.

**Deferred (see ADR-044):**
- `mode = "smart"`: observation-driven caps that consume cosmon's
  existing instrumentation (EnergyBudget, claudion entropy, backlog
  pressure). Parsed today, enforced later.

## Alternatives considered

1. **Global runtime flag `--max-parallel`.** Rejected: hides the intent
   inside the operator's shell history, not in the formula that declares
   the structural decision. The "why" belongs with the step.
2. **Formula-level cap (one number per formula).** Rejected: steps have
   different concurrency profiles — a `decompose` step fans out; a
   `verify` step is serial. Putting the number on the step keeps the
   declaration local.
3. **Default the cap to 8.** Rejected explicitly by the pilot: *« je
   préférerais que cela soit désactivé par défaut ».* Defaulting a limit
   silently changes the behavior of every existing DAG.

## Links

- Supersedes: nothing.
- Related: [ADR-016](016-autonomy-regimes-and-resident-runtime.md)
  (resident runtime perimeter),
  [ADR-041](041-atomic-frontier-projection.md) (merged-before-dispatch),
  [ADR-044](044-smart-resource-limits-roadmap.md) (future smart-limit
  policy surface).
- Derived from: `delib-20260415-6b9d` (IDEA-3).
