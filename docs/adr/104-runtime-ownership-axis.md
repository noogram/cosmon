# ADR-104 — `RuntimeOwnership` axis on `ValidatedAdapterName` (TS-3, successor to ADR-103)

**Status:** Proposed (2026-05-19).
**Date:** 2026-05-19.
**Decider:** Noogram.
**Empirical motive:**
`delib-20260519-f6c3`
§D2 — *ADR-103 amendment* and §Q-OP-3 — *Amend ADR-103 to a two-axis
split (`LoopOwnership × RuntimeOwnership`)?* The synthesis records
tolnay and feynman converging from independent premises (API
discipline vs. *who can pull the plug*) on the same observation:
ADR-103's single-axis `LoopOwnership::{External, Cosmon}` today
conflates two distinctions that come apart precisely when cosmon
operates a self-hosted model runtime. The forcing function is the
sibling task
`task-20260519-2a75`
— ship vllm-mlx as an HTTP sidecar (Path B), point
`cosmon-provider::openai` / `::anthropic` at `localhost:8000`. Under
ADR-103 today, that case is unnamable: the supervisor (cosmon) runs
the agent loop, but the model server is operator-supervised — not the
Anthropic / OpenAI cloud — and not in cosmon's address space.

**Authoring task:** `task-20260519-7b72`.
**Authoring discipline:** tolnay (API discipline, narrow public seams,
non-exhaustive enums with widening hooks) + feynman (*the bit that
matters is who can pull the plug*).

**Binds:** [ADR-079](079-worker-spawn-port-and-adapter-contract.md)
(four-word closure preserved — `RuntimeOwnership` is an Adapter
attribute, not a new primitive);
[ADR-099](099-dispatch-site-stability.md) (TS-0 — this ADR is its
fourth axis after ADR-101 added the second and ADR-103 added the
third; the per-Adapter typed identity stays
`ValidatedAdapterName × SupervisionMode × LoopOwnership × RuntimeOwnership`,
returned jointly at the validation seam);
[ADR-100](100-direct-api-adapters-r2-amendment.md) (Direct-API
Adapters — Path B reuses the same `openai` / `anthropic` adapter
names; the runtime axis is keyed off the per-instance `base_url`
configuration, not off a new adapter name);
[ADR-101](101-supervision-mode-typed-on-validated-adapter.md)
(`SupervisionMode` typestate — orthogonal axis, untouched);
[ADR-102](102-cosmon-agent-harness-and-agentloop-port.md) (the
in-process loop runner that made the loop-ownership axis visible);
[ADR-103](103-loop-ownership-axis.md) (predecessor — this ADR
**refines, does not contradict** ADR-103: the `LoopOwnership` axis
stays binary and load-bearing; a new orthogonal `RuntimeOwnership`
axis is added beside it).

**Cites:** `delib-20260519-f6c3` (parent deliberation, panel of five
— einstein / feynman / torvalds / tolnay / niel — converging on Path
B by 2026-06-15 and naming the two-axis split as the natural
ADR-103 amendment);
`task-20260519-2a75` (the vllm-mlx-sidecar case that surfaces the
axis split);
`task-20260519-fedc` (the narrow Rust-llama Path A v0, Q3 2026
follow-up — same `Cosmon × Operated` cell as Path B, distinguished
only by whether the model server runs as a sidecar or as a
`pub(crate)` Rust library inside the cosmon process);
`task-20260519-f7fd` (the chronicle on rule re-reading — *« Rust où
il pèse — pas où il étouffe »* — cited but not recapitulated here).

**Architectural invariants:** `docs/architectural-invariants.md`
§8b (*propose mechanisms of verification, do not impose them — unless
the mechanism is free at runtime and lossless at compile time, at
which point it becomes a contract*) and §14 (karpathy badge — every
Adapter's runtime-ownership contract becomes visible in the type the
dispatch site receives, parallel to `SupervisionMode` and
`LoopOwnership`).

---

## Context

### The distinction ADR-103 conflates

ADR-103 named the load-bearing question:

> *who runs the agent loop.*

The synthesis of `delib-20260518-ac8e` answered it with one bit:
`LoopOwnership::{External, Cosmon}`. The mapping at the time was
clean — every shipped Adapter sat on one of two diagonals:

| Adapter | `LoopOwnership` | Address space |
|---|---|---|
| `claude` / `aider` / `codex` | `External` | external CLI binary cosmon spawns |
| `openai` / `anthropic` (cloud) | `Cosmon` | in-process inside `cosmon-agent-harness` |

The diagonal was tight enough to look like one axis. But the
mapping was actually conflating two:

1. **Who runs the agent loop.** Cosmon's `cosmon-agent-harness::spine::run_loop` (in-process FSM + tool dispatch) vs. an external binary cosmon spawns (`claude` / `aider` / `codex`).
2. **Who runs the model server.** Anthropic / OpenAI's cloud endpoints — a vendor cosmon merely consumes — vs. a server cosmon operates (installs, version-pins, restarts, reads logs).

For every Adapter cosmon had shipped through 2026-05-18, the two
axes lined up: an `External` loop talked to a vendor model server
(claude.ai backing the `claude` CLI) and a `Cosmon` loop also talked
to a vendor model server (Anthropic / OpenAI directly). The
diagonal was an empirical artifact, not a structural identity.

### The case that breaks the diagonal — Path B (vllm-mlx sidecar)

`task-20260519-2a75` ships an HTTP sidecar — `vllm-mlx` listening on
`http://localhost:8000` — and points `cosmon-provider::openai` /
`::anthropic` at the local URL. Under ADR-103 today this case is
unnamable:

- Calling it `LoopOwnership::External` is wrong — cosmon
  *operates* the agent loop end-to-end via
  `cosmon-agent-harness::spine::run_loop` (Path B is a config-only
  change to `base_url`; the in-process loop code is unchanged).
- Calling it `LoopOwnership::Cosmon` is also defensible (cosmon runs
  the loop) — but the variant's documentation today implies *the
  loop runs in-process inside cosmon* without naming the address
  space of the model server. A sidecar on `localhost:8000` is in a
  different process; only by re-reading ADR-103 as feynman did
  (*« qui peut tirer la prise »*) does Path B resolve to `Cosmon`.

Both readings carry information loss. The honest answer is that
ADR-103's axis names *one* property of the Adapter (loop ownership)
and silently coupled it to *another* (runtime ownership). When the
two properties decouple, the type cannot express the case.

### tolnay's diagnosis (parent synthesis §D2, response §3 verbatim)

> *« ADR-103's `LoopOwnership::{External, Cosmon}` is currently
> using* address space *as a proxy for the real distinction — but
> the real distinction is* who owns the orchestration*, not* who
> owns the model forward pass*. Anthropic's API is `External`
> because cosmon does not operate that endpoint, cannot restart it,
> cannot version-pin it, cannot read its logs. A vllm-mlx sidecar
> on `localhost:8000` is none of those things: cosmon supervises it
> (…), pins it (`pip install vllm-mlx==X.Y.Z`), restarts it, and
> reads its logs. The right grammar is a two-axis split:
> `LoopOwnership` (who runs the agent loop) × `RuntimeOwnership`
> (who runs the model server), with values `{Operated, Vendor}`.
> Path B is `LoopOwnership::Cosmon × RuntimeOwnership::Operated`;
> Path A is `LoopOwnership::Cosmon × RuntimeOwnership::Operated`
> with the runtime being a Rust library rather than a sidecar.
> Both honour cosmon's supervision invariants; only one of them is
> honest about the address-space cost. If ADR-103 today conflates
> the two, fix the ADR — don't conclude that Path B "breaks the
> typestate". The typestate is just under-named. »*

feynman, from a different angle, arrives at the same shape (parent
response §2 verbatim):

> *« the bit that matters is* who can pull the plug. *Operator pulls
> vllm-mlx by killing the process. Operator cannot pull Anthropic.
> That is the typestate distinction. »*

Two panelists, two registers, one structural answer.

---

## Decision

### 1. Type the new axis in `cosmon-core::spawn_seam`

`RuntimeOwnership` joins `SupervisionMode` and `LoopOwnership` in
the same module, with the same shape (`#[non_exhaustive]`, snake-case
serde, derives mirroring its siblings):

```rust
/// Who runs the model server an Adapter forwards completions to.
///
/// Orthogonal to [`LoopOwnership`] (who runs the agent loop) and
/// [`SupervisionMode`] (how cosmon learns the worker died). The
/// canonical reading: *who can pull the plug on the model forward
/// pass?* The operator can pull a self-hosted sidecar
/// (`localhost:8000`) by killing the process; the operator cannot
/// pull Anthropic.
///
/// `#[non_exhaustive]` reserves the widening hook for future
/// cosmon-lab variants (e.g. an `Embedded` runtime running as
/// a `pub(crate)` Rust library — Path A v0 territory — that may
/// later separate from `Operated` if address-space matters in its
/// own right; see ADR-104 §"Why no third variant").
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeOwnership {
    /// Cosmon (or the operator) installs, version-pins, restarts, and
    /// reads logs from the model server. Path B (vllm-mlx sidecar on
    /// `localhost:8000`) and Path A (a `pub(crate)` Rust library
    /// backing the same `LlmProvider` trait) both resolve here. The
    /// distinguishing capability is *the operator can pull the plug*.
    Operated,
    /// A third-party vendor endpoint cosmon merely consumes
    /// (`api.openai.com`, `api.anthropic.com`, `api.x.ai`, etc.).
    /// Cosmon can configure the request but cannot restart the
    /// server, version-pin its release, or read its logs. The
    /// distinguishing inability is *the operator cannot pull the
    /// plug*.
    Vendor,
}
```

### 2. The 2×2 grid (canonical mapping at acceptance)

The product `LoopOwnership × RuntimeOwnership` partitions every
Adapter cosmon ships into one of four cells:

|                              | `RuntimeOwnership::Operated`                                                  | `RuntimeOwnership::Vendor`                                                |
|------------------------------|-------------------------------------------------------------------------------|---------------------------------------------------------------------------|
| **`LoopOwnership::Cosmon`**   | `openai` / `anthropic` pointed at `localhost:8000` (Path B sidecar; Path A v0) | `openai` / `anthropic` pointed at the canonical cloud URL (today's default) |
| **`LoopOwnership::External`** | (today empty; reserved — e.g. a self-hosted Codex CLI driven by cosmon)        | `claude` / `aider` / `codex` (external CLI; vendor model server)            |

The empty `External × Operated` cell is not pathological — it names
a real future case (a self-hosted external CLI cosmon spawns, e.g.
a forked `codex` pointed at an operator-run inference server). The
ADR leaves the cell open by construction; no early-allocation of a
synthetic Adapter is justified.

### 3. The axis is per-instance, **not** part of the adapter name

A first-class design decision the ADR records in writing.

**Two shapes considered:**

- **(a) Per-instance, runtime-resolved from config.** The adapter
  name `openai` stays as one name; the `RuntimeOwnership` axis is
  resolved from per-installation `.cosmon/config.toml` at validation
  time. Built-in default: `Vendor`. The operator opts into
  `Operated` by declaring `[adapters.openai] runtime = "operated"`
  (typically alongside a `base_url = "http://localhost:8000"`
  override).
- **(b) Part of the adapter name.** Mint new validated names —
  `openai-local` vs. `openai-vendor` — and key the runtime axis off
  the name. The `BUILT_IN_AXES` table gains rows.

**Choice: (a).** Three reasons:

1. **CLI ergonomics preserved.** `cs tackle --adapter openai`
   continues to name the *protocol family* (OpenAI-compatible chat
   completions wire schema), which is the abstraction the operator
   reasons about. Deployment topology (localhost vs. cloud) is an
   installation detail, not an Adapter-identity detail. (b) would
   leak deployment topology into the CLI surface — the operator
   would have to remember which name maps to which topology, and the
   names would multiply combinatorially as Path A v0, Path B, and
   future operator-hosted runtimes each split off.
2. **Wire-schema seam preserved (tolnay §2, §3).** The whole point
   of Path B is that the OpenAI / Anthropic schema is the
   semver-stable boundary. The same `LlmProvider` impl serves
   `Vendor` and `Operated` cases — only the `base_url` differs. (b)
   would force two `LlmProvider` impls (or one impl behind two adapter
   names), duplicating the seam tolnay's whole argument is that
   *not* duplicating is the discipline.
3. **Existing precedent.** ADR-103's `LoopOwnership` axis is
   *already* per-instance: TOML rows can override the built-in
   default via `[adapters.<name>] ownership = "cosmon"`
   (see `cosmon-core::config::AdapterEntry::ownership` and the
   `resolve_loop_ownership` helper at `cmd::tackle::tackle`). The
   `RuntimeOwnership` axis follows the same pattern — symmetry
   beats invention.

The price of (a) is that the validator no longer resolves the
runtime axis from the name alone — it consults a per-installation
configuration row. The validator's contract widens to "name in
declared registry + axes resolved from config". The contract
remains:

> *The byte sequence reaching the spawn site is a registered name,
> AND every Adapter property the post-spawn pipeline consumes is
> typed in the validator's return.*

### 4. Validator signature (proposed)

The validator return widens from a triple to a quadruple. The
proposed shape preserves ADR-099 / ADR-101 / ADR-103 grammar
verbatim:

```rust
pub fn validate_adapter_name(
    raw: &str,
    declared: &[String],
) -> Result<
    (ValidatedAdapterName, SupervisionMode, LoopOwnership, RuntimeOwnership),
    UnknownAdapter,
>;
```

The validator's built-in mapping (closed at the validator) gains a
runtime column:

| Adapter | `SupervisionMode` | `LoopOwnership` | `RuntimeOwnership` (built-in default) |
|---|---|---|---|
| `claude` | `TmuxPane` | `External` | `Vendor` |
| `aider` | `TmuxPane` | `External` | `Vendor` |
| `codex` | `TmuxPane` | `External` | `Vendor` |
| `openai` | `InProcess` | `Cosmon` | `Vendor` *(overridable to `Operated` via TOML)* |
| `anthropic` | `InProcess` | `Cosmon` | `Vendor` *(overridable to `Operated` via TOML)* |

A doctest `built_in_axes_cover_every_built_in_name` extends to the
quadruple — the existing ADR-103 invariant ratchets up to four
columns. Adding a new built-in without a matching runtime cell fails
the test before the binary ships.

TOML `[adapters.<name>] runtime = "operated" | "vendor"` overrides
the built-in default. Absence preserves the legacy contract:
caller-supplied (non-built-in) names default to `Vendor` (the
pre-ADR-104 implicit). This default is the *honest* default — a
hand-authored row that does not declare otherwise is almost
certainly pointing at a vendor endpoint; an operator running a
sidecar is the one who opts in.

The TOML override is consumed downstream of the validator by a
`resolve_runtime_ownership(name, validator_default, entry)` helper,
parallel to the existing `resolve_loop_ownership` helper. This
mirrors ADR-103's pattern: the validator returns the built-in
default; the call-site helper applies TOML override. This shape
keeps `validate_adapter_name`'s signature independent of
`cosmon-core::config` (one fewer crate-level coupling) while still
ensuring the resolved axis travels with the validated name.

### 5. Event-log shape (proposed — emission deferred to the impl PR)

Same gesture as ADR-103 §3 — a `string-newtype` on the wire:

```rust
// cosmon-core::event_v2
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RuntimeOwnershipTag(String); // "operated" | "vendor"

impl From<RuntimeOwnership> for RuntimeOwnershipTag { /* ... */ }
impl Default for RuntimeOwnershipTag { /* "vendor" */ }
```

`runtime_ownership: RuntimeOwnershipTag` is added with
`#[serde(default)]` to both `AdapterSelected` and `WorkerSpawned` so
existing `events.jsonl` lines that pre-date the field round-trip
cleanly through serde. The default tag is `"vendor"` — the legacy
contract for every Adapter cosmon shipped before Path B lands.

Cat-test invariant (mirroring ADR-099's
`adapter_selected.adapter_name == worker_spawned.adapter_name`
discipline):

> `adapter_selected.runtime_ownership == worker_spawned.runtime_ownership`

A routing mismatch (the validator resolved `Operated` but a
downstream dispatch fell back to `Vendor`) becomes a single-grep
diagnostic on `events.jsonl`, the same way the journal/result
divergence in `task-20260518-b6d7` did for the `adapter_name` axis.

**Emission is deferred** to the impl PR that lands once Path B ships
(post-2026-06-15). At ADR acceptance, only the type and the built-in
mapping land. This is the same gating discipline as ADR-101 (which
landed Status Accepted as documentation-first, with the call-site
match arms coming in `task-20260518-18ba`).

### 6. Two `compile_fail` doctests

Type-system witnesses, parallel to ADR-103 §5:

```rust
/// ```compile_fail
/// // RuntimeOwnership is `#[non_exhaustive]`; an exhaustive match
/// // from outside the crate must not compile, pinning the
/// // widening hook against an accidental loss in a future refactor.
/// fn classify(r: RuntimeOwnership) -> &'static str {
///     match r {
///         RuntimeOwnership::Operated => "operated",
///         RuntimeOwnership::Vendor => "vendor",
///     }
/// }
/// ```
///
/// ```compile_fail
/// // The future validator return is a QUADRUPLE — destructuring
/// // as a triple must not compile once the impl PR lands. Until
/// // then, this doctest is staged in the ADR as the contract
/// // shape; the impl PR re-homes it to spawn_seam.rs.
/// let (_name, _supervision, _loop_ownership) =
///     validate_adapter_name("openai", &["openai".to_owned()]).unwrap();
/// ```
```

The first doctest is structural — it ships with the type at this
ADR's acceptance and starts failing-to-fail the moment a future PR
silently turns the enum exhaustive. The second is staged: it lands
as part of the impl PR once `validate_adapter_name` widens to a
quadruple.

### 7. Doc-comment freshness in `spawn_seam.rs`

The module header that today describes *three axes* (name,
supervision, loop) ratchets up to four:

```rust
//! # Per-Adapter typed identity — the four axes
//!
//! [`SupervisionMode`] names *how cosmon learns the worker died*.
//! [`LoopOwnership`] names *who runs the agent loop*.
//! [`RuntimeOwnership`] names *who runs the model server an Adapter
//! forwards completions to — the* who can pull the plug *axis*.
//! All three are orthogonal questions answered jointly per Adapter
//! at validation time and threaded through the spawn pipeline so
//! none has to be re-derived from a string allowlist downstream.
//!
//! See `delib-20260518-ac8e` for the binary [`LoopOwnership`] axis
//! (ADR-103) and `delib-20260519-f6c3` §D2 for the binary
//! [`RuntimeOwnership`] axis (this ADR).
```

Worked example threading all four cells:

```rust
/// # Example — the 2×2 `(LoopOwnership × RuntimeOwnership)` grid
///
/// ```rust,no_run
/// use cosmon_core::spawn_seam::{LoopOwnership, RuntimeOwnership};
///
/// fn dispatch_summary(loop_o: LoopOwnership, runtime_o: RuntimeOwnership) -> &'static str {
///     match (loop_o, runtime_o) {
///         (LoopOwnership::Cosmon, RuntimeOwnership::Operated) =>
///             "cosmon runs the loop and the operator can pull the model server's plug \
///              (vllm-mlx sidecar; cosmon-llama in-process)",
///         (LoopOwnership::Cosmon, RuntimeOwnership::Vendor) =>
///             "cosmon runs the loop; the model server is a vendor cosmon merely consumes \
///              (Anthropic API, OpenAI API)",
///         (LoopOwnership::External, RuntimeOwnership::Operated) =>
///             "cosmon spawns an external CLI; that CLI talks to an operator-run runtime \
///              (reserved — e.g. self-hosted Codex driven by cosmon)",
///         (LoopOwnership::External, RuntimeOwnership::Vendor) =>
///             "cosmon spawns an external CLI talking to its vendor cloud \
///              (claude / aider / codex today)",
///         _ => "unreachable (both axes are #[non_exhaustive] — widening hook only)",
///     }
/// }
/// ```
```

The `_ =>` arm in the example is intentional — both enums are
`#[non_exhaustive]`, so a future cosmon-lab variant on either
axis is admissible without breaking exhaustive matches downstream.

---

## Alternatives considered

Named-for-the-record per ADR-082 INV-ADR-OPTIONS-CONSIDERED.

### A. Mint new adapter names (`openai-local`, `openai-vendor`)

Rejected per §3. The CLI surface would grow combinatorially as new
operator-hosted runtimes land, and the wire-schema seam would
duplicate — every operator-hosted variant would force a parallel
`LlmProvider` impl (or one impl behind two names) for no
discriminating capability beyond `base_url`. Symmetric with
ADR-103's choice to key `LoopOwnership` off the name's built-in
default rather than minting `openai-cosmon` vs `claude-external`.

### B. Re-interpret `LoopOwnership::External` to cover sidecars

Rejected (parent synthesis §D2). feynman initially floated a
*sharpened reading* of `LoopOwnership::Cosmon` — "cosmon operates
the loop end-to-end (spawn, supervise, tear down, no third-party)"
— under which Path B is still `Cosmon`. But the sharpened reading
*and* tolnay's two-axis split agree on the same conclusion: the
single-axis grammar today is under-named. Sharpening the reading
without typing the second axis would push the load-bearing
information back into doc comments rather than into the type, which
is the exact pattern ADR-099 / ADR-101 / ADR-103 each closed for a
different Adapter property. **The sharpened reading and the two-axis
split are convergent, not exclusive: this ADR adopts both — the
sharpened reading is the prose for `LoopOwnership::Cosmon`, and the
typed second axis carries the address-space discrimination.**

### C. Add a third value `RuntimeOwnership::Hybrid` for multi-runtime fallback

Rejected (parallel to ADR-103 §"Why no third variant"). A
fleet-level concept (cosmon falls back between a local sidecar and
the cloud when the local OOMs) is set-level, not atom-level. Putting
a `Hybrid` variant on the Adapter atom would re-open the ADR-099
seam: the dispatch site silently aggregates a heterogeneous
topology behind a single value. The right home for fleet-level
runtime topology is a prospective
`cosmon-core::runtime_topology` module (gated on a second
cosmon-side example beyond the speculative one), parallel to the
`fleet_topology` module ADR-103 reserved. Until then, the axis
stays binary.

### D. Promote `RuntimeOwnership` to `LoopOwnership`'s replacement

Rejected. The synthesis (parent §D2, both panelists explicit) names
both axes as orthogonal, not one-replaces-the-other. Removing
`LoopOwnership` would erase the (External, Vendor) → (External,
Operated) discrimination — the reserved cell that names self-hosted
external CLIs cosmon spawns. The two axes carry distinct
information; collapsing them re-opens the ADR-103 seam under a new
name.

### E. Pure-prose ADR (chronicle-only path)

Rejected. tolnay's whole argument (§3) is that *the typestate is
under-named, fix the type*. A chronicle paragraph would record the
cognitive arc but leave the dispatch site re-deriving the runtime
axis from `base_url` strings at every consumer — the exact pattern
ADR-099 closed for `adapter_name`. The chronicle of the rule
re-reading lives in `task-20260519-f7fd` (cited, not recapitulated).
This ADR makes the structural commitment binding.

---

## Invariants

**Preserved.**

- ADR-079 four-word closure (`Worker · Port · Adapter · Tier`)
  verbatim — `RuntimeOwnership` is an Adapter attribute, not a
  vocabulary addition.
- ADR-099 §4 *permanent contract* — extended by this ADR to *no
  future PR may bypass runtime-ownership selection without breaking
  the type system*.
- ADR-101's `SupervisionMode` typestate — orthogonal axis,
  untouched.
- ADR-103's `LoopOwnership` typestate and its `CosmonProof` token —
  orthogonal axis, untouched. The sharpened reading of
  `LoopOwnership::Cosmon` (feynman's *« qui peut tirer la prise »*
  for the loop) is the prose; the typed second axis carries the
  parallel discrimination for the model-server side. **Both axes
  load-bearing; neither replaces the other.**

**Newly inscribed.**

- **`RuntimeOwnership` is the typed runtime-ownership contract.**
  Every built-in Adapter and every TOML-declared Adapter carries
  exactly one runtime-ownership value at validation time. The
  dispatch chain and the event log read the typed value, never the
  `base_url` string.
- **`#[non_exhaustive]` is the widening hook.** A new variant
  (e.g. `Embedded` for a `pub(crate)` Rust library runtime in the
  same address space as cosmon-agent-harness) cannot land without
  forcing every in-tree `match` to consume it. Downstream crates
  must carry a `_ =>` arm; that arm is the stable widening hook.
- **The 2×2 grid is the canonical decomposition.** Documented in
  the `spawn_seam.rs` module header as the worked example of
  §7. Any future Adapter must land in exactly one cell at
  acceptance.
- **Per-instance resolution.** The axis is keyed off the
  per-installation `.cosmon/config.toml` row, not off the adapter
  name. Two `cs tackle --adapter openai` invocations on two
  different installations may resolve to different runtime-ownership
  cells. The validator's return reflects the per-installation
  resolution.

**Modified** (proposed; lands in the impl PR — see §4 and §5).

- `validate_adapter_name`'s return type from `Result<(ValidatedAdapterName, SupervisionMode, LoopOwnership), UnknownAdapter>` to `Result<(ValidatedAdapterName, SupervisionMode, LoopOwnership, RuntimeOwnership), UnknownAdapter>`. Four in-tree call sites (`cmd::tackle`, `cmd::resurrect`, two test helpers) — same mechanical destructure as ADR-101's `(name, mode)` migration.
- `AdaptersConfig::AdapterEntry` gains a `runtime: Option<String>` field, mirroring `ownership` (`"operated" | "vendor"`, absence = built-in default).
- `AdapterSelected` and `WorkerSpawned` events gain
  `runtime_ownership: RuntimeOwnershipTag` with `#[serde(default)]`.

---

## Out of scope

- Adding a `Hybrid` / `Federated` / `Composite` variant to
  `RuntimeOwnership` (Alternative C — gated on a second cosmon-side
  example).
- Creating `cosmon-core::runtime_topology` or
  `cosmon-core::fleet_topology` modules (gated on the Composite
  case being structurally needed).
- Folding `RuntimeOwnership` into `LoopOwnership` (Alternative D —
  the two axes carry distinct information).
- Re-evaluating ADR-099's TS-0 dispatch typestate. ADR-099's axis
  count grows from 3 to 4 only via this new ADR's product — ADR-099
  stays as-is.
- Re-evaluating ADR-101's `SupervisionMode`. Orthogonal axis;
  untouched.
- Writing the long-form chronicle on the rule re-reading (*« Rust
  où il pèse — pas où il étouffe »*); that lives in
  `task-20260519-f7fd`. This ADR cites it; it does not recapitulate.
- Emitting `runtime_ownership` on `AdapterSelected` and
  `WorkerSpawned` events. The shape is named in §5; the wiring lands
  in the impl PR once Path B ships.

---

## Implementation sequence

Documentation-first at acceptance, mirroring ADR-101's discipline.
Code follows as a gated impl PR once `task-20260519-2a75` (Path B
sidecar) ships and the empirical motive is operational, not
prospective.

1. **This ADR** (`task-20260519-7b72`). Add `RuntimeOwnership` to
   `cosmon-core::spawn_seam` with `#[non_exhaustive]`, derives,
   serde shape mirroring `LoopOwnership`. Extend `BUILT_IN_AXES` to
   carry the runtime column. Add `runtime_for_built_in(name)`
   accessor. Add the `built_in_axes_pin_canonical_mapping` test
   coverage for the new column. Add the structural
   `compile_fail` doctest for `#[non_exhaustive]`. Add
   `AdapterEntry::runtime` TOML field. `cs reconcile` updates
   `docs/adr/INDEX.md`; CHANGELOG entry.
2. **Impl PR — validator widening** (post-2026-06-15, gated on Path B
   shipping). Widen `validate_adapter_name` to return the
   quadruple. Migrate the four in-tree call sites. Add
   `resolve_runtime_ownership` helper. Land the second
   `compile_fail` doctest (validator-return shape). Land the
   `RuntimeOwnershipTag` event field on `AdapterSelected` /
   `WorkerSpawned` with `#[serde(default)]` and the cat-test
   invariant.
3. **Smoke test — Path B end-to-end.** With the impl PR landed and
   vllm-mlx running on `localhost:8000`, `cs tackle --adapter openai`
   (with `[adapters.openai] base_url = "http://localhost:8000" runtime
   = "operated"`) produces `events.jsonl` lines carrying
   `runtime_ownership: "operated"`. The cat-test
   `adapter_selected.runtime_ownership ==
   worker_spawned.runtime_ownership` succeeds end-to-end. The 35c4
   regression test family extends with a `cs tackle --adapter
   openai --runtime operated` smoke.

---

## L9 framing — the same pattern, a finer cut

ADR-099 named the journal/result divergence at the *validation*
seam. ADR-101 named it at the *supervision* seam. ADR-103 named it
at the *loop* seam. This ADR names it at the *runtime* seam. The
pattern is the same: *an Adapter property the pipeline consumes is
encoded as an implicit assumption rather than as a typed value*. b6d7
exposed it on `name`. 35c4 exposed it on `supervision`. The
in-process loop runner (ADR-102) exposed it on `loop`. Path B
(`task-20260519-2a75`) exposes it on `runtime`.

Five different Adapter properties, five different empirical
forcing functions, one structural invariant: *every Adapter property
the dispatch path consumes is part of the Adapter's typed identity*.
Each successor ADR widens the typed identity by one axis without
contradicting the previous axes. The discipline holds because each
axis is binary (`#[non_exhaustive]`, two values, one bit), each axis
is orthogonal, and each axis is resolved at the same validation
seam.

The cosmon stance is unchanged: *propose mechanisms of verification,
do not impose them — unless the mechanism is free at runtime and
lossless at compile time, at which point it becomes a contract*
(invariants §8b). An enum discriminant has zero runtime cost; the
`match` is a jump table; `#[non_exhaustive]` forces forward-compat
decisions to be visible.

**Tattoo.** *Le runtime est dans le type, pas dans la `base_url`.*

---

## References

- **Parent deliberation:**
  `delib-20260519-f6c3`
  — Path A vs. Path B local-inference framing.
  - `responses/tolnay.md` §3 — *« the right grammar is a two-axis split »*.
  - `responses/feynman.md` §2 — *« who can pull the plug »*.
  - `synthesis.md` §D2 — convergence with shape (tolnay's two-axis
    split is a refinement of feynman's sharpened single-axis).
  - `synthesis.md` §Q-OP-3 — *Amend ADR-103 to a two-axis split?*
    (default: *yes, post-June-15*).
- **Empirical motive:**
  `task-20260519-2a75` — Path B (vllm-mlx HTTP sidecar) shipment;
  the case that surfaces the axis split.
- **Sibling tasks:**
  - `task-20260519-fedc` — Path A v0 (narrow Rust-llama
    `LlmProvider` impl over `llama-cpp-2`, Q3 2026 follow-up).
  - `task-20260519-f7fd` — chronicle on the CLAUDE.md rule
    re-reading.
- **Bound ADRs:** [ADR-079](079-worker-spawn-port-and-adapter-contract.md),
  [ADR-099](099-dispatch-site-stability.md),
  [ADR-100](100-direct-api-adapters-r2-amendment.md),
  [ADR-101](101-supervision-mode-typed-on-validated-adapter.md),
  [ADR-102](102-cosmon-agent-harness-and-agentloop-port.md),
  [ADR-103](103-loop-ownership-axis.md).
- **Authoring task:** `task-20260519-7b72`.
