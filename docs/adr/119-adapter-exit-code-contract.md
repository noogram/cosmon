# ADR-119 — Structured Adapter Exit-Code Contract

**Status:** proposed
**Date:** 2026-06-05
**Decider:** Noogram
**Authoring task:** `task-20260427-2435`
**Source finding:** `caae` experiment (`task-20260426-caae`),
experiment-report §6 finding #5 — *cosmon primitive #5*.

**Binds:**
[ADR-079](079-worker-spawn-port-and-adapter-contract.md) (the Worker-Spawn
Port and its four Adapter obligations — this ADR adds the **fifth**),
[ADR-099/103/104](079-worker-spawn-port-and-adapter-contract.md) (the
per-Adapter typed axes in `cosmon_core::spawn_seam`, which this primitive
sits beside as a per-Adapter contract).

**Architectural invariants:** `docs/architectural-invariants.md` §8j
(every Port is a typed ingress binding).

---

## Context

The `caae` experiment ran cosmon's worker-spawn pipeline against the
`codex` Adapter and recorded (finding #5) a structural blindness: **`codex
exec` returns exit `1` indistinctly** for two failures that demand opposite
responses —

- a **malformed prompt** (the operator's brief is wrong; retrying replays
  the same mistake — *escalate*), and
- an **over-quota refusal** (transient; the identical brief succeeds after a
  backoff — *wait and retry*).

A supervisory recovery loop (`cs patrol`) that observes only the integer
`1` cannot distinguish them. It must therefore either escalate every stall
(burying recoverable quota waits under operator noise) or retry every stall
(burning budget on unrecoverable user errors). Both are wrong. The recovery
loop is **structurally impossible** until the exit signal carries a typed
meaning rather than a vendor-private integer.

This is not a `codex` bug to patch — it is a missing cosmon primitive. Every
Adapter (`claude`, `aider`, future headless-API substrates) has its own raw
exit conventions; the *Port* has no shared vocabulary for "how did the
worker process end, and is it worth retrying?"

## Decision

Cosmon commits a **five-class exit-code contract** as the output alphabet
every worker-spawn Adapter must speak. This is the **fifth Adapter
obligation**, extending ADR-079 §5.

### 1. The five classes (`AdapterExitClass`)

| Code | Class | Retryable | `RecoveryAction` |
|------|-------|-----------|------------------|
| `0`  | `Ok`              | —   | `None` |
| `1`  | `UserError`       | no  | `EscalateToOperator` |
| `64` | `CredentialError` | no  | `FixCredentials` |
| `65` | `QuotaError`      | yes | `BackoffAndRetry` |
| `66` | `SpawnError`      | yes | `Respawn` |

The numeric values reuse the `sysexits.h` `64`–`78` band so an Adapter
realised as a **shell wrapper** can `exit 64` / `exit 65` / `exit 66` with
codes the shell will not clobber. The *meanings* are cosmon's, not BSD's;
the pairing is pinned by round-trip tests
(`cosmon_core::adapter_exit::tests::code_round_trips_through_from_code`).

`AdapterExitClass` is deliberately **not** `#[non_exhaustive]`: the
five-class alphabet *is* the contract. A sixth class changes `cs patrol`
recovery semantics and must break every downstream exhaustive match so the
change is reviewed — contrast `spawn_seam::LoopOwnership`, which *is*
`#[non_exhaustive]` because new loop owners do not change recovery.

### 2. Per-Adapter classification (`classify_exit`)

The mapping from a raw `(exit_code, stderr_tail)` to an `AdapterExitClass`
is **per-Adapter**, because the raw signal is per-Adapter. `classify_exit`
dispatches on the validated Adapter name with these rules, in order:

1. `None` (signal kill / never ran) → `SpawnError`.
2. `Some(0)` → `Ok`.
3. `Some(64|65|66)` → **trusted verbatim** — an Adapter that already speaks
   the contract (a shell wrapper, a structured Adapter) is believed without
   stderr inspection. This is the "do the mapping yourself and
   short-circuit" path.
4. otherwise (the ambiguous band — `Some(1)` and any other non-zero) →
   scan `stderr_tail` for **quota** then **credential** markers; on a hit
   return that class, else fall back to `UserError` (conservative,
   non-retryable — never silently retry an uncharacterised failure).

Quota is scanned before credential because a `429` body sometimes also
mentions "key", and the recoverable quota stall is the one the `caae`
finding most wants rescued. The `codex` branch widens the quota marker set
(`usage limit`, `529`, `capacity`) — that is the entire point of dispatching
per-Adapter.

### 3. The diagnostic loop (`to_patrol_action`)

`AdapterExitClass::to_patrol_action(worker_id)` projects a verdict onto a
`cosmon_core::patrol::PatrolAction`: retryable classes become
`RestartWorker`, non-retryable classes become `AlertHuman` carrying the
code and reason, `Ok` becomes `NoAction`. This is the concrete unblock the
finding asked for — `cs patrol` reads one typed verdict and picks the right
recovery, with **no stderr string-matching at the patrol layer** (the
Adapter boundary already did it once).

## What this ADR does *not* do

- It does **not** wire `classify_exit` into the live spawn-site supervisor
  or `cs patrol` command yet — that is a follow-up consuming this primitive.
  The primitive lands first (pure, zero-I/O, fully tested) so the contract
  exists for any consumer to call.
- It does **not** rename or migrate the existing `harness` prose word
  (ADR-079 §2 governs that). "Per harness" in the finding text maps to "per
  Adapter" in code.
- It does **not** make Adapters *enforce* the mapping at the type level (no
  `Spawn` trait method). Enforcement today is the contract + the
  classification helper; a typed obligation can follow if a future Adapter
  is added that silently skips it.

## Consequences

- `cs patrol` can finally tell a recoverable quota stall from a dead-on-
  arrival user error — the supervisory loop the finding said was impossible
  becomes a pure function call.
- New Adapters declare their disambiguation by adding a branch to
  `stderr_signals_quota` / `stderr_signals_credential`, or by emitting the
  canonical `64/65/66` codes directly and being trusted.
- The five-class alphabet is a hard contract: widening it is ADR-grade,
  surfaced by compile errors on every exhaustive match.

## References

- `crates/cosmon-core/src/adapter_exit.rs` — the primitive (this ADR's
  implementation).
- [ADR-079](079-worker-spawn-port-and-adapter-contract.md) §5 — the four
  prior Adapter obligations this ADR extends to five.
