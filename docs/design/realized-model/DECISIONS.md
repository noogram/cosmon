# Realized-model attribution — settled decisions

Operator-confirmed settlement of the three open decisions from
`delib-20260718-c70e` (see its `synthesis.md`). These are **binding** for the
implementation (`task-20260718-6b8d`) and the pre-mortem (`task-20260718-f111`).

The load-bearing convergences from the deliberation are **settled and not
re-openable here**: (1) intention and realization are two coexisting facts, not
one slot; (2) honesty by disjoint source — realization folds from a dedicated
event and never reads the intention/pin; (3) drift is the signal, agreement is
silence (render realized only when it differs from, or exists without, the pin);
(4) silent adapters degrade to "intended, not confirmed"; (5) capture is one
serde field for openai/anthropic/mistral, already wired for claude via claudion.

## D1 — Realized slot cardinality: **tri-state (faithful)** ✅ operator-confirmed

The realized value is a tri-state, NOT an `Option<String>` last-wins:

```rust
enum Realized {
    Unknown,               // "?"  — worker died before any observation event
    Silent,                // "-"  — ran, never reported its model
    Observed(Vec<String>), // "opus→sonnet" — the trajectory of models that ran
}
```

Rationale: last-wins fabricates on the exact case the feature exists to reveal —
a real Opus→Sonnet quota fallback (per-turn, confirmed real by feynman) collapses
into a single-model session that never happened; and `None` cannot distinguish a
crashed worker (`Unknown`/`?`) from a silent one (`Silent`/`-`). `-` is the
*positive* claim "ran, said nothing" — applying it to a crashed worker would
invent an execution.

## D2 — Naming: act/fact split

- **Event** (runtime act of observing): `EventV2::ModelObserved`.
- **Attribution axis / display label** (the fact — what ran): `realized`.
- **Struct field**: `realized_model` (reads truer at the display than
  `observed_model`).
- Reserve the bare verb `observe` for any surface that *reads* state, per
  wheeler — do not overload it for the runtime capture.

## D3 — Display grammar: ASCII, drift-only

Byte-safe ASCII per the module's existing `EMPTY_CELL` note (implementer:
confirm whether the TUI surface already commits to UTF-8 before using any
non-ASCII):

- `~>` — pin→realized **drift** (e.g. `claude/opus~>sonnet [cli]`); the arrow
  joins *intention* and *realization*.
- `X→Y` — realized **trajectory** (case b), rendered *inside* the realized
  segment; a different arrow from the drift one, and it must stay distinct.
- `...` — live-pending (running, not yet observed) — motion.
- `-` — completed-silent (ran, reported nothing) — a closed door.
- `?` — dead-before-event (worker crashed before any observation).
- Agreement (realized == pin) renders **no** realized glyph. `realized` carries
  no source tag (it is an outcome, not a choice).

## D4 — Emission cadence (converged, no vote)

Emit `ModelObserved` on the **first** assistant turn carrying a concrete model
id, and **re-emit only on change**. Not per-turn, not at teardown.

## Honesty invariant (from the deliberation — enforce structurally)

The `ModelObserved` event carries a **bare `String`** model id (never `Option`):
silence is expressed by *not emitting the event*, so "never fabricate a record of
execution" is true by construction, not by a runtime check. The realized fold arm
names **only** the realized field and cannot reach the intention field — the
no-clobber property is structural. Mirror the existing
`reasoning_effort_is_never_inferred` discipline: `realized` is never back-filled
from the pin or config.
