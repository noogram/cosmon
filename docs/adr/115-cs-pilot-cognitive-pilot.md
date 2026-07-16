# ADR-115 — `cs pilot` — the external cognitive pilot

**Status:** Accepted
**Date:** 2026-05-31
**Parent:** `task-20260531-14eb` (framing molecule — ADR + TLA+ spec only, no production Rust)
**Spec source:** `docs/delib-prep/2026-05-31-cs-pilot-external-cognitive-pilot.md`
(claw-vs-cosmon-pilot fleet, 10 agents, 2026-05-31)
**Mechanical spec:** [`specs/tla/cs_pilot_interactive_fsm.tla`](../../specs/tla/cs_pilot_interactive_fsm.tla)
(+ `.cfg`, `.check.log`) — the interactive harness FSM and its load-bearing invariant.

**Cites / complies with:**
- [ADR-096](096-openclaw-as-bibliography.md) — claw-code is **bibliography**, never dependency; forbidden vocabulary (Gateway / Sandbox / Session-as-context / Plugin / Channel / agent-as-daemon); rename-on-adopt.
- [ADR-102](102-cosmon-agent-harness-and-agentloop-port.md) — the `run_loop<P: Provider>` spine (§D-3) and the deferred `Harness<S: HarnessState>` typestate (§D-6).
- [ADR-080](080-remote-pilot-port-https-oidc.md) — Remote Pilot Port (§8j HTTPS+OIDC ingress, §8p API ⊊ CLI subset).
- [ADR-071](071-cs-ask.md) — `cs ask` stays the one-shot, rule-first fast front door.
- [ADR-016](016-autonomy-regimes-and-resident-runtime.md) — no daemon in the transactional core; Inert/Propelled/Autonomous regimes.

---

## Context

The operator's chantier: build the **external piloting tool** — the
equivalent of using Claude Code to drive the `cs` CLI, but owning our own
interactive loop, calling cosmon's **internal API directly** (not shelling
out to `cs`), usable both **locally** (inside a cosmon instance, model
in-process) and **remotely** (a thin CLI installed outside an avatar — e.g.
Tenant-Demo — to pilot it over the network).

The build-framing fleet read `instructkr/claw-code` (a clean-room Claude
Code clone, ~34k LOC Rust) as **bibliography only** under ADR-096, mapped
its capability surface against cosmon-now, and produced the spec source
cited above. This ADR records the **accepted decisions** from that study so
the build-fleet DAG (its §7) has a governance anchor; the companion TLA+
spec mechanically pins the one invariant that the whole interactive register
rests on.

### What already exists (verified substrate)

- `cosmon-agent-harness::spine::run_loop<P: Provider>` — the eight-state FSM
  as control flow (ADR-102 §D-6 PR-A), **one-shot only**: one immutable
  `SealedBriefing` in, one `Synthesis` out, **hard terminal on `Turn::Stop`**
  (`return Ok(text)`), no operator-input exit point. `MessageLog` is
  write-only (no read accessor).
- `cosmon-provider` — concrete-dispatch providers (openai / anthropic /
  ollama / llama / claude-code), no shared trait, `Capabilities` struct.
- `cosmon-rpp-adapter` (ADR-080 §8j HTTPS+OIDC), `cosmon-client`,
  `cosmon-remote`, `cosmon-saas`, `cosmon-api` — the remote wire, **all
  mechanical CRUD that shells out to `cs`**. None carries an LLM loop.
- `cs ask` (ADR-071) — rule-first, one-shot, stateless conversational
  ingress that dispatches to existing `cs` verbs.

### The gap

Nothing in cosmon owns an **interactive** cognitive loop with **mid-session
operator steering** whose tools call the internal API **directly**. That is
the gap `cs pilot` fills.

---

## Decision

### 1. Two new crates inside cosmon — `cosmon-pilot` + `cosmon-ops-tools`

**`cosmon-pilot`** — the interactive cognitive driver. Owns the
read-line → step → render REPL loop, the `PilotDirective` enum (in-REPL
meta-commands that never hit the model: `/help`, `/quit`, `/observe`,
`/compact`), and transcript append. Depends on `cosmon-agent-harness`,
`cosmon-provider`, `cosmon-core`, `cosmon-state`, `cosmon-filestore`.

**`cosmon-ops-tools`** — the cosmon-operation tool registry. Each cosmon op
is a tool (`Deserialize` input struct + `Tool::execute`) that calls
`cosmon-core` / `cosmon-state` **directly — no `cs` subprocess**. Returns a
`BTreeMap<&str, Box<dyn Tool>>` registry builder.

**Why two crates, not one, not a galaxy:**

- **Local requirement forces in-tree.** The whole value is calling
  `cosmon-core` / `cosmon-state` *directly*. That demands a Cargo dependency
  on cosmon's internal crates — only possible in-tree. A separate galaxy
  would have to shell out or go over HTTP, defeating the design.
- **Remote is a second adapter, not a second home.** Remote piloting reuses
  the same `cosmon-pilot` loop + the same `cosmon-ops-tools` *contract*, but
  swaps the tool backend from direct-internal-API to a `cosmon-client` HTTP
  call against `cosmon-rpp-adapter`. One loop, two tool-backends — a clean
  hexagonal port. This argues *for* keeping the loop in cosmon (so local and
  remote share it) and *against* growing `cosmon-client` (which stays a thin
  transport, never hosts an LLM loop).
- **Zero-I/O-core discipline + ADR-096 isolation.** Tools (which touch state)
  are separable from the driver (which owns the loop and I/O), so
  `cosmon-ops-tools` is independently testable and reusable by the remote
  adapter.

### 2. New verb `cs pilot`; `cs ask` is unchanged

`cs pilot --experimental` launches the interactive driver against the local
provider. **`cs ask` stays the one-shot fast front door** (ADR-071):
rule-first, stateless, dispatches to existing verbs. `cs ask` may, on low
confidence or an explicit `--pilot` flag, *hand off* to `cs pilot` — but it
does **not** grow a REPL (that would violate its single perimeter). The two
verbs are distinct perimeters: `cs ask` = fast reflex, `cs pilot` = sustained
multi-turn cognition.

The new verb honours the CLI doc-sync invariant: `cs help` + `man cs` land in
the same PR that ships the subcommand, and the UX↔CLI parity audit
(ADR-068) is updated.

### 3. `step()` refactor of `cosmon-agent-harness::spine`

The minimal refactor exposes a **`step()`** the REPL drives repeatedly
(mirroring claw's proven `run_turn` shape — ADR-096-cited pattern, **no claw
vocabulary imported**), with the **REPL owning the loop**. The one-shot
worker path (`run_loop`) is **preserved unchanged**: it still consumes one
`SealedBriefing` and terminates on `Turn::Stop`.

The load-bearing difference between the two callers of `step()`:

| Caller | On `Turn::Stop` |
|--------|-----------------|
| one-shot worker (`run_loop`) | **terminate** — `return Ok(synthesis)` |
| interactive REPL (`cs pilot`) | **yield to the operator** — render, then await the next operator line |

This is the invariant the TLA+ spec pins (§ below). The refactor also adds a
`MessageLog::messages()` read accessor so the REPL can render history. The
`step()` seam is the empirical dispatch site that **unblocks** the deferred
`Harness<S: HarnessState>` typestate (ADR-102 §D-6 PR-A.5) — the typestate
lands once the seam is alive, not pre-emptively. This ADR does **not**
nucleate the typestate bead; it records that the seam belongs to it.

### 4. v0 model = local Ollama, in-process

v0 runs against the existing **Ollama provider in-process** — no auth, no
cost, no network in the walking skeleton. Matches the "autonomy local-first"
posture and proves the direct-internal-API tool path end-to-end. A
`cosmon:internal` model-string routing hint (or simply passing the Ollama
provider) selects it. Anthropic-API routing is a later increment behind the
same provider dispatch.

### 5. v0 tools read-only; write tools in increment 2; `done` operator-only forever

- **v0 (increment 1):** read-only tools — `observe`, `peek`, `ensemble`. A
  pilot that can *see* the fleet is the honest walking skeleton.
- **Increment 2:** write tools — `nucleate`, `tackle` — land *with* the
  permission tiers (worker-callable vs operator-only), mapped onto cosmon's
  existing worker/human perimeter (**not** claw's `PermissionMode` /
  `DangerFullAccess` labels — ADR-096 forbids them).
- **`done` is excluded from every increment of the tool set.** Teardown
  (merge, kill tmux, remove worktree, delete branch) is an **operator-only**
  gesture (`cs done`, human-callable per the command-perimeter table). A
  cognitive pilot — local or remote — never self-destroys a molecule. This
  holds on the wire too (§6).

### 6. Remote dimension reuses `cosmon-client` → `cosmon-rpp-adapter` as a second tool-backend (v1)

The wire already exists mechanically (ADR-080 §8j HTTPS+OIDC). To carry a
**cognitive** loop the loop stays whole; only the **tool backend swaps**:

- **Local:** `cosmon-ops-tools` tools call `cosmon-core` / `cosmon-state`
  in-process. No network.
- **Remote:** the *same* `cosmon-ops-tools` interface is implemented by a
  backend that calls `cosmon-client` → `cosmon-rpp-adapter` endpoints — the
  **§8p strict subset** only (`observe` / `nucleate` / `tackle`). The model
  runs **client-side** (on the Tenant-Demo box — its own Ollama or Anthropic
  API), so the avatar stays a pure orchestrator with no inbound LLM compute,
  honouring RPP one-way topology (ADR-080 clause d). Auth is the ADR-080
  JWT (`sub → nucleon_id`); the cognitive loop adds **nothing** to the wire's
  trust model — it only emits *more of the same* admitted requests.
- **`done` / `evolve` / `complete` stay off the wire** (§8p subset),
  consistent with §5: remote `cs pilot` is **read + nucleate + tackle**;
  teardown is an operator-only avatar-side gesture.

**Follow-up (v1, not v0):** agent loops want an *await-completion* /
*stream-events* verb to avoid N polling round-trips; ADR-080 §8p has none
yet. Flagged as a successor ADR amendment (spec source §6 / §7 #6), **not a
v0 blocker** — v0 polls `observe`.

---

## Mechanical specification — the interactive harness FSM

[`specs/tla/cs_pilot_interactive_fsm.tla`](../../specs/tla/cs_pilot_interactive_fsm.tla)
models the FSM the `step()` refactor introduces. States:
`awaiting-operator-input`, `sending-to-model`, `decoding`,
`dispatching-tool`, `yield-to-operator`, `stopped`. A single `mode` variable
(`interactive` | `worker`), chosen non-deterministically at `Init`, lets one
model cover both the new REPL path and the preserved one-shot worker path.

The **load-bearing invariant**:

> **`InteractiveStopYields`** — in an interactive session, the harness is in
> `stopped` only because the operator *explicitly* quit. A model `Turn::Stop`
> never silently terminates the interactive session; it routes to
> `yield-to-operator`, which loops back to `awaiting-operator-input`.

Formally: `(mode = "interactive" /\ pc = "stopped") => operator_quit`.

The spec also pins:

- **`WorkerPathUnchanged`** — `mode = "worker"` never visits the
  interactive-only states (`awaiting` / `yield`); `Turn::Stop` terminates, as
  today. This is the mechanical statement that the refactor preserves the
  one-shot path.
- **Turn-boundedness** — `turn_count \in 0..MaxTurns`; the model↔tool
  ping-pong inside a single operator turn is bounded, and exhausting the
  budget **yields to the operator** (interactive) rather than silently
  terminating — even budget exhaustion respects `InteractiveStopYields`.
- **No-livelock** — `[]<>(pc \in {awaiting, stopped})`: the busy region
  (sending / decoding / dispatching / yield) is always eventually exited
  under weak fairness on the internal actions.
- **No-deadlock** — the only terminal absorbing state is `stopped` (intended);
  `CHECK_DEADLOCK` is `FALSE` and no-livelock is checked as a liveness
  property instead. The spec carries a dormant, constant-guarded
  `SilentTerminate` action (`AllowSilentTerminate`) that models the bug the
  invariant forbids: with it enabled, TLC produces the counterexample,
  demonstrating the invariant is load-bearing rather than vacuous.

TLC run log: [`specs/tla/cs_pilot_interactive_fsm.check.log`](../../specs/tla/cs_pilot_interactive_fsm.check.log).

---

## Coherence checklist (architectural-invariants.md)

1. **Stateless?** `cs pilot` is a foreground interactive process, stateless
   between invocations save for an on-disk transcript — no daemon (ADR-016,
   ADR-096 anti-daemon refusal). ✓
2. **Idempotent?** Read tools (v0) are pure projections. Write tools
   (increment 2) inherit `nucleate` / `tackle` idempotency. ✓
3. **Regime-aware?** `cs pilot` is a **Propelled** human-driven gesture; it
   does not introduce the Autonomous runtime. ✓
4. **Single perimeter?** New verb, distinct from `cs ask` (one-shot) and
   `cs run` (DAG). `cs ask` keeps its perimeter. ✓
5. **Symmetric undo?** v0 read-only creates no state. `tackle` (increment 2)
   already has `cs done` as its reverse — kept operator-only. ✓
6. **Runtime-compatible?** The `step()` seam is exactly what the future
   resident runtime's policy layer drives; it does not pre-empt it. ✓
7. **Worker/human boundary respected?** `done` excluded from the tool set;
   teardown stays operator-only, on the wire too. ✓
8. **Write-read asymmetry preserved?** Read tools and write tools are
   separate tools; no tool writes state and returns a coupling report. ✓
9. **Merge-before-dispatch respected?** `cs pilot` does not merge or
   dispatch DAGs; `cs run` keeps that role. ✓
10. **CLI-first for workers?** Tools call the **internal API directly**, not
    MCP and not `cs` subprocess — stronger than CLI-first, and consistent
    with the `cosmon-mcp` deprecation (ADR-096 §3 refusal of MCP-as-worker-path). ✓

---

## Consequences

### Good

- One interactive loop serves both local (in-process model, direct tools)
  and remote (client-side model, RPP tool-backend) without forking.
- The `step()` seam unblocks ADR-102's deferred typestate without forcing it
  prematurely.
- The load-bearing yield invariant is mechanically pinned before any Rust is
  written — the FSM cannot regress to silent termination unnoticed.
- `cs ask` keeps its single, fast perimeter; cognition is a separate verb.

### Acceptable cost

- Two new crates to maintain. Justified by the zero-I/O-core split and
  remote-backend reuse.
- The `step()` refactor touches the harness hot path; the worker one-shot
  path must stay byte-for-byte behaviour-identical, mechanically asserted by
  `WorkerPathUnchanged` and the existing `run_loop` tests.

### Acknowledged limit

- v0 is a walking skeleton: local-only, read-only, plain text, no streaming,
  no session save beyond the transcript. Streaming, permissions UI, cost
  tracking, and the remote backend are later increments (spec source §7).
- The remote await-completion verb (§6) is an unsolved efficiency gap, not a
  correctness gap; v0 polls.
- **Per-request timeout vs local prefill cost (task-20260601-2940).** The
  pilot injects the *entire* repo `CLAUDE.md` (~36 KB) as opening bootstrap
  context, so on a local model prefill dominates the first round-trip. The
  smoke test (task-20260601-71c7) measured `qwen2.5:32b` — the instruct model
  with the *best* tool-calling quality — at ~190s for that first round-trip,
  which silently tripped `OpenAIProvider`'s 60s library default and killed
  `cs pilot` with `openai http error: error sending request`. Net effect: the
  best local pilot model was silently un-usable; only smaller fast models
  (`qwen3:8b`, ~48s) fit under 60s. v0 fixes the *symptom* with a
  `--timeout` flag + `COSMON_PILOT_TIMEOUT` env defaulting to **300s** for
  the local-first path. The deeper *cause* — re-injecting the full briefing
  every turn — is left as a later increment: the bootstrap context should
  likely be summarized or made opt-in for the pilot path, since for a local
  model it is the dominant cost. Tracked as a `temp:warm` follow-up, not a v0
  blocker.

---

## Related

- Spec source: `docs/delib-prep/2026-05-31-cs-pilot-external-cognitive-pilot.md`
- [ADR-096](096-openclaw-as-bibliography.md), [ADR-102](102-cosmon-agent-harness-and-agentloop-port.md), [ADR-080](080-remote-pilot-port-https-oidc.md), [ADR-071](071-cs-ask.md), [ADR-016](016-autonomy-regimes-and-resident-runtime.md), [ADR-068](068-ux-cli-equivalence.md)
- [`docs/architectural-invariants.md`](../architectural-invariants.md) — §8j / §8p (remote), the command perimeters, the coherence checklist.
- TLA+ spec: [`specs/tla/cs_pilot_interactive_fsm.tla`](../../specs/tla/cs_pilot_interactive_fsm.tla)
