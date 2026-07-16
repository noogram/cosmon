# ADR-099 — Dispatch-site stability (TS-0)

**Status:** Accepted (2026-05-18).
**Date:** 2026-05-18.
**Decider:** Noogram.
**Parent deliberation:**
`delib-20260518-cf2e`
— synthesis §5.4 names this ADR as the *universal prerequisite chain*:
TS-0 must land before any further Worker-Spawn additions, because every
later port-shaped expansion (third Adapter, dispatcher refactor, runtime
plugin) reopens the same compile-time hole if the seam is still untyped.
**Authoring task:** `task-20260518-d670`.
**Authoring discipline:** knuth (typestate / invariant programming).

**Binds:**
- [ADR-079](079-worker-spawn-port-and-adapter-contract.md) — the
  four-word vocabulary `{ Worker · Port · Adapter · Tier }` and the
  four-obligation Adapter contract.
- ADR-097 — the
  L1-L5 cross-reference framework; §C5/C8 specifically (the perimeter
  this ADR closes structurally).
- [ADR-098](098-worker-spawn-port-operationalisation-ifbdd.md) — the
  IFBDD operationalisation of the Worker-Spawn Port; §C8
  *spawn_and_prompt dispatches* is the runtime instrument TS-0
  promotes to a type-level invariant.
- Chronicle: an internal chronicle (2026-05-18, c8 spawn-routing fix)
  — the empirical motif. *« Le journal disait « aider », la pane montrait Claude. »*

**Architectural invariants:** `docs/architectural-invariants.md` §8j
(every Port is an ingress binding), §14 (karpathy badge — *"you can `cat`
cosmon's state"*: the value `cat`'d on `events.jsonl` is now byte-identical
to the value that traversed validation, by construction).

---

## Context

Academy smoke `task-20260518-b6d7` (2026-05-18 AM) produced the simplest
possible adversarial probe of the Worker-Spawn Port: invoke
`cs tackle --adapter aider`, read `events.jsonl`, capture the tmux pane.
The journal said *adapter_selected: aider*. The pane showed Claude. Both
statements were literally true at the source level — the flag was parsed,
the event was emitted — and yet the output (`events.jsonl`) and the
result (the binary running in the pane) disagreed silently. C8
(`task-20260518-958e`) repaired the runtime path with an explicit
`match adapter_name { "claude" => …, "aider" => …, other => Err(...) }`
inside `spawn_and_prompt`, and added `WorkerSpawned.adapter_name` so the
cat-test can cross-reference `adapter_selected.adapter_name ==
worker_spawned.adapter_name`.

C8 closed the *empirical* gap. The *structural* gap remained: the spawn
seam's signature was `adapter_name: &str`, and the validated name from
`get_adapter_or_err` was decoupled from the spawn call by ~120 lines of
unrelated tackle logic. Nothing in the type system prevented a future
PR from passing a literal, a stale variable, or a freshly-parsed
`env::var` to the spawn site and reproducing the b6d7 failure mode
under a different name.

The panel's load-bearing observation (delib-20260518-cf2e §5.4, knuth
detail in `responses/knuth.md`): *every* later expansion of the
Worker-Spawn Port — a third Adapter, the trait extraction of PR-4, an
external dispatcher reached through MCP, the resident runtime's
adoption of the port — passes through this seam. If the seam stays
untyped, every expansion re-imports the b6d7 risk. TS-0 must precede
the next port-shaped change, not follow it.

---

## Decision

### 1. The typestate

A new module `cosmon_core::spawn_seam` introduces:

```rust
pub struct ValidatedAdapterName(String);     // tuple field private
impl ValidatedAdapterName { pub fn as_str(&self) -> &str; }

pub fn validate_adapter_name(
    raw: &str,
    declared: &[String],
) -> Result<ValidatedAdapterName, UnknownAdapter>;
```

`ValidatedAdapterName` has **no public constructor** other than the
return of `validate_adapter_name`. The tuple field is private; there
is no `impl From<String>`, no `pub fn new`, no `Default`. The crate
boundary completes the seal: even within `cosmon-core`, no module
outside `spawn_seam` can mint a value.

### 2. The dispatch-site signature

The Worker-Spawn Port dispatch site (today
`cosmon_cli::cmd::tackle::spawn_and_prompt`) takes
`adapter: &ValidatedAdapterName` rather than `adapter_name: &str`.
`register_tackle_worker` — the single writer of
`WorkerSpawned.adapter_name` to `events.jsonl` — takes the same type
and emits via `adapter.as_str()`. Together these two signatures make
"spawn called without validation" a compile error, in any call site,
present or future.

### 3. Two distinct invariants, both preserved

The runtime `match adapter.as_str()` inside `spawn_and_prompt`
remains, with the C8 catch-all reframed as a registry-completeness
guard:

```rust
match adapter.as_str() {
    "claude" => …,
    "aider"  => …,
    other    => Err("registered in registry but not wired here — build-time bug"),
}
```

TS-0 enforces *validation-before-spawn* at compile time. The C8 match
arm enforces *registry-completeness-vs-dispatch-table* at runtime —
fires when a new adapter is added to the registry but no branch is
added here. These are different invariants. Keep both. The catch-all
is now unreachable from in-tree callers (TS-0 has eliminated the only
path to it from a literal `&str`), but it documents the contract for
future adapter additions.

### 4. Permanent contract

> *No future addition to the cosmon tackle chain — a new Adapter, a
> dispatcher refactor, an external orchestrator over MCP, the
> resident runtime's adoption of the port, a test-only entry point —
> may bypass adapter validation without breaking the type system.*

This sentence is the load-bearing one. It is the reason
`ValidatedAdapterName` has no escape hatch (no `unsafe_validated_for_testing`,
no `#[cfg(test)] pub fn new`); the test helper in `cmd::tackle::tests`
calls `validate_adapter_name` like any other caller. If a future PR
proposes such an escape hatch, that PR is a regression on this ADR and
requires a successor ADR before it lands.

### 5. Compile-fail witnesses

`spawn_seam.rs` ships two `compile_fail` doctests that prove the
contract via the type checker:

```rust
/// ```compile_fail
/// use cosmon_core::spawn_seam::ValidatedAdapterName;
/// let _bad = ValidatedAdapterName("claude".to_owned()); // private field
/// ```
///
/// ```compile_fail
/// use cosmon_core::spawn_seam::ValidatedAdapterName;
/// let _bad: ValidatedAdapterName = "claude".to_owned().into(); // no From<String>
/// ```
```

These are executable witnesses, not commentary: `cargo test --doc -p
cosmon-core` will fail to fail if either case compiles, which is the
exact signal we want when a future PR weakens the type.

---

## Consequences

**Direct.**

- `cosmon_cli::cmd::tackle::spawn_and_prompt` and
  `cosmon_cli::cmd::tackle::register_tackle_worker` accept
  `&ValidatedAdapterName` rather than `&str`. The b6d7 failure mode
  cannot be reproduced under any flag combination without first
  weakening the type.
- `cosmon_cli::cmd::resurrect` threads `validate_adapter_name("claude", …)`
  before its `spawn_and_prompt` call, applying the same gate to the
  resurrection path.
- The pre-existing `get_adapter_or_err` in
  `cosmon_transport::dispatch` is unchanged but is no longer the
  *validation* gate at the tackle call site; it remains the
  *resolution* gate (raw `&str` → `Box<dyn Spawn>`) for future
  trait-based dispatch (PR-4, ADR-098 §2).

**Downstream constraints.**

- A third Adapter (PR-3, ADR-098 §7) adds: (a) one entry to the
  registry composed at the tackle call site
  (`vec!["claude", "aider", "<new>"]`), (b) one `match` arm in
  `spawn_and_prompt`. Both additions are mechanical and the C8
  catch-all flags omissions of (b) at runtime; TS-0 guarantees (a)
  was applied before any spawn fires.
- The Spawn trait extraction (PR-4, ADR-098 §2) refactors the
  `match` body but cannot change the seam signature without first
  weakening this ADR.
- The resident runtime (ADR-016, ADR-095) inherits the seam: when it
  invokes `cs evolve`-equivalent paths that reach the Worker-Spawn
  Port, it must thread `ValidatedAdapterName`, not raw strings.

**Cost.**

- ~30 lines of new code in `cosmon-core/src/spawn_seam.rs` (type,
  validator, error, doctests, tests).
- ~10 lines of mechanical signature change in `cmd::tackle.rs` and
  `cmd::resurrect.rs`.
- One test rewrite (`test_spawn_and_prompt_rejects_unknown_adapter`
  → `test_validate_adapter_name_rejects_ghost`), unchanged semantics
  at a different layer.
- Zero runtime cost (the typestate is erased at compile time;
  `as_str` is a borrow).

---

## L9 framing

C8 was the local fix to b6d7: the runtime route now does what the
journal says. TS-0 is the *universal* fix: the next time the same
class of bug tries to land — under a different Adapter, a different
flag, a different call path — it does not compile. The journal-vs-result
divergence (*« le journal disait aider, la pane montrait Claude »*)
becomes a category of error the type system refuses to admit.

This is the cosmon stance on silent failure: *propose mechanisms of
verification, do not impose them* (invariants §8b), unless the
mechanism is free at runtime and lossless at compile time — at which
point it stops being a mechanism and becomes a contract.

**Tattoo.** *La validation est dans le type, pas dans la diligence.*

---

## Amendment — `task-20260531-c99e` (2026-05-31): env + global preference tiers

Q5a (the adapter-default resolution chain, `task-20260530-c089` /
`delib-20260530-0877`) has no standalone ADR; it lives in the dispatch
site this ADR governs, so the order amendment is recorded here. The chain
gains two operator-preference tiers, **`$COSMON_DEFAULT_ADAPTER`** and a
**global `~/.config/cosmon/config.toml::[adapters.default]`**. Chosen
order (highest first): `--adapter` flag → formula-step pin →
`$COSMON_DEFAULT_ADAPTER` → per-galaxy config → global config →
built-in `"local"` floor. **Justification of the env slot:** the env var
ranks *above both config files* because it is the operator's explicit
"right now, everywhere" intent, but *below the formula-step pin* because a
step pinning `adapter = "claude"` expresses a correctness need (frontier
reasoning) that a blanket session preference must not silently override.
The global file ranks *below* the per-galaxy config so a committed project
choice always beats the uncommitted machine-wide one. The built-in
`"local"` floor is untouched (no env, no per-galaxy default, no global
default still resolves to `local`). Provenance stays honest: the
`adapter_selected` event gains `EnvVar` / `GlobalConfig`
`selection_source` variants rather than mislabelling either as
`config`/`default`.
