# ADR-021: Principal Separation — Caller vs Worker in the MCP Surface

## Status
Proposed (2026-04-10)

## Context

Cosmon's MCP surface exposes tools (`cosmon_nucleate`, `cosmon_evolve`,
`cosmon_observe`, `cosmon_complete`, `cosmon_collapse`) that mutate
molecule state. Today, every MCP caller is treated identically: there
is no distinction between a caller that _created_ a molecule (the
**requester**) and the worker that was dispatched by `cs tackle` to
_execute_ that molecule's formula steps (the **executor**). Both speak
the same JSON-over-stdio protocol, both see the same tool surface,
and neither carries a credential that proves its role.

Deliberation `delib-20260409-915a` named this defect **principal
conflation** (wheeler's term): the LLM caller and the worker executor
are distinct principals but indistinguishable in the MCP transport.
The synthesis converged from five independent angles:

- **Wheeler** reframed the symptom ("agent attacks molecule itself")
  as a mechanism: principal conflation. Two principals — the caller
  who nucleated the molecule and the worker dispatched by `cs tackle`
  — share the same undifferentiated MCP channel. Without a credential,
  the server cannot tell them apart.
- **Architect** identified that fleet registration (performed by
  `cs tackle`) _is_ the worker principal's credential. `cs tackle`
  creates a `WorkerData` entry in `fleet.json` with a `WorkerId`
  bound to a specific molecule, worktree, and tmux session. This
  registration is the proof of dispatch — the capability token that
  should distinguish a worker from an arbitrary caller.
- **Karpathy** showed that the anti-pattern (caller self-executing
  formula steps) is a _default-prior problem_: an LLM with a step
  list and an `Agent()` tool will always pick the shortest path. Docs
  cannot fix a verb whose shortest-path reading points to
  self-execution; the schema must block the shortcut.
- **Jobs** demanded that `cosmon_nucleate` hide raw formula steps
  entirely, returning only `next_action: "cs tackle <id>"`. The
  caller should never see a recipe it might execute.
- **Torvalds** confirmed the code-level shape: `cs tackle` already
  registers the worker in `fleet.json` via `register_tackle_worker`,
  binding a `WorkerId` to a `MoleculeId`. The credential _exists_;
  it is simply not checked on the MCP surface.

### The anti-pattern

Without principal separation, the following scenario recurs:

1. An LLM session calls `cosmon_nucleate(formula: "deep-think")`.
2. The response (or the formula file) reveals the formula steps.
3. The LLM reads the steps as a recipe and begins executing them
   inline — calling `Agent()` to spawn personas, editing files,
   calling `cosmon_evolve` — all within its own session.
4. This work happens in the wrong process, outside the worktree
   guardrail, and conflicts with (or duplicates) the real worker
   that `cs tackle` would have dispatched.

The root cause is not the LLM's "eagerness" — it is the absence
of a mechanism that distinguishes the caller from the worker. The
MCP surface treats both identically, so neither the server nor the
LLM can enforce the boundary.

### Prerequisites

This ADR presupposes [ADR-020](020-mcp-project-agnostic-cwd-per-call.md)
(MCP Server is Project-Agnostic; `cwd` is Per-Call). Principal
separation builds on stable `cwd` semantics because the worker's
credential is resolved _within_ the project identified by the
caller's `cwd`. Without ADR-020, the server cannot reliably locate
the fleet registry that contains the worker's registration.

## Decision

### 1. Two principals, one transport

Every MCP tool invocation involves exactly one of two principals:

| Principal | Identity source | Example |
|-----------|----------------|---------|
| **Caller** | Unauthenticated. Any LLM session, script, or human that connects to the MCP server. | A Claude session that runs `cosmon_nucleate` to create a molecule. |
| **Worker** | Fleet-registered. Holds a `WorkerId` that `cs tackle` wrote into `fleet.json`, bound to a specific `MoleculeId`. | The Claude session spawned by `cs tackle mol-123` inside a dedicated worktree and tmux pane. |

The distinction is _not_ about trust or authorization in the
security sense. Both principals are local processes on the same
machine. The distinction is about **role**: a caller requests work;
a worker executes it. Conflating the two causes the anti-pattern
described above.

### 2. The credential: `COSMON_WORKER_ID`

The worker principal's credential is the `COSMON_WORKER_ID`
environment variable, set by `cs tackle` when it spawns the
worker's Claude session. This variable already exists in the
codebase (`cosmon-mcp/src/tools.rs:939`) but is used only for
cognitive-state persistence, not for principal identification.

The credential chain is:

1. **`cs tackle <mol-id>`** creates a worktree, a tmux session,
   and a fleet entry (`WorkerData` in `fleet.json`) binding the
   `WorkerId` to the `MoleculeId`.
2. **`cs tackle`** exports `COSMON_WORKER_ID=<worker-id>` into
   the tmux session's environment.
3. The worker's Claude session inherits `COSMON_WORKER_ID`. When
   it calls MCP tools, the MCP server reads this variable from
   the caller's environment (or from an explicit `worker_id`
   parameter — see §3).
4. The server looks up the `WorkerId` in `fleet.json` and
   verifies that the worker is registered, active, and bound to
   the molecule being mutated.

A caller that was _not_ spawned by `cs tackle` does not have
`COSMON_WORKER_ID` in its environment and therefore cannot present
a worker credential. It is, by construction, a caller principal.

### 3. `worker_id: Option<String>` on worker-callable tools

MCP tools that perform worker-only mutations MUST accept an
optional `worker_id` parameter:

```rust
pub struct EvolveParams {
    /// The molecule to advance.
    pub id: String,
    /// Caller's working directory (per ADR-020).
    pub cwd: Option<String>,
    /// Worker credential. When set, the server verifies that this
    /// WorkerId is fleet-registered and bound to the target molecule.
    /// When absent, the server falls back to `COSMON_WORKER_ID` from
    /// the environment, then treats the caller as a non-worker.
    pub worker_id: Option<String>,
}
```

The resolution precedence for worker identity:

1. **Explicit `worker_id` parameter.** If the tool call includes
   `worker_id: Some("cosmon-mol-123")`, the server uses it directly.
   This path is for programmatic callers (scripts, the future
   resident runtime) that know their worker identity.
2. **`COSMON_WORKER_ID` environment variable.** If `worker_id` is
   `None`, the server reads `std::env::var("COSMON_WORKER_ID")`. This
   is the default path for `cs tackle`-spawned Claude sessions, which
   inherit the variable automatically.
3. **No credential.** If neither is present, the caller is treated
   as a caller principal with no worker role.

### 4. Tool classification by principal

Every MCP tool falls into one of three categories:

| Category | Principal required | Examples | Behavior without credential |
|----------|-------------------|----------|---------------------------|
| **Caller-only** | Caller (any) | `cosmon_nucleate`, `cosmon_list`, `cosmon_search`, `cosmon_stats`, `cosmon_get` | Normal operation. |
| **Worker-required** | Worker (fleet-registered) | `cosmon_evolve` | Reject with error: `"cosmon_evolve requires a fleet-registered worker. Use cs tackle <id> to dispatch a worker."` |
| **Worker-preferred** | Either, but response varies | `cosmon_observe`, `cosmon_complete`, `cosmon_collapse` | Caller sees summary + `next_action`; worker sees full step detail + state. |

The classification follows the command perimeters defined in
`docs/architectural-invariants.md`:

- **`cosmon_evolve`** advances a molecule along its formula. Only a
  worker dispatched by `cs tackle` — with its worktree, tmux session,
  and fleet registration — should perform this mutation. A caller
  evolving a molecule it nucleated is the principal-conflation
  anti-pattern. This tool is **worker-required**.

- **`cosmon_nucleate`** creates a molecule. This is a caller action
  by definition (the caller is the requester). This tool is
  **caller-only**. It MUST NOT return raw formula steps; it returns
  `next_action: "cs tackle <id>"`.

- **`cosmon_observe`** reads molecule state. Both principals may
  observe, but the response is shaped by principal:
  - **Worker** sees the full step list, current step body, evidence
    log, and formula details — everything needed to execute.
  - **Caller** sees status, kind, title, and `next_action` guidance.
    Formula steps are either omitted or wrapped in a `principal:
    "worker"` envelope that marks them as belonging to the worker.

- **`cosmon_complete`** and **`cosmon_collapse`** are terminal
  transitions. Workers call them to signal they are done (or stuck).
  Callers may also call them to force-complete or force-collapse a
  molecule that is stuck. Both principals are accepted; the fleet
  entry is cleaned up either way. These are **worker-preferred**.

### 5. Enforcement mechanism

When a **worker-required** tool receives a call:

1. Resolve the `WorkerId` per §3 precedence.
2. If no `WorkerId` is found → reject with an error message that
   names the mechanism and the remedy (`cs tackle`).
3. If a `WorkerId` is found → look up the `WorkerData` in
   `fleet.json` (resolved via `cwd` per ADR-020).
4. If the `WorkerId` is not registered, or its `current_molecule`
   does not match the target molecule → reject with error.
5. If the `WorkerData.status` is not `Active` → reject with error.
6. Proceed with the mutation.

This is a _capability check_, not an authentication protocol. The
`WorkerId` is not a secret — it is a coordination token that proves
the caller was dispatched by `cs tackle` and is bound to the
molecule it claims to be working on. The threat model is not
adversarial; it is _structural_: preventing well-intentioned but
confused LLM sessions from mutating molecules they do not own.

### 6. The `principal` envelope on observe responses

When `cosmon_observe` returns molecule state, the response carries
a `principal` field that marks whose perspective the data represents:

```json
{
  "molecule_id": "task-20260409-6c72",
  "status": "active",
  "principal": "caller",
  "next_action": {
    "command": "cs tackle task-20260409-6c72",
    "why": "This molecule requires a fleet-registered worker."
  }
}
```

vs. a worker-principal response:

```json
{
  "molecule_id": "task-20260409-6c72",
  "status": "active",
  "principal": "worker",
  "current_step": {
    "index": 1,
    "title": "Implement the solution",
    "body": "Do the work. Read the project's CLAUDE.md for conventions.",
    "exit_criteria": "Implementation complete, compiles clean"
  },
  "steps_remaining": 1,
  "evidence": ["..."]
}
```

The `principal` field is informational, not a security boundary. Its
purpose is to shape the LLM's behavior: a caller-principal response
does not contain a step list, so the LLM cannot self-execute. A
worker-principal response contains the step list because the worker
_is_ the executor.

### 7. What this ADR explicitly does NOT decide

- **MCP-level authentication or session tokens.** MCP does not
  model sessions or connections. This ADR uses environment variables
  and optional parameters as the credential transport, not a new
  protocol extension.
- **Worker self-destruction.** Workers cannot call `cs done` (which
  tears down the worktree, kills the tmux session, and purges the
  fleet entry). `cs done` remains human-only per `architectural-
  invariants.md`. This ADR does not change that boundary.
- **Automatic dispatch (auto-tackle on nucleate).** Rejected by the
  delib-915a panel (4–1). `cosmon_nucleate` returns `next_action`
  guidance; it does not spawn a worker. The caller–worker boundary
  is the _reason_ auto-dispatch was rejected.
- **The resident runtime's principal model.** When the resident
  runtime (ADR-016 Phase 3+) owns Autonomous-regime molecules, it
  will be a new principal type. This ADR covers Inert and Propelled
  regimes only. The Autonomous principal is deferred to the runtime
  ADR.
- **Splitting `cosmon-mcp` into core + runtime.** The P3 work
  (`cosmon-mcp-core` for Inert tools, `cosmon-mcp-runtime` for
  Propelled tools) is a natural follow-on but is scoped separately.

## Consequences

**Positive:**

- The "agent attacks molecule itself" anti-pattern becomes a
  type-enforced invariant, not a social rule. `cosmon_evolve` rejects
  callers who do not hold a fleet-registered `WorkerId`. The LLM
  literally cannot evolve a molecule it did not `cs tackle`.
- The principal model aligns with ADR-016's regime boundaries:
  Inert-regime tools (nucleate, list, search) are caller-accessible;
  Propelled-regime tools (evolve) require a worker credential
  derived from `cs tackle`'s fleet registration.
- The `principal` envelope on `cosmon_observe` solves the visibility
  tension from the deliberation: steps are hidden from callers (who
  would self-execute them) but visible to workers (who need them).
  Both jobs's "hide everything" and architect's "expose with
  marking" are satisfied.
- The credential mechanism is zero-cost for workers: `cs tackle`
  already sets `COSMON_WORKER_ID` and registers in `fleet.json`.
  No new ceremony is added to the worker's path. The enforcement
  is server-side, not client-side.
- The mechanism is forward-compatible with the resident runtime.
  When `cs run <dag>` (ADR-016 Phase 3+) dispatches workers, it
  will register them in the fleet with the same `WorkerData`
  structure, and the same credential check applies.

**Negative:**

- Programmatic callers (scripts, external tools) that call
  `cosmon_evolve` today without a `WorkerId` will be rejected after
  enforcement lands. The migration path is: either use `cs tackle`
  to obtain a credential, or set `COSMON_WORKER_ID` explicitly.
  This is a deliberate breaking change — those callers were
  exercising the principal-conflation anti-pattern.
- The `COSMON_WORKER_ID` environment variable is not a strong
  credential. Any process in the same tmux session or shell
  environment can read it. This is acceptable because the threat
  model is structural (preventing confused LLMs), not adversarial
  (preventing malicious access). If adversarial isolation is ever
  needed, the credential should be upgraded to a nonce stored in
  `fleet.json` and verified per-call.
- Adding `worker_id: Option<String>` to tool parameter structs
  increases the MCP surface area. However, the parameter is
  optional and auto-resolved from the environment in the common
  case, so workers do not need to send it explicitly.

**Neutral:**

- This ADR does not change the CLI. `cs evolve`, `cs complete`,
  and `cs collapse` on the CLI already run inside the worker's
  environment (where `COSMON_WORKER_ID` is set by `cs tackle`).
  The enforcement applies to the MCP surface, which is the channel
  where principal conflation occurs.
- The `principal` field on observe responses is additive. Existing
  clients that ignore it are unaffected; they simply do not benefit
  from principal-shaped visibility.

## Implementation Sequence

1. **Phase 1: `cosmon_evolve` rejects non-workers.** Add `worker_id:
   Option<String>` to `EvolveParams`. Resolve via §3 precedence.
   Verify against `fleet.json`. Return error if unregistered. This
   is the minimum enforcement that blocks the anti-pattern.
2. **Phase 2: `principal` envelope on `cosmon_observe`.** Add
   principal detection to the observe handler. Shape the response
   based on whether the caller is a worker (full steps) or not
   (summary + `next_action`).
3. **Phase 3: Classification sweep.** Audit every MCP tool against
   the three-category table in §4. Add `worker_id` to worker-
   required and worker-preferred tools. Update MCP INSTRUCTIONS.
4. **Phase 4: Upgrade to nonce credential (optional, future).**
   If the environment-variable credential proves insufficient,
   generate a random nonce in `cs tackle`, store it in `fleet.json`,
   and verify it per-call. This is a strictly stronger mechanism
   that replaces `COSMON_WORKER_ID` with a secret token.

## References

- `delib-20260409-915a` — the deep-think deliberation that named
  principal conflation and produced the ranked fix list. This ADR
  is item P2-021 in that list.
- [ADR-020: MCP Server is Project-Agnostic; cwd is Per-Call](020-mcp-project-agnostic-cwd-per-call.md)
  — the prerequisite ADR. Principal separation presupposes stable
  `cwd` semantics because the fleet registry is resolved via the
  caller's `cwd`.
- [ADR-016: Autonomy Regimes and the Resident Runtime](016-autonomy-regimes-and-resident-runtime.md)
  — defines the Inert / Propelled / Autonomous regime model. The
  principal categories in §4 map to regime boundaries: caller tools
  are Inert-safe; worker tools are Propelled-regime.
- [`docs/architectural-invariants.md`](../architectural-invariants.md)
  — command perimeters, the worker/human boundary, and the coherence
  checklist. This ADR enforces the "worker-callable = no self-
  destroy" invariant at the MCP layer.
- [`crates/cosmon-cli/src/cmd/tackle.rs`](../../crates/cosmon-cli/src/cmd/tackle.rs)
  — `register_tackle_worker`, the function that writes `WorkerData`
  into `fleet.json`. This is the credential issuance point.
- [`crates/cosmon-mcp/src/tools.rs`](../../crates/cosmon-mcp/src/tools.rs)
  — the MCP tool surface where enforcement will be added. Line 939
  already reads `COSMON_WORKER_ID` for cognitive-state persistence;
  this ADR promotes that variable to a principal credential.
- [`crates/cosmon-core/src/worker.rs`](../../crates/cosmon-core/src/worker.rs)
  — `WorkerData`, `WorkerId`, `WorkerStatus` — the domain types
  that model the worker principal.
