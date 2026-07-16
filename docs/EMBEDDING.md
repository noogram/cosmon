# Embedding Cosmon in a Project

**Status.** Contract document. Normative.

**Scope.** How any project — another Rust crate, a Python research lab, a
LaTeX repo, a shell-only workspace — consumes cosmon as the orchestration
substrate for its molecules. This is the per-project view, not the
cosmon-repo-developing-cosmon view.

**Governing ADRs.**
- [ADR-016 — Autonomy Regimes and the Resident Runtime](adr/016-autonomy-regimes-and-resident-runtime.md)
- ADR-020 — *MCP Server is Project-Agnostic; cwd is Per-Call* (see
  [`task-20260409-6cac`](../.cosmon/state/fleets/default/molecules/task-20260409-6cac/))
- Architectural invariants: [`docs/architectural-invariants.md`](architectural-invariants.md)

**Motivating synthesis.**
[`delib-20260409-915a`](../.cosmon/state/fleets/default/molecules/delib-20260409-915a/synthesis.md)
— the five-persona panel that re-affirmed substrate identity and named the
anti-pattern as *principal conflation* rather than missing documentation.

This document is a contract: the behavior described here is the behavior
cosmon must guarantee for embedding projects. If code disagrees with this
document, either the code is wrong and must be patched, or this document
is wrong and must be amended with a successor ADR. Drift is not acceptable
in either direction.

---

## Table of Contents

1. [Who this document is for](#1-who-this-document-is-for)
2. [Two consumption modes](#2-two-consumption-modes)
3. [Path resolution contract](#3-path-resolution-contract)
4. [`cwd` parameter schema and error modes](#4-cwd-parameter-schema-and-error-modes)
5. [Regime availability matrix](#5-regime-availability-matrix)
6. [Principal model: caller vs worker](#6-principal-model-caller-vs-worker)
7. [Formula distribution, builtins, and walk-up](#7-formula-distribution-builtins-and-walk-up)
8. [The agent-must-not-self-execute rule (with worked example)](#8-the-agent-must-not-self-execute-rule-with-worked-example)
9. [Surface sync in embedded mode](#9-surface-sync-in-embedded-mode)
10. [Upgrade and versioning](#10-upgrade-and-versioning)

---

## 1. Who this document is for

Read this if you are one of:

- **A project owner** who wants cosmon to track the work happening in
  their repo — ideas, tasks, decisions, issues, deliberations — without
  vendoring cosmon's source or turning the project into a cosmon
  subdirectory.
- **An agent author** writing an LLM-driven workflow on top of the cosmon
  MCP server and trying to decide whether the client should execute formula
  steps inline (it should not; see §8).
- **A library author** evaluating `cosmon-embed` (the Inert-only facade
  crate) for use inside a tool, a test harness, or a CI job that needs to
  nucleate and observe molecules but has no business running worker
  processes.
- **A platform maintainer** integrating cosmon alongside other agent
  runtimes (Gas Town, claude-code, external LLM planners) and needing to
  understand which parts of the API are cross-project safe.

You do **not** need this document if you are:

- Developing cosmon itself from the cosmon repo (use `docs/CONTRIBUTING.md`).
- Using cosmon purely interactively via `cs` from the project root (the
  `--help` output and `THESIS.md` cover that case).

The contract below is what you rely on when cosmon is one dependency
among many and the project that embeds it may outlive or pre-date any
particular cosmon release.

---

## 2. Two consumption modes

Cosmon supports exactly two embedding modes. There is no in-between:
pick one and commit.

### Mode A — `cosmon-embed` crate, Inert-only

**What it is.** A small facade crate that re-exports the Inert-regime
operations from `cosmon-core`, `cosmon-state`, `cosmon-filestore`, and
`cosmon-graph`. No `cosmon-transport`, no fleet provisioning, no
`cs tackle`, no resident runtime.

**When to use it.** When the embedding project needs to create, read,
evolve, complete, and collapse molecules *in-process* — as a library,
under the project's own scheduler — and does not need cosmon to spawn
worker processes.

Typical consumers:

- Test harnesses that assert on molecule state transitions.
- CI jobs that nucleate a tracking issue and complete it when their
  pipeline finishes.
- Research notebooks that log deliberations as molecules without needing
  tmux sessions.
- Third-party schedulers (Oxymake, Burr, Hamilton) that run the actual
  work and use cosmon only as a state store.

**How to depend on it.**

```toml
# Cargo.toml
[dependencies]
cosmon-embed = "0.X"   # see §10 for version policy
```

**What you get.** The Inert surface: `nucleate`, `observe`, `evolve`,
`complete`, `collapse`, `freeze`, `thaw`, `decay`, `merge`, `transform`,
plus state-store access via `cosmon-filestore`. Every call is a pure,
synchronous, idempotent operation against `.cosmon/state/`.

**What you do not get.** `tackle`, `done`, `patrol`, `spawn`,
`resume`, `rolling_restart`, any fleet lifecycle command, or any
transport backend. These belong to the Propelled and Autonomous regimes;
see §5. An embedding project that needs them must run the full `cs` CLI
(or the MCP server) as an external tool, which is Mode B.

### Mode B — Globally-installed `cs` + cosmon MCP server + project `.cosmon/`

**What it is.** A single globally-installed `cs` binary (for humans on
the command line) and/or a single globally-running cosmon MCP server (for
agents), each of which reaches into *any* project's `.cosmon/` directory
at invocation time. The project itself holds only its `.cosmon/`
directory and — optionally — its own formulas.

**When to use it.** Whenever you want the full regime (Inert → Propelled
→ Autonomous) available for the project, need `cs tackle` to spawn
workers, or want agents running against the MCP surface to operate on
the project's state without the project having to vendor cosmon.

Typical consumers:

- Any repo where humans use `cs` interactively.
- Any repo where an LLM agent talks to a long-running cosmon MCP server.
- The cosmon repo itself (which is the degenerate case: `caller_cwd ==
  server_launch_cwd`).

**How it is installed.** One cosmon install per host. The `cs` binary and
the MCP server share the same release version. The project has no cosmon
source in its dependency graph.

**How it reaches the project.** Per-invocation path resolution, governed
by §3 and parameterized by §4. The short version: `cs` walks up from its
own CWD; the MCP server walks up from a `cwd` parameter supplied by each
tool call.

### Mode selection rule

| You need... | Use... |
|-------------|--------|
| In-process molecule creation from Rust, no spawning | Mode A (`cosmon-embed`) |
| `cs tackle` or the MCP server | Mode B (globally installed) |
| Both, simultaneously | Mode B — Mode A is a subset |
| A per-project cosmon daemon | Neither. Prohibited by architectural invariants. |

The two modes are not mutually exclusive, but Mode B is strictly more
powerful. If you depend on `cosmon-embed` *and* run `cs` alongside, you
are already in Mode B; the embed crate merely spares you a subprocess
round-trip for the Inert operations.

---

## 3. Path resolution contract

Every cosmon operation — CLI, MCP, or embedded — must resolve three
directories before it can do useful work:

| Directory | Purpose |
|-----------|---------|
| `state_dir` | The on-disk source of truth. `.cosmon/state/` by default. |
| `formulas_dir` | Where formula TOMLs are looked up by name. `.cosmon/formulas/` by default. |
| `fleet_dir` | A subdirectory of `state_dir` that holds per-fleet molecule data. |

All three must resolve to paths that belong to the **same project**.
Crossing a project boundary within a single call is a bug: it means the
caller's state is being written into a different project's
`.cosmon/` or vice versa.

### Ordered fallback (normative)

For each of the three directories, the resolver attempts these sources in
order and returns the first one that yields a usable path:

1. **Explicit override.** A caller-supplied path:
   - CLI: `--config` / `--formulas-dir` / `--state-dir` flags.
   - MCP: today, `cwd` parameter (which becomes the walk-up origin, see
     step 3 below). An explicit state- or formulas-dir parameter is
     reserved for future extension; the current API surface uses
     `cwd` as the single handle.
   - Embedded (`cosmon-embed`): the path you pass to the store
     constructor.
2. **Environment variable.** `COSMON_STATE_DIR` / `COSMON_FORMULAS_DIR`.
   These are intended for developer-local overrides and test harnesses;
   production deployments should prefer the explicit override.
3. **Walk-up discovery from a start directory.** The resolver walks
   upward looking for a `.cosmon/` directory, like `git` finds `.git/`.
   The start directory is:
   - The process CWD for `cs` CLI invocations.
   - The caller-supplied `cwd` parameter for MCP tool calls (§4).
   - The directory passed to `resolve_*_from()` in `cosmon-filestore`
     when called from `cosmon-embed` or custom Rust consumers.

   If the starting directory is inside a git worktree, the resolver
   detects this (via the `.git` *file* convention) and redirects to the
   main repo's `.cosmon/`. This is load-bearing: worktree checkouts
   contain a git-tracked `.cosmon/` shell that lacks the `state/`
   subdirectory, so walking up to the worktree root would dead-end. The
   main-repo redirect makes `cs` work identically from a worktree and
   from the main checkout. See
   `crates/cosmon-filestore/src/resolve.rs::resolve_worktree_main_cosmon`.
4. **Global fallback.** `$HOME/.cosmon/state` and `$HOME/.cosmon/formulas`.
   Used when a caller has not installed cosmon in a project and wants a
   cross-project scratchpad. Safe as a last resort; dangerous as a
   primary target for agent writes because it silently crosses project
   boundaries.

### Rules the contract imposes

- **The MCP server must not freeze the walk-up origin at startup.** The
  server's own launch CWD is irrelevant to the caller's project. If the
  server resolves paths against `std::env::current_dir()` at `new()`
  time and never re-resolves, every call from a different project
  silently writes to the wrong state store. This was the root cause
  named by every persona in `delib-20260409-915a` and is the reason
  `resolve_formulas_dir_from(start: &Path)` exists.
- **`cs` walks up from its own process CWD and that is correct.** The
  CLI is a short-lived process that inherits the user's intent from
  their shell; the walk-up origin is *already* caller-supplied.
- **Walk-up must stop at the first `.cosmon/` hit.** Ambiguity is a
  configuration bug, not a feature. If two projects nest `.cosmon/`
  directories, the inner one wins. This is identical to `git`'s
  behavior and by design.
- **Cross-project calls never fall through silently.** If a caller
  asks for project X's state and the resolver would fall through to
  the global fallback because X has no `.cosmon/`, that is a
  configuration error and the call should fail loudly, not write into
  `$HOME/.cosmon/`.

### Implementation entry points

| Function | Role |
|----------|------|
| `cosmon_filestore::resolve_state_dir(explicit)` | Process-CWD walk-up; used by `cs`. |
| `cosmon_filestore::resolve_formulas_dir(explicit)` | Same, for formulas. |
| `cosmon_filestore::resolve_formulas_dir_from(start)` | Walk-up from an arbitrary start; used by the MCP server when the client supplies `cwd`. |
| *(reserved)* `resolve_state_dir_from(start)` | Future: extend the per-call semantics to state-dir lookups on MCP tools that mutate state directly. Currently absent; see §10. |

---

## 4. `cwd` parameter schema and error modes

This section is the client-facing contract for the `cwd` parameter on
MCP tool calls. Read it before wiring an agent to cosmon from a new
project.

### Schema

```jsonc
{
  "cwd": {
    "type": ["string", "null"],
    "description": "Caller's working directory. When set, the MCP server resolves the formulas directory (and in future: state directory) by walking up from this path instead of the server's own launch CWD. Absolute or relative; relative paths are resolved against the server's CWD."
  }
}
```

- **Type.** Optional string. `null` or absent means *fall through to the
  server's startup-time default*.
- **Presence.** Accepted on any MCP tool that needs project-scoped
  resolution. Today, `cosmon_nucleate` is the canonical carrier; the
  rest of the surface is expected to follow as `cwd` support is
  extended (tracked by the child molecules of `delib-20260409-915a`).
- **Backward compatibility.** A caller that does not know about `cwd`
  (existing agents, old clients) omits the field and continues to
  operate exactly as before. This is why the field is optional and not
  required — adding it cannot break existing callers.
- **Path form.** Absolute paths are strongly recommended. Relative paths
  are resolved against the server's CWD, which is almost never what the
  caller wants. An agent that hard-codes `"cwd": "."` is a bug.

### Error modes

When the server receives a `cwd` value, each of the following is a
distinct, observable failure:

| Situation | Behavior | Rationale |
|-----------|----------|-----------|
| `cwd` absent | Fall through to server default (legacy behavior). | Backward compat. |
| `cwd` present, walk-up finds a `.cosmon/` in that subtree | Use it. | The success path. |
| `cwd` present, walk-up does *not* find a `.cosmon/` | Fall through to global fallback. | Not an error today; the global fallback is a valid target. Callers that want strictness should validate `.cosmon/` existence before the call. |
| `cwd` present, path is not a directory | Walk-up proceeds from the nearest existing ancestor. | Permissive; mirrors git's behavior with typos. |
| `cwd` present, path traverses a symlink | Follow it. | No special handling; symlinks are part of the file system contract. |
| `cwd` present, but the resolved `.cosmon/` belongs to a different project than the one the caller *thinks* they are in | **Data integrity hazard.** | Not detectable by the server. The caller is responsible for passing a cwd inside its own project tree. |
| Formula lookup in the resolved dir misses | `formula not found: <name>` error, surfaced directly as an MCP `invalid_params` error. | Fast-fail, no silent fallback across projects. |

### Invariants the caller must preserve

1. **Pass `cwd` on every call that mutates project state** once the
   server supports it for that tool. The legacy fallback is only safe
   for single-project servers, and those are vanishing.
2. **Do not pass a cwd for a project that does not yet have a
   `.cosmon/`.** Bootstrap the project first (`cs init` from Mode B, or
   an explicit `cosmon-embed` state-dir constructor in Mode A).
3. **Do not mutate the working directory of the calling process between
   construction of the cwd string and the tool call.** The string is
   captured at send time; the server does not observe the caller's
   process-level CWD.
4. **Treat the cwd as a *naming* signal, not a *locking* signal.** The
   server uses it to name the project. It does not acquire any kind of
   exclusive lock; other callers and humans can simultaneously operate
   on the same project.

---

## 5. Regime availability matrix

This table mirrors the three regimes of
[ADR-016](adr/016-autonomy-regimes-and-resident-runtime.md) §2. It
answers the question "which operations are safe in which mode?" and
is the definitive reference for what `cosmon-embed` exposes and what it
refuses.

| Operation | Regime | Mode A (`cosmon-embed`) | Mode B (installed `cs` / MCP) |
|-----------|--------|-------------------------|-------------------------------|
| `nucleate` | Inert | ✅ | ✅ |
| `observe` | Inert | ✅ | ✅ |
| `complete` (from pending) | Inert | ✅ | ✅ |
| `collapse` | Inert | ✅ | ✅ |
| `freeze` / `thaw` | Inert | ✅ | ✅ |
| `decay` | Inert | ✅ | ✅ |
| `merge` / `transform` | Inert | ✅ | ✅ |
| `reconcile` (surface projection) | Inert | ✅ (pure) | ✅ |
| `evolve` (worker-internal) | Propelled | ❌ — no worker principal | ✅ — only from inside a worker |
| `complete` (from running) | Propelled | ❌ | ✅ |
| `tackle` | Inert → Propelled | ❌ — requires transport | ✅ — human only |
| `patrol --propel` | Propelled | ❌ — requires transport | ✅ — external scheduler only |
| `resume` / `done` / `stuck` | Propelled | ❌ | ✅ |
| `spawn` / `kill` / `purge` (workers) | Propelled | ❌ | ✅ |
| `cs run <dag>` | Autonomous | ❌ | 🛠 future (ADR-016 Phase 3) |
| LLM planner via MCP policy | Autonomous | ❌ | 🛠 future, external tenant |

**Reading the matrix.**

- **Inert row.** Every Inert operation is available in both modes. These
  are pure state transitions: they read the store, compute the next
  state, write it back, and exit. No process management, no transport,
  no scheduler assumptions. `cosmon-embed` exists precisely to expose
  this row as a library without dragging in the rest.
- **Propelled row.** These require a worker principal (see §6) and a
  transport (tmux, or a future replacement). `cosmon-embed` does not
  depend on `cosmon-transport` and therefore cannot offer them; an
  embedding project that needs `tackle` must invoke the installed `cs`
  binary as a subprocess or talk to the MCP server.
- **Autonomous row.** Nothing is available today. The resident runtime
  is ADR-016 Phase 3+, scheduled after `MoleculeLink::Blocks` and the
  native DAG scheduler (ADR-022) land. When it ships, it will be a
  *tenant* of Mode B, not a second embedding mode.

**The rule this matrix encodes.** An embedding project can always
*request* work (Inert) in-process. It must *delegate* the execution of
that work (Propelled / Autonomous) to an out-of-process cosmon or to an
external runner. The boundary between "request" and "execute" is the
principal boundary in §6.

---

## 6. Principal model: caller vs worker

The root cause of the "agent attacks molecule itself" anti-pattern is
**principal conflation**: the entity that asks for work and the entity
that does the work are two different principals, but the MCP transport
makes them look like one.

### Two principals, formally

| Principal | Who it is | What it does | How it is authenticated |
|-----------|-----------|--------------|--------------------------|
| **Caller** | The LLM session, human shell, or third-party tool that invokes an MCP tool or a `cs` command. | *Requests* work: nucleates molecules, observes state, collapses failures, reads surfaces. | Implicit — whoever holds the MCP transport or a shell. |
| **Worker** | A cosmon-managed process (today: a tmux session with an Agent SDK client) spawned by `cs tackle`. | *Executes* work: runs formula steps, reads briefings, emits evidence via `cs evolve`. | Registered in the fleet by `cs tackle`; identified by a `WorkerId` that is pinned in the fleet state. |

The caller's observation ("I see a molecule with steps 1–5") does *not*
grant it the right to execute those steps. The right to execute comes
from being the fleet-registered worker for that molecule, and that
registration is produced exclusively by `cs tackle`.

### Consequences for API shape

- **`cosmon_nucleate` hides the raw formula steps.** The MCP response
  carries `caller_role`, `plan_summary`, `next_action`, and `do_not`
  fields — but not a `steps: []` array. The deliberate absence
  prevents the caller from reading a step list and acting it out
  inline. See `crates/cosmon-mcp/src/tools.rs` around the
  `cosmon_nucleate` response builder for the current schema.
- **`next_action` on every creation response points to `cs tackle <id>`.**
  The only legitimate thing a caller does after creating a work molecule
  is ask a human (or an orchestrator outside cosmon) to tackle it.
- **`cosmon_evolve` is worker-callable only.** Future ADR-021 will
  formalize this as a capability check: an `evolve` call that cannot
  present a `WorkerHandle` derived from a prior `cs tackle` is refused.
  Today the rule is enforced by convention; tomorrow it will be
  enforced by the API shape.
- **`cs tackle` is never exposed as an MCP tool.** It is a human-only
  verb per the architectural invariants: the caller cannot, by
  design, spawn its own worker from within a nucleate response.
- **`cs done` is likewise human-only.** A worker cannot self-destroy;
  only the human (or an orchestrator with tear-down authority) can
  collapse the Propelled regime back to Inert.

### Quick check

When you are designing a new MCP tool, ask yourself: *is this operation
safe to expose to a caller that knows nothing about the worker
identity?* If the answer is no, the tool belongs behind a capability
check, not in the open MCP surface.

---

## 7. Formula distribution, builtins, and walk-up

Formulas are the scripts that tell workers what to do. An embedding
project can obtain them from any of three sources, resolved in this
precedence (highest wins):

1. **Project-local `.cosmon/formulas/`.** The normal case: each project
   keeps its own formula TOMLs under version control. Walk-up discovery
   (§3) finds them automatically.
2. **Environment / explicit override.** `COSMON_FORMULAS_DIR` or the
   `--formulas-dir` CLI flag points at a directory out-of-tree. Useful
   for testing a new formula in isolation.
3. **Compiled-in builtins.** The `cs` binary bundles a minimal set of
   formulas — currently `deep-think`, `task-work`, and `idea-to-plan`
   — via `include_str!`. `cs init` writes them to a fresh project's
   `.cosmon/formulas/` so that the first call to a builtin formula name
   succeeds without any extra setup. See
   `crates/cosmon-cli/src/cmd/init.rs::BUILTIN_FORMULAS`.

### The walk-up + builtins guarantee

For an embedding project that has never customized its formulas, the
canonical startup sequence is:

```sh
# From the project root:
cs init              # writes .cosmon/state/ and .cosmon/formulas/*.toml
cs nucleate task-work --vars topic="…" --assign some-worker
```

Both calls succeed without the user having to supply a path or clone
anything. The builtins are the floor; the project's own formulas are
the ceiling.

### Rules

- **Never ship an empty `.cosmon/formulas/`.** An empty directory is
  strictly worse than a missing one: it short-circuits walk-up (§3) and
  guarantees formula lookup fails. `cs init` must either write the
  builtins or not create the directory at all.
- **Projects may override builtins.** A project-local `task-work.formula.toml`
  wins over the compiled-in one, because walk-up resolves
  `.cosmon/formulas/` before the binary consults its include-str table.
- **Builtin formulas are versioned with the `cs` binary.** Upgrading
  `cs` can change the builtin templates. If a project depends on a
  specific version of a builtin, it must commit the file locally —
  otherwise it inherits whatever ships with the installed `cs` version.
- **`cosmon-embed` (Mode A) does not ship builtins.** The embed crate
  cannot assume the project has any particular formula; it exposes the
  parser (`Formula::parse`) and lets the caller supply the TOML text.
  If the embedding project wants the Mode-B builtins, it must vendor
  them or depend on `cs` being installed.

---

## 8. The agent-must-not-self-execute rule (with worked example)

**Rule.** An LLM caller (agent, script, pipeline, human in a chat) that
nucleates a molecule via `cosmon_nucleate` must not also execute the
formula's steps inline. The caller's obligation ends at scheduling the
worker; the worker's obligation begins at running the steps.

**Why.** A formula describes work that consumes a budget, fills a
worktree, emits evidence, and advances on its own clock. When a caller
reads the step list and acts it out inside its own conversation, the
worker is never registered, the fleet never sees the work, the surfaces
are not projected, the energy budget is not charged, and `cs done`
cannot tear anything down because there is nothing to tear down. Cosmon
loses its observability and becomes a glorified notes file.

This anti-pattern is a predictable consequence of LLM behavior: given
the option to "reduce distance between task and artifact", an agent
will pick the inline path unless the schema physically blocks it. The
schema-level block is the hidden `steps` array (§6); this section
covers the behavioral side.

### Worked example — wrong

A caller agent receives this `cosmon_nucleate` response:

```jsonc
{
  "id": "task-20260410-aaaa",
  "formula": "task-work",
  "status": "pending",
  "assigned_worker": "some-worker",
  "caller_role": "You are the CALLER. You requested this work; you do not execute it.",
  "plan_summary": "Do the work. Read the project's CLAUDE.md for conventions.",
  "next_action": {
    "command": "cs tackle task-20260410-aaaa",
    "why": "cs tackle spawns a dedicated worker (worktree + tmux + fleet entry) that executes the formula; the caller must not run formula steps inline.",
    "do_not": [
      "do not execute formula steps inline",
      "do not call Agent() to act out personas",
      "do not edit molecule files by hand"
    ]
  }
}
```

The **wrong** response from the caller is:

> "I see this is a task-work molecule with steps like 'implement the
> solution' and 'verify and validate'. Let me go ahead and implement
> the solution by editing these files…"

This is wrong because:

- The caller has no `WorkerId` in the fleet for `task-20260410-aaaa`.
- No worktree has been created; any edits corrupt the caller's own CWD.
- No `cs evolve` calls are valid from the caller principal; the
  molecule sits in `pending` forever.
- The energy budget records zero spend even though the LLM session has
  burned real tokens. Accounting diverges from reality.

### Worked example — right

The **right** response from the caller is to stop, surface the
`next_action.command` to whoever owns the tackle decision (a human, a
trusted orchestrator, or an external scheduler), and exit:

> "Created `task-20260410-aaaa` for formula `task-work`. To execute
> this, run `cs tackle task-20260410-aaaa` from the project root.
> The formula's steps will be carried out by the worker in a fresh
> worktree; I will not run them inline."

If the caller is itself the orchestrator (for example: a claude-code
session running from the project root that *is* authorized to call
`Bash` with `cs tackle`), then the correct follow-up is a **single
Bash call** to `cs tackle <id>`, not a chain of file edits. After
tackle, the session's interaction with the molecule is confined to
`cs observe`, `cs deps`, and similar read-only verbs; the writing
happens inside the worker.

### Detection

Symptoms that the anti-pattern is happening in your deployment:

- Molecules with `status: pending` and `assigned_worker: <something>`
  that never transition.
- Commits on the project branch that the fleet has no record of.
- A mismatch between "work done in this session" and `cs energy`
  reported spend.
- `cs deps --transitive` showing orphaned children that never appear
  in a tackle worktree.

When you see any of these, the remediation is not to patch the
symptom; it is to audit the caller and confirm that every agent wired
to cosmon is enforcing the caller-worker split.

---

## 9. Surface sync in embedded mode

Cosmon projects its internal JSON state onto human-readable surfaces:
`STATUS.md`, `ISSUES.md`, `IDEAS.md`, `DELIBERATIONS.md`,
`docs/adr/INDEX.md`, and (optionally) GitHub Issues. The mechanism is
documented in [`docs/surface-sync-protocol.md`](surface-sync-protocol.md);
this section covers what embedding projects must know.

### Rules

- **Surfaces are derived views, never inputs.** Editing `STATUS.md`,
  `ISSUES.md`, `IDEAS.md`, or any projected file by hand is silently
  overwritten by the next `cs reconcile`. This is the single most
  common way embedding projects lose work. Change the molecule state;
  do not edit the surface.
- **The embedding project owns its surfaces.** Each project has its own
  `.cosmon/surfaces.toml` declaring which surfaces to project. Cosmon
  does not project anything the project has not opted into.
- **`cs reconcile` must be run from the project root** (or with a `cwd`
  equivalent, once `reconcile` gains the `cwd` parameter). It reads
  `.cosmon/state/` and writes the surfaces declared in
  `.cosmon/surfaces.toml`; crossing a project boundary here corrupts
  both sides.
- **Reconcile is strictly idempotent.** Running it twice is the same as
  running it once, modulo timestamps. If you observe non-idempotent
  output, file a bug — the projection pipeline is tested for
  idempotence in CI.
- **Mode A consumers should reconcile explicitly.** `cosmon-embed` does
  not run reconcile automatically after mutations; the embedding
  project owns the reconcile call. This mirrors the CLI discipline:
  mutate, batch, reconcile once.
- **Mode B agents should reconcile after batches.** The MCP INSTRUCTIONS
  tell agents the same — the instruction block is the authoritative
  form of this rule for LLM callers.

### GitHub as a surface

When `surfaces.toml` enables the GitHub projection, cosmon keeps a
local mirror at `.cosmon/state/surfaces/github/` for idempotent sync.
Embedding projects that want GitHub Issues to reflect their molecules
must:

1. Provide a GitHub token and repo configuration in `surfaces.toml`.
2. Run `cs reconcile` (human or scheduled) to push updates.
3. *Not* edit the projected issues directly on GitHub; the mirror will
   detect the drift but cannot resolve it automatically.

The GitHub surface is optional; projects with no external visibility
needs can skip it entirely.

---

## 10. Upgrade and versioning

### Versioning scheme

Cosmon uses SemVer. The scope of each component:

| Component | SemVer scope |
|-----------|--------------|
| `cs` CLI | Commands and their documented flags. A `--json` output schema change is a breaking change. |
| MCP tool surface | Tool names, parameter names, and documented fields in responses. Adding an optional parameter is additive; removing one or changing its type is breaking. |
| `cosmon-embed` | Public API (types, functions, trait signatures). Internal crates (`cosmon-core`, `cosmon-state`, `cosmon-filestore`) are *not* covered by embedding SemVer — only `cosmon-embed` is. |
| On-disk state format | Independent versioning. Migrations are required for breaking changes and must be testable with `cs reconcile --check`. |
| Builtin formulas | Versioned with the `cs` binary. A project that needs stability must vendor them locally (§7). |

### What counts as breaking

- Renaming or removing an MCP tool.
- Removing a parameter field or making an optional field required.
- Changing the meaning of an existing field (semantic break, not
  schema break — worse, because it is silent).
- Changing the precedence order in §3 path resolution. A caller that
  previously saw walk-up hit one `.cosmon/` must continue to see the
  same one on the new version.
- Changing the `cwd` parameter semantics in §4 (e.g., making the empty
  string mean something different from absent).
- Removing an operation from the Mode A availability matrix in §5.

### What is explicitly *not* breaking

- Adding a new MCP tool.
- Adding an optional parameter to an existing tool.
- Adding a new field to a response object (additive).
- Adding a new regime command that only exists in Mode B.
- Adding a new builtin formula.
- Tightening error messages.

### Upgrade discipline

- **Mode A.** Pin `cosmon-embed` in your `Cargo.toml` with a
  `~0.Y` or `=0.Y.Z` constraint; bump across minor versions requires
  re-reading this document for matrix or principal changes. Major
  version bumps require running the project's test suite against the
  new release before merging.
- **Mode B.** Upgrade `cs` and the MCP server together; mismatched
  versions sharing a state store is undefined behavior. The state
  format version is visible in `.cosmon/state/version.json` (when
  present); compare it against the `cs` release notes before
  upgrading.
- **State-format migrations.** Cosmon does not auto-migrate state on
  upgrade. An upgrade that requires a migration must be accompanied by
  an explicit `cs migrate` step documented in the CHANGELOG. Attempting
  to run a new `cs` against an old state directory without migration
  is an error, not a silent conversion.
- **ADR trail.** Any change to the contract in this document must be
  preceded by an ADR and referenced from the CHANGELOG. The document
  itself is not a contract unless the ADRs behind it are stable; it is
  the ADRs that carry the commitment.

### Reading order for an upgrade

1. CHANGELOG entry for the new version.
2. Any ADRs referenced in the CHANGELOG.
3. This document, re-read for the sections touched by those ADRs.
4. The embedding project's own integration tests.

If any of these four steps surfaces a conflict, the upgrade is not
safe for the project and should be deferred until the conflict is
resolved — either by cosmon (patch release) or by the project
(integration fix).

---

## References

- [ADR-016](adr/016-autonomy-regimes-and-resident-runtime.md) — the
  regime model and the two-layer architecture.
- ADR-020 — *MCP Server is Project-Agnostic; cwd is Per-Call*
  (pending; tracked by
  [`task-20260409-6cac`](../.cosmon/state/fleets/default/molecules/task-20260409-6cac/)).
- ADR-021 — *Principal Separation: Caller vs Worker in the MCP
  Surface* (future; motivated by §6).
- [Architectural invariants](architectural-invariants.md) — the
  non-negotiable rules that govern any change to cosmon commands.
- [Surface sync protocol](surface-sync-protocol.md) — the reconcile
  mechanism and snapshot model.
- [`delib-20260409-915a` synthesis](../.cosmon/state/fleets/default/molecules/delib-20260409-915a/synthesis.md)
  — the five-persona deliberation that produced this document's
  requirements.
- `crates/cosmon-filestore/src/resolve.rs` — the path resolution
  implementation that this contract binds.
- `crates/cosmon-mcp/src/tools.rs` — the MCP tool surface and the
  current `NucleateParams::cwd` wiring.
- `crates/cosmon-cli/src/cmd/init.rs` — the builtin formulas table
  and the `cs init` bootstrap.
