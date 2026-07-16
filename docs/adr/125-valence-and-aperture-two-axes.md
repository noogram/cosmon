# ADR-125 — Valence and Aperture: two orthogonal axes of model/harness selection

**Status:** Proposed (filed for operator decision — one atomic choice, §9)
**Date:** 2026-06-15
**Decider:** Noogram
**Parent deliberation:** `delib-20260615-73f9` (panel: wheeler, architect,
torvalds, turing, tolnay) — see `synthesis.md` in the molecule directory.

**Binds:**
- [ADR-079](079-worker-spawn-port-and-adapter-contract.md) — the Worker-Spawn
  Port and its four Adapter obligations (the parity bar's first four).
- [ADR-099](099-dispatch-site-stability.md) — TS-0 dispatch-site stability;
  the cat-test (`adapter_selected.adapter_name == worker_spawned.adapter_name`).
- [ADR-119](119-adapter-exit-code-contract.md) — the fifth Adapter obligation
  (five-class exit-code contract).
- [ADR-118](118-llmport-doctrine-and-degradation-matrix.md) — the LLMPort
  doctrine the execution-adapter axis lives under.
- [ADR-080](080-remote-pilot-port-https-oidc.md) / [ADR-124](124-tenant-bounded-drain-run.md)
  — the §8p frozen rpp-v1 surface and its tenant verbs (the remote aperture).
- ADR-020 (mcp project-agnostic cwd-per-call) — the cwd/state pinning bug
  class that motivated retiring the long-lived MCP surface.
- `crates/cosmon-cli/src/cmd/mcp.rs`, `crates/cosmon-mcp/` (deprecated
  2026-04-11, dead-code-by-construction).

**Architectural invariants:** `docs/architectural-invariants.md` §8j (every
Port is a typed ingress binding), §8k/§8l/§8m (UX↔CLI parity), the CLI-first
invariant.

---

## 1. Context

The operator asked to ratify and plan the **orthogonal, decoupled axes** along
which cosmon chooses a model/harness — and to drive both toward **parity**
(every execution adapter usable on equal footing) and **portability** (cosmon
pilotable from harnesses other than Claude Code). Two axes were posed; a third
surface (`cosmon-remote`) was injected mid-deliberation.

The deliberation found the two axes are not merely *intended* to be orthogonal
— they are **structurally decoupled in the code's shape**, and the temptation
to call the remote surface a "third axis" is a locality-vs-kind confusion. This
ADR ratifies the model, the parity bar, the portability surface, and a roadmap.

## 2. Decision — the two axes

Cosmon selects a model/harness along **two orthogonal axes**. One axis bears
multiple **apertures** (surfaces), which differ in *locality*, not in *kind*.

### Axis 1 — **Valence** (the execution adapter)
*Who does the work inside the worker.* Bound at nucleation via `--adapter`;
intrinsic to the worker; decides what work it can perform (chemistry metaphor:
the bonding capacity an atom brings to a molecule). Resolution order
(`resolve_adapter_selection`, `crates/cosmon-cli/src/cmd/tackle.rs`):
`--adapter flag → formula-step adapter → $COSMON_DEFAULT_ADAPTER →
[adapters.default] (per-galaxy) → [adapters.default] (global) →
BUILTIN_FLOOR_ADAPTER ("local")`. Live valences today: `claude`, `aider`,
`openai`, `anthropic` (stub), `llama-cpp`/`llama`, `local` (floor). Gaps:
`codex` declared-but-undispatchable (Gap#5), `opencode` absent.

### Axis 2 — **Aperture** (the pilot surface)
*Where the operator's natural-language intent enters cosmon.* Chosen freely,
at any time, by the pilot harness. Three apertures, same kind (each pilots the
same molecule lifecycle), differing only in locality/transport:
- **(a) local `cs` CLI** — shell, walk-up state store. What this very session uses.
- **(b) remote `cosmon-remote` CLI** — shell, thin client over the §8p frozen
  rpp-v1 API; pilots D-AVATARs.
- **(c) remote rpp-v1 REST** — HTTP, shell-less; the OpenAPI spec
  (`crates/cosmon-rpp-adapter/openapi/v1.yaml`) is the contract.

> **`cs mcp` is a welded-shut door.** The `cosmon-mcp` crate is deprecated
> (2026-04-11, dead-code-by-construction, out of default workspace members).
> It is neither an axis nor an aperture — it is a *closed* aperture. It must
> not be resurrected (see §5).
>
> **Update (2026-07-12, decision C14, task-20260712-74a1).** The *local*
> `cs mcp` stdio door named here was **removed** (command, `serve_stdio`,
> standalone binary), so this passage's spirit is now enforced by absence
> rather than by welding. The `cosmon-mcp` *crate* is **not** dead code: it
> was repurposed as the transport substrate for the *remote* `/mcp` endpoint
> served by `cosmon-rpp-adapter`. So "MCP" is closed as a **local** aperture
> and deliberately open as a **remote** RPP surface — the two must not be
> conflated. See `crates/cosmon-mcp/README.md`.

**Why two axes, not three** (wheeler): an *axis* is a degree of freedom you can
move while holding the others fixed; a *surface/aperture* is a doorway onto the
same phenomenon. Holding the valence fixed, choosing `cs` vs `cosmon-remote` vs
REST changes *where you stand*, not *what is produced or who produces it*. They
are three apertures of one axis. (The operator framed `cosmon-remote` as "AXE
3"; the parity-and-decoupling requirement is honored identically under either
count — see §9 for the one open decision.)

## 3. The decoupling is structural, not aspirational (Q5)

The two axes are **provably decoupled**, established three ways:
- **Behavioral (turing):** the piloting harness is *not one of the six inputs*
  to `resolve_adapter_selection`. There is no parameter through which a pilot
  harness could leak into adapter selection — decoupling by *absence of an
  edge*. A small theorem, not a test result.
- **Type-level (tolnay):** `ValidatedAdapterName` is a clean newtype whose only
  consumer is the spawn seam; no type couples an aperture to a valence.
- **Remote (turing falsifier c):** the frozen rpp-v1 surface carries *no*
  executor axis; the noyau selects the valence via its own config (it can
  nucleate on Mistral while a gemini-cli pilot drives the avatar over REST).

**The one default that looks like coupling** (turing falsifier b): omitting
`--adapter` pins the executor to the fixed named `local` floor — *not* to the
piloting harness's model — with a loud `AdapterSelected{Default}` receipt and a
one-flag override. This is a documented default, not a hidden coupling; the
theorem holds because the floor is a constant, not a function of the caller.

**Ratified requirement:** any future change must preserve this — no aperture may
become an input to valence selection, and no valence may demand a specific
aperture. A compile-time forbidden `(aperture, valence)` pair would be a breach.

## 4. The parity bar (Q1)

An execution adapter is **at parity** when `cs tackle --adapter X` dispatches a
real worker that does the job and tears down cleanly, and the journal does not
lie about it. Concretely, the bar is the obligation set (torvalds empirical +
tolnay typed):

| Obligation | Source | Load-bearing? |
|---|---|---|
| Reads `briefing.md`; writable MOLECULE_DIR; `cs` on PATH in worktree; idempotent terminate | ADR-079 §5 (the four) | **Yes** |
| cat-test event fidelity (validated name == emitted name) | ADR-099, typestate on `spawn` | **Yes** |
| Five-class exit-code contract | ADR-119 | Contractual, but **not yet wired** into the spawn-site / `cs patrol` — theatre until wired |
| Readiness probe (`LiveProbe::await_live`) | transport | **Yes for tmux arms**; N/A for in-process |
| Egress/clearance posture (pass the ADR-075 envelope, mint no authority) | ADR-079 §4 | **Yes** |
| Teardown symmetry (`cs done` reverses `cs tackle`) | — | **Yes** |

Private / free-to-vary (NOT parity obligations): `permission_mode`, TUI status
enums, JSONL parsers, pane signature, internal provider cascade, the
`classify_exit` stderr-marker *rule* (only the five-class *output* is public).
**Do not draw a `trait Adapter` yet** (tolnay): the obligations are binding in
prose and enforced by the compiler (ADR-099 typestate + ADR-119 exhaustive
enum); the trait is drawn when the second real adapter (codex) earns it.

## 5. The portable pilot surface (Q3) — MCP is dominated, not just deprecated

The portable surface is **a context-pack per aperture handed to a shelling or
HTTP-speaking harness** — never MCP. Decisive facts:
- *Every named harness shells* (codex, gemini-cli, opencode, claude-code,
  aider). The CLI *is* the tool (git model: git has no MCP).
- For the HTTP/shell-less case, the rpp-v1 REST API serves the harness directly,
  and `openapi/v1.yaml` *is already* the context-pack. **HTTP-capable ⊃
  MCP-capable**, so the last edge case justifying MCP (no-shell-but-MCP-capable)
  is dominated by no-shell-but-HTTP-capable → direct REST.
- **Falsifier (does AXE-3 ever need MCP the CLI/REST can't give?) — NO.** Every
  candidate (streaming, conversational channel, tool discovery, no-shell
  harness, session state) resolves to an existing frozen verb (`events` SSE,
  `converse`, `--json`) or to direct REST.

The context-pack per aperture (start by pointing harnesses at existing
artifacts — `man cs` / `man cosmon-remote` / `openapi/v1.yaml` + curated
CLAUDE.md sections; build a generated `cs pilot-pack` projection only if manual
assembly drifts) is specified in the portability child (§7).

## 6. Decision — what is ratified vs deferred

**Ratified now:** the two-axis model (Valence / Aperture), the structural
decoupling requirement (§3), the parity bar obligation set (§4), the
MCP-dominated portability doctrine (§5).

**Deferred to children:** the parity-bar codification, the three adapter gaps
(codex/aider-model/opencode), and the portability context-pack + test matrix.

## 7. Roadmap — child molecules

All `--blocked-by delib-20260615-73f9`, `temp:warm`, with synthesis-derived
briefings:

1. **Parity-bar codification** — the per-adapter parity checklist as a doc/test;
   decide whether/when to draw `trait Adapter`.
2. **codex spawn arm (Gap#5)** — `declared_names` + `spawn_and_prompt` arm
   (clone aider) + optional `CodexProbe`. Highest ROI; do first.
3. **aider model-wire** — thread `adapter_entry`, read `[adapters.aider].model`,
   drop the `kimi-k2.6` hardcode. Orphan S-fix.
4. **opencode adapter** — full 5-touch onboarding cloning the codex arm
   (tmux-subprocess). Blocked-by child 2.
5. **Portability context-pack + 3-access-mode test matrix** — `cs` local /
   `cosmon-remote` CLI / direct rpp-v1 REST; the minimal pilot contract; the
   point-at-existing-vs-`cs pilot-pack` decision.

## 8. Consequences

- The roadmap is ordered and de-risked (codex is the template for opencode).
- The decoupling theorem becomes a reviewable invariant: PRs that make an
  aperture an input to valence selection are structural breaches.
- MCP stays welded; any future "MCP need" is first tested against direct REST.

## 9. The one open operator decision (atomic)

Does this ADR ratify wheeler's **"2 axes (Valence / Aperture), the Aperture
axis bearing three apertures (local `cs`, remote `cosmon-remote` CLI, remote
rpp-v1 REST)"** — with "axis 3" kept as colloquial shorthand — **or** does the
operator want the literal **"3 axes"** framing in the title/structure? The
substance (parity + decoupling across all surfaces) is identical; only the
vocabulary differs. **Default (this draft): the 2-axis model.** Flip with one
word and the title/§2 restructure to three axes.
