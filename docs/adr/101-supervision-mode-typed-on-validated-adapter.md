# ADR-101 — `SupervisionMode` typestate on `ValidatedAdapterName` (TS-0+1)

**Status:** Accepted (2026-05-18).
**Date:** 2026-05-18.
**Decider:** Noogram.
**Empirical motive:** academy smoke `task-20260518-35c4`
(an internal academy chronicle, 2026-05-18 grok-direct-api smoke result)
§*« Sur le vrai gap entre Direct-API et l'orchestrateur cosmon »* —
`cs tackle --adapter openai` now routes correctly to the Direct-API
agent loop (ADR-100), but the post-spawn pipeline still calls
`install_harvest_hook` unconditionally. The hook is a tmux pane-died
witness; for an in-process HTTP agent loop there is no pane to
witness. The journal/result divergence migrated one metre down the
pipeline.
**Authoring task:** `task-20260518-bab7`.
**Authoring discipline:** knuth-style typestate (same family as
ADR-099).

**Binds:** [ADR-079](079-worker-spawn-port-and-adapter-contract.md)
(four-word closure preserved; `SupervisionMode` is an Adapter
attribute, not a vocabulary addition); [ADR-099](099-dispatch-site-stability.md)
(TS-0 — this ADR is its natural extension); [ADR-100](100-direct-api-adapters-r2-amendment.md)
(`SupervisionMode::InProcess` is the type-level expression of the
address-space property distinguishing Direct-API Adapters from
subprocess CLI Adapters); internal chronicles
(2026-05-18 c8 spawn-routing fix, b6d7 origin) and the 35c4 sibling above.

**Blocks:** `task-20260518-18ba` (GAP #1 — tactical gate on
`install_harvest_hook`, natural impl PR for this typestate) and
`task-20260518-aec3` (GAP #2 — `cleanup_partial_tackle` preserves the
worktree when the in-process agent loop succeeded but the post-config
hook failed).

**Architectural invariants:** `docs/architectural-invariants.md`
§8b (*propose mechanisms of verification, do not impose them — unless
the mechanism is free at runtime and lossless at compile time, at
which point it becomes a contract*) and §14 (karpathy badge — every
adapter's supervision contract is now visible in the type the
dispatch site receives).

---

## Context

ADR-099 (TS-0) made *validation-before-spawn* a compile-time
invariant. ADR-100 (R2) admitted in-process HTTP agent-loop Adapters
(`openai`, `anthropic`) under the existing Worker-Spawn Port. The
first post-R2 smoke test (`task-20260518-35c4`) probed the route
end-to-end and revealed that the post-spawn pipeline still encodes an
implicit assumption inherited from the day Claude Code was the only
citizen:

> *Every Adapter spawns a tmux session that cosmon supervises via the
> pane-died hook.*

The contract was implicit. `cmd::tackle.rs` L490 calls
`install_harvest_hook` unconditionally after `spawn_and_prompt`
returns; for `openai` / `anthropic` the call either fails (no session
to hook into) or — worse — succeeds against a session nobody watches,
then `cleanup_partial_tackle` wipes the worktree (and the artefact
the agent loop wrote) on the failure path.

The forcing function is identical to b6d7's. b6d7 ended with *« the
journal said `aider`, the pane showed Claude »*; 35c4 ends with *« the
journal said `openai`, the supervisor expected a pane that does not
exist »*. Both are output/result divergences (academy L9). Both
reduce to the same structural diagnosis: **an Adapter property the
dispatch path consumes is not part of the Adapter's typed identity**.
TS-0 closed that gap for the `name` property. This ADR closes it for
the `supervision` property.

The next adapter family that opens — FUSE-backed pseudo-files,
container-spawned workers, MCP-orchestrated remote workers — exposes
a third supervision modality. If supervision stays untyped, every new
modality re-imports the 35c4 risk under a new name.

---

## Decision

### 1. The typestate

A new type `cosmon_core::spawn_seam::SupervisionMode` joins
`ValidatedAdapterName` in the same module:

```rust
/// How cosmon learns the worker has terminated. Every Adapter
/// declares exactly one mode at validation time; the post-spawn
/// pipeline branches on this enum, never on the adapter name string.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SupervisionMode {
    /// Worker runs in a tmux pane; cosmon installs a pane-died hook
    /// that exec's `cs harvest`. `hook_required = true` whenever
    /// the Adapter cannot be reconciled without the hook.
    TmuxPane { hook_required: bool },
    /// Worker runs in-process inside `cs tackle`; liveness is the
    /// agent-loop return. No tmux pane, no hook to install.
    /// `liveness_via_loop = true` records that the `Result` returned
    /// by the loop *is* the supervision signal.
    InProcess { liveness_via_loop: bool },
}
```

`#[non_exhaustive]` is load-bearing: it reserves space for future
modalities (`FuseFs`, `Container`, `RemoteMcp`, …) and forces every
in-tree dispatch `match` to consume new variants before merge —
the IFBDD signal ADR-098 §C8 named.

`SupervisionMode` is deliberately a small enum, not a trait. A
`Box<dyn Supervisor>` would re-open the dispatch hole at the trait
boundary (a future Adapter could `impl Supervisor { fn install(&self)
{ /* noop */ } }` and the pipeline would silently accept it). The
`match` over an enum is the structural invariant.

### 2. The validator signature evolves

`validate_adapter_name` returns the supervision mode alongside the
validated name:

```rust
pub fn validate_adapter_name(
    raw: &str,
    declared: &[String],
) -> Result<(ValidatedAdapterName, SupervisionMode), UnknownAdapter>;
```

The migration is mechanical
(`let (adapter, mode) = validate_adapter_name(...)?;`) and the new
`mode` binding must be consumed by the dispatch chain — see §4.
`UnknownAdapter` is unchanged.

### 3. Initial mapping (registry → mode)

The five built-in Adapters of ADR-100 (extended by
delib-20260518-5178 §S7) carry:

| Adapter | `SupervisionMode` |
|---------|-------------------|
| `claude` | `TmuxPane { hook_required: true }` |
| `aider` | `TmuxPane { hook_required: true }` |
| `codex` | `TmuxPane { hook_required: true }` |
| `openai` | `InProcess { liveness_via_loop: true }` |
| `anthropic` | `InProcess { liveness_via_loop: true }` |

`codex` joins the `TmuxPane` row of the mapping as the third
subprocess-CLI sibling (delib-20260518-5178 §S7 / §D4). The
`@openai/codex` npm package wraps a Node.js entry point; it runs in
a tmux pane and the existing `pane-died` hook is its supervisor,
verbatim. ADR-102 §D4 deliberately rejected promoting `Subprocess`
to a new variant on the strength of speculative non-tmux paths — the
empirical answer is that the codex CLI is tmux-shaped today, so it
reuses the `TmuxPane` mode unchanged. (`task-20260519-2d33` ships
the adapter and the version-pin runtime check.)

The mapping is **closed in the validator**: a name that validates
under `declared` but for which no mapping exists is a build-time bug
in the same sense as ADR-099 §3's registry-completeness invariant. A
doctest in `spawn_seam.rs` asserts the table covers every built-in
name.

TOML `[adapters.<name>]` extras (per-installation declarations) carry
an explicit `supervision = "tmux_pane" | "in_process"` field; absence
defaults to `TmuxPane { hook_required: true }` to preserve the legacy
contract for hand-authored Adapter rows that predate this ADR.

### 4. The call site becomes exhaustive

`cmd::tackle.rs` L490 is rewritten as an exhaustive `match` on
`SupervisionMode`:

```rust
match supervision_mode {
    SupervisionMode::TmuxPane { hook_required: true } => {
        install_harvest_hook(&backend, &session_name, &mol_id, &repo_root)?;
    }
    SupervisionMode::TmuxPane { hook_required: false } => {
        let _ = install_harvest_hook(&backend, &session_name, &mol_id, &repo_root);
    }
    SupervisionMode::InProcess { liveness_via_loop: true } => {
        // Agent loop returned Ok; the worker is already done.
        // No hook to install, no pane to watch.
    }
    // No catch-all. `#[non_exhaustive]` forces every consumer to
    // re-evaluate when a new variant lands — that is the point.
}
```

The absence of a `_ => ...` arm is load-bearing. Adding a fifth
variant (e.g. `FuseFs`, `Container`) requires touching every in-tree
`match`; `cargo check` fails until the new branch is consumed.

### 5. Why this is TS-0+1, not a successor

ADR-099 closed *validation-before-spawn*. ADR-101 closes
*supervision-mode-known-at-dispatch*. The two invariants are
orthogonal: ADR-099 ensures the byte sequence that reaches the spawn
site is a registered name; ADR-101 ensures the post-spawn pipeline
branches on a typed property of that name rather than on the name
itself. A future Adapter must satisfy both: it must be added to the
registry **and** it must declare a supervision mode. The compiler
enforces both, in the same `let (adapter, mode) = …` expression.

### 6. Compile-fail witnesses

`spawn_seam.rs` ships two new `compile_fail` doctests:

```rust
/// ```compile_fail
/// use cosmon_core::spawn_seam::SupervisionMode;
/// // Tuple-style construction must not work — variants are struct-like.
/// let _bad = SupervisionMode::TmuxPane(true);
/// ```
```

A downstream-crate `match` without `_` over `#[non_exhaustive]`
SupervisionMode is a second compile-fail witness, asserting the
forward-compat property at the crate boundary. These witnesses are
executable contracts; they fail to fail if a future PR weakens the
type.

---

## Consequences

**Direct.** `validate_adapter_name` returns
`Result<(ValidatedAdapterName, SupervisionMode), UnknownAdapter>`.
The two in-tree callers (`cmd::tackle`, `cmd::resurrect`) migrate
mechanically. The `install_harvest_hook` call site becomes a `match`
on `SupervisionMode`; the 35c4 failure mode (unconditional hook
against an in-process Adapter) cannot be reproduced without
weakening the type.

**Unblocks.** GAP #1 (`task-20260518-18ba`) lands its tactical gate
as the impl PR — the gate the chronicle proposed *becomes* the §4
`match` arm. GAP #2 (`task-20260518-aec3`) consumes
`SupervisionMode` in `cleanup_partial_tackle` to distinguish tmux
spawn failure (wipe legitimate) from in-process-post-config failure
(worktree preserved, `SF6SupervisionSetupFailed` emitted per the SF
taxonomy extended in `task-20260518-a823`).

**Downstream constraints.** A new HTTP Adapter gets
`InProcess { liveness_via_loop: true }` by declaration; a future
FUSE-backed or container-spawned Adapter adds a `SupervisionMode`
variant and `cargo check` fails until every dispatch site consumes
the new arm. The resident runtime (ADR-016, ADR-095) inherits the
typestate when it reaches the Worker-Spawn Port.

**Cost.** ~25 LOC in `cosmon-core/src/spawn_seam.rs` (enum, mapping
table, doctests). ~15 LOC of mechanical signature change in
`cmd::tackle.rs` and `cmd::resurrect.rs`. One TOML schema field
(`[adapters.<name>].supervision`) with a backward-compatible default.
Zero runtime cost.

**Negative / accepted.** `validate_adapter_name`'s signature breaks
once. Three in-tree call sites; no external consumers today. The
break is justified by the load-bearing invariant — wrapping the
tuple in a struct would force callers to learn a new accessor
without closing the dispatch-site hole.

---

## Alternatives considered

Named-for-the-record per ADR-082 INV-ADR-OPTIONS-CONSIDERED.

- **Local guard only (no typestate)** — `if matches!(adapter.as_str(),
  "claude" | "aider") { install_harvest_hook(...)? }`. Rejected as
  the anti-pattern this ADR retires; lands as the body of the §4
  `match`, never as a standalone `&str` predicate.
- **`Box<dyn Supervisor>` trait.** Rejected: trait dispatch re-opens
  the hole at the trait boundary (silent noop impls accepted). The
  enum `match` is the structural invariant.
- **Boolean `is_tmux_based: bool`.** Rejected: closes today's gap
  but cannot express *why* the supervisor differs; future modalities
  collapse into one bit. `#[non_exhaustive]` has no `bool` analogue.
- **Mode declared on the `Adapter` trait, not on
  `ValidatedAdapterName`.** Rejected: splits the two checks across
  two crates. Co-locating with name validation is the smallest cut.
- **Recompute mode at the spawn site from the validated name.**
  Rejected: recomputation is the exact pattern TS-0 closed for
  `name`. The property must travel with the validated value.

---

## Invariants

**Preserved.** ADR-079 four-word closure verbatim (`SupervisionMode`
is an Adapter attribute, not a vocabulary addition). ADR-099 §4
*permanent contract* — extended by this ADR to *no future PR may
bypass supervision-mode selection without breaking the type system*.
ADR-100 R2 module layout under `cosmon-transport::api::*` unchanged.
The C8 catch-all match arm in `spawn_and_prompt` is preserved as the
registry-completeness guard (distinct from the supervision-mode
exhaustivity of §4).

**Newly inscribed.**

- **`SupervisionMode` is the typed supervision contract.** Every
  built-in Adapter and every TOML-declared Adapter carries exactly
  one mode at validation time. The post-spawn pipeline branches on
  the mode, never on the name.
- **`#[non_exhaustive]` is the IFBDD signal.** A new variant cannot
  land without forcing every in-tree `match` to consume it. The
  catch-all `_` is forbidden in cosmon workspace code over
  `SupervisionMode`.
- **Validator returns `(name, mode)` as one operation.** A caller
  that obtains the name without the mode is suspect by construction.

**Modified.** `validate_adapter_name`'s return type from
`Result<ValidatedAdapterName, UnknownAdapter>` to
`Result<(ValidatedAdapterName, SupervisionMode), UnknownAdapter>`.
This is the only breaking change. ADR-099's compile-fail witnesses
for `ValidatedAdapterName` continue to compile-fail verbatim.

---

## L9 framing — the tmux-postulated pattern

ADR-099 named the journal/result divergence at the validation seam.
ADR-101 names it at the supervision seam. The pattern is the same:
*an Adapter property the pipeline consumes is encoded as an implicit
assumption rather than as a typed value*. b6d7 exposed it on `name`;
35c4 exposed it on `supervision`. The chronicle's load-bearing
sentence — *« le pipeline post-spawn présuppose tmux pour tout
adapter »* — names a class of error, not a single bug.

The cosmon stance is unchanged: *propose mechanisms of verification,
do not impose them — unless the mechanism is free at runtime and
lossless at compile time, at which point it becomes a contract*
(invariants §8b). `SupervisionMode` meets the bar: an enum
discriminant has zero runtime cost; the `match` is a jump table;
`#[non_exhaustive]` forces forward-compat decisions to be visible.

**Tattoo.** *La supervision est dans le type, pas dans le
postulé.*

---

## Implementation sequence

Documentation-only at acceptance. Code follows in the GAP #1 fix:

1. **This ADR.** Accept; `cs reconcile` updates `docs/adr/INDEX.md`;
   CHANGELOG entry.
2. **GAP #1 (`task-20260518-18ba`).** Add `SupervisionMode` to
   `cosmon-core::spawn_seam`; evolve `validate_adapter_name`'s
   signature; migrate `cmd::tackle.rs` and `cmd::resurrect.rs`;
   rewrite the `install_harvest_hook` call site as the §4 `match`.
   Compile-fail doctests in the same PR. TOML schema field
   `[adapters.<name>].supervision` with backward-compatible default.
3. **GAP #2 (`task-20260518-aec3`).** `cleanup_partial_tackle`
   matches on `SupervisionMode`; `SF6SupervisionSetupFailed` event.
4. **Smoke re-run.** Re-execute academy 35c4 after GAP #1 and GAP #2
   land. Expected: `cs tackle --adapter openai` produces
   `status: completed`, worktree preserved, no
   `install_harvest_hook` error. The 35c4 symptom becomes a
   regression test.

---

## References

- **Empirical motive:**
  an internal academy chronicle (2026-05-18, grok-direct-api smoke result)
  §*« Sur le vrai gap entre Direct-API et l'orchestrateur cosmon »*.
- **Cosmon-side chronicle (b6d7 cascade origin):**
  an internal chronicle (2026-05-18, c8 spawn-routing fix).
- **Bound ADRs:** [ADR-079](079-worker-spawn-port-and-adapter-contract.md),
  [ADR-099](099-dispatch-site-stability.md),
  [ADR-100](100-direct-api-adapters-r2-amendment.md).
- **Sibling tasks blocked:** `task-20260518-18ba` (GAP #1 — impl PR
  for this ADR), `task-20260518-aec3` (GAP #2 — consumer PR).
- **Authoring task:** `task-20260518-bab7`.
