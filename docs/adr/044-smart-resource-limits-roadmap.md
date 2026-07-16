# ADR-044: Smart Resource Limits — Roadmap (DRAFT / FUTURE)

## Status

**DRAFT** (2026-04-15). Forward-looking positioning document. No
implementation today. The static side of the machinery lives in
[ADR-043](043-parallel-limit-per-step.md); this ADR sketches the smart
side that reuses its declaration surface.

## Context

ADR-043 introduced `parallel_limit = { max = N, mode = "static" }` —
the minimum viable concurrency cap. `mode = "smart"` is parsed but does
nothing. This ADR records the direction in which the smart side should
evolve, so today's `Smart { policy: String }` placeholder is not a
leftover but a committed extension point.

Two observations motivate the roadmap:

1. **Cosmon already carries the instrumentation.** We have
   `EnergyBudget` and `Temperature` on every molecule, `claudion`
   session probes parsing Claude Code JSONL logs, Shannon-style entropy
   measurements on response streams, and `temp:*` tag curation on the
   backlog. These are signals a scheduler could act on — they exist
   because we *already* care about the physical quantities a smart
   limiter would read.
2. **Static caps are a ceiling, not a policy.** Declaring
   `max = 8` says "never run more than 8 of this step at once". It does
   not say "slow down when the entropy of Claude's output collapses" or
   "speed up when the backlog of `temp:hot` is growing" or "back off
   when this laptop is thermally throttled". Those are policy
   decisions — and cosmon is one of the few agentic runtimes with the
   telemetry to make them evidence-based rather than vibes-based.

## Candidate observational signals

Any of these can serve as the input channel for a smart cap. None are
committed — this ADR only enumerates the candidates and the direction.

| Signal | Source | Lever |
|--------|--------|-------|
| Tokens-per-minute per worker | claudion JSONL probes | Throttle dispatch when aggregate rate exceeds budget |
| Response entropy (claudion / shannon) | claudion entropy analyzer | Back off the offending worker when entropy collapses (signals degradation / loop) |
| Backlog pressure | `ensemble --tag temp:hot` count | Raise the cap transiently when actionable backlog is long |
| Machine saturation | CPU / RAM / thermal probes | Self-throttle below a host-level threshold |
| Merge-queue contention | `git` operation latency | Back off when `cs done` is blocking on stale locks |

## Candidate architecture

The contract can stay narrow. A `LimitPolicy` trait with a single
question:

```rust
pub trait LimitPolicy {
    /// Return the effective cap for (formula, step_order) right now,
    /// given a fleet snapshot and a telemetry view. `None` = unbounded.
    fn effective_cap(
        &mut self,
        formula: &FormulaId,
        step_order: usize,
        snapshot: &FleetSnapshot,
        telemetry: &TelemetrySnapshot,
    ) -> Option<u32>;
}
```

- A `StaticLimitPolicy` would implement ADR-043 exactly (`map.get(key)`).
- Each smart policy would be its own implementation, keyed on the
  `policy` string from the TOML (`"energy-aware"`, `"entropy-backoff"`,
  `"backlog-boost"`, …).
- The resident runtime would hold one `Box<dyn LimitPolicy>` and ask it
  every tick. The DAG policy stays unchanged — it only knows *a cap
  exists*, not how it was derived.

Telemetry is the new surface. It would aggregate:

- `EnergyBudget` / `Temperature` from molecule state.
- claudion session rollups (already parsed; today just surfaced for
  operators).
- Machine-local readings (via `sysinfo` or similar).
- `cs ensemble` backlog counts by tag.

None of this requires new domain concepts — it reuses existing
instrumentation and just pipes it into one place.

## Rationale

Two reasons this is worth reserving an ADR number for:

1. **The static surface is the right shape.** Per-step, opt-in,
   policy-keyed — that signature scales to smart modes without a
   breaking change to formulas or the CLI.
2. **Differentiator vs. other agent orchestrators.** Bittensor / Morpheus
   / most token-gated networks regulate concurrency via **incentive
   tokens**: you run what you can afford, and the market prices
   congestion. Cosmon's angle is the inverse: concurrency is regulated
   by **observation of the physical signals** the runtime already
   measures. That is only possible because cosmon is physics-native
   (energy, entropy, temperature are first-class). Smart limits is how
   we cash in on that thesis at the scheduler layer.

## Deferred decisions

- Whether `Smart { policy: String }` should accept structured
  parameters (`{ policy = "energy-aware", budget_per_min = 50000 }`) or
  keep it opaque and let each policy parse its own sidecar config.
- Whether `LimitPolicy` should be global (one per runtime) or per-step
  (composable via `(formula, step_order) → policy name`). Current lean:
  per-step name, global registry of implementations.
- How telemetry flows into the runtime without bloating
  `FleetSnapshot`. Current lean: a separate `TelemetrySnapshot` fed by
  the same file-store that drains claudion and energy state.

## Non-goals

- This ADR does **not** commit to implementing any smart policy. It
  reserves the name and shape so today's `Smart` variant is not
  leftover cruft.
- This ADR does **not** deprecate `Static` mode. Static is the default,
  is obvious to reason about, and covers the common case where the
  operator just wants "cap this at N".

## Links

- Depends on: [ADR-043](043-parallel-limit-per-step.md) (the surface).
- Related: energy instrumentation (THESIS Part XI), claudion session
  probes, Shannon-style entropy work.
- Cross-references: `delib-20260414-89dc` (roadmap), `delib-20260414-7322`
  (Mythos — why physics-native differentiates cosmon from token-gated
  orchestrators).
- Derived from: `delib-20260415-6b9d` (IDEA-3).
