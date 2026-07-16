# ADR-043 — Provider Abstraction (multi-LLM dispatch)

- **Status**: Accepted — 2026-04-15
- **Provenance**: delib-20260415-6b9d idée #4
- **Related**: ADR-016 (Autonomy regimes), ADR-040 (Runtime cognition)

## Context

Cosmon today dispatches every worker through the Claude Code CLI over a
tmux-paste transport (`cosmon-transport::claude`). That integration is battle-tested
and has absorbed a long tail of morphological quirks — bracketed paste,
permission mode, session recovery — which we do **not** want to disturb.

Three pressures push us to introduce a formal provider abstraction now:

1. **Test the "cosmon amplifies faillible cognition" thesis.** Delib-7322
   claims cosmon's scaffolding (attestation, P_external, typed DAG, formula
   discipline) can lift a weaker model to useful output. We cannot falsify
   that claim without running a formula against a deliberately weaker
   provider.
2. **Programmatic, non-interactive callers.** Synthetic tests, cockpit
   benchmarks, and future `cs probe`-style tooling want a one-shot completion
   without spinning a tmux session.
3. **Future optionality.** Ollama local models, Gemini Flash, Mistral, xAI.
   Each can ride on the same port if we define one now; otherwise every new
   provider is an ad-hoc integration against `cosmon-cli`.

## Decision

Introduce a **new additive crate** `cosmon-provider` that defines a typed
port `LlmProvider` and ships three adapters:

- `ClaudeCodeProvider` — shells out to `claude -p` in one-shot mode. The
  default tmux-paste worker path is **untouched**; this adapter only serves
  non-interactive callers.
- `ClaudeApiProvider` — direct Anthropic HTTP API, opt-in.
- `OllamaProvider` — local HTTP daemon for weak-model experiments.

The crate lives under `crates/cosmon-provider/` and is declared in the
workspace members list. It depends on `cosmon-transport` (for future
tmux-backed adapters) but **nothing** in the core dispatch path depends on
it yet. This preserves the additive guarantee.

### Trait shape (v0)

```rust
#[async_trait]
pub trait LlmProvider: Send + Sync {
    fn id(&self) -> ProviderId;
    fn capabilities(&self) -> &Capabilities;
    async fn complete(&self, request: CompletionRequest) -> Result<CompletionResponse, ProviderError>;
}
```

- **No `anyhow` in public signatures.** `ProviderError` is exhaustive:
  `RateLimited`, `ContextOverflow`, `AuthInvalid`, `TransportFailed`,
  `ProviderSpecific`, `UnsupportedCapability`. Policy layers (retry,
  fallback) dispatch without string-sniffing.
- **Streaming deferred to v0.2.** The mission asked for streaming; we chose
  not to ship it in v0. Rationale: a `Stream<Item = ChunkResult>` shape
  needs to survive the tool-calling interleave and the cancellation story,
  both of which are themselves open design questions (cf. dust handling of
  empty deltas + tool-input JSON reassembly). Shipping `complete()` first
  lets us validate the trait boundary against three real adapters without
  prematurely freezing streaming semantics.
- **Capabilities pre-dispatch.** `Capabilities::max_context` is the basis
  for a pre-request clamp — the Dust field heuristic that replaces
  "catch overflow after the fact".

## Discipline (non-negotiable)

This crate must not leak the code hygiene issues we observed in Dust:

| Dust pattern | Cosmon stance |
|--------------|---------------|
| `anyhow::Result` in trait surface | Forbidden. Use `ProviderError`. |
| `Option<...>` fields populated by `initialize()` and then `.unwrap()`ed | Forbidden. Providers are constructed fully configured. |
| `pub mod` flat re-export | Forbidden. `lib.rs` re-exports explicitly. |
| `unwrap` / `expect` in library code | Forbidden. |
| Env-var lookups at call time | Forbidden. Credentials are constructor-injected. |

All lints from the workspace CI profile apply:
`#![forbid(unsafe_code)]`, `#![deny(missing_docs)]`, clippy `-D warnings`.

## Phasing

- **v0 (this ADR)**: `complete()` only. Three adapters. Exploration + stable
  tier tests.
- **v0.1**: wire `OllamaProvider` behind a formula-level `provider` hint so
  a single cosmon run can target the weak-model path end-to-end.
- **v0.2**: streaming. Revisit tool calling. Promote to production tier
  (mutation testing gated in CI).
- **v1.0**: split `Tool` / `Vision` / `Reasoning` sub-traits feature-gated
  by `Capabilities`. Add routing policies (cost, latency, quota).

## Alternatives considered

1. **Stay mono-provider.** Rejected: makes the delib-7322 falsification
   experiment impossible and concentrates future cloud-deploy risk in the
   tmux path.
2. **Adopt Dust's provider crate wholesale.** Rejected: its error model
   (`anyhow`), its init lifecycle (`Option` + `unwrap`), and its module
   layout conflict with cosmon's discipline. Useful as *field heuristics*,
   not as dependency.
3. **Start with an HTTP-only abstraction, forget Claude Code.** Rejected:
   breaks the additive guarantee. `ClaudeCodeProvider` is the bridge that
   lets the trait include today's reality.

## Consequences

- Three new adapters to maintain.
- New workspace member; `reqwest` enters the build graph (behind the
  `http` feature).
- The field-heuristic document
  becomes an evolving internal note, not a thesis.
- `ClaudeCodeProvider` intentionally omits the rich session lifecycle
  (permission modes, bracketed paste); callers who need that stay on
  `cosmon-transport::claude`. This duality is explicit and documented.

## Out of scope

- MCP tool-call uniformisation.
- Smart multi-provider routing (coûts, latence) — tracked under idée #3
  (smart-limits).
- Anthropic Vertex / Bedrock adapters.
- Streaming (see Phasing, v0.2).
