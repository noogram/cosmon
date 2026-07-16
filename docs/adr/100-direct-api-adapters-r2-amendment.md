# ADR-100 — Direct-API Adapters proceed (R2 amendment to ADR-098 §5)

**Status:** Accepted (2026-05-18).
**Decider:** Noogram.
**Parent deliberation:**
`delib-20260518-cf2e`
— four-persona mini-panel (architect, forgemaster, torvalds, knuth)
re-deliberating the 2-2-1 tie of `delib-20260518-20a4`. Tally:
R3 rejected 4/4, R2 wins 2-1 over R1' (architect + forgemaster vs
torvalds), knuth orthogonal (TS-0 prerequisite). The conditional R2
chain (synthesis §5.4) is promoted by the operator answering **yes**
to *Q-cf2e-load-bearing* on 2026-05-18.

**Empirical motive:** the academy smoke test
`task-20260518-b6d7`
chronicled in the internal chronicles put a sensor in the L9 window
between ADR-098 §C6 (cat-test) and §C8 (multi-Adapter wiring) and
forced forgemaster's flip from R3 to R2 by ruling out his previous
*"co-located in the Resident Runtime address space"* argument.

**Amends:** [ADR-098](098-worker-spawn-port-operationalisation-ifbdd.md)
§5 (Cargo-tier discipline) and §7 (non-acceptance list). Replaces §5's
blanket refusal of *"Anthropic API direct … or any other completion-only
substrate"* with a narrow, feature-gated acceptance scoped to HTTP
**agent-loop** Adapters. ADR-098 §§1–4 and 6 carry forward verbatim.

**Binds:** [ADR-079](079-worker-spawn-port-and-adapter-contract.md)
(four-word closure preserved; HTTP Adapters are *values* of `Adapter`);
[ADR-095](095-resident-runtime-ifbdd-path.md) RR-1 read **behaviourally**
(*"mutations via CLI shell-out only"*, synthesis §5.3 default); ADR-099
(Dispatch-site stability, sibling `task-20260518-d670` in flight) —
universal prerequisite; TS-0 + `WorkerSpawned.adapter_name` already
shipped under C8 (`task-20260518-958e`, merged 2026-05-18);
[ADR-043](043-provider-abstraction.md) — `cosmon-provider` retains
its completion-API home role; this ADR does **not** move the loop into
it (R3 foreclosed by ADR-0002 §1 + RR-1 behavioural).

---

## Context

ADR-098 §5 drew a hard line between subprocess CLI Adapters
(`cosmon-transport`) and completion-API providers (`cosmon-provider`)
on a stated *+2–4 MB* binary-delta ground. Two signals materialised
within 36 hours:

1. **The dep-cost figure was wrong** (synthesis C4). `reqwest v0.12.28`
   already sits in `cosmon-transport`'s transitive graph via
   `cosmon-state → cosmon-thin-cli`. Real net new deps (rustls-family,
   ~9 crates) sum under 1 MB. §5's economic argument does not hold;
   the structural argument (subprocess vs HTTP is a real category
   split) does.
2. **The academy smoke chronicle.** `task-20260518-b6d7` exhibited the
   gap between signalled and realised behaviour and re-framed the
   Adapter-extension question. Once the dispatch site is type-stable
   (ADR-099 + TS-0), the cost of a second HTTP Adapter is dominated
   by the agent-loop body (~200 LOC), not by a category-defending
   crate boundary.

The panel re-deliberated under these two signals plus ADR-095's
acceptance, which the operator had read as a benefit-for-R3
(*"the loop lives in the same address space as the DAG scheduler"*).
Architect and forgemaster independently traced the dep chain
`cosmon-runtime → cosmon-provider → cosmon-transport → cosmon-state`
and showed RR-1 (literal or behavioural) **forbids** that co-location:
under both readings the agent loop lives in a spawned worker process,
never in the Resident Runtime address space. This dissolved the
load-bearing argument of forgemaster's previous R3 vote.

---

## Decision

### 1. R3 retired from the menu

4/4 panel-unanimous rejection. The three R3-escapes (split-A, split-B,
Adapter-in-provider) all fail — the first two violate ADR-0002 §1;
the third regresses SF-0 to a runtime registry. R3 is **not** parked
as future-conditional; it is named-superseded with the RR-1 ruling +
the academy b6d7 chronicle as joint evidence. Future deliberations
must not re-propose R3 unless ADR-0002 §1 or ADR-095 RR-1 are
themselves successor-amended.

### 2. R2 selected — narrow, feature-gated

`cosmon-transport` is extended with an HTTP **agent-loop** Adapter
family under a Cargo feature gate. The gate preserves ADR-098 §5's
lean-binary intent while admitting the schema-heterogeneous case:

```toml
# crates/cosmon-transport/Cargo.toml (sketch)
[features]
default = []
http-adapter = ["dep:reqwest", "dep:rustls", "dep:rustls-pemfile"]
```

Under `--no-default-features`, HTTP Adapter modules compile out; the
legacy three-dep surface (`cosmon-core`, `cosmon-bridge-claude`,
`thiserror`) is preserved. The musl-static lean-path benchmark of
ADR-098 §5 remains the default; the HTTP path is opt-in.

Cargo module layout (forgemaster nomenclature, against
one-crate-per-provider over-decomposition):

```
crates/cosmon-transport/src/api/
  mod.rs           // feature-gated; re-exports openai, anthropic
  openai.rs        // first HTTP Adapter — see §3
  anthropic.rs     // second HTTP Adapter — see §3
  common.rs        // shared HTTP client builder, retry, rate-limit
```

`api::openai` and `api::anthropic` are **modules** of the existing
`cosmon-transport` crate, not new crates. `cosmon-runtime`'s relation
to `cosmon-transport` is unchanged.

### 3. Adapter sequencing — OpenAI first, Anthropic API direct second

**First HTTP Adapter: OpenAI** (closes panel question Q-T5).

- OpenAI's tool-call schema is structurally distinct from Claude's
  content-block schema. Schema heterogeneity from day one forces the
  Worker-Spawn Port to express the divergence in its trait, not in
  Claude-shaped defaults a second Anthropic-like Adapter would conceal
  (forgemaster's IFBDD-purer argument, panel-endorsed).
- Grok, Kimi, Mistral, DeepSeek expose OpenAI-compatible endpoints
  and free-ride on the OpenAI Adapter via per-Adapter `base_url`
  config, satisfying academy jalon-2 across model families without
  N separate Adapter modules.

**Second HTTP Adapter: Anthropic API direct.**

- Provides a **regression baseline** against the existing subprocess
  Claude Adapter. Lineage equivalence (modulo `adapter_name` and
  transport-layer fields) is exactly the test ADR-098 §6 Trigger #3
  inscribes (*"second-Adapter lineage 100% identical to first"*) —
  two Adapters converging on identical `events.jsonl` lineages give
  the empirical instrument §6 expected to never have before the
  90-day window closed.
- Closes academy L3 *"open-weights as symmetry lever"* natively;
  the federation does not depend on Aider's release cadence.

Codex (a) and additional CLI subprocesses (Goose, Cline) remain
ADR-098 §7's deferred Adapter-#3-or-later slots.

### 4. Dep-cost correction to ADR-098 §5

The §5 table line *"Anthropic API direct: +2–4 MB"* is empirically
wrong. Corrected figure (rustls-family delta only; `reqwest` already
transitively present via `cosmon-state → cosmon-thin-cli`):

| Candidate | Net new deps under `--features http-adapter` | Binary delta (musl static, feature on) |
|-----------|---|---|
| Aider (subprocess CLI) | 0 | 0 bytes |
| OpenAI (HTTP agent loop) | rustls-family ~9 crates | ~0.6–0.9 MB |
| Anthropic API direct | shared with OpenAI | 0 bytes incremental |

Agent servers that do not want the HTTP surface compile with
`--no-default-features` and the 0-byte path is preserved verbatim.

### 5. RR-1 read behaviourally (named for the record, not gating)

ADR-095 RR-1 is read in its **behavioural** form: *"the Resident
Runtime does not mutate cosmon state except via CLI shell-out."* The
literal reading (*"never imports `cosmon-state`, `cosmon-filestore` …"*)
is not adopted because the existing `cosmon-runtime` skeleton imports
`cosmon-state` and `cosmon-filestore` for read-only StateStore /
event_log access; the literal text would fail `cargo check` on day
one. This is a **default for the next ADR-095-touching ADR**, not a
re-amendment of ADR-095 itself. Auxiliary question Q-arch-cf2e
closed by this default.

### 6. SF-0 / C8' / TS-0 — universal prerequisite, already in motion

The IFBDD pact (*"the instrument exists before the behaviour"*) is
honoured by stacking on three prerequisites:

- **C8 spawn dispatch** (`task-20260518-958e`, merged 2026-05-18) —
  `spawn_and_prompt` dispatches on `adapter_name`; Aider branch
  reachable; `EventV2::WorkerSpawned.adapter_name` ships so the
  cat-test is empirically falsifiable from disk.
- **ADR-099 dispatch-site stability** (`task-20260518-d670`, in flight)
  — typestate making SF-0 *impossible to compile*. HTTP Adapters
  inherit the invariant for free.
- **Aider as first non-Claude Adapter** (ADR-098 C4, PR-3) — the
  subprocess-CLI sibling proving the trait is not single-Adapter-only.

WS-1…WS-5 (ADR-098 §1) ship verbatim in `cosmon-core` emission
infrastructure; HTTP Adapter modules **call** the stable free
functions. The callsite-stability rule (ADR-098 §2.4) is preserved.

### 7. Scope boundary — narrow acceptance

This ADR amends ADR-098 §5 **only** for HTTP **agent-loop** Adapters
(multi-turn agentic execution, tool-call orchestration, persistent
context within a single worker session) that satisfy the four
ADR-079 §5 Adapter obligations. It does **not** re-open:

- One-shot completion-API substrates with no agent loop
  (`cosmon-provider`'s perimeter under ADR-043).
- Adapters that bypass the briefing-seal discipline.
- Adapters that share an address space with `cosmon-runtime`.

---

## Consequences

**Positive.** Academy L3 *"open-weights as symmetry lever"* honoured
natively. Schema-heterogeneity discipline lands from day one (OpenAI
first). ADR-098 §6 Trigger #3 gains its expected instrument
(Claude-subprocess vs Anthropic-API-direct lineage comparison). IFBDD
pact preserved (WS-1…WS-5 ship before HTTP bodies; TS-0 + ADR-099
ship before the dispatch sites consuming them). R3 structurally
retired with explicit evidence; future deliberations are not
re-litigating it.

**Negative / accepted.** `cosmon-transport` carries a default-off
feature gate; operators forgetting `--features http-adapter` get a
clean compile error at the dispatch site (under ADR-099's typestate),
not a silent fall-through. Two HTTP Adapters land within the 90-day
window — Triggers #1 and #2 become testable against three Adapters
plus a regression baseline; the forensic budget triples,
intentionally. The lean-binary musl-static promise becomes
conditional on `--no-default-features`.

**Structural.** `cosmon-transport`'s perimeter is **no longer closed**
for HTTP-client deps, but closure is preserved under
`--no-default-features` — the smallest §-leak admitting the
schema-heterogeneous family while keeping subprocess-only operation
unchanged. `cosmon-provider` (ADR-043) retains its role as the
completion-API home for **one-shot, no-loop** HTTP usage; the
agent-loop vs completion-API split is the load-bearing category
surviving this amendment.

---

## Alternatives considered

Named-for-the-record per ADR-082 INV-ADR-OPTIONS-CONSIDERED.

- **R3 — agent loop in `cosmon-provider`.** Rejected unanimously
  (4/4). Dep chain forbidden by ADR-095 RR-1; three R3-escapes
  (split-A, split-B, Adapter-in-provider) all fail under ADR-0002 §1
  or regress SF-0. Retired from the menu.
- **R1' — hold §5 line, stop at Aider via `--model`.** Torvalds's
  vote. Real on a multi-year horizon, but the operator's *yes* to
  Q-cf2e-load-bearing decides against parking. Re-openable via
  ADR-098 §6 90-day window if HTTP Adapters fail the cross-lineage
  falsifier.
- **Per-provider crates.** Rejected as over-decomposition;
  ADR-0002 §1 splitting rule does not fire.
- **HTTP Adapter family on by default.** Rejected. Feature gate
  preserves ADR-098 §5 intent.
- **Anthropic-first HTTP Adapter** (galileo's previous-round
  argument). Rejected on IFBDD purity: Anthropic-first conceals the
  schema divergence; OpenAI-first forces it from day one.
- **Move HTTP family into `cosmon-provider`.** Rejected: blocks on
  the Cargo cycle; forgemaster's `cosmon-provider`-on-top-of-transport
  Fact 1 establishes the tier ordering R2 respects.

---

## Invariants

**Preserved.** ADR-079 §1 four-word closure verbatim; HTTP Adapters
are *values* of `Adapter`. ADR-079 §5 four-obligation contract.
ADR-098 §1 WS-1…WS-5 taxonomy. ADR-098 §2.4 callsite-stability rule
(HTTP Adapter bodies call stable free functions in `cosmon-core`;
they never contain emission callsites). ADR-098 §6 90-day gate
verbatim (Triggers #1 and #3 sharpened by three-Adapter lineage).
ADR-095 RR-2…RR-5 verbatim; RR-1 read behaviourally per §5.
ADR-099 dispatch-site stability consumed verbatim. ADR-043
`cosmon-provider` perimeter unchanged for completion-API one-shot usage.

**Newly inscribed.** `cosmon-transport` **feature-gate discipline**
— HTTP Adapter deps behind `--features http-adapter`; default build
preserves subprocess-only 0-byte path. **OpenAI-first sequencing
rule** — when a Worker-Spawn Port HTTP Adapter family opens, the
first Adapter to ship maximises schema divergence from existing
subprocess Adapters, never minimises lineage delta.
**`cosmon-transport::api::*` module layout** —
one-crate-per-provider is forbidden unless ADR-0002 §1 splitting rule
fires.

**Modified.** ADR-098 §5 narrowed from *"`cosmon-transport` admits no
HTTP-client deps"* to *"…admits HTTP agent-loop Adapter deps behind
the `http-adapter` feature; subprocess-only operation under
`--no-default-features` preserves the legacy three-dep surface"*.
Dep-cost figure corrected from *+2–4 MB* to *~0.6–0.9 MB rustls-family
delta only, 0 bytes incremental for a second HTTP Adapter*.
ADR-098 §7 non-acceptance list: Anthropic API direct removed and
inscribed as the second HTTP Adapter slot (§3). Moonshot / Kimi via
HTTP remains non-accepted **as a separate Adapter** — Kimi reaches
academy via OpenAI-compatible endpoint configuration on the OpenAI
Adapter.

---

## Implementation sequence

Documentation-only at acceptance. Code follows, stacked on §6's
three prerequisites:

1. **Immediate (this ADR).** Accept ADR-100; `cs reconcile` updates
   `docs/adr/INDEX.md`; CHANGELOG entry.
2. **Prerequisite (in flight).** ADR-099 + TS-0 via
   `task-20260518-d670`.
3. **First HTTP Adapter PR.** `cosmon-transport::api::openai` (~200
   LOC); feature-gate `http-adapter`; calls `cosmon-core` emission
   free functions for WS-1…WS-5; smoke test `#[ignore]`-gated per
   PR-3 pattern. UX ↔ CLI parity same-PR (`[adapters.openai]` in
   `.cosmon/config.toml`; `cs help tackle` updated).
4. **Second HTTP Adapter PR.** `cosmon-transport::api::anthropic`
   (~150 LOC, shares `api::common`); regression-baseline smoke test
   against Claude subprocess Adapter. ADR-098 §6 Trigger #3 becomes
   empirically active.
5. **90-day forensic gate** (ADR-098 §6, inherited). Three Adapters
   + regression baseline; cross-lineage comparison gains its first
   real measurement.

---

## References

- **Parent deliberation:**
  `delib-20260518-cf2e/synthesis.md`
  §2 (C1–C4), §3 (T1–T3), §5 (integrated synthesis + Q-cf2e-load-bearing),
  §5.4 (conditional R2 chain this ADR realises).
- **Per-persona responses (same molecule dir):** `responses/architect.md`
  §1 (dep-chain forensics + R2 skeleton); `responses/forgemaster.md`
  §1 (R3→R2 flip), §3 (Cargo dep-delta correction);
  `responses/torvalds.md` §6 (Q-cf2e-load-bearing framing);
  `responses/knuth.md` §5.4 (TS-0 universal prerequisite).
- **Empirical motive:**
  an internal chronicle (2026-05-18, c8 spawn-routing fix)
  (academy b6d7 smoke chronicle). Source on academy side: an internal
  academy chronicle (2026-05-18, grok-aider smoke result).
- **Bound ADRs:**
  [ADR-079](079-worker-spawn-port-and-adapter-contract.md),
  [ADR-095](095-resident-runtime-ifbdd-path.md),
  [ADR-098](098-worker-spawn-port-operationalisation-ifbdd.md),
  ADR-099 (`task-20260518-d670`, in flight),
  [ADR-043](043-provider-abstraction.md).
- **Sibling tasks:** `task-20260518-958e` (C8, merged);
  `task-20260518-d670` (ADR-099 + TS-0, in flight);
  `task-20260518-e28a` (this ADR).
