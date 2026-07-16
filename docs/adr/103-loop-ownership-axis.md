# ADR-103 — `LoopOwnership` axis on `ValidatedAdapterName` (TS-2)

**Status:** Proposed (2026-05-19).
**Date:** 2026-05-19.
**Decider:** Noogram.
**Empirical motive:** méta-délib
`delib-20260518-ac8e`
on fractal classification of cosmon Adapters. The synthesis (wheeler,
knuth, einstein, feynman convergent shape, §C2) named the seam
ADR-099/101 close — *which adapter* and *how supervised* — but left
open one axis: *who runs the agent loop*.
[ADR-102](102-cosmon-agent-harness-and-agentloop-port.md) shipped the
in-process loop runner (`cosmon-agent-harness::spine::run_loop`)
beside the existing external-binary path
(`cosmon-transport::claude::spawn`). Both paths produce
`AdapterSelected` / `WorkerSpawned` events with no field
distinguishing "cosmon ran the loop" from "a binary ran the loop".

**Authoring task:** `task-20260518-9851`
(`delib-20260518-ac8e` Child C).
**Authoring discipline:** knuth/wheeler/einstein/feynman convergent
shape — type the binary axis, one bit, pin with `compile_fail`
doctests.

**Binds:** [ADR-079](079-worker-spawn-port-and-adapter-contract.md)
(`Adapter` primitive, four-word closure preserved — `LoopOwnership`
is an Adapter attribute, not a new primitive);
[ADR-099](099-dispatch-site-stability.md) (TS-0 — this ADR is its
third axis, after ADR-101 added the second);
[ADR-100](100-direct-api-adapters-r2-amendment.md) (Direct-API
Adapters — `LoopOwnership::Cosmon` is the type-level expression of
the address-space property distinguishing in-process loops from
external binaries);
[ADR-101](101-supervision-mode-typed-on-validated-adapter.md)
(`SupervisionMode` typestate — `LoopOwnership` is the orthogonal
axis the synthesis named);
[ADR-102](102-cosmon-agent-harness-and-agentloop-port.md) (the
in-process loop runner whose existence forces the axis to be visible
at the spawn seam).

**Cites:** `delib-20260518-5178` (the upstream intra-worker
deliberation that produced the cosmon-agent-harness path);
`delib-20260518-ac8e` (this meta-deliberation).

**Architectural invariants:** `docs/architectural-invariants.md`
§8b (*propose mechanisms of verification, do not impose them — unless
the mechanism is free at runtime and lossless at compile time, at
which point it becomes a contract*) and §14 (karpathy badge — every
adapter's loop-ownership contract is now visible in the type the
dispatch site receives, and as a string-newtype on the wire).

---

## Context

The synthesis of `delib-20260518-ac8e` is verbatim explicit on the
load-bearing question:

> *« Le seam fermé par ADR-099 / ADR-101 (validation pré-dispatch,
> SupervisionMode typestate) reste ouvert d'un axe : qui possède la
> boucle. Aujourd'hui, `cosmon-agent-harness::spine::run_loop` en
> cours d'exécution et `cosmon-transport::claude::spawn` en cours
> d'exécution produisent des événements qui ne portent aucun champ
> distinguant les deux. »*
> — synthesis §S1 (wheeler's load-bearing observation, six of seven
>    panelists convergent)

Six of the seven panelists arrived at the same enum shape under four
different names:

- **wheeler**: `pub enum LoopOwnership { External, Cosmon }` in
  `cosmon-core::spawn_seam`, beside `SupervisionMode`.
- **einstein**: `pub enum CognitiveOwnership { Hosted, Supervised }`.
- **knuth**: `pub enum AgentLoopLocus { External, Native }` joining
  the ADR-099/101 product
  (`ValidatedAdapterName × SupervisionMode × AgentLoopLocus`).
- **feynman**: binary `External` vs `InProcess` — push `Composite`
  to the formula level.

The convergent shape is the per-Adapter typed identity widened by one
bit at the validation seam. The names disagreed; the structure
agreed. wheeler's name was the most informative pair (says *who*,
not *where*) and parallels `SupervisionMode { TmuxPane, InProcess }`
cleanly (orthogonal question, parallel grammar). Pick `LoopOwnership`.

### Why no third variant

`Composite` (a fleet that mixes external and in-process loops, e.g.
the gas-town pattern of refinery + polecat workers) is **set-level,
not atom-level**. Six of seven panelists (synthesis §C1) named this
as the structural diagnosis. Adding a `LoopOwnership::Composite`
variant would re-open the ADR-099 seam: a `Composite` adapter
silently aggregates a heterogeneous set behind a single name, and
the dispatch site cannot reason about its supervision or its loop
without re-parsing children. The right home for fleet-topology
concepts is a prospective `cosmon-core::fleet_topology` module —
gated on Child D's verdict that there *is* a second cosmon-side
Composite example. Until then, the axis stays binary.

### Why no `|distinct| ≥ 2|` invariant

The academy L3 lever — refusing constructions whose typed perimeter
has cardinality `< 2` — does **not** transfer to cosmon (synthesis
§C3). Cosmon's analogue, if any, is Child E's perimeter
(`task-20260518-3a3a`, post-1.5-example invariant audit). This ADR
does not import it.

---

## Decision

### 1. Type the axis in `cosmon-core::spawn_seam`

```rust
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LoopOwnership {
    External,  // claude / aider / codex — external binary, cosmon owns spawn + supervision
    Cosmon,    // openai / anthropic via cosmon-agent-harness — in-process FSM + tool dispatch
}
```

`#[non_exhaustive]` so the widening hook exists for future
cosmon-lab variants without breaking exhaustive matches
downstream.

### 2. Extend `validate_adapter_name`

The validator returns a triple instead of a single name:

```rust
pub fn validate_adapter_name(
    raw: &str,
    declared: &[String],
) -> Result<(ValidatedAdapterName, SupervisionMode, LoopOwnership), UnknownAdapter>;
```

The built-in mapping (closed at the validator):

| Adapter | `SupervisionMode` | `LoopOwnership` |
|---|---|---|
| `claude` | `TmuxPane` | `External` |
| `aider` | `TmuxPane` | `External` |
| `codex` | `TmuxPane` | `External` |
| `openai` | `InProcess` | `Cosmon` |
| `anthropic` | `InProcess` | `Cosmon` |

A doctest in `spawn_seam.rs`
(`built_in_axes_cover_every_built_in_name`) asserts the table covers
every built-in name. Adding a new built-in without a matching row
fails the test before the binary ships.

TOML `[adapters.<name>]` per-installation rows may declare an
explicit `ownership = "external" | "cosmon"` field. Absence defaults
to `External` for caller-supplied (non-built-in) names — the legacy
pre-ADR-103 contract preserved so hand-authored rows keep their
observable behaviour. Built-in names ignore the TOML row (the
validator is authoritative for them); a stale TOML override on a
built-in adapter cannot mis-route the dispatch.

### 3. Emit on the event log as a string-newtype

The event-log replay contract is the strictest stability tier in
cosmon: replay must keep working across crate revisions, and
serde-tagged enums on events ossify variant shapes. Carry the
discrimination as a **string-newtype** with `Display`:

```rust
// in cosmon-core::event_v2
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct LoopOwnershipTag(String); // "external" | "cosmon"

impl From<LoopOwnership> for LoopOwnershipTag { /* ... */ }
impl Default for LoopOwnershipTag { /* "external" */ }
```

The conversion is one-way at the seam:

| Site | Surface |
|---|---|
| Call site (Rust) | `LoopOwnership { External, Cosmon }` — exhaustive matches, `#[non_exhaustive]` widening |
| Wire (events.jsonl) | `LoopOwnershipTag(String)` — `"external"` / `"cosmon"` |

Same gesture as `ValidatedAdapterName` (typed in Rust, string on the
wire). tolnay-aligned per `delib-20260518-ac8e` §D2.

`loop_ownership: LoopOwnershipTag` is added with `#[serde(default)]`
to both `AdapterSelected` and `WorkerSpawned` so existing
`events.jsonl` lines that pre-date the field round-trip cleanly
through serde. The default tag is `"external"` (the legacy
contract).

### 4. Thread the triple through the dispatch site

`cs tackle` consumes the triple at the validation gate and threads
`LoopOwnership` jointly with `&ValidatedAdapterName` and
`SupervisionMode` to `emit_adapter_selected`, `register_tackle_worker`,
and onward into `spawn_and_prompt`. Wherever the dispatch chain
matches on `SupervisionMode`, the match arms now consume the joint
shape (or fall through `_ =>` for the loop-ownership axis the arm
does not interpret). `#[non_exhaustive]` on `LoopOwnership` forces
every downstream `match` to add a `_ =>` arm or to widen explicitly.

### 5. Two `compile_fail` doctests

A type-system witness for the axis (knuth's mechanism, bounded
form). Both live in `cosmon-core::spawn_seam`:

```rust
/// ```compile_fail
/// // The validator returns a TRIPLE — destructuring as a pair must
/// // not compile. If this starts compiling, the axis was dropped.
/// let (_name, _supervision) =
///     validate_adapter_name("claude", &["claude".to_owned()]).unwrap();
/// ```
///
/// ```compile_fail
/// // `LoopOwnership` is `#[non_exhaustive]`; an exhaustive match
/// // from outside the crate must not compile, pinning the widening
/// // hook against an accidental loss in a future refactor.
/// fn classify(o: LoopOwnership) -> &'static str {
///     match o {
///         LoopOwnership::External => "external",
///         LoopOwnership::Cosmon => "cosmon",
///     }
/// }
/// ```
```

The runtime witness is the standard `From` conversion +
event-log round-trip test, exercised in
`cosmon-state::events::worker_spawn::tests`.

### 6. `SupervisionMode` migration to `cosmon-core::spawn_seam`

`SupervisionMode` was tactically scaffolded in
`cosmon-transport::registry` with a doc comment announcing the
ADR-101 migration to `cosmon-core::spawn_seam::ValidatedAdapterName`.
The validator's triple return forces the migration: it cannot return
a type from a downstream crate. The enum moves verbatim to
`cosmon-core::spawn_seam`; `cosmon-transport::registry` re-exports
the canonical type so existing observer-side imports keep
compiling. The helper `supervision_mode_for` becomes a thin wrapper
around the validator's axis table — its observer-side call sites
migrate to the triple on their own cadence.

---

## Alternatives considered

### A. Three-variant `HarnessSource { Inert, Vendor, Native }`

Rejected (synthesis §C1). The structural diagnosis from six of seven
panelists: `Composite` is set-level, not atom-level. Putting it on
the Adapter atom re-opens the ADR-099 seam — the dispatch site
silently aggregates a heterogeneous fleet behind a single name.

### B. Typed `HarnessRoutingTable` in `cosmon-core`

Rejected (synthesis §C4). The deferred indirection adds a table
nobody is reading yet. ADR-103 keeps the validator as the single
authority; the table is a `const &[(...)]` two lines below the
validator definition, not a separately-typed surface.

### C. Soft chronicle-only path (Child A)

Kept as Child A (`task-20260518-fecf`), complementary not exclusive.
The chronicle paragraph records the cognitive arc that produced this
ADR; ADR-103 makes the structural commitment binding.

### D. `LoopOwnership::Composite` from day one

Rejected (synthesis §C1). Gated on Child D's verdict
(`task-20260518-2c2c`) that there *is* a second cosmon-side
Composite example. With 1.5 examples today, the axis stays binary.

### E. `|distinct| ≥ 2|` construction-time refusal

Rejected (synthesis §C3). The academy L3 lever does not transfer to
cosmon as written; if cosmon needs a perimeter analogue, Child E
(`task-20260518-3a3a`) is where it lands. ADR-103 stays inside its
bounded perimeter.

---

## Invariants

- `H(loop_ownership) > 0` observable on every `AdapterSelected` and
  `WorkerSpawned` line in `events.jsonl`. The cat-test
  `adapter_selected.loop_ownership == worker_spawned.loop_ownership`
  surfaces a routing mismatch the way ADR-099's `adapter_name`
  cat-test surfaces a routing mismatch.
- Per-Adapter typed identity stays a product
  `ValidatedAdapterName × SupervisionMode × LoopOwnership` returned
  jointly at the validation seam. No downstream code re-derives the
  axes from a string allowlist; the validator owns the answer.
- `#[non_exhaustive]` on `LoopOwnership` is the stable widening
  hook. Adding `Composite` (or another cosmon-lab variant) is
  an additive change downstream — no caller match has to break, but
  every caller's `_ =>` arm has a chance to widen.
- `BUILT_IN_AXES` covers every name in `built_in_adapter_names()`
  (enforced by `built_in_axes_cover_every_built_in_name` test).
- `LoopOwnershipTag` wire format is `"external"` / `"cosmon"`. A
  future serde rename is a deliberate ADR-grade decision, not a
  silent serde attribute edit.

---

## Out of scope

- Adding a `Composite` variant to `LoopOwnership` (Child D's
  verdict gates that path).
- Importing the academy L3 lever as a `|distinct| ≥ 2|`
  construction-time refusal (Child E's perimeter).
- Creating `cosmon-core::topology` or `cosmon-core::fleet_topology`
  modules (gated on Child D's verdict + a second cosmon-side
  Composite example).
- Touching `cosmon-agent-harness/src/lib.rs:85` (Child B's
  perimeter, `task-20260518-4f4a`).
- Writing the chronicle paragraph (Child A's perimeter,
  `task-20260518-fecf`).

---

## References

- `delib-20260518-ac8e` — méta-délib on fractal classification of
  cosmon Adapters (parent of this child).
  - `responses/wheeler.md` — `LoopOwnership { External, Cosmon }`
    name and the seam observation §Q4.
  - `responses/knuth.md` — `AgentLoopLocus { External, Native }`,
    `compile_fail` mechanism §Q4.
  - `responses/einstein.md` — `CognitiveOwnership { Hosted, Supervised }`,
    invariant *"one bit, that is the distinction"*.
  - `responses/feynman.md` — `External` vs `InProcess` honest
    binary §Q1, Composite-at-formula-level §C1.
  - `synthesis.md` — convergent shape §C2, seam diagnosis §S1,
    string-on-wire recommendation §D2.
- `delib-20260518-5178` — intra-worker deliberation that produced
  the cosmon-agent-harness path (upstream of this ADR's enabling
  conditions).
- [ADR-079](079-worker-spawn-port-and-adapter-contract.md) — the
  `Adapter` primitive this ADR widens at the typed-identity seam.
- [ADR-099](099-dispatch-site-stability.md) — TS-0, the
  validation-before-spawn invariant this ADR extends to a third
  axis.
- [ADR-100](100-direct-api-adapters-r2-amendment.md) — the
  Direct-API Adapters whose existence forced the loop-ownership
  axis to be visible.
- [ADR-101](101-supervision-mode-typed-on-validated-adapter.md) —
  the `SupervisionMode` typestate this ADR sits beside.
- [ADR-102](102-cosmon-agent-harness-and-agentloop-port.md) — the
  in-process loop runner whose existence made the axis observable.
