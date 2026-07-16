# ADR-102 — `cosmon-agent-harness` and the `AgentLoop` port

**Status:** Accepted (2026-05-18).
**Date:** 2026-05-18.
**Decider:** Noogram.
**Parent deliberation:**
`delib-20260518-5178`
— 7-persona panel (architect, torvalds, forgemaster, karpathy, feynman,
tolnay, knuth) on *« Comment construire `cosmon-agent-harness` — la
couche au-dessus du Worker-Spawn Port qui rend les Direct-API Adapters
capables d'exécuter de vraies tâches agentic long-horizon ? »* The
panel reached 6/7 convergence on the crate boundary (C1), the
hybrid spine + per-provider message-log shape (C5/S1), and the
"both, not either" framing of harness vs subprocess delegation (C7).
Three axes (D3, D4, D6) were deliberately left to this ADR.
**Authoring task:** `task-20260518-ffbd` (child #1 of 9 in the
Path-1 decomposition; upstream of crate skeleton and every tool
task).
**Authoring discipline:** wheeler (vocabulary triage) — the panel
reached majority on the four-word closure before any code was
drawn; this ADR commits the words.

**Binds:**
- [ADR-079](079-worker-spawn-port-and-adapter-contract.md) — the
  four-word closure `{ Worker · Port · Adapter · Tier }` and the
  four-obligation Adapter contract. The `AgentLoop` port lives
  **inside** an Adapter; it does not amend §1's vocabulary, it
  declares a second closure scoped to a strictly smaller perimeter.
- [ADR-099](099-dispatch-site-stability.md) — TS-0
  dispatch-site typestate. The v0.5 promotion this ADR sequences
  (`Harness<S: HarnessState>`) extends the same knuth-style
  invariant-programming pattern to the in-process loop seam.
- [ADR-100](100-direct-api-adapters-r2-amendment.md) — Direct-API
  substrate (R2). The in-process `openai` / `anthropic` Adapters
  this ADR draws against are exactly the Adapters R2 admitted to
  `cosmon-transport::api::*`.
- [ADR-101](101-supervision-mode-typed-on-validated-adapter.md) —
  `SupervisionMode` typestate. The `Subprocess` variant this ADR
  defers (see §4 — D4) would extend §3's mapping table; the
  deferral is named, not silent.
- [ADR-043](043-provider-abstraction.md) — `cosmon-provider`
  remains the home for one-shot, no-loop HTTP usage; this ADR does
  not move the agent loop into it (R3 stays retired per ADR-100 §1).

**Architectural invariants:** `docs/architectural-invariants.md`
§8j (ingress bindings — every Port is one; the `AgentLoop` port is
the ingress binding through which a *turn* enters cosmon's
in-process supervision perimeter, just as the Worker-Spawn Port is
the binding through which a *worker* enters the process
perimeter). A new §8 clause is proposed inline in §6 below.

---

## Context

The first post-R2 smoke test
`task-20260518-35c4`
demonstrated that `cs tackle --adapter openai` reaches the
Direct-API agent loop end-to-end and produces a haiku. The same
test demonstrated, by elimination, that the toy harness in
`crates/cosmon-provider/src/openai.rs::run_agent_loop`
— 8-turn cap, single `write_file` tool, no persistent shell, no
file-edit primitive, no `AGENTS.md` ingestion — is **sufficient
for haikus and insufficient for real Rust refactors**. The toy
loop is exactly what the panel called a *category error* (synthesis
§S6): it copied Codex's "two tools" headline without auditing
whether two is the right number for cosmon's experiment (Rust
refactor against a multi-crate workspace).

The empirical motive is on the public record. Céline Hajjar's
X-IA #34 §1 talk
(`/srv/cosmon/knowledge/groupe-x-ia/2026-05-18-x-ia-34-show-me-your-agent.md`
§1) names the anatomy of a shipping harness — `exec_command`
persistent + `edit_file` as separate primitives, `AGENTS.md` as
map-not-encyclopedia, autocompaction + prompt-cache as the two
unavoidable scaling concerns past the first month — and reports
the OpenAI internal data point: **3 engineers, 5 months, ~1,500
PRs, 5 billion tokens, 100% Codex-authored**. The verdict the
panel quoted (synthesis §C7 prefatory line): *« le bottleneck
n'est plus le modèle, c'est la couche au-dessus »*. The academy
cascade that landed ADR-100 (Direct-API substrate) and ADR-101
(`SupervisionMode` typestate) closes the *substrate* gap; this
ADR opens the *harness* gap — what cosmon does **above** the
Worker-Spawn Port once a Direct-API Adapter has been dispatched.

The panel's load-bearing convergence (C5 + S1, knuth): the only
provider-shaped invariant is **I4 message-log well-formedness**.
The agent loop FSM is provider-agnostic; the message-log types
(`role:tool` for OpenAI, `tool_result` content blocks for
Anthropic) are non-isomorphic at the type level. This is the
*reason* — not a stylistic preference — the harness must be a
shared spine with per-provider `MessageLog` impls, and not a
single monolithic adapter or a per-provider clone.

This ADR is a **vocabulary commit only — zero code change in this
PR**, in the same shape as ADR-079 (worker-spawn vocabulary). The
crate skeleton, tool implementations, message-log impls, and
v0.5 typestate promotion are downstream beads.

---

## Decision

Cosmon commits **{ Loop · Tool · Turn · Schema }** as the
load-bearing four-word vocabulary for the agent-harness perimeter,
draws the spine NOW against the two existing concrete schemas
(OpenAI + Anthropic) — but only the FSM-shaped spine and the
`MessageLog` trait carrying I4, **not** a unified
`ToolCall`/`ToolResult` wire envelope — and sequences the
implementation as a two-step trajectory (minimal spine in v0,
typestate `Harness<S>` in v0.5). The codex subprocess Adapter is
a perpendicular safety valve and remains decoupled from this
trajectory.

### 1. D-1 — The port boundary and the four-word closure

The **`AgentLoop` port** is the typed perimeter through which a
single *turn* of an in-process agent loop crosses the
cosmon-supervised boundary. It is the second port (after the
Worker-Spawn Port of ADR-079) that an in-process Adapter is
obliged to traverse; subprocess Adapters (`claude`, `aider`,
future `codex`) traverse only the first.

The closure is:

| Word | Definition | Origin |
|------|-----------|--------|
| **Loop** | The provider-agnostic FSM that drives a single worker session from `Bootstrapping` through `Sending` / `Reading` / `Dispatching` to `Terminated`. Eight states, twelve transitions, four loop invariants `{I1 turn-bounded, I2 tool-budget, I3 context-token, I4 message-log well-formedness}` (knuth §5–§7). | new — this ADR |
| **Tool** | A named, typed, registry-resolved capability the model can call inside a `Dispatching` step. v0 set: `{ exec_command (persistent), edit_file (exact-match), read_file }`. Tools live in `cosmon-agent-harness::tools`; the registry is `BTreeMap<&'static str, Box<dyn Tool>>` (S5 — stable iteration order for future prompt-cache prefix stability). | new — this ADR |
| **Turn** | One round-trip across the `Sending` → `Reading` → `Dispatching` arc of the FSM. The turn cap (v0: ~30; toy: 8) is a loud, bounded upper bound on the lexicographic variant that proves termination in `O(K · provider.timeout)` wall-clock (knuth §6). | new — this ADR |
| **Schema** | The provider-shaped serialization surface that produces the wire envelope (`ChatRequest`/`Messages`) from the spine's `MessageLog` state, and parses the wire response back into spine events. **Lives per-provider** (`cosmon-provider::openai::Schema`, `cosmon-provider::anthropic::Schema`), never in the spine. | new — this ADR |

`Loop`, `Tool`, and `Turn` are owned by the
`cosmon-agent-harness` crate (D-2). `Schema` is owned by the
provider crates (`cosmon-provider::openai`,
`cosmon-provider::anthropic`). The spine consumes `Schema` through
the `MessageLog` trait whose contract is **I4 well-formedness**
(every turn produces a message-log that round-trips through the
provider's schema without loss).

This closure is **closed** by this ADR. Adding a fifth primitive
(e.g. *Plan*, *Memory*, *Subagent*, *Cache*) requires a successor
ADR, not a silent introduction. See §7.

#### Closure preservation w.r.t. ADR-079

The ADR-079 closure `{ Worker · Port · Adapter · Tier }` is
preserved verbatim. The `AgentLoop` port is a **value** of *Port*
in the ADR-079 sense — it is a typed perimeter through which a
class of perturbation (a *turn*) crosses the cosmon boundary, and
it is realised by per-Adapter `Schema` implementations the way the
Worker-Spawn Port is realised by per-Adapter `Spawn`
implementations. *Adapter* and *Tier* are untouched. *Worker* is
the entity in which an `AgentLoop` runs; the relation is "the
Worker hosts the Loop", not "the Loop is a kind of Worker".

### 2. D-2 — Crate topology

A new crate `crates/cosmon-agent-harness/` is created at the same
Cargo tier as `cosmon-provider`. It depends on `cosmon-core` only
(for `MoleculeId`, `WorkerId`, `SealedBriefing`, event_v2 emission
types). It is **not** a sub-module of `cosmon-provider`, **not**
an extension of `cosmon-transport`, and **not** a feature gate
inside an existing crate.

The provider crates (`cosmon-provider::openai`,
`cosmon-provider::anthropic`) depend on `cosmon-agent-harness` —
they implement the spine's `Provider` (v0) or `MessageLog` (v0.5)
trait — and `cosmon-transport::api::*` (the HTTP Adapter modules
of ADR-100 §2) calls the spine's `run_loop<P: Provider>` free
function from inside the in-process Adapter's `spawn_and_prompt`
dispatch arm.

Synthesis-recorded rejections of the alternatives (architect §1,
forgemaster §1, panel-unanimous 6/7 on C1):

- **Sub-module of `cosmon-provider`** — rejected. `cosmon-provider`'s
  perimeter (per ADR-043) is one-shot, no-loop HTTP usage. The
  agent loop is a structurally distinct perimeter: it carries
  turn-bounded state, a tool registry, a sub-FSM for `ExecSession`,
  briefing-seal verification, and WS-event emission across many
  turns. Co-locating with one-shot provider usage either pollutes
  the latter or hides the former.
- **Extension of `cosmon-transport`** — rejected. `cosmon-transport`
  realises the Worker-Spawn *Port* (process-level supervision,
  pane signatures, the four ADR-079 §5 obligations).
  `cosmon-agent-harness` realises the *contents* of an in-process
  worker session, which is a separate perimeter; merging them
  would dissolve the Worker-Spawn Port back into a generic
  "transport" bag and re-open the supervision-mode hole ADR-101
  just closed by type.
- **Feature flag inside `cosmon-provider` (`agent-loop` feature)**
  — rejected. Feature flags hide perimeters from the dependency
  graph; the architecture audit (ADR-082) cannot see the new tier.
  A new crate makes the dependency edge explicit and reviewable.
- **Per-provider crates (`cosmon-agent-harness-openai`,
  `cosmon-agent-harness-anthropic`)** — rejected as
  over-decomposition (ADR-0002 §1 splitting rule does not fire;
  same reasoning as ADR-100 §2 against per-provider
  HTTP Adapter crates).

### 3. D-3 — Draw the spine NOW, but only the spine

The panel split on *when* to draw the spine trait (synthesis D3).
Five panelists (architect, torvalds, forgemaster, karpathy, tolnay,
knuth) recommended drawing now against OpenAI + Anthropic; one
(feynman) recommended waiting until `anthropic.rs` is alive as
code, citing the ADR-100 lineage as authority for "two examples,
not one anticipated example, defines IFBDD-purer".

This ADR adopts the **draw-now** position with feynman's
discipline folded in: **draw the FSM-shaped spine and the
`MessageLog` trait (the parts where two concrete examples genuinely
exist on disk or in canonical reference clients), but do not
extract a unified wire envelope** (the part where only OpenAI
exists as code today). Concretely:

- **Extracted into `cosmon-agent-harness` (v0):**
  - The eight-state FSM (`Bootstrapping`, `Budgeting`, `Sending`,
    `Reading`, `Dispatching`, `Tooling`, `Compacting`,
    `Terminated`) as control flow over a single `loop {}`.
  - The `Tool` trait + `BTreeMap` registry.
  - The `Provider` trait (v0 — two methods: `one_turn` and
    `tool_schema`) carrying the I4 obligation through the
    `MessageLog` associated type.
  - Loop invariants `{I1 turn-bounded, I2 tool-budget, I3
    context-token, I4 message-log well-formedness}` named in
    `invariants.rs`.
  - WS-event emission free functions (callsite stability per
    ADR-098 §2.4).
- **NOT extracted (per-provider, duplicated until the second
  example proves the abstraction):**
  - `ChatRequest`/`Messages` Serde types.
  - The HTTP request/response wire serialization functions
    (`build_request_body`, `parse_response`).
  - A normalized `ToolCall` / `ToolResult` wire envelope. (This is
    the load-bearing exclusion. Forcing every provider to round-trip
    through a normalized envelope would sediment OpenAI's
    `role:tool` shape into the spine and break I4 the day a
    third schema lands.)

The falsifier for this cut: if writing `anthropic.rs` against the
v0 spine requires adding a method to the `Provider` trait (other
than a new tool registry), the cut was wrong and a successor ADR
revisits the boundary. If it requires adding a wire envelope
type to the spine, same outcome.

### 4. D-4 — `SupervisionMode` for the codex subprocess Adapter

The codex subprocess Adapter (`task-20260518-9be4`, sibling
not blocked by this ADR) starts with
`SupervisionMode::TmuxPane { hook_required: true }` — the existing
ADR-101 §3 variant, reused. A new
`SupervisionMode::Subprocess { binary, liveness_via_exit,
wait_timeout }` variant is **deferred** to that sibling PR; it is
promoted only if the codex Adapter shows a non-tmux supervision
path (bare-PTY, container, MCP-orchestrated remote).

The panel split 3-3 on this (D4). Architect, torvalds, forgemaster
recommended reusing `TmuxPane` (codex runs in a tmux pane just
like claude/aider; the binary-name distinction is a *pane-signature*
concern per ADR-098 §C3, not a *supervision-mode* concern; adding
the variant against one example is the ADR-098 §C8 anti-pattern).
Karpathy, tolnay, knuth recommended adding `Subprocess` now (the
subprocess `wait()` exit code is a structurally different
termination witness from pane-died; knuth §6's termination proof
distinguishes them).

This ADR adopts the **reuse-`TmuxPane`** position as the v0
default and names the **promotion criterion** explicitly: if and
only if the codex Adapter PR (`task-20260518-9be4`) demonstrates a
non-tmux supervision path, ADR-101 §3's table grows a `codex →
Subprocess { binary: "codex", liveness_via_exit: true, wait_timeout:
... }` row and the `cmd::tackle.rs` §4 match grows a new arm.
`#[non_exhaustive]` on `SupervisionMode` is exactly what makes
this a one-line addition under compiler protection (ADR-101 §6
compile-fail witnesses preserved).

This deferral is **named, not silent**. The codex Adapter sibling
PR carries the decision-point as its first checklist item; if it
ships `Subprocess`, a successor amendment ADR (small, single-line
amendment to ADR-101 §3) records the promotion.

### 5. D-6 — Two-step trajectory: minimal spine (v0) → typestate (v0.5)

The panel split on trait shape (D6). Torvalds (minimal spine, two
methods, burn-when-wrong), forgemaster + karpathy (`Conversation`
trait, three methods, hybrid topology), knuth + tolnay + architect
(typestate `Harness<S: HarnessState>` with eight states, ~12
monomorphisations, RPITIT shape for tolnay, `AgentLoopContext` +
`AgentLoop` trait for architect).

This ADR adopts the **two-step trajectory**:

- **PR-A (v0).** Ship the **minimal spine** — function
  `run_loop<P: Provider>(provider: P, briefing: SealedBriefing,
  work_dir: &Path, telemetry: Option<&AdapterTelemetry>) ->
  Result<Synthesis, HarnessError>` with a `Provider` trait
  carrying **two methods** (`one_turn`, `tool_schema`) and one
  associated type (`MessageLog: I4`). The eight FSM states live in
  *control flow* (named local enums, named loop labels), not in
  the type system. ~300–400 LOC.
- **PR-A.5 (v0.5 — separate, follow-up bead, not nucleated by
  this ADR).** Promote to **typestate-encoded `Harness<S:
  HarnessState>`** with phantom-parameterised states. The
  twelve transitions of PR-A become typestate methods; the four
  loop invariants become trait bounds. This is the cosmon pattern
  from ADR-099 / ADR-101 itself — the typestate landed *after* the
  empirical seam was identified at the dispatch site, not before.

The PR-A.5 bead is the operator's call to file; it is **not**
nucleated as a child of this ADR. The ADR records the trajectory
to keep the v0 deliberately small without losing the typestate
target in institutional memory.

The falsifier for the trajectory: if PR-A's spine survives the
end-to-end smoke test on a single named Rust refactor
(`task-20260518-95cc` — the "C-clamp-in-ice-water" demonstration
of synthesis §C9) without exhibiting a seam-leak in the b6d7/35c4
family, the trajectory is on track. If it exhibits one, PR-A.5 is
fast-tracked.

### 6. Architectural invariants — §8j and a proposed §8n

`docs/architectural-invariants.md` §8j states that *every Port is
an ingress binding*. The `AgentLoop` port satisfies this: a *turn*
is an ingress of a class of perturbation (model-decided tool
calls, model-emitted assistant messages) into cosmon's in-process
supervision perimeter. The §8j framing is preserved.

A new clause is proposed inline for §8 (to be ratified in a
follow-up update to `architectural-invariants.md`):

> **§8n. The agent loop is a Port, not a sub-module of any HTTP
> adapter.** Every in-process Adapter (per ADR-100) that runs an
> agent loop traverses the `AgentLoop` port; the spine
> (`cosmon-agent-harness`) is the realisation, not an
> implementation detail of `cosmon-transport::api::*` or
> `cosmon-provider::*`. A future PR that re-inlines the loop into
> a single HTTP Adapter for "simplicity" is a structural breach;
> file a bead, do not patch the surface.

The proposed §8n is named here for the record and will land in
the same surface-sync wave as this ADR's acceptance (the
invariants doc is updated by the next `cs reconcile` after a
ratification PR — out of scope for this vocabulary-only ADR).

### 7. Failure-mode taxonomy — SF-6 and SF-7

Two new silent-failure classes enter cosmon's vocabulary through
this ADR's perimeter. Naming them pre-emptively keeps the SF
taxonomy loud (per the ADR-097 / ADR-098 §1 WS-1…WS-5
discipline).

- **SF-6 — `StaleBasePatch` (`edit_file` path).** When `edit_file`
  is shipped with unified-diff or hunk-based patch semantics
  (architect's variant in synthesis D2 — *defer to v1 per
  synthesis recommendation, exact-match search-and-replace
  in v0*), the model can emit a patch whose `base_blake3` no
  longer matches the file on disk because a previous turn or a
  parallel writer mutated the file. The patch apply succeeds at
  the line level but silently overwrites a divergent base. The
  named class is *SF-6 StaleBasePatch*; the named witness is
  `edit_file` returning `Err(StaleBase { expected_blake3,
  observed_blake3 })`. The v0 exact-match variant does not
  carry this failure mode (it refuses ambiguous matches at the
  source level); v1 unified-diff does.
- **SF-7 — `BinaryVersionMismatch` (codex Adapter path).** When
  the codex subprocess Adapter ships (`task-20260518-9be4`),
  its CLI ABI may drift across `codex` binary versions in ways
  invisible to a `--help` parse. The named class is *SF-7
  BinaryVersionMismatch*; the named witness is a runtime
  `codex --version` pre-flight check inside the Adapter's
  `Spawn::is_alive` (or `Spawn::spawn` pre-flight), refusing the
  worker with `SF7BinaryVersionMismatch { expected, observed }`.
  This is tolnay's discipline from synthesis §C8: *"without a
  runtime check, the pin is decorative"*. The named SF class is
  inscribed here pre-emptively so the codex Adapter PR has a
  taxonomy slot ready; the discipline is a runtime pre-flight,
  not a doc comment.

Both SF classes are **named, not implemented**, by this ADR. The
v0 spine does not need either: the exact-match `edit_file` cannot
exhibit SF-6, and no codex Adapter exists yet to exhibit SF-7.

### 8. Vocabulary obligation — no fifth word

The closure `{ Loop · Tool · Turn · Schema }` is closed. The
panel enumerated five candidates that were *deliberately* not
admitted to v0 (synthesis §C4: autocompaction, prompt cache,
sub-agents, MCP, planning mode, TodoWrite). If implementation
reveals that any of these requires a fifth primitive in the
spine's vocabulary (rather than a per-Adapter or per-tool
extension), that forces a successor ADR — not a silent
introduction.

The candidate fifth words the panel flagged for future
deliberation (not commitments):

- **Plan.** If `planning mode` (Codex's first-pass plan emission)
  lands as a structurally distinct phase of the FSM rather than a
  prefix on `Sending`, it may earn primitive status.
- **Memory.** If autocompaction adds a state machine over the
  `MessageLog` (compact, replay, prune), `Memory` may earn
  primitive status separate from `Tool`.
- **Subagent.** If `cs nucleate --blocked-by` proves insufficient
  for sub-task delegation inside a single worker session, the
  cosmon-lab sub-agent path may earn primitive status.
- **Cache.** If prompt-cache discipline (S5's `BTreeMap` is a
  prerequisite, not a commitment) demands a typed cache-prefix
  abstraction, `Cache` may earn primitive status.

None of these is committed by this ADR. Each would arrive through
a successor ADR sequenced after the v0 spine ships and exhibits
the specific friction.

### 9. Non-goals — explicitly OUT

The following are out of scope for the v0 spine, the v0.5
typestate, and any PR sequenced from this ADR. v1+ deliberation
required before any of these enters the perimeter:

1. **Autocompaction** — summarising the message log past a
   context-window threshold and re-injecting the summary
   (Hajjar's *« plomberie »* of section 1).
2. **Prompt cache** — provider-side cache_control headers
   (Anthropic) or implicit prefix freezing (OpenAI). S5's
   `BTreeMap` is the *prerequisite* for prompt cache, not a
   commitment to ship it.
3. **Sub-agents in-loop** — spawning a child agent session inside
   a turn. cosmon's existing `cs nucleate --blocked-by` primitive
   is the v0 substitute.
4. **MCP** — Model Context Protocol tool exposure. v1+.
5. **Planning mode** — Codex's first-pass plan emission as a
   distinct FSM state. v1+.
6. **TodoWrite** — Claude Code's session-local todo list as a
   first-class tool. v1+.

---

## Consequences

**Positive.**
- The agent-loop perimeter becomes a named Port (the second after
  ADR-079) with a closed vocabulary. Every later deliberation on
  in-process agent loops — third Direct-API Adapter, autocompaction
  ADR, prompt-cache ADR, sub-agent ADR — shares one referent for
  *Loop*, *Tool*, *Turn*, *Schema*.
- The hybrid Q2 outcome of the synthesis (shared spine +
  per-provider `MessageLog`) lands as a typed boundary in
  `cosmon-agent-harness`'s `Provider` trait. I4 is the only
  provider-shaped invariant; this ADR names it as such and lifts
  it into a trait obligation.
- The crate-tier ordering (`cosmon-core` ← `cosmon-agent-harness`
  ← `cosmon-provider::{openai,anthropic}` ← `cosmon-transport::api::*`)
  is reviewable on the architecture audit (ADR-082); no feature
  flag hides the perimeter.
- The two-step trajectory (PR-A spine → PR-A.5 typestate)
  preserves ADR-099 / ADR-101's empirical-first discipline: the
  typestate lands once the seam is alive, not pre-emptively.
- SF-6 and SF-7 are named pre-emptively; the taxonomy stays loud
  before the first failure.

**Negative / accepted.**
- One new crate (`cosmon-agent-harness`) joins the workspace. The
  blast radius of the crate boundary is bounded (depends on
  `cosmon-core` only), but the cosmon Cargo graph grows by one
  node and the architecture audit gains one row.
- The v0 spine's two-method `Provider` trait is *not* the
  typestate target. Reviewers must read both PR-A and PR-A.5 to
  see the final shape. The trajectory is named in this ADR
  specifically to make the staging visible.
- The codex subprocess Adapter (`task-20260518-9be4`) ships under
  `TmuxPane { hook_required: true }` rather than a dedicated
  `Subprocess` variant. If the codex Adapter PR discovers it
  needs `Subprocess`, ADR-101 §3 grows a row in a follow-up
  amendment ADR; the deferral is named, not silent.
- A unified `ToolCall` / `ToolResult` envelope is *not* extracted
  in v0. The two providers carry duplicated wire-serialization
  code in their `Schema` modules. This is the deliberate
  ADR-100-lineage discipline: extract only what two examples
  prove; let the rest stay duplicated until the third example
  forces the abstraction.

**Structural.**
- The four-word closure `{ Loop · Tool · Turn · Schema }` is
  closed for this perimeter. Adding a fifth primitive (the panel
  flagged Plan / Memory / Subagent / Cache as candidates)
  requires a successor ADR, not a silent introduction.
- The ADR-079 closure `{ Worker · Port · Adapter · Tier }` is
  preserved verbatim. `AgentLoop` is a *value* of *Port*, not a
  vocabulary addition.
- The ADR-101 closure (`SupervisionMode` variants) is preserved
  verbatim. `Subprocess` is named as a *deferred* variant under a
  named promotion criterion, not a silent addition.
- `cosmon-provider`'s perimeter (per ADR-043) is preserved for
  one-shot, no-loop HTTP usage. The agent loop lives one tier up
  in `cosmon-agent-harness`; R3 stays retired (ADR-100 §1).

---

## Alternatives considered

Named-for-the-record per ADR-082 INV-ADR-OPTIONS-CONSIDERED.

- **Skip the vocabulary ADR; ship the crate skeleton first.**
  Rejected. Same reason as ADR-079: the crate would need a name,
  the trait would need a name, the tool registry would need a
  shape — all without a shared referent. The five panelists
  who voted "draw the spine now" (D3) explicitly vote to draw
  *the vocabulary* now; the spine *as code* is the next bead
  (`task-20260518-1835`).
- **Promote *harness* to a primitive of the agent-loop perimeter.**
  Rejected. *Harness* is a colloquial prose word for the substrate
  (per ADR-079 §2); the typed perimeter is *Loop*. Promoting
  *harness* would force a redundant level on every type — the
  exact pleonasm ADR-079 §3 retired with `WorkerAdapter`. The
  crate is named `cosmon-agent-harness` because the colloquial
  word is the right level for the *crate name* (operator-facing
  prose); the types inside it are `Loop`, `Tool`, `Turn`, `Schema`.
- **Single monolithic Direct-API adapter (no spine).** Rejected.
  Architect §1 + forgemaster §1 + karpathy + tolnay + knuth: I4
  (message-log well-formedness) is provider-shaped; the FSM is
  not. Monolithic forces either provider-shape leakage into the
  FSM (defeats I4) or provider-shape duplication of the FSM
  (defeats reuse).
- **Per-provider clone of the entire harness
  (`cosmon-agent-harness-openai`, `-anthropic`).** Rejected as
  over-decomposition (synthesis C1; same reasoning as ADR-100 §2
  against per-provider HTTP Adapter crates). ADR-0002 §1 splitting
  rule does not fire — the duplicated surface is the FSM and the
  tool registry, both genuinely provider-agnostic.
- **Ship the typestate `Harness<S>` from PR-A.** Rejected per D6
  synthesis. Three panelists (knuth, tolnay, architect) want the
  typestate; three (torvalds + forgemaster + feynman on shape and
  budget grounds) want the spine. The cosmon-precedent
  (ADR-099/101) is *empirical-first* — typestate lands once the
  seam is alive. PR-A.5 follows; the trajectory is named.
- **Extract a unified `ToolCall` / `ToolResult` wire envelope into
  the spine.** Rejected per D3 synthesis. Forces every provider
  to round-trip through a normalized envelope; sediments OpenAI's
  `role:tool` shape into the spine; breaks I4 the day a third
  schema (Gemini, Mistral, OAuth-passthrough) lands. The
  duplicated wire code in `openai::Schema` and `anthropic::Schema`
  is the deliberate IFBDD-honouring cut.
- **Ship the codex subprocess Adapter as the v0 long-horizon
  capability instead of building the harness.** Rejected
  panel-unanimously (synthesis C7 + D8). The supervision invariants
  `{I1, I2, I3, I4}` vanish across the subprocess boundary
  (knuth §9, feynman D8). Codex gives empirical capability today;
  the harness gives supervision typestate tomorrow. Both, not
  either.

---

## Invariants

**Preserved.**
- ADR-079 §1 four-word closure `{ Worker · Port · Adapter · Tier }`
  verbatim. `AgentLoop` is a value of *Port*.
- ADR-079 §5 four-obligation Adapter contract verbatim. The
  in-process Adapters that consume the spine
  (`cosmon-transport::api::openai`, `…::anthropic`) honour all
  four obligations through their existing `Spawn` impl; the spine
  does not touch this contract.
- ADR-099 dispatch-site stability verbatim. The spine is called
  *after* the dispatch typestate has validated the adapter name;
  the spine never re-parses adapter names.
- ADR-100 §2 module layout (`cosmon-transport::api::*`) unchanged.
  The spine sits one tier up in `cosmon-agent-harness` and is
  called *from* the api modules.
- ADR-101 §1 `SupervisionMode` typestate verbatim. The
  `Subprocess` variant is named as deferred under a promotion
  criterion; not added by this ADR.
- ADR-098 §2.4 callsite-stability rule preserved. The spine calls
  `cosmon-core` emission free functions for WS-1…WS-5; it never
  contains emission callsites of its own.
- ADR-043 `cosmon-provider` perimeter unchanged for completion-API
  one-shot usage. The agent loop lives one tier up.

**Newly inscribed.**
- The four-word closed set **`{ Loop · Tool · Turn · Schema }`**
  for the agent-harness perimeter.
- **Crate `cosmon-agent-harness`** at the `cosmon-provider` tier,
  depending on `cosmon-core` only.
- **Spine extraction discipline**: draw the FSM + `MessageLog`
  trait (I4) NOW against OpenAI + Anthropic; do NOT draw a
  unified `ToolCall` / `ToolResult` wire envelope until a third
  schema lands.
- **Two-step trajectory**: minimal spine (PR-A, v0) →
  typestate-encoded `Harness<S>` (PR-A.5, v0.5). The trajectory is
  named to keep the typestate target in institutional memory
  while keeping v0 deliberately small.
- **`Subprocess` deferral with promotion criterion**:
  `SupervisionMode::Subprocess { binary, liveness_via_exit,
  wait_timeout }` is added to ADR-101 §3 if and only if the codex
  Adapter PR (`task-20260518-9be4`) demonstrates a non-tmux
  supervision path. The deferral is named.
- **SF-6 `StaleBasePatch`** — silent-failure class for hunk-based
  `edit_file` (deferred to v1; named pre-emptively).
- **SF-7 `BinaryVersionMismatch`** — silent-failure class for the
  codex subprocess Adapter (named pre-emptively so the sibling PR
  has a taxonomy slot ready; runtime check, not doc comment).
- **§8n proposed**: *the agent loop is a Port, not a sub-module
  of any HTTP adapter*. Inscribed inline for ratification in the
  next `architectural-invariants.md` update.

**Modified.** None. This ADR is additive.

---

## Implementation sequence

Documentation-only at acceptance. The implementation chain is
sequenced as nine sibling beads under the parent deliberation,
**none of which are nucleated by this ADR** — see synthesis
*"Step 4 decomposition signal"* §1–9:

1. **This ADR** (`task-20260518-ffbd`). Accept; `cs reconcile`
   updates `docs/adr/INDEX.md`; CHANGELOG entry. Vocabulary
   commit; zero code change.
2. **Crate skeleton** (`task-20260518-1835`). Create
   `crates/cosmon-agent-harness/` with `state.rs`, `harness.rs`,
   `budget.rs`, `invariants.rs`, `tools/mod.rs`. Migrate the
   existing 8-turn `run_agent_loop` from
   `crates/cosmon-provider/src/openai.rs` into the spine
   behaviour-preservingly. `BTreeMap` tool registry (S5). PR-A.
3. **`exec_command` tool with persistent `ExecSession`**
   (sibling task). PTY/shell sub-FSM; UUID sentinel prompt
   protocol; 5-min default timeout.
4. **`edit_file` tool** (sibling task). Exact-match
   search-and-replace with strict uniqueness;
   `EditError::{NoMatch, Ambiguous, AlreadyExists, Io}`. Hunk /
   unified-diff variant deferred to v1; SF-6 named in §7 above.
5. **`read_file` tool** (sibling task). Three lines on `std::fs`
   with `sanitize_join` (reused from `openai.rs`).
6. **`Bootstrapping` state** (sibling task). Walk up from
   `work_dir`, read `AGENTS.md` / `CLAUDE.md`, prepend to system
   prompt before the first turn. ~20 LOC.
7. **Anthropic spine extraction + `MessageLog` impl** (sibling
   task). Write `cosmon-provider::anthropic` against the v0
   spine; validate the trait shape against the second concrete
   schema. Closes ADR-100 §3's second-HTTP-Adapter slot.
8. **Long-horizon smoke test** (sibling task). Pick one named
   Rust refactor (rename a `pub fn` in `cosmon-core`, fix 3
   call-sites across 2 crates, run `cargo check` / `cargo test`,
   commit); end-to-end, publish trace. The "C-clamp-in-ice-water"
   demonstration. Without it, the harness ships unfalsifiable.
9. **`cosmon-transport::codex` subprocess Adapter** (sibling task,
   optional/parallel). `TmuxPane { hook_required: true }`
   default; version pin; SF-7 detector. Sibling of `claude.rs`
   and `aider.rs`. Independent PR; does not block 2–8.
10. **PR-A.5 (v0.5, follow-up bead — operator to file).**
    Promote the minimal spine to typestate-encoded `Harness<S>`.
    Sequenced **after** PR-A is alive against two providers; not
    nucleated by this ADR.

---

## References

- **Parent deliberation:**
  `delib-20260518-5178/synthesis.md`
  §C1 (crate boundary), §C5 + §S1 (hybrid spine + I4),
  §C6–C7 (codex perpendicular, not substitute), §D3 (when to
  draw), §D4 (`SupervisionMode::Subprocess`), §D6 (spine vs
  typestate trajectory), §D8 (subprocess opacity hazard), §S6
  ("two tools" category error).
- **Per-persona responses (same molecule dir):**
  `responses/architect.md` (crate boundary + `AgentLoopContext`
  shape), `responses/torvalds.md` (Q1 minimal-spine framing +
  `ExecSession` PTY/sentinel),
  `responses/forgemaster.md` (Option C hybrid + IFBDD-purer
  on D3), `responses/karpathy.md` (Layers 1–6 + `Subprocess`
  variant), `responses/feynman.md` (D3 wait-for-second-example +
  D8 opacity), `responses/tolnay.md` (RPITIT + version pin
  discipline), `responses/knuth.md` (8-state FSM + 4 invariants +
  I4 the only provider-shaped invariant + termination proof).
- **Empirical motive:**
  `/srv/cosmon/knowledge/groupe-x-ia/2026-05-18-x-ia-34-show-me-your-agent.md`
  §1 (Hajjar — Codex architecture, public reference);
  an internal academy chronicle (2026-05-18, grok-direct-api smoke result)
  (the 35c4 smoke that proved the toy harness is insufficient).
- **Source artifacts:**
  `crates/cosmon-provider/src/openai.rs`
  — the toy harness this ADR sequences the replacement of.
- **Bound ADRs:**
  [ADR-043](043-provider-abstraction.md),
  [ADR-079](079-worker-spawn-port-and-adapter-contract.md),
  [ADR-099](099-dispatch-site-stability.md),
  [ADR-100](100-direct-api-adapters-r2-amendment.md),
  [ADR-101](101-supervision-mode-typed-on-validated-adapter.md).
- **Sibling tasks** (Step 4 decomposition; siblings of this ADR's
  authoring task, all blocked by `delib-20260518-5178`):
  `task-20260518-1835` (crate skeleton, PR-A),
  `task-20260518-9be4` (`cosmon-transport::codex` subprocess
  Adapter, optional/parallel),
  plus tool implementations and the long-horizon smoke (citekey
  forms `c4d8` / `f9c7` / `6c4a` / `95cc` per the operator
  brief).
- **Authoring task:** `task-20260518-ffbd`.

---

## Tattoo

*Le harness est dans le type, le wire est dans le provider.*
