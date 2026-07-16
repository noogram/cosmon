# ADR-150 — Directional routing is a named policy over Incarnation

**Status:** Accepted (2026-07-11)  
**Decision owner:** Noogram  
**Origin:** C1 of `delib-20260711-c6c8`  
**Depends on:** ADR-004, ADR-079, ADR-100, ADR-106, ADR-142

## Context

Cosmon already resolves the launch-time `Incarnation` axes at `cs tackle`:
adapter and model have a single precedence fold, formula steps can pin both,
and every carrier terminates at the Worker-Spawn Port. What is missing is a
directional policy: a workstream or task type should be able to opt into a
named `(adapter, model, effort)` preference without installing one provider as
the galaxy-wide default.

A second selector in transport would split provenance and precedence. A new
core router would also violate formulas-first: the existing formula step is
already the workstream/type boundary and already carries the two live slots.

## Decision

Directional routing is a **named, positively selected policy which produces a
partial Incarnation**. It is policy above the existing selector, not a new
core primitive and not transport logic.

For the current release, the named policy is a formula (or a shared formula
fragment) whose worker steps pin `adapter` and/or `model`. Workstream and task
type select that formula explicitly at nucleation. No implicit classifier and
no global provider profile are introduced. This reuses the shipped schema and
the single `resolve_adapter_selection` / `resolve_model_selection` seam with
zero new CLI surface.

The effective precedence is:

```text
explicit tackle flag > literal formula-step pin > env > galaxy config
> global config > safe floor
```

When a future profile catalog is justified, a formula may name a profile and
the fold becomes `flag > literal formula pin > named profile > env/config >
floor`. A literal pin and a profile value are not a conflict: the literal pin
wins and provenance records both. Two profile values for the same slot are a
typed, pre-spawn conflict; profiles never use last-writer-wins. Requirements
compose monotonically by union and can never be weakened by a profile.

This staged decision is deliberate: today's formula pin is already the named
directional policy producer. A catalog is additive syntax only when reuse
pressure proves it; it must feed the existing fold rather than create a
parallel selector.

## Carrier parity

A carrier is eligible for a policy slot only if it transports that slot to its
native launch surface. Registration alone is insufficient.

| Carrier | adapter | model | Mechanism |
|---|---:|---:|---|
| `claude` | yes | yes | launch-scoped `ANTHROPIC_MODEL` |
| `aider` | yes | yes | native model argument |
| `codex` | yes | yes | native `--model` in interactive and exec modes |
| `openai` | yes | yes | direct API request model |
| `anthropic` | yes | yes | direct API request model |
| `opencode` | yes | no | ineligible for model-pinning profiles |
| `local` / `llama-cpp` | yes | implementation-specific | eligible only for slots their adapter consumes |

This ADR closes the `codex` gap by threading the common resolved model into
`CodexSessionConfig` and rendering `--model` in both launch modes. Tests assert
both carriers. Unsupported tuples fail eligibility; they never silently drop a
slot.

## Aggregator venues

OpenRouter and LiteLLM are **venues** in Smart Order Routing terminology, not
new carriers. Both expose an OpenAI-compatible endpoint and therefore attach
to the existing `openai` adapter with **zero code**:

```toml
[adapters.openai]
base_url = "https://openrouter.ai/api"
api_key_env = "OPENROUTER_API_KEY"
default_model = "<venue model id>"
```

```toml
[adapters.openai]
base_url = "http://127.0.0.1:4000"
api_key_env = "LITELLM_API_KEY"
default_model = "<proxy model id>"
```

OpenRouter is a hosted, pay-per-use venue spanning many models. LiteLLM is a
self-hosted proxy which unifies providers. They let operators trial models
(for example GLM 5.2) per use before buying a subscription. Venue identity is
the endpoint tuple `(adapter, base_url, model family)`, never the adapter name
alone; aliases behind one endpoint do not create provider diversity.

Secrets remain environment references. Formulas may select an adapter/model,
but never embed API keys or mutate `base_url`.

## Conflict and failure rules

1. Explicit flags and literal formula pins are authoritative and attributable.
2. Unknown adapters fail before worktree/tmux side effects.
3. A model pin on a carrier without a model carrier is ineligible and fails
   closed; it is never ignored.
4. Conflicting named profiles for one slot fail before spawn and name both
   sources.
5. Requirements join monotonically; no routing preference removes a hard
   requirement.
6. Absence selects the existing safe floors. It never installs a global
   provider preference.

## Verification contract

- precedence remains covered by the tackle resolver unit tests;
- conflict behavior is structural today because one formula step contains at
  most one literal value per slot; a future profile catalog must add an
  explicit two-profile conflict test before shipping;
- carrier parity is tested at each command-builder seam; this change adds the
  missing codex interactive/exec model test;
- OpenRouter/LiteLLM require no dispatch arm and must continue to work through
  `[adapters.openai].base_url`.

## Consequences

Directional routing stays stateless, formula-selected, auditable, and cheap to
remove. C3 may add a SOR policy that ranks eligible venues, but its output must
still be one partial Incarnation entering this same fold. C4 may add committee
requirements, but cannot weaken carrier eligibility or venue-diversity rules.

