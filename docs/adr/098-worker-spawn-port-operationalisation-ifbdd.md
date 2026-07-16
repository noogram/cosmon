# ADR-098 — Worker-Spawn Port: operationalisation under the IFBDD lens

**Status:** Proposed (2026-05-17).
**Date:** 2026-05-17.
**Decider:** Noogram.
**Parent deliberation:**
`delib-20260517-3899`
— five-persona panel on *"how to implement the Worker-Spawn Port of
ADR-079 under the IFBDD lens of ADR-095, using the physics-intern
reproduction in galaxy academy as the adversarial use-case"*. Panel:
architect, torvalds, galileo, forgemaster, karpathy. Synthesis §7
enumerates the seven sections this ADR commits.
**Authoring task:** `task-20260517-9220` (C1 of the parent
decomposition; L9 BLOCKING gate for C2…C9).

**Renumbering note.** The synthesis (§5.4) anticipated this ADR at
number **097**; between synthesis-completion and authoring, ADR-097
was issued to `097-fleet-validator-l1-l5-fact-check-coherence.md`
(commit `83f863b15`, 2026-05-17). The next free number is therefore
098. Doctrinal content is unchanged; cross-references throughout the
synthesis and child briefings that cite "ADR-097" for this perimeter
should be read as ADR-098.

**Binds:**
- [ADR-079](079-worker-spawn-port-and-adapter-contract.md) — the
  vocabulary commit this ADR operationalises. §1 four-word closure,
  §5 four-obligation Adapter contract, §6 pane-signature §-leak
  naming.
- [ADR-095](095-resident-runtime-ifbdd-path.md) — the doctrinal
  precedent. RR-1…RR-5 invariants, §4 90-day falsification gate,
  §14 karpathy badge.
- [ADR-038](038-whisper-perturbation-port.md) — the §-leak substrate
  (the `pane_current_command == "claude"` literal in `whisper.rs` and
  `propel.rs`).
- [ADR-043](043-provider-abstraction.md) — `cosmon-provider` crate-tier
  commitment (completion APIs); the substrate for the §5
  category-error correction below.
- [ADR-016](016-autonomy-regimes-and-resident-runtime.md) — the
  Resident Runtime, parallel L9 inscription whose form this ADR
  mirrors at a different perimeter.
- [ADR-075](075-oracle-boundary-cs-tackle.md) — the oracle envelope
  every Worker-Spawn carries; preserved verbatim through the Port.

**Architectural invariants:** `docs/architectural-invariants.md` §8j
(every Port is an ingress binding), §8k (UX surfaces are an Adapter
family), §14 (karpathy badge — *"you can `cat` cosmon's state"*).

---

## Context

ADR-079 (2026-04-26) committed the four-word vocabulary
**{ Worker · Port · Adapter · Tier }** for the worker-spawn perimeter
and named the four-obligation Adapter contract (briefing.md read,
writable MOLECULE_DIR, `cs` on PATH in worktree, idempotent
termination). It drew no trait, named no second Adapter, prescribed
no benchmark. It cost one page; it fixed nothing in code; it prevented
every later deliberation on this topic from re-litigating the basic
words.

ADR-095 (2026-05-17) re-opened the Resident Runtime under the
**IFBDD lens** — *Investigation/Forensics-Before-Decision-Driven
Development* (`docs/vocabulary.md` §*Forensics*) — by inverting the
empirical-feature-pull polarity that retired Phase 3+ in ADR-054:
the absence of a forensic instrument is not evidence of the absence
of demand; the instrument must exist *before* the behaviour it traces.
ADR-095 §RR-5 inscribes the four silent-failure-mode hooks the
Resident Runtime emits before any code path could trigger them.

This ADR applies the same discipline to the Worker-Spawn Port. The
panel's load-bearing observation (synthesis §5.4): cosmon's pinned
L9 rule — *"toute divergence du pattern claude.rs unique doit être
inscrite avant résolution"* — forces an ADR to land before any code
that diverges from the single-Adapter pattern. ADR-079 was the
vocabulary inscription. ADR-098 is the operationalisation
inscription: forensic taxonomy, construction order, academy-shim
contract, Cargo-tier discipline, 90-day falsification gate, and
the second-Adapter verdict — committed in prose before any line of
multi-Adapter code lands.

This ADR is documentation-only. Code follows in C2…C9.

---

## Decision

### 1. The five silent-failure-mode taxonomy for the Worker-Spawn Port

Each ADR-079 §5 obligation has exactly one corresponding silent-failure
mode and one detection event in `EventV2`. All five variants ship in
`cosmon-core`'s emission infrastructure (per §2 *callsite-stability*);
no Adapter body emits directly. Audit queries (one-liner each) are
specified verbatim in synthesis §4 and forwarded to C2's briefing.

| # | Name | Mechanism | EventV2 variant | Key fields | ADR-079 §5 obligation | Ships in |
|---|------|-----------|-----------------|-----------|----------------------|----------|
| **WS-1** | silent-double-spawn | Two `cs tackle` invocations claim the same worktree; second silently no-ops on collision | `AdapterWorktreeClaimed` (synonym: `WorkerSpawnAttempted` per galileo §2.1) | `mol_id, worker_id, adapter_name, worktree_path, invocation_uuid, pre_existing_worker: Option<WorkerId>` | (4) idempotent termination (spawn inverse) | **PR 1** — back-fills `claude.rs::spawn_claude_session` early-return |
| **WS-2** | silent-Adapter-shadow-stuck | Pane exists, `check_alive == true`, but CLI process wedged on TLS / keyring / token-cap | `AdapterLivenessProbed` | `mol_id, worker_id, adapter_name, probe_kind, probe_result: Alive { evidence: String } \| Stuck { reason }, elapsed_since_last_advance_ms` | (4) eventual termination | **PR 1** — back-fills `claude.rs::check_alive` |
| **WS-3** | silent-pane-signature-mismatch | Registered signature ≠ actual `pane_current_command`; whisper / propulsion silently drop | `AdapterPaneSignatureChecked` | `mol_id, worker_id, adapter_name, registered_signature: Vec<String>, observed_command: String, matched: bool, channel: Propulsion \| Whisper` | (3) `cs` on PATH in worktree (observable side) | **PR 2** — non-negotiable same-PR with the ADR-038 §5/§6 fix |
| **WS-4** | silent-briefing-seal-skip | Adapter calls `cs evolve` without reading sealed `briefing.md`; produces plausible-but-orthogonal output | `AdapterBriefingConsumed` | `mol_id, worker_id, adapter_name, briefing_path, briefing_seal_observed: Blake3Hash, briefing_seal_recorded: Blake3Hash, bytes_read: u64, consumed_at` | (1) reads `briefing.md` | **PR 1** — emitted by new private helper `consume_briefing(path, recorded_seal)` |
| **WS-5** | silent-Adapter-orphan | CLI process gone, pane dead, but Adapter handle still reports `Alive`; next `cs evolve` sent to corpse | `AdapterHandleReconciled` | `mol_id, worker_id, adapter_name, handle_state: Held \| ReleasedClean \| ReleasedOrphan, underlying_exit_observed_at: Option<Timestamp>, handle_released_at, gap_ms: i64` | (4) eventual termination (dual of WS-2) | **PR 1** trivial back-fill at `kill_session` / `check_alive`; **PR 3** is where the non-trivial emission surfaces (Aider connection-pool / async-IO) |

**Dual interpretation of WS-2 / WS-5** (galileo §2.5). WS-2 fires
when the process is alive but stuck; WS-5 fires when the process is
dead but the handle has not noticed. The two lie about liveness in
opposite directions; cosmon's existing implicit `WorkerHeartbeat`
cannot distinguish them. Both events together close the gap.

### 2. IFBDD construction order (the six-PR sequence)

The work is sequenced so the instrument predates the behaviour at
every PR boundary. Five panelists, one converged order
(synthesis §1.3):

```
ADR-098 (this document)                        — L9 inscription
  ↓
PR 1 — EventV2 variants in cosmon-core         — the instrument
        + emission infrastructure
        + emit-sites back-filled on claude.rs
  ↓
PR 2 — Pane-signature registry                 — ADR-038 §5/§6 fix
        + whisper.rs / propel.rs de-hard-coding
        + AdapterPaneSignatureChecked (WS-3) wired same-PR
  ↓
PR 3 — Second Adapter (Aider, per §7)          — structural sibling
        + cross-Adapter smoke test (Tier-1 MockBackend)
  ↓
PR 4 — Spawn trait extraction                  — pure refactor
        (zero behaviour change; null-move under IFBDD)
  ↓
PR 5 — cs tackle --adapter <name>              — user surface
        + [adapters] in .cosmon/config.toml
        + AdapterSelected EventV2 variant
  ↓
PR 5b (academy-shim) — role→Adapter table       — driver-side translation
                                                 in galaxy academy
```

**Callsite-stability rule (forgemaster §2.4 — load-bearing).** Every
`EventV2` emit-site lives in `cosmon-core`'s emission infrastructure
(a free function, e.g. `cosmon_core::events::emit_adapter_briefing_consumed(...)`).
Adapter bodies in `cosmon-transport` *call* the stable free function;
they never *contain* the emission callsite. The trait extraction in
PR 4 is therefore a pure refactor that does not move emission
callsites: the audit timeline of `events.jsonl` against source remains
continuous across every `cosmon-transport` reorganisation. This is
the stronger IFBDD claim: not just *"instrument before behaviour"*
but *"instrument at a stable location that remains valid under every
foreseen refactor of the behaviour"*.

**Karpathy's `≤200` LOC discipline** applies to each PR. PR 1 may
need to split into PR 1.α (variants + emission infrastructure in
`cosmon-core`) and PR 1.β (back-fills on `claude.rs`) if the cap is
strict; the operator may accept architect's ~350-LOC budget for PR 1
as an explicit exception (synthesis §6 item 2). Karpathy's
*cat-test* per spike (the §14 *"you can `cat` cosmon's state"* badge)
is the per-PR gate.

**`AdapterSelected` placeholder discipline (galileo §5.3).** The
`AdapterSelected` EventV2 variant ships in PR 1 with
`selection_source: Default { fallback_reason: "only one Adapter
registered" }` always set. This preserves the IFBDD pact (the
instrument exists before the multi-Adapter behaviour) without
shipping a yard-sale flag — the user-visible `--adapter` flag itself
lands in PR 5 when `[adapters]` has more than one entry.

### 3. The academy-shim contract (driver-side translation)

The academy-shim is the component in galaxy academy that drives cosmon
on behalf of the physics-intern fleet. It is a hexagonal **driver**
(it calls into cosmon), never a **driven port** (cosmon does not call
back into it). This is non-negotiable on architecture-tier grounds
(architect §4.4): cosmon is `substrate` (CLAUDE.md
`governance_tier: substrate`); academy is a downstream consumer;
substrate cannot depend on consumers.

| Component | Cosmon side | Academy side |
|-----------|-------------|-------------|
| **Flag surface** | `cs tackle <mol> --adapter <name>` (optional, default `claude`) | academy-shim sets `--adapter` per invocation |
| **Inventory** | `[adapters]` section in `.cosmon/config.toml` (static; pane signatures + adapter-specific args) | — |
| **Selection logic** | Looks up `[adapters.<name>]`; typed error `AdapterNotFound { name, available }` if unknown (no `anyhow` in CLI surface) | academy-shim's own Rust `MODEL_TO_ADAPTER` function reads `[routing.<role>].model` from `meta-fleet/physics-intern.fleet.toml` and maps to an Adapter name |
| **Trace** | `EventV2::AdapterSelected { mol_id, adapter_name, selected_at, selection_source: Cli \| EnvelopeRole \| Config \| Default, role_hint: Option<String> }` | academy-shim's own per-run log (`experiments/<run-id>/dispatch.log`) records the `(role, model, adapter_name)` triple |
| **Vocabulary** | `Adapter` primitive unchanged; `"claude"`, `"aider"`, … are *values* of that primitive | `role` and `model` are academy's primitives; never leak into cosmon code |
| **State discipline** | RR-2 honoured: no new state file. `[adapters]` is config (static); `AdapterSelected` is event-trace (dynamic) | — |

**`role_hint` is optional and forensic-only.** It carries academy's
role-string across the seam so a later audit can correlate cosmon's
Adapter selection with academy's routing decision. cosmon stores it;
cosmon does not interpret it. This is the
**anti-corruption-layer** discipline from DDD (architect §4.4):
academy's vocabulary is translated *at academy's boundary*, not inside
cosmon.

**The single new affordance on the cosmon CLI surface:**

```
cs tackle <molecule-id> [--adapter <name>]
```

Default: `--adapter claude` (back-compat). Values: any name
registered in `[adapters.<name>]`. UX ↔ CLI parity obligation
(CLAUDE.md / ADR-068): the native pilot apps gain an Adapter picker
on the `cs tackle` dialog in the same PR as the flag lands, with
`cs help tackle` + `man cs` + UX parity audit updated in the same PR
(CLAUDE.md `feedback_cli_doc_sync`).

### 4. Vocabulary closure preservation (no fifth primitive)

The four-word set **{ Worker · Port · Adapter · Tier }** committed
by ADR-079 §1 is preserved verbatim by this ADR. `--adapter <name>`
is a **value-naming mechanism** for the existing `Adapter` primitive
— structurally analogous to `git branch <name>`, where `branch` is
the primitive and `<name>` is a value. The `[adapters]` section in
config is an **inventory** of Adapter values, not a new primitive.
The `AdapterName` newtype introduced in PR 1 is a value type
associated with `Adapter`, not a fifth word.

**The five-word closure test** (forgemaster §4.4): does any addition
in this ADR require explaining a new concept to a reader who already
knows { Worker · Port · Adapter · Tier }? No. `AdapterName`,
`AdapterConfig`, `AdapterSelected`, `[adapters.<name>]` are all
obviously derivable from the existing vocabulary. Promoting any of
`Role`, `Substrate`, `Backend`, `Provider`, `Tenant` to a primitive
would require a successor ADR; this ADR refuses that promotion.

### 5. Cargo-tier discipline (cosmon-transport vs cosmon-provider)

**Load-bearing correction** (forgemaster §3.1):

- `cosmon-transport` is the home of **subprocess CLI Adapters** —
  external binaries spawned in a worktree with `cs` on PATH that
  realise the four ADR-079 §5 obligations.
- `cosmon-provider` (ADR-043) is the home of **completion-API
  providers** — single-shot HTTP request/response with no
  subprocess, no persistent pane, no worktree.

Wiring `reqwest` / `hyper` / `tokio-tls` into `cosmon-transport` to
support a completion API is a **structural breach**. The dependency
delta is concrete (forgemaster §3.4):

| Candidate | Worker-Spawn Port? | New Cargo deps in `cosmon-transport` | Binary delta (musl static) |
|-----------|---------------------|--------------------------------------|----------------------------|
| Aider (subprocess CLI) | **Yes** | 0 | 0 bytes |
| Codex (subprocess CLI) | **Yes** | 0 | 0 bytes |
| Anthropic API direct | **No** — `cosmon-provider` task | `reqwest+hyper+tokio-tls+...` | +2–4 MB |
| Moonshot / Kimi via HTTP | **No** — `cosmon-provider` task | same | +2–4 MB |

**This ADR explicitly does NOT accept Anthropic API direct,
Moonshot/Kimi via HTTP, or any other completion-only substrate as
Worker-Spawn Port candidates.** If the operator wants those
substrates stress-tested, the place is `crates/cosmon-provider`
under ADR-043's hexagonal cell — file a separate deliberation on
the Completion Port. The Worker-Spawn Port stays at three
Cargo-dependency surfaces (`cosmon-core`, `cosmon-bridge-claude`,
`thiserror`); breaking that for a wrong-port Adapter is a regression
with a real runtime cost (≥40 MB on a 10-instance agent server).

### 6. The 90-day falsification gate

Mirrors ADR-095 §4 form. **Three triggers; any one falsifies the
Worker-Spawn Port abstraction.** Measurement window: 90 days from
PR 1 merge.

1. **Trigger #1 — cross-Adapter golden test diverges on a
   non-declared field.** After PR 3 ships, the smoke test
   (`crates/cosmon-transport/tests/cross_adapter_smoke.rs`,
   `#[ignore]`-gated for real binaries) runs the same `briefing.md`
   through Claude and Aider; their `events.jsonl` lineages are
   compared field-by-field modulo *declared* Adapter-specific fields
   (`probe.evidence`, `elapsed_since_last_advance_ms`, …). Divergence
   on a non-declared field means the four-obligation contract is
   incomplete — the Port admits behaviour the contract did not
   contemplate. (galileo §8.1)

2. **Trigger #2 — at least one silent-failure mode fires in the
   wild without 24h audit detection.** The five audit queries from
   §1 are run daily on production `events.jsonl`. Any incident that
   surfaces *only* in a weekly sweep, not in the daily 24h cycle,
   is a structural gap in the instrument — the forensic taxonomy is
   wrong. Reproducible across two distinct invocations = structural,
   not one-off. (galileo §8.2; structurally equivalent to ADR-095
   §4's silent-drift criterion.)

3. **Trigger #3 — second-Adapter lineage 100% identical to
   first-Adapter lineage.** If Aider's `events.jsonl` is bit-for-bit
   identical to Claude's (modulo `adapter_name`) across all flows
   in 90 days, the abstraction is degenerate — it admits no
   behavioural delta and the Port has not earned its existence.
   Could be replaced by `claude.rs + --adapter` flag without a trait.
   (galileo §8.3)

**Falsified outcome.** Any one trigger → a fresh ADR ratifies the
revision (Port abstraction reshaped) or, in the extreme, the
excision (the trait is dismantled per RR-3-analogue discipline:
delete `crates/cosmon-transport/src/aider.rs` and the trait module,
restore `claude.rs`, `cargo check --workspace` green). The deletion
is not a defeat; it is the IFBDD pact honoured.

Architect's three secondary falsifiers (A: `--adapter` value ends
up *role*-shaped — driver-side translation wrong; B: 3+ new event
variants needed beyond the five — taxonomy under-enumerated;
C: pane-signature registry needs a third dimension for non-tmux
Adapters — ADR-038 §5/§6 fix was symptomatic, not structural) are
absorbed as *"any two together also falsify"* sub-conditions.
Torvalds's single hard falsifier (zero silent-failure events in 90
days AND ≥3 operator-reported pain incidents the events did not
catch) is structurally identical to galileo's Trigger #2.

### 7. The Aider verdict for the second Adapter (sub-Q2 closure)

**Verdict: (d) Aider.** Rationale (synthesis §2.3):

- Aider is a subprocess CLI; zero new Cargo deps in `cosmon-transport`.
- Aider's TUI markers differ structurally from Claude Code's; the
  existing `readiness.rs::classify_output()` will *loudly fail* on
  Aider, forcing the pane-signature registry (PR 2) from theoretical
  placeholder to required implementation. Loud failures drive real
  fixes; silent passes build false confidence.
- Aider's `--model` flag accepts a wide model range
  (`claude-3-5-sonnet`, `gpt-4o`, `gemini-2.0-flash`,
  `kimi-k2.6`, local Ollama). **One Adapter binary serves all nine
  physics-intern roles** via per-role `--model`; academy jalon-2 is
  unblocked by Aider directly, without a separate Kimi Adapter.
- The two-axes concern (substrate + model-routing) is decoupled by
  configuration: `[adapters.aider].extra_args = ["--model",
  "${model}"]` is set in `.cosmon/config.toml` (static); the
  `cs tackle --adapter aider` invocation does not expose `--model`
  to the operator.

**Codex (a) is deferred** to a later child (Adapter #3 or later).
Once Aider validates the trait shape against a behaviourally distant
substrate, Codex becomes the *"is the trait shape right for *other*
CLIs that look like Claude?"* check (torvalds §6.7). It is not lost;
it is sequenced.

**Operator-override path, named for the record:** *Aider (default) /
Codex (alternative) / later*. The default (Aider) holds for the
decomposition unless the operator overrides at C4-dispatch time.

**Non-acceptance, explicit:**
- **Option (b) Anthropic API direct** is **not** a Worker-Spawn Port
  candidate. It is a `cosmon-provider` task (ADR-043).
- **Option (c) Moonshot / Kimi via HTTP** is **not** a Worker-Spawn
  Port candidate. It is a `cosmon-provider` task. Kimi-via-Aider
  (per §7 above) is the Worker-Spawn Port path for Kimi.

---

## Consequences

**Positive.**

- The IFBDD pact is honoured at the Worker-Spawn Port perimeter.
  Five silent-failure modes have detection events that ship before
  the behaviour they observe; a future audit can answer *"did the
  abstraction earn its place?"* with trace data, not with the
  agent's claim. Mirrors the ADR-095 RR-5 discipline for the
  Resident Runtime at a different perimeter.
- The four-word vocabulary closure (ADR-079 §1) is preserved by
  construction — `--adapter` and `[adapters.<name>]` are values, not
  primitives. No successor ADR required for the multi-Adapter regime.
- The Cargo-tier discipline (ADR-043 derivation) is named explicitly,
  closing a real category-error trap that would have wired HTTP
  clients into `cosmon-transport` and inverted the existing
  crate-tier separation.
- academy jalon-2 (the physics-intern reproduction) is unblocked via
  Aider's `--model` flag — one Adapter, N model values, one
  role→Adapter mapping in the shim. The substrate decision and the
  use-case decision are satisfied by the same PR.
- The excision path is named (RR-3-analogue): if the 90-day gate
  falsifies the abstraction, deletion is one PR. Decision is
  reversible at low cost.

**Negative / accepted.**

- PR 1's budget may overflow karpathy's 200-LOC strict cap; the
  operator opt-out is named (synthesis §6 item 2) but the discipline
  is the default.
- The `AdapterSelected` EventV2 variant is *degenerate* until PR 3
  lands (always `selection_source: Default`). This is the price of
  IFBDD purity — the instrument ships before the behaviour, even
  when the behaviour has only one value.
- The 90-day forensic measurement window is a commitment. The
  operator surfaces the evidence at day 90 regardless of whether
  the verdict is confirmed or falsified. Same shape as ADR-095 §4.

**Structural.**

- `crates/cosmon-transport` perimeter is closed for HTTP-client
  Cargo dependencies. Future Adapter candidates that would force
  HTTP must be routed through `cosmon-provider` (ADR-043) instead.
  This is a hard rule, not a guideline.
- `[adapters]` section in `.cosmon/config.toml` becomes the only
  sanctioned inventory of Adapter values. Hard-coding an Adapter
  identity anywhere in `cosmon-transport`, `cosmon-cli`, or any
  channel implementation is a structural breach (§-leak).
- The `Spawn` trait (or whatever name PR 4 commits — synthesis §6
  item 1 defers this) is extracted only against ≥2 concrete impls;
  drawing it against one impl is forbidden by this ADR.

---

## Alternatives considered

Named-for-the-record per ADR-082 INV-ADR-OPTIONS-CONSIDERED.

- **Path (a) — parallel ship of trait + 2nd Adapter + instrument in
  one PR set** (synthesis §1.2). Rejected: three changes in one diff;
  CI cannot distinguish *"the abstraction holds"* from *"both
  implementations happen to satisfy the assertions we wrote"*. Under
  IFBDD, worse than shipping nothing — a green test that cannot be
  falsified into the structural finding.

- **Path (b) — strict trait-first** (architect's original position,
  dissolved into path (c) by forgemaster's §2.4 callsite-stability
  argument). Rejected on its strict reading: a `Spawn` trait drawn
  against one impl is *writing the theorem statement before the
  axioms that prove it*. The trait would overfit `claude.rs`'s
  accidents (PermissionMode, prompt-as-flag, tmux-pane-as-IO) and
  the second Adapter would force a trait-reshape that re-emits all
  events under a slightly different shape — worst-of-both-worlds.

- **Option (a) Codex as the second Adapter** (architect, karpathy
  preference). Deferred to a later Adapter slot. Codex's TUI
  ergonomics are too close to Claude's; `readiness.rs`'s hardcoded
  markers may *silently pass* for Codex output. The forensic value
  of a second Adapter is proportional to its behavioural distance
  from the first; Aider maximises that distance.

- **Option (b) Anthropic API direct as the second Adapter**
  (galileo's forensic-winner argument). Non-accepted as
  Worker-Spawn Port candidate. The API-direct substrate is a
  completion-API artifact and belongs in `cosmon-provider`
  (ADR-043). Galileo's argument is *internally* correct but
  falsifies the wrong port — the Worker-Spawn Port should not make
  the assumption that an HTTP completion is a candidate at all.

- **Option (c) Kimi/Moonshot via HTTP as a separate Adapter.**
  Non-accepted: same category-error correction as option (b).
  Kimi reaches academy via Aider's `--model kimi-k2.6` flag, which
  is `cosmon-transport`-clean.

- **`Role` (or `Substrate`, `Provider`, `Backend`, `Tenant`) as a
  fifth primitive.** Rejected unanimously. Would require a
  successor ADR. The driver-side translation pattern (§3) absorbs
  the role concept into academy-shim's boundary, preserving
  cosmon's four-word closure.

- **Cosmon owns a Role → Adapter registry** (synthesis §3.2).
  Rejected: *role* is a academy concept; cosmon does not know what
  a research-role is. The mapping lives at the driver boundary, on
  academy's side. Anti-corruption-layer (DDD).

- **`.cosmon/state/adapters/<name>.json` per-Adapter state file**
  (forgemaster §4.5). Rejected: RR-2 violation. Static inventory in
  config; dynamic selection in events; no intermediate state.

---

## Invariants

**Preserved.**

- ADR-079 §1 four-word closure { Worker · Port · Adapter · Tier }
  — verbatim.
- ADR-079 §5 four-obligation Adapter contract — verbatim. Each
  obligation maps to exactly one silent-failure mode and one
  detection event in §1 above.
- ADR-095 RR-1 (client-of-core), RR-2 (owns-no-state), RR-3
  (deletable-Cargo-target), RR-4 (JSON-on-disk authoritative),
  RR-5 (failure-mode hooks baked in from day one) — applied to the
  Worker-Spawn Port perimeter. The Spawn trait + Aider Adapter form
  a closed Cargo unit deletable as one PR (RR-3-analogue).
- ADR-038 *Port* / *Adapter* — used verbatim; the §5/§6 §-leak fix
  is the exit criterion for ADR-079 §6 and lands in PR 2 alongside
  the WS-3 detection event.
- ADR-016 *runtime* — reserved exclusively for the Resident Runtime;
  Adapters are not runtimes.
- `docs/architectural-invariants.md` §14 (karpathy badge) — every
  PR in the §2 sequence must pass the *"you can `cat` cosmon's
  state"* test.

**Newly inscribed.**

- **The five silent-failure-mode taxonomy** (WS-1 … WS-5 in §1) —
  each variant, key fields, audit query, and same-PR ship anchor.
  Mirrors ADR-095 RR-5 for the Worker-Spawn Port perimeter.
- **The callsite-stability rule** — all EventV2 emit-sites for the
  Worker-Spawn Port live in `cosmon-core`'s emission infrastructure
  (free functions), never in `cosmon-transport`'s Adapter bodies.
  The trait extraction (PR 4) does not move emission callsites.
- **The academy-shim contract** (§3) — `--adapter <name>` as the
  cosmon CLI surface; `[adapters]` inventory in `.cosmon/config.toml`;
  `AdapterSelected` event with optional `role_hint`; driver-side
  role translation in academy-shim. Substrate cannot depend on
  consumer.
- **The Cargo-tier discipline** (§5) — `cosmon-transport` is for
  subprocess CLI Adapters; `cosmon-provider` is for completion-API
  providers. HTTP-client deps in `cosmon-transport` are a structural
  breach. The Worker-Spawn Port admits no completion-only substrate
  as an Adapter candidate.
- **The 90-day falsification gate** (§6) — three triggers, any one
  falsifies; the path back is RR-3-analogue excision plus a fresh
  ADR ratifying the retirement with forensic evidence.

**Modified.** None. This ADR is purely additive over ADR-079; the
four-word vocabulary closure is preserved and the four-obligation
contract is operationalised, not changed.

---

## Implementation sequence (ADR-098 → C2…C9)

This ADR is documentation-only at acceptance. Code follows on the
IFBDD construction order in §2:

1. **Immediate (this ADR, C1).** Accept ADR-098; run `cs reconcile`
   to update `docs/adr/INDEX.md`; add CHANGELOG entry; the L9
   inscription is in place; C2…C9 unblocked.
2. **C2 — PR 1.** Five EventV2 variants + emission infrastructure
   in `cosmon-core` + back-fills on `claude.rs`. ≤350 LOC (or
   karpathy split). No behaviour change.
3. **C3 — PR 2.** Pane-signature registry + ADR-038 §5/§6 §-leak
   fix in `whisper.rs` / `propel.rs` + WS-3 detection event
   (`AdapterPaneSignatureChecked`) same-PR. ≤200 LOC.
4. **C4 — PR 3.** Aider Adapter
   (`crates/cosmon-transport/src/aider.rs`). Zero new Cargo deps.
   ≤250 LOC. Registers pane signature; emits the five events from C2.
5. **C5 — PR 4.** `Spawn` trait extraction; pure refactor against
   Claude + Aider; ≤200 LOC; CI gated on event-equivalence.
6. **C6 — PR 5.** `cs tackle --adapter <name>` flag + `[adapters]`
   in `.cosmon/config.toml` + `AdapterSelected` becomes
   non-degenerate. ≤200 LOC. UX ↔ CLI parity update same-PR.
7. **C7 — academy-shim.** `MODEL_TO_ADAPTER` Rust function +
   `--adapter` flag invocation in galaxy academy, not in cosmon.
   ≤60 LOC.
8. **C8 — cross-Adapter smoke test.** Tier-1 MockBackend in CI;
   Tier-2 real-binary integration `#[ignore]`-gated.
9. **C9 — 90-day forensic gate.** Read `events.jsonl` for the three
   falsification triggers; verdict: confirmed / extended / falsified.

---

## References

- **Parent deliberation (verbatim sections cited):**
  `delib-20260517-3899/synthesis.md`
  §1 (path-c convergence), §2 (Aider verdict + category correction),
  §3 (academy-shim contract), §4 (5-mode silent-failure taxonomy),
  §5 (cross-couplings), §7 (the load-bearing sections this ADR
  commits).
- **Per-persona responses:**
  `responses/forgemaster.md`
  §2.4 (callsite-stability), §3.1 (category-error correction),
  §5 (silent-failure modes);
  `responses/galileo.md`
  §2 (five EventV2 variants with audit queries),
  §8 (90-day falsification gate);
  `responses/architect.md`
  §4 (hexagonal driver framing);
  `responses/karpathy.md`
  §2 (5-spike sequence + cat-test per spike);
  `responses/torvalds.md`
  §4.2 (`--adapter` value-not-primitive).
- **Bound ADRs:**
  [ADR-079](079-worker-spawn-port-and-adapter-contract.md),
  [ADR-095](095-resident-runtime-ifbdd-path.md),
  [ADR-038](038-whisper-perturbation-port.md),
  [ADR-043](043-provider-abstraction.md),
  [ADR-016](016-autonomy-regimes-and-resident-runtime.md),
  [ADR-075](075-oracle-boundary-cs-tackle.md).
- **Vocabulary:** `docs/vocabulary.md` §*Forensics* (the IFBDD lens
  definition); `docs/architectural-invariants.md` §8j (ingress
  bindings), §8k (UX surface family), §14 (karpathy badge).
- **Academy cross-reference:** galaxy academy
  `meta-fleet/physics-intern.fleet.toml` `[routing.*]` (the
  per-role model table the academy-shim reads).
- **Authoring task:** `task-20260517-9220` (C1 of the parent
  decomposition; L9 BLOCKING gate for C2…C9).
