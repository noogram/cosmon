# ADR-118 — LLMPort doctrine + graceful-degradation matrix

**Status:** Proposed (filed for operator decision — `task-20260514-51e9`)
**Date:** 2026-06-05
**Parent:** `task-20260514-51e9` (🔧 task), itself born of
`idea-20260423-abad` — Étienne Lempereur's question on compute lockout,
corroborated by the Mensch audition.
**Kind:** ADR-grade because it ratifies a federation-wide *port* doctrine
(which abstraction is the LLM boundary, and where adapters live) and adds
a **doctrine matrix** that governs how cosmon behaves under degraded
compute. Per CLAUDE.md — *"Do not backdoor architectural changes through
individual PRs"* — the doctrine is filed as a decision; only the small,
already-live `Capabilities` query methods are shipped as code.

**Cites / complies with:**

- [ADR-043](043-provider-abstraction.md) — the original provider-abstraction
  rationale and the *"does cosmon's scaffolding amplify a faillible
  cognition?"* experiment that the open-weights adapters exist to run.
- [ADR-100](100-direct-api-adapters-r2-amendment.md) — direct-API adapters,
  R2 *no-network-at-inference* for the in-process llama path.
- [ADR-092](092-license-bascule-mpl-to-agpl.md) — `cosmon-provider` is a
  permissive **frontier crate** (Apache-2.0); the doctrine must not push
  AGPL-coupled infrastructure into it.
- `docs/architectural-invariants.md` — the coherence checklist
  (stateless, zero-I/O-in-core, single-perimeter, no speculative
  scaffolding).
- chronicle `2026-05-19-w6-speculative-rip.md`
  — the rip of the `LlmProvider` trait; this ADR is bound by it.
- **noogram ADR-049** (LLMPort, *never materialised*) and the noogram
  **compute-sovereignty red-line** (open-weights degraded mode +
  bare-metal) — the conceptual ancestors this ADR finally grounds in
  cosmon.

---

## Context

### The question that started it

`idea-20260423-abad` recorded Étienne Lempereur's worry, sharpened by the
Mensch audition: **what happens to a fleet whose cognition lives entirely
behind one company's API the day that API says no?** Rate limit, price
hike, policy refusal, account lockout, sanctions — the failure mode is not
*"the model is worse today"*, it is *"the model is gone"*. A fleet that
cannot orchestrate without a frontier API is a fleet with a single point
of capture.

The molecule brief asked for three things:

1. a `LLMPort` trait — zero I/O in the core, one adapter per provider;
2. a **second, open-weights, *activable*** adapter (a port with one
   adapter is *vapeur architectural*);
3. a **graceful-degradation matrix** — which `cs` verbs stay reliable on a
   weaker model, which disable.

### What the fleet already built (the molecule was overtaken)

The molecule is an April-23 idea. By the time it was tackled (2026-06-05),
three weeks of fleet work had already shipped most of it. An honest worker
does not rebuild what exists — that is how you reintroduce the empty
closets the 2026-05-19 rip just swept out. The ground truth:

| Brief item | Status on 2026-06-05 | Where |
|---|---|---|
| LLMPort trait, zero-I/O in core | **Already exists** | `cosmon_core::llm::LlmBackend` (`crates/cosmon-core/src/llm.rs`) |
| Second open-weights adapter, activable | **Already exists** (two of them) | `cosmon_provider::OllamaProvider` (feature `http`), `cosmon_provider::LlamaProvider` (feature `llama`) |
| Graceful-degradation matrix | **Did not exist** | ← the genuine gap this ADR fills |

So the contribution of `task-20260514-51e9` is **not** new infrastructure.
It is (a) *consolidating the doctrine* that was scattered across two
crates and one deletion, (b) *documenting activation* of the existing
open-weights adapters, and (c) *shipping the degradation matrix* — the one
artefact that was truly missing.

### The two-abstraction confusion this ADR resolves

A future reader hits an apparent contradiction and must not mis-resolve
it:

- `cosmon-core/src/llm.rs` defines a **trait** `LlmBackend` (the port),
  with one stub adapter (`cosmon_bridge_claude::AnthropicSubprocess`,
  which returns `Unavailable` until V1).
- `cosmon-provider` has **real concrete adapters** (`claude_code`,
  `claude_api`, `ollama`, `llama`, `openai`) but **no shared trait** — its
  `LlmProvider` trait was *deliberately deleted* on 2026-05-19 because a
  kill-switch grep showed zero live callers; dispatch in `cs tackle` is a
  five-line `match adapter.as_str()`.

The naive reading — *"there are two competing LLM abstractions, unify
them"* — is **wrong** and would resurrect a pruned closet. The correct
reading is the doctrine below.

---

## Decision

### D1 — `cosmon_core::llm::LlmBackend` is *the* LLMPort.

The port (hexagonal sense) is the **trait in the core**: `LlmBackend`,
zero-I/O, object-safe via `async-trait`, taking a `CompletionRequest` +
`TenantContext` and returning `CompletionResponse | LlmError`. Every
concrete adapter that wants to be a *first-class, runtime-swappable*
backend (`Arc<dyn LlmBackend>`) implements this trait **outside** the
core. This is where BYOK, billing, streaming, and tool-use will thread
through (the `#[non_exhaustive]` V0 shapes already reserve the room — see
`delib-20260503-8127` §10).

### D2 — `cosmon-provider` adapters stay *trait-free at the crate
boundary*; the provider `LlmProvider` trait stays dead.

`cosmon-provider` is a permissive **frontier crate** (Apache-2.0, ADR-092)
whose adapters are consumed today by **static dispatch on concrete
types** (`match ProviderId`). The 2026-05-19 rip proved the shared
provider trait earned nothing: nobody passed through that door. **This ADR
forbids re-adding it.** When a `cosmon-provider` adapter needs to be a
`dyn LlmBackend`, the binding is an *adapter-of-adapter* shim that
`impl LlmBackend for OllamaProvider {…}` at the **call site that needs the
trait object** — written only when a live caller exists, never
speculatively.

> **Why two layers and not one?** The core port (`LlmBackend`) is the
> *domain contract* — minimal, zero-I/O, the thing the runtime depends on.
> `cosmon-provider` is a *reusable adapter library* a third party can
> `cargo add` without dragging in `cosmon-core`'s domain. Collapsing them
> would either pull the domain into the frontier crate or pull the
> frontier crate into the core. The seam is load-bearing; keep it.

### D3 — the open-weights deliverable is **met**; this ADR documents
*activation*, not construction.

cosmon has **two** open-weights paths today, both activable:

- **`OllamaProvider`** (feature `http`, *on by default*) — talks to a
  local Ollama daemon (`http://localhost:11434`). Zero build chain. The
  cheapest way to run cosmon against a deliberately weak model
  (Llama 3.2 3B, Mistral Nemo, Qwen3) — exactly ADR-043's experiment.
- **`LlamaProvider`** (feature `llama`, *off by default*) — in-process
  llama.cpp via the safe `cosmon-llama` wrapper. **No network at inference
  time** (ADR-100 R2): the only I/O is the initial mmap of the GGUF. Off
  by default so `cargo add cosmon-provider` does not pull a C++ toolchain.
  Carries GBNF grammar support (`cosmon-llama/src/gbnf.rs`,
  `cosmon-provider/src/llama/tool_grammar.rs`) — the load-bearing fact for
  `StructuredExtraction` below.

**Activation recipe:**

```bash
# Path A — Ollama (no build chain):
ollama pull qwen3:14b           # or llama3.2:3b for the weak-model floor
cargo build -p cosmon-provider  # `http` is default-on; nothing to enable
# adapter: cosmon_provider::OllamaProvider::new()  → localhost:11434

# Path B — in-process llama.cpp (bare-metal, network-free):
cargo build -p cosmon-provider --features llama
# adapter: cosmon_provider::LlamaProvider over a local GGUF path
```

`ProviderId::LlamaCpp` is **always present** in the persisted enum (state
cannot diverge by build flag); `ProviderId::ensure_compiled()` is the
runtime boundary that turns a *"feature not built"* mismatch into a typed
`ProviderError::FeatureNotCompiled` rather than a panic.

### D4 — the graceful-degradation matrix (the genuine new artefact).

The matrix's first job is to **dissolve a false fear**. *Most* `cs` verbs
do not touch an LLM at all. The cosmon **control plane** —
`nucleate`, `evolve`, `tackle`, `done`, `observe`, `reconcile`, `tag`,
`freeze`, `collapse` — is a typed state machine over JSON files on disk.
It runs with a weak model, a stub model, or **no model at all**. The
degradation question is therefore *not* about the control plane; it is
about the **worker cognition** inside a tackled molecule, and the handful
of LLM-backed features.

We group every LLM-touching cosmon feature into five **verb-classes**, and
every backend into three **tiers** derived purely from advertised
`Capabilities` (no probe):

- **Local** — small, tool-less model, short window (Llama 3.2 3B class).
- **Mid** — capable open-weights: tool-calling *or* a roomy (`≥16k`)
  window (Qwen3 14–32B, Mixtral, a generous Ollama deployment).
- **Frontier** — large window (`≥100k`) *and* robust tool-calling
  (Claude / GPT-4 class).

| Verb-class | Examples | Local | Mid | Frontier |
|---|---|:---:|:---:|:---:|
| **ControlPlane** | `nucleate` `evolve` `tackle` `done` `observe` `reconcile` `tag` `freeze` | ✅ Reliable | ✅ Reliable | ✅ Reliable |
| **FreeformGeneration** | worker note draft, one-shot summary, `cs ask` | 🟡 Degraded | ✅ Reliable | ✅ Reliable |
| **StructuredExtraction** | session-route tier-2, `spec_audit`, triage classify | 🟡 Degraded¹ | ✅ Reliable | ✅ Reliable |
| **MultiStepAgentic** | `deep-think` panel, mission decompose, fleet-validator L1–L5 | ⛔ Unavailable | 🟡 Degraded | ✅ Reliable |
| **LongContext** | review over a large diff, whole-corpus questions | ⛔ Unavailable² | 🟡 Degraded | ✅ Reliable |

¹ *Local `StructuredExtraction` is reliable **only** behind a GBNF /
tool-schema guard rail (which `LlamaProvider` carries); an unconstrained
weak model free-forms past the schema — hence Degraded, not Reliable.*
² *`LongContext` is additionally floored at 32 768 tokens: below that
window it is Unavailable regardless of tier — the prompt simply does not
fit.*

**Reading the matrix as policy:**

- **✅ Reliable** — route freely.
- **🟡 Degraded** — works with a guard rail (GBNF, smaller batch, retry,
  human spot-check) and reduced quality. Expect rougher output, *not*
  failure. A degraded feature should *warn*, not *block*.
- **⛔ Unavailable** — the feature should **disable** rather than emit
  confident garbage. A `deep-think` panel on a 3B model is worse than no
  panel: it manufactures false consensus.

### D5 — the matrix is shipped as an *executable twin*, not just prose.

The table above is encoded once in
`crates/cosmon-provider/src/degradation.rs` as pure functions of
`(tier, class)` plus two inherent query methods on the **already-live**
`Capabilities` type — `degradation_tier()` and `reliability_for(VerbClass)`
— siblings of the existing `can_fit()`. Unit tests pin every cell so the
prose table here cannot silently drift from the constant the system
reasons with.

> **Anti-scaffolding compliance (binding on the reviewer).** Per the
> 2026-05-19 rip, new abstraction must justify itself. This artefact is
> **not** a new trait, dispatch table, or `cs` verb. It is two pure
> methods on a type whose sibling method (`can_fit`) is live, plus the
> enums they return. It has **no production caller yet**; its consumers
> today are its own tests (which pin the doctrine) and this ADR. The
> intended live consumer is a future `cs doctor --model <provider>` /
> model-routing readout — filed as a `temp:warm` follow-up, *not* built
> speculatively here. This declaration is the up-front version of the
> chronicle the rip asks for: if and when a caller lands, the closet was
> never empty.

---

## Consequences

**Positive**

- The two-abstraction confusion is settled in writing; future readers
  route correctly instead of "unifying" a pruned trait.
- The compute-sovereignty floor is now *explicit and tested*: cosmon's
  orchestration survives a total frontier-API lockout; only worker
  cognition degrades, and the matrix says exactly how.
- The open-weights deliverable is documented as *activable today*, with a
  copy-paste recipe — no new code needed to honour the brief's
  *"non-vapeur"* demand.

**Costs / risks**

- The tier heuristic reads *advertised* capability, not real model size.
  An adapter that over-claims `max_context` will be mis-tiered; honesty in
  the advertisement is the operator's lever (documented on
  `degradation_tier`).
- The matrix is a **routing aid, not a guarantee**. A Mid model can still
  botch a `StructuredExtraction`; the matrix lowers the odds, it does not
  remove the verdict-door.

**Follow-ups (filed, not built here)**

- `temp:warm` — `cs doctor --model <provider>`: print the degradation
  readout for a configured backend (the first live consumer of D5).
- `temp:warm` — when the runtime accepts `Arc<dyn LlmBackend>` for
  open-weights, write the `impl LlmBackend for OllamaProvider` shim **at
  that call site** (D2), with a test.

---

## Alternatives considered

- **Rebuild the `LLMPort` trait + a fresh open-weights adapter as the
  brief literally says.** Rejected: it duplicates `LlmBackend` +
  `OllamaProvider`/`LlamaProvider` and resurrects the trait the
  2026-05-19 rip deleted. The brief was written before that work existed.
- **Ship the matrix as prose only (this ADR, no code).** Rejected: the
  brief is emphatic that a port with one adapter is *vapeur*; a matrix
  that lives only in Markdown drifts. The executable twin (D5) is the
  minimum that keeps doc and code honest, at near-zero new surface.
- **Encode the matrix in `cosmon-core` instead of `cosmon-provider`.**
  Rejected: the matrix reasons over a backend's *advertised
  capabilities*, which is a `cosmon-provider` concept (`Capabilities`).
  Putting it in the core would either duplicate that type or pull
  provider concerns into the domain.
