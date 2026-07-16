# ADR-106 — Adapter naming canonicalisation (D2 / D3 / D4 of `delib-20260519-a20b`)

**Status:** Accepted (2026-05-19).
**Date:** 2026-05-19.
**Decider:** Noogram.
**Empirical motive:** parent deliberation
`delib-20260519-a20b`
— *"Cosmon-doc-harness — making the harness fractal observable"*.
The synthesis surfaced four atomic decisions to the operator (D1–D4 in
synthesis §B.2). D1 (subtract three doc forms to one) is parked as a
sequencing constraint, not a name. D2 / D3 / D4 are **name choices** that
need to be frozen before the implementation children (C3, C4, C6, C7) can
ship without rework. This ADR records the operator's verdict on each.
**Authoring task:** `task-20260519-1d72` (C2 of `delib-20260519-a20b`).
**Authoring discipline:** tolnay-style name-stability discipline —
pin the rename-bait, lock the canonical, declare aliases for legacy
operator vocabulary at the CLI seam without contaminating the persisted
schema.

**Binds:**
[ADR-079](079-worker-spawn-port-and-adapter-contract.md) (`Adapter`
primitive — naming is an attribute of an Adapter row, not a new
primitive); [ADR-099](099-dispatch-site-stability.md) (TS-0 — the
`ValidatedAdapterName` newtype that this ADR populates);
[ADR-101](101-supervision-mode-typed-on-validated-adapter.md) (TS-1
`SupervisionMode` axis); [ADR-103](103-loop-ownership-axis.md) (TS-2
`LoopOwnership` axis); [ADR-068](068-ux-cli-equivalence.md) (UX ↔ CLI
parity — the `cs config adapters` subcommand inherits this rule);
[`docs/specs/CosmonDocHarness.tla`](../specs/CosmonDocHarness.tla) (C1
of `delib-20260519-a20b`, `task-20260519-aecb`) — this ADR supplies the
concrete `ProtocolStableNames` and `KebabRenameBait` constants the
TLA+ invariant `I3 — RegistryTruth` reads.

**Blocks:** C3 (`task-20260519-a226` — registry row),
C4 (`task-20260519-64e7` — `cs demo --adapter` flag),
C6 (`task-20260519-638a` — `cs config adapters` subcommand),
C7 (`task-20260519-caa4` — `man cs LOOPS` / `ADAPTERS` sections).

---

## Context

The parent deliberation panel (godin, jr, karpathy, tolnay, torvalds)
converged on a doc-harness mission with four invariants (I1–I4) but
diverged on **vocabulary**: which words appear in `man cs`, in
`cs --help`, and in the published adapter list. Tolnay's name-stability
table (synthesis §B.2 D4 / §B.3 insight #3) flagged three classes of
hazard:

1. **Section-title conflation.** `HARNESS` would name the
   `cosmon-agent-harness` crate (L-Native specifically), making the
   `man cs HARNESS` section unable to cover L-Ext or L-Composite
   without contradicting the crate name when those families ship.
2. **Top-level verb saturation.** `cs` already exposes ~80 verbs; a
   new top-level `cs adapters` consumes scarce `cs --help` real estate
   for what is structurally a projection of configuration.
3. **Kebab-rename-bait.** `llama` collides with Meta's model family
   (`--adapter llama` vs `--model llama-3-8b`); `openai` and `codex`
   are vendor / coding-agent names that future competitors will
   reuse; `anthropic` carries the same vendor-not-protocol disease.
   The persisted vocabulary (`ProviderId` in
   `crates/cosmon-provider/src/provider.rs:32`) uses serde
   `snake_case`, but the CLI vocabulary remains malleable until
   canonicalised.

This ADR locks the three decisions and publishes the canonical
**`ProtocolStableNames`** set the TLA+ skeleton reads.

---

## Decisions

### D2 — `man cs` section heading: `LOOPS` (not `HARNESS`)

**Accepted:** the man-page sections are **`LOOPS`** and **`ADAPTERS`**.

*Rationale.* `cosmon-agent-harness` is the L-Native crate specifically.
Using `HARNESS` as a section title would conflate L-Native with L-Ext
+ L-Composite; when L-Composite ships, the section cannot cover it
without contradicting the crate name. *Boucle d'agent*
(internal chronicles) is family-neutral and already in
chronicled vocabulary. Source: synthesis §B.2 D2, tolnay
`responses/tolnay.md` §4.

### D3 — Adapter list location: `cs config adapters` (subcommand)

**Accepted:** the adapter list is exposed via **`cs config adapters
[--json]`** — a subcommand of `cs config`, not a top-level verb.

*Rationale.* Adapters are *environment*, not *state*; the canonical
source `built_in_adapter_names()` (at
`crates/cosmon-core/src/spawn_seam.rs:327`) is a configuration
projection, not a separate concept. Subcommand placement honours the
top-level verb saturation argument and the ADR-068 parity-audit
discipline (a new top-level verb must earn its UI counterpart on
every pilot surface; a `cs config` subcommand inherits the existing
`config` surface). The `cs config adapters` output ships a versioned
`--json` envelope (`cs.adapters.list/v1`, tolnay §3) so column
headers never become a scraped contract. Source: synthesis §B.2 D3,
torvalds `responses/torvalds.md` §2. Godin's minority view (cut
entirely; teach via error path) is preserved as an *additive* rule:
the `cs tackle --adapter <unknown>` error path enumerates available
names regardless of whether `cs config adapters` is invoked.

### D4 — Llama adapter canonical name: `llama-cpp` (alias `llama`)

**Accepted:** the canonical name is **`llama-cpp`**; `llama` is a
**legacy alias** at the CLI seam and on disk.

*Rationale.* Bare `llama` collides with Meta's model family — the
runtime adapter (`--adapter llama`) cannot be unambiguously
distinguished from the weights name (`--model llama-3-8b`) in
operator memory or in shell history. `llama-cpp` names the in-process
engine (`llama.cpp` via the `cosmon-llama` safe wrapper) without
ambiguity. The legacy alias is mandatory because the operator's
mission text and existing chronicles use `llama` verbatim — breaking
that vocabulary at the CLI seam would erase the trail. The persisted
`ProviderId::LlamaCpp` serialises as `"llama_cpp"` and reads
`"llama"` via `#[serde(alias)]` (locked at
`crates/cosmon-provider/src/provider.rs:51–52`). Source: synthesis
§B.2 D4, tolnay `responses/tolnay.md` §2.

The registry row for both names landed via C3 (`task-20260519-a226`)
at `crates/cosmon-core/src/spawn_seam.rs:268–273` (canonical) and
`:273` (legacy alias) — the doctest at
`built_in_runtimes_cover_every_built_in_name` keeps the two tables in
lockstep.

---

## `ProtocolStableNames` — the I3 constant

The TLA+ invariant `I3 — RegistryTruth` (synthesis §B.4) reads two
constants: `ProtocolStableNames` (allowed to publish in `man cs`,
`cs --help`, guides) and `KebabRenameBait` (forbidden to publish
until canonicalised). This ADR sets both:

```tla
\* docs/specs/CosmonDocHarness.tla — populated by ADR-106
ProtocolStableNames == {
    "claude",        \* stable — frozen by ProviderId::ClaudeCode
    "aider",         \* stable — non-LLM coding agent, vendor-neutral name
    "codex",         \* legacy, kept while openai-codex ages in
    "openai-chat",   \* canonical for OpenAI chat-completions HTTP path
    "openai-codex",  \* canonical for OpenAI Codex CLI subprocess path
    "anthropic",     \* canonical — kept stable, vendor name doubles as protocol
    "ollama",        \* stable — local-daemon HTTP protocol name
    "llama-cpp"      \* canonical for in-process llama.cpp FFI (this ADR)
}

KebabRenameBait == {
    "openai",        \* until openai-chat canonicalises (alias accepted)
    "llama"          \* alias of llama-cpp (this ADR — accepted, not bait)
}
```

**Alias table** (CLI seam — operator vocabulary tolerated, persisted
schema is canonical):

| Legacy operator input | Canonical canonicalises to | Locked at |
|---|---|---|
| `--adapter llama` | `llama-cpp` | `crates/cosmon-provider/src/provider.rs:51` (`#[serde(alias = "llama")]`) + `crates/cosmon-core/src/spawn_seam.rs:273` (registry row) |
| `--adapter openai` | `openai-chat` *(provisional — only if operator confirms)* | not yet locked — `openai` remains in `BUILT_IN_AXES:253` pending operator confirmation; alias would land at a future C-class child |

The `openai → openai-chat` alias is **provisional**: the operator
reserves the right to refuse the rename for stability (state.json
files already serialise `"openai"`; the alias migration is forward
work and not in this ADR's scope). The other rename-bait names
(`codex`, `anthropic`) are kept as **stable canonical** per the
operator's accepted defaults — the synthesis flagged them as
*hazardous* but the operator's verdict is to leave them be.

---

## Consequences

1. **C3** (registry row) is consistent with this ADR — the
   `llama-cpp` canonical + `llama` legacy alias rows already shipped
   at `crates/cosmon-core/src/spawn_seam.rs:268–273` and
   `crates/cosmon-core/src/spawn_seam.rs:317–318` (runtime row).
2. **C4** (`cs demo --adapter <name>`) MUST accept both `llama-cpp`
   and `llama` (alias path) and emit the canonical name in
   `events.jsonl` `AdapterSelected` payloads — never the alias.
3. **C6** (`cs config adapters`) MUST emit `ProtocolStableNames`
   only — never the legacy aliases — under `cs.adapters.list/v1`.
   The error path (`cs tackle --adapter <unknown>`) MAY enumerate
   both canonical names and well-known aliases for discoverability.
4. **C7** (`man cs LOOPS` / `ADAPTERS`) inherits the canonical
   vocabulary from this ADR and from the C5 acceptance-test
   evidence (forensic discipline — written *from* what `events.jsonl`
   contains, not predictively).
5. **TLA+** — the `I3 — RegistryTruth` predicate is now closed
   under the constants above; C1's `docs/specs/CosmonDocHarness.tla`
   may freeze them as model values.

## Non-decisions (explicitly out of scope)

- D1 (three doc forms vs subtract to one) is a *sequencing*
  constraint, not a name — recorded in synthesis outcomes.md, not
  duplicated here.
- The `openai → openai-chat` migration (alias + persisted-schema
  rename) is forward work. This ADR names the canonical form so I3
  has a target, but does not amend `BUILT_IN_AXES` or `ProviderId`
  for the openai family.
- L-Composite Adapter family (synthesis §B.3) is not yet a registry
  row; when it ships, this ADR's name list is amended additively
  (kebab-canonical, alias only for legacy CLI seam).
