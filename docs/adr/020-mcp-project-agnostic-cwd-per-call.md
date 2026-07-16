# ADR-020: MCP Server is Project-Agnostic; cwd is Per-Call

## Status
Proposed (2026-04-09)

## Context

The Cosmon MCP server (`cosmon-mcp`) runs as a single, long-lived
process. An operator launches it once — typically at login, via a
LaunchAgent or a globally installed MCP client config — and every
subsequent Claude / agent session connects to that same instance.

Because the server is long-lived, `std::env::current_dir()` at startup
is effectively a frozen accident: the directory the LaunchAgent
happened to be in when it spawned the binary, or wherever the user's
shell was when they ran `cosmon-mcp` by hand. Until this ADR, the
server used that startup CWD as the implicit root for every lookup
of `.cosmon/state/`, `.cosmon/formulas/`, and fleet metadata. A
caller in `~/dev/project-a` who asked `cosmon_nucleate(formula:
"task-work")` would silently get project-b's formulas if the server
had been launched from there — or, more commonly, the server's own
source tree. Cross-project usage was broken by construction.

Deliberation `delib-20260409-915a` convened five personas
(feynman, jobs, wheeler, karpathy, torvalds, architect) to dissect
the symptom. The diagnosis converged from five independent angles:

- **jobs** called it "the lie": the server claims to orchestrate
  your project but silently resolves paths against its own launch
  directory.
- **wheeler** reframed it as a **site-of-reference** question:
  "When an MCP tool on the cosmon server is invoked from a caller
  whose cwd is not the server's launch cwd, *whose cwd* names the
  `.cosmon/` the call operates on?"
- **karpathy** identified it as the seam where verb design fails
  the agent: the LLM caller cannot even name its own project.
- **torvalds** opened the files and found the fix already present
  in `cosmon-filestore::resolve_formulas_dir` — the walk-up
  scaffolding existed, but the MCP call site froze it at `new()`
  instead of parameterising it per request. Five lines of plumbing.
- **architect** named the underlying property: **cosmon is a
  substrate, not an application**. ADR-016 already committed the
  project to a git-like transactional core where the CWD is an
  input, not a property of a daemon. A server that pins its own CWD
  is an implicit application-mode regression.

The walk-up machinery landed in `task-20260409-335f` (completed
2026-04-09):

- `cosmon-filestore/src/resolve.rs` factored
  `walk_up_find_cosmon_dir_from(start: &Path)` out of the
  process-CWD version and exposed a new public function
  `resolve_formulas_dir_from(start: &Path) -> PathBuf`.
- `cosmon-mcp/src/tools.rs` added `cwd: Option<String>` to
  `NucleateParams` and resolved the formulas dir per-call with
  `params.cwd.as_deref().map_or_else(|| self.formulas_dir…,
  |cwd| cosmon_filestore::resolve_formulas_dir_from(Path::new(cwd)))`.

This ADR encodes the **policy** that patch implements, so that
`cosmon_observe`, `cosmon_evolve`, `cosmon_complete`,
`cosmon_collapse`, `cosmon_ensemble`, and every future MCP tool
follow the same discipline without re-deriving it from scratch.

## Decision

### 1. The project-agnosticism invariant

**The Cosmon MCP server is project-agnostic.** It does not belong to
any single repository. It does not remember which project it was
launched from. It does not impose the launch CWD on the tools it
exposes. For every tool call, the question *"which `.cosmon/` does
this operate on?"* is answered from the **caller's** site of
reference, not the server's.

Concretely: the server holds no project identity in its durable
state. Any identity the tools use must be re-derived from
per-request parameters on every invocation. The only things the
server carries between calls are its router, its handlers, and
fallback defaults — never a resolved state directory, formulas
directory, or fleet id.

### 2. `cwd: Option<String>` on every tool parameter struct

Every MCP tool parameter struct that touches state MUST expose an
optional `cwd` field of type `Option<String>`:

```rust
pub struct NucleateParams {
    // …existing fields…
    /// Caller's working directory. When set, the MCP server
    /// resolves .cosmon/ by walking up from this path instead of
    /// from the long-lived server's own CWD.
    pub cwd: Option<String>,
}
```

The contract is deliberate:

- **Optional, not required.** Clients that omit `cwd` keep working;
  the server falls through to its startup-time defaults. This is
  the backward-compat hinge discussed in §4.
- **A string, not a typed path.** MCP is JSON-over-stdio; a `String`
  deserialises cleanly across every MCP client SDK and avoids
  `PathBuf` serde gymnastics. The server converts to `&Path` at
  the resolution boundary.
- **Per-tool, not a session property.** MCP does not model
  connections, sessions, or attach handles. Every call is a fresh
  transaction. Putting `cwd` on the tool itself matches the wire
  model.
- **The same name across every tool.** `cwd` and only `cwd`. No
  `project_dir`, `working_directory`, or `root`. One word, one
  meaning, zero aliases.

### 3. Resolution precedence (four tiers, in order)

Every tool implementation MUST resolve `state_dir`, `formulas_dir`,
and `fleet_dir` through exactly this precedence:

1. **Explicit `cwd` parameter → walk-up.** If the caller supplied
   `params.cwd = Some("/abs/path")`, the server walks up from that
   path looking for a `.cosmon/` directory (same algorithm git uses
   to find `.git/`). This is the happy path for any client that
   knows where it is.
2. **Walk-up from the server's own process CWD.** If `cwd` is
   `None`, the server falls through to `std::env::current_dir()`
   and walks up from there. For a server launched at login this
   almost always fails to find a project; for a `cargo run`
   developer invocation inside the cosmon repo, it finds the
   cosmon project — which is exactly the correct behaviour for
   self-application.
3. **Server startup default.** If the walk-up from the process
   CWD also fails, the server uses the `PathBuf` it cached in
   `CosmonService::new()`. This is the same value the old code
   returned unconditionally, so every client that worked before
   this ADR keeps working after it.
4. **Compiled-in builtins / global fallback.** `$HOME/.cosmon/`
   for state and `$HOME/.cosmon/formulas/` for formulas. The
   latter is the hook for the `cs init` bundled-formula work
   (`task-20260409-4334`, the P0-3 sibling of this molecule) so
   that even a caller whose project has no formulas can still
   reach `deep-think`, `task-work`, and `idea-to-plan` via
   `include_str!`-baked copies.

Precedence is strict: each tier is tried only if the previous one
returned nothing. The first tier that yields a usable path wins.
There is no merging, union, or fallthrough on partial matches —
the CWD that resolves the lookup is the single site of reference
for the rest of that request.

The `COSMON_FORMULAS_DIR` / `COSMON_STATE_DIR` environment
variables, where supported by the filestore helpers, sit
*between* tier 1 and tier 2: they are honoured when resolving
from any `start` directory, to match the CLI's long-standing
behaviour. They are documented as an operator escape hatch, not
a normal client path.

### 4. Backward compatibility: `cwd: Option<String>`

The backward-compatibility contract has three load-bearing
guarantees:

- **Every existing MCP client continues to work unmodified.**
  Clients that do not know about this ADR simply omit the `cwd`
  field; `serde` deserialises it as `None`, and the resolution
  falls through to tiers 2–4. Every call that resolved to a
  project before this ADR resolves to the same project after
  it.
- **`cwd` is never rejected for being unknown.** The server does
  not validate that `cwd` points into a known project, nor that
  the walk-up succeeds. A `cwd` that resolves to nothing just
  means "tier 1 found nothing, fall to tier 2"; it is never an
  error response. This keeps clients composable with untrusted
  caller environments (Docker containers, CI runners, sandboxes)
  where the caller may not know whether its `cwd` is really
  inside a cosmon project.
- **Adding `cwd` to a tool is not a breaking change.** Because
  `Option<String>` is additive in JSON Schema and in the Rust
  deserialiser, every tool that gains a `cwd` field in the
  course of §5 rollout is a non-breaking release. No MCP client
  has to upgrade in lockstep with the server.

### 5. Rollout scope

This ADR applies to the full MCP tool surface, not just
`cosmon_nucleate`. The rollout is staged so that each tool lands
under review:

- **Shipped in `task-20260409-335f`.** `cosmon_nucleate` is the
  first and pilot implementation. Its `NucleateParams.cwd`
  field, the `cosmon_filestore::resolve_formulas_dir_from`
  helper, and the per-call resolution logic in
  `tools.rs:365-371` are the reference implementation every
  subsequent tool must match.
- **Covered by follow-up molecule (`P1-cwd-full-surface` in the
  delib-915a ranked list).** `cosmon_observe`, `cosmon_evolve`,
  `cosmon_complete`, `cosmon_collapse`, `cosmon_ensemble`,
  `cosmon_freeze`, `cosmon_thaw`, `cosmon_decay`, `cosmon_merge`,
  `cosmon_transform`, `cosmon_nudge`, `cosmon_list`,
  `cosmon_search`, `cosmon_get`, `cosmon_stats`,
  `cosmon_declare`, and any signal-bus or energy tool that
  touches state.
- **Out of scope for this ADR.** Tools that legitimately need
  *server-scoped* state (configuration introspection, version
  reporting, a hypothetical `cosmon_mcp_healthcheck`) do not
  take `cwd`. This ADR is about *state resolution*, not
  *parameter uniformity*.

### 6. What this ADR explicitly does NOT decide

- **Principal separation between caller and worker.** The
  delib-915a synthesis identified a second architectural bug
  where the MCP caller and the worker executor are
  indistinguishable in the transport. That is the subject of a
  separate ADR (ADR-021, queued). This ADR only answers
  *"whose CWD?"*; it does not answer *"whose credentials?"*.
- **Per-project MCP servers.** Rejected by architect during the
  delib on the grounds that it breaks globally-installed UX.
  The whole point of `cwd: Option<String>` is to avoid needing
  one server per project.
- **A stateful `cosmon_attach(cwd) → handle` session model.**
  Rejected because MCP does not model sessions. Per-call
  resolution is simpler and composes with every existing
  client.
- **Automatic discovery of the caller's CWD from transport
  metadata.** MCP does not expose the client process's working
  directory to the server; the caller must opt in by sending it.
  That opt-in is fine: it matches git's model where every
  command explicitly chooses its root.
- **Template pluggability or resident-runtime changes.** Both
  are tracked under separate ADRs (ADR-018 and ADR-016 Phase
  3+). This ADR is a policy layer over the existing
  transactional core, not a new runtime.

## Consequences

**Positive:**

- The cross-project use case — which the delib unanimously
  identified as cosmon's **default** validation, not an edge
  case — works from any `$PWD` on the same machine, with a
  single globally installed MCP server.
- The resolution policy is now explicit contract instead of an
  implicit property of whichever code path the author last
  touched. Future tools have a fixed four-tier recipe to
  follow, and reviewers have a checklist to enforce.
- The fix is additive: every existing client keeps working, no
  lockstep upgrade is required, and the new field is a
  one-liner in every parameter struct.
- The policy is regime-consistent with ADR-016. The
  transactional core stays stateless; the caller's CWD is an
  input on every request, mirroring how the CLI already
  resolves `.cosmon/` from the shell's current directory.
- When the resident runtime (ADR-016 Phase 3+) lands, the same
  resolution contract transfers unchanged: a long-lived process
  that receives per-call `cwd` inputs is already a resident
  runtime in embryo, and the MCP server's eventual split into
  `cosmon-mcp-core` / `cosmon-mcp-runtime` inherits this
  behaviour for free.

**Negative:**

- Every MCP tool must be audited and updated to accept `cwd`.
  The rollout is not a one-shot patch; it is a sweep across
  ~15 tools in `cosmon-mcp/src/tools.rs`. Each addition is
  small, but the total is non-trivial, and forgetting a tool
  leaves a silent frozen-CWD hole.
- Clients that want cross-project correctness must now send
  `cwd` on every call. The MCP INSTRUCTIONS string becomes
  load-bearing documentation: if it does not tell clients to
  send `cwd`, they won't, and the silent fallback to tier 2
  will mask the bug. The companion task `P1-mcp-instructions`
  owns that update.
- `Option<String>` makes the contract ergonomic but imprecise:
  a client can send `cwd: Some("/nonexistent")` and the
  server silently falls through. Operators debugging a
  resolution problem cannot distinguish "I didn't send `cwd`"
  from "my `cwd` failed walk-up". A future telemetry hook
  (`resolution_source: "cwd" | "server_cwd" | "startup" |
  "builtin"` in the tool response) could close this gap, but
  it is not mandated by this ADR.

**Neutral:**

- This ADR introduces no new CLI commands, no new formulas,
  and no new surfaces. It is a policy layer over existing
  machinery (`resolve_formulas_dir_from`, the `NucleateParams`
  field, and the per-call resolution in `cosmon_nucleate`).
  The value is in pinning the contract so every tool that
  follows inherits the same resolution story.
- The transactional-core bias of the MCP server is
  reinforced, not changed. Whether that is the right long-term
  shape is the subject of ADR-016 and its eventual
  `cosmon-mcp-runtime` split — but this ADR is orthogonal to
  that decision: the four-tier precedence holds for both the
  current server and its future resident-runtime successor.

## References

- `delib-20260409-915a` — the deep-think deliberation that
  produced the synthesis this ADR encodes. See §6 of the
  synthesis (ranked fixes) for the P1 slot this ADR owns,
  and §1(a) for the strong-convergence finding that every
  persona named the frozen `formulas_dir` as a root cause.
- `task-20260409-335f` — the P0-1 patch that shipped the
  reference implementation this ADR codifies. Completed
  2026-04-09. This ADR is deliberately blocked on it so the
  text references shipped code, not planned code.
- [ADR-016: Autonomy Regimes and the Resident Runtime](016-autonomy-regimes-and-resident-runtime.md)
  — the invariant that cosmon's transactional core is
  stateless and git-like. This ADR is the consistent MCP
  projection of that invariant; without ADR-016 the
  project-agnosticism claim would be a product decision
  instead of an architectural one.
- [`docs/architectural-invariants.md`](../architectural-invariants.md)
  §1 — the two-layer model (Transactional Core + future
  Resident Runtime) that frames the server as a client of
  the transactional core, not a replacement for it.
- [`crates/cosmon-mcp/src/tools.rs`](../../crates/cosmon-mcp/src/tools.rs)
  — `NucleateParams.cwd`, the per-call resolution logic in
  `cosmon_nucleate`, and the `tool_router` surface that
  every subsequent tool extends.
- [`crates/cosmon-filestore/src/resolve.rs`](../../crates/cosmon-filestore/src/resolve.rs)
  — `resolve_formulas_dir_from` and `walk_up_find_cosmon_dir_from`,
  the factored walk-up primitives that every MCP tool uses
  to implement tier 1 of the precedence.
- **ADR-021 (queued): Principal Separation: Caller vs Worker
  in the MCP Surface** — the successor ADR that covers the
  second half of the delib-915a diagnosis (principal
  conflation). Blocked on this ADR so it can assume stable
  `cwd` semantics.
- **`docs/EMBEDDING.md` (queued)** — the 10-section contract
  document that will cite this ADR as the canonical
  specification of resolution precedence for any cosmon
  consumer.
