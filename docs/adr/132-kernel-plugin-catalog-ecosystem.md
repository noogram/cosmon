# ADR-132 — Cosmon is a kernel; the private crates are an installable plugin catalog

**Status:** Accepted
**Date:** 2026-06-23
**Accepted:** 2026-06-24 (ratification deliberation
`delib-20260624-8d8f`,
7-seat adversarial panel: architect · torvalds · buterin · niel · godel ·
feynman · adversary — unanimous **ACCEPT-WITH-AMENDMENT**, no REVISE). The five
non-blocking amendments A1–A5 from that panel are folded inline below (see
§0 — Ratification amendments) and resolved against the live tree at `main`.
**Decider:** Noogram
**Authoring molecule:** `task-20260623-4ac2` (📐 decision)
**Accepting molecule:** `task-20260624-6355` (📐 decision)
**Operator vision source:** operator brief 2026-06-23 ("cosmon = the KERNEL; the
28 currently-private crates are a CATALOG of installable plugins/apps").

**Builds on:**
- `delib-20260620-ca76`
  — open-core licensing: AGPL kernel + permissive network SDK, and the
  **network-boundary doctrine** that makes the partition coherent.
- `delib-20260622-187a`
  — pre-publication adversarial architecture review (B1 scope-trim, B2
  truth-in-claims).
- `task-20260622-eeb9`
  — the scope-trim that `git rm`'d the 28 crates and **ripped the in-process
  llama.cpp dependency out of the kernel** (the first plugin extraction,
  already done).

**Recoupe:**
[ADR-126](126-crate-frontier-two-gates.md) (Scope/Confidentiality two-gate
frontier) ·
ADR-127 (deny-by-default release membrane) ·
[ADR-128](128-d7-attribution-vacuum-and-publish-gate.md) (Noogram attribution) ·
[ADR-092](092-license-bascule-mpl-to-agpl.md) (AGPL core / Apache frontier) ·
[ADR-079](079-worker-spawn-port-and-adapter-contract.md) (Worker-Spawn **Port**
+ Adapter contract — the deferred `Spawn` trait) ·
[ADR-119](119-adapter-exit-code-contract.md) (structured Adapter exit-code
contract) ·
[ADR-043](043-provider-abstraction.md) (provider abstraction, the deleted
`LlmProvider` port) ·
[ADR-118](118-llmport-doctrine-and-degradation-matrix.md) (LLMPort doctrine).

---

## 0. Ratification amendments (delib-20260624-8d8f)

The 7-seat adversarial panel ratified the kernel/plugin/catalog **model** as the
right foundational shape — safe to move Proposed→Accepted — subject to five
non-blocking amendments. None blocks acceptance of the model; **A1 blocks the
*distribution build*, not this ADR.** They are folded into the sections below and
summarised here for the audit trail.

- **A1 — port disjointness is a REQUIRED wiring invariant, not an accomplished
  fact** (architect/torvalds/adversary). §6 F1 previously read as a present-tense
  claim that "the assembler validates port disjointness at build time." That
  check **has no implementation** — there is no `distribution.toml` parser in
  `crates/`, and the live POC still ships the very `:8080` collision F1 claims to
  have fixed (`llama-plugin` and `cosmon-rpp-adapter` both default to `:8080`).
  §6 F1 is rewritten as a **required-but-not-yet-enforced** contract on the
  downstream assembler. Tracked follow-up (`temp:warm`): implement
  `[kernel.reserved_ports]`, make the plugin `port` a required explicit field,
  and add the build-time disjointness gate. See `task-20260624-107f` (the
  smithy-flags resolution this folds in).
- **A2 — the plugin seam contract is versioned** (buterin). The plugin
  integration contract (S1–S4 obligations + manifest schema) carries a version
  so a moving kernel cannot silently strand permissive plugins; version-skew is
  the one kernel→plugin failure mode otherwise unguarded. Folded into §2c.
- **A3 — the anti-resale claim is right-sized to its true scope** (buterin).
  AGPL stops a reseller forking a *modified* kernel closed; it does **not** stop
  a proprietary-plugin moat wrapped around an *unmodified* kernel — the same
  network boundary that lets plugins be permissive keeps a reseller's value-add
  on the permissive side. The §2d/§13 anti-resale language is demoted
  accordingly.
- **A4 — the baked substrate is declared in the taxonomy** (adversary). A
  distribution image bakes binaries that are *neither* a `members` crate *nor* a
  `[[plugins]]` entry — today the `claude` worker-spawn substrate. The manifest
  must enumerate the image via a required `[[baked_substrate]]` array (name +
  version + licence tag, or explicit `baked = false`), not only what cosmon
  *considers* a plugin. Folded into §6. **(Refined by F6.)** An earlier draft of
  A4 also listed `cosmon-rpp-adapter` here, "on the kernel side"; **F6 corrects
  that** — a baked *kernel* binary is kernel-delivered-with-the-kernel, accounted
  for by the kernel pin, **not** baked substrate. `[[baked_substrate]]` is
  reserved for the genuinely-out-of-kernel binaries (today: `claude`).
- **A5 — the catalog ceremony is right-sized to the N=0-published reality**
  (niel). No plugin repos exist yet (llama is the only forced graduation; the
  two MCPs are cheap-maybe; the rest are realistic tombstones). The §3 four-state
  lifecycle table is demoted to **"revisit at N=3 real published plugins."** §2d
  leads with **K1 as the enforceable invariant** and presents the network
  boundary as the *mechanism that makes K1 hold*, not as a co-equal definition
  (which `cosmon-rpp-adapter` then visibly violates as a heuristic).

## 1. Context — three reversals later, name the model

The lineage of "what ships publicly" reversed itself repeatedly (ADR-113 §9,
ADR-126 §1): crates were called private, then public, then held-out, because
**three different questions were welded into the word "private."** ADR-126
disentangled two of them — *Scope* (is it the product?) and *Confidentiality*
(does the source leak a secret?). The scope-trim (`task-20260622-eeb9`) then
`git rm`'d 28 crates so the public `cosmon` repo equals its product closure
(43 crates, exact match).

But the trim only answered *"does it ship in v0.1.0?"* with **no** for those 28.
It left the **third** question unanswered: *what are they, then — dead weight,
forever-private, or something with a future?* The operator's 2026-06-23 vision
answers it, and this ADR ratifies the answer.

> **The model: cosmon is a kernel. The 28 are a plugin catalog.**
> Like a Linux distribution — the kernel ships free and open; each application
> is published independently, when it is ready, under its own appropriate
> licence. The 28 are **not** "forever private." They are **catalog
> candidates**: each graduates from `cosmon-private` to its own public home
> when it earns it.

This is not new infrastructure. It is a **name** for a structure the codebase
already has, plus a **lifecycle** for moving a crate across the frontier, plus
the **coherence proof** that the integration boundary and the licence boundary
are the same seam.

## 2. Decision

### 2a. The kernel / catalog partition

- **The kernel** is the public `cosmon` repo: the 43-crate product closure —
  the crash-recovery agent runtime (`cs`), its domain core, state/persistence
  ports, transport, graph, surface projection, the agent-engine and the
  network SDK. It is licensed **AGPL-3.0-only** at its code-linked core +
  product + federation, **Apache-2.0** on the pure-network SDK and vendored
  deps (delib-ca76 per-layer table; ADR-092).

- **The catalog** is the set of independently-publishable artifacts that
  *integrate with* cosmon but are *not part of* the kernel. The 28 trimmed
  crates are its founding members (`almanac-*`, `mailroom-voice-*`,
  `foundry-*`, `ga-*`, `neurion-mcp`, `topon-mcp`, `cosmon-llama`,
  `cosmon-llama-sys`, `cosmon-saas`, `schedulerd`, `cosmon-matrix-tick`,
  `cosmon-bridge-gastown`, `noogram-mycelial-monitor`, …). Each is published
  **on its own clock, in its own repo, under its own licence.**

### 2b. The kernel ships ZERO dependency on any catalog member (load-bearing)

The kernel's `[dependencies]` closure contains **no plugin**. This is already
true and must stay true:

- The in-process llama.cpp path was removed (`task-20260622-eeb9`):
  `cosmon-provider` dropped the `llama`/`mock-ffi` features and the
  `cosmon-llama` optional dep; `ProviderId::LlamaCpp` survives **only** as a
  state-deserialisation contract (`ensure_compiled()` always returns
  `FeatureNotCompiled("llama")`). The local-inference path is now **pluggable,
  not built-in**.
- A kernel that code-linked a plugin would (i) re-grow the build-graph
  entanglement the scope-trim removed, and (ii) drag the plugin's licence into
  the kernel's closure, breaking the partition of §2d.

**Invariant K1.** No crate in the public `cosmon` `members` set may carry a
catalog member in its real (`-e normal --no-dev`) dependency closure. This is
the dependency-direction face of the network-boundary doctrine, and it is the
same shape as the existing `cargo deny` / "no Apache crate links AGPL" CI gate
(delib-ca76 §d4).

### 2c. The plugin extension point — it already exists, at the process/network boundary

This ADR's central technical finding (verified against the live tree, not
assumed):

> **Cosmon has a clean plugin extension point today — but it is a
> process/network boundary, never a code-link boundary.** A plugin integrates
> by being *spawned* or *talked to*, never by being *imported*.

Four sanctioned integration seams exist, in descending order of decoupling.
A catalog member uses one of them; it does **not** get linked into the kernel.
(How an assembled kernel+plugin is *deployed* — the distribution profile and its
supervision/port discipline — is §6 and
[`docs/distribution-mechanism.md`](../distribution-mechanism.md).)

| Seam | Mechanism | Code change to kernel? | Example |
|------|-----------|:---:|---------|
| **S1 — Network provider** | An OpenAI/Ollama-compatible HTTP backend, reached via `[adapters.*] base_url` in `.cosmon/config.toml`. | **None** (pure TOML) | `cosmon-llama` republished as a `llama-server` the existing `openai`/`ollama` adapter POSTs to. |
| **S2 — Subprocess adapter** | A binary the worker-spawn Port launches, conforming to the [ADR-119](119-adapter-exit-code-contract.md) structured exit-code contract + the [ADR-079](079-worker-spawn-port-and-adapter-contract.md) Adapter obligations. | **None** if it reuses an existing spawn shape; in-tree `match` arm only for a genuinely new substrate (§2e). | Aider / Codex / a custom CLI harness. |
| **S3 — MCP tool/server** | An out-of-process MCP server the kernel or a worker calls as a client. | **None** | `neurion-mcp`, `topon-mcp`. |
| **S4 — Formula** | A TOML formula over existing molecules (the *only* sanctioned in-repo extension per the composability principle). | **None** | any workflow plugin. |

The kernel's job at the seam is to **discover and dispatch**, never to embed.
`AdapterEntry` (`cosmon-core::config`) already lets a TOML row declare an
adapter's `base_url`, `api_key_env`, `default_model`, `extra_args`,
`briefing_format`, and `pane_signatures` — **so an entire class of inference
plugins (S1) plugs in with zero kernel code.** This is how `cosmon-llama`
re-plugs after its extraction: not as an FFI crate the kernel imports, but as a
local server an adapter is *pointed at*.

**The seam contract is versioned (A2, buterin).** The plugin integration
contract — the S1–S4 seam obligations plus the distribution manifest schema (§6)
— carries an explicit **contract version**. The asymmetry the catalog must guard
is **kernel→plugin**, not plugin→kernel: a moving kernel could otherwise silently
strand a permissive plugin with no holdout cost and no forkable contract to pin
against (version-skew capture is the one of three failure modes with no other
guard). A versioned seam gives a permissive plugin a stable, forkable contract to
declare compatibility against and a visible break when the kernel moves under it.

### 2d. Licensing — the integration seam IS the licence seam (the coherence proof)

> **Framing (A5, architect).** The **enforceable invariant is K1** (§2b): no
> `members` crate links a catalog member in its real dependency closure. The
> network/process boundary below is the **mechanism that makes K1 hold by
> construction** — a *correlated heuristic*, not a co-equal definition. The
> distinction is load-bearing because the heuristic does **not self-classify**:
> `cosmon-rpp-adapter` is network-coupled at runtime yet AGPL by code-link, so
> the ADR must manually carve it kernel-side (§6 F3). K1 — the cargo
> dependency-direction — is the definition that always decides; the network
> boundary is how the catalog is built so that K1 stays true cheaply.

The licence partition (delib-ca76) and the plugin boundary (§2c) are **not two
policies that happen to agree — they are the same line viewed twice.**

- The **network-boundary doctrine** (delib-ca76 §b): *a crate may stay
  permissive iff its real dependency closure contains zero AGPL crates — i.e.
  it reaches the kernel only over the network/process boundary, never by
  code-linking.*
- A **plugin**, by §2c, reaches the kernel only over that same boundary.
- **Therefore a catalog plugin can carry a permissive licence (MIT/Apache)
  coherently and by construction** — it never code-links the AGPL kernel, so it
  imports no copyleft obligation. The kernel stays AGPL; plugins stay permissive
  (adoption). The architecture *enforces* the licence story instead of merely
  asserting it.

**Anti-resale — right-sized to its true scope (A3, buterin).** AGPL on the
kernel is genuine protection against **one** resale shape: a reseller who forks a
*modified* kernel and ships it closed must either publish the modified source or
stop. It is **not** a moat against a **proprietary-plugin wrapper around an
*unmodified* kernel** — the very network boundary that lets plugins be permissive
(§2c) is the boundary that keeps a reseller's proprietary value-add on the
permissive side, code-linking nothing. The honest claim is therefore narrow:
AGPL stops *modified-kernel closed resale*, not *value-add on the permissive side
of the network boundary*. Any "anti-SaaS-resale" framing elsewhere in the lineage
(delib-ca76) must be read at this reduced scope.

**Directionality note (the one subtle case).** A plugin that a *downstream
distribution assembler* chooses to **code-link** into a kernel build (e.g. a
Rust inference crate added behind a cargo feature, as the old in-process
`cosmon-llama` was) keeps its own permissive licence on its own source —
permissive→consumed-by-AGPL is a legal direction; only the *combined* binary is
AGPL. But Invariant K1 forbids the *published kernel* from doing this; such
linking is a distribution-builder's act, outside cosmon's shipped closure. The
catalog's **default and recommended** integration is the decoupled S1–S4
boundary, precisely so the permissive licence is unconditionally honest.

### 2e. The one residual coupling — name it, defer its closure to ADR-079

The S2 subprocess seam is clean for any adapter that reuses an existing spawn
shape *or* an HTTP-compatible backend (S1). The single place in-tree code is
still required is a **genuinely new spawn substrate**: `cs tackle` dispatches
via a hardcoded `match adapter.as_str()` (`cmd/tackle.rs` ≈L2779) into
`spawn_*_session` functions, paired with the closed `ProviderId` enum
(`cosmon-provider::provider`). Adding a brand-new substrate touches both.

This is the same coupling [ADR-079](079-worker-spawn-port-and-adapter-contract.md)
already diagnosed: it committed the `{ Worker · Port · Adapter · Tier }`
vocabulary and explicitly **named but did not draw** the future trait
(`Spawn` / `WorkerSpawnPort`). The `LlmProvider` port that *did* exist was
deleted (ADR-043 rationale, `cosmon-provider/src/lib.rs`) once a kill-switch
grep found zero live non-test callers — concrete `match` dispatch earned more
than a speculative trait.

**This ADR does not draw the trait either, and adds no code.** Per the
composability principle (CLAUDE.md: *"before adding new infrastructure, ask: can
this be a formula over existing primitives?"*) and the stateless-CLI / no-daemon
invariants, cosmon must **not** grow a dynamic plugin-loader / dlopen registry /
manifest subsystem — that would be exactly the "new plugin interface" the
composability rule forbids. The S1/S3/S4 seams already cover the dominant plugin
classes with zero kernel code. **The `Spawn` trait remains the correct, minimal
closure for the S2 residual, and is drawn only when a *second* in-tree spawn
substrate actually demands it** (the same "two real callers before an
abstraction" discipline that deleted `LlmProvider`). Until then the residual is
a named, bounded `temp:warm` item, not a release blocker.

## 3. The publishability lifecycle of a plugin

> **Ceremony right-sized to N=0 (A5, niel).** No catalog plugin is published
> today: `cosmon-llama` is the only *forced* graduation, the two MCPs
> (`neurion-mcp`, `topon-mcp`) are cheap-maybes, and the remaining ~25 are
> realistic tombstones. The four-state lifecycle table below is therefore
> **ratified as doctrine but priced ahead of its reality** — it is ceremony at
> N=0. **Revisit and harden the lifecycle machinery at N=3 real published
> plugins**, not before. Until then the load-bearing content of this ADR is the
> two sentences of §2d (seam = licence-seam; K1 is the enforceable face) plus
> the K1 invariant of §2b; the table is the map for when the territory exists.

A catalog member is not "private forever" and not "publish on day one" — it
**graduates** through states. The gates reuse machinery cosmon already ships;
nothing here is new infrastructure.

| State | Meaning | Lives in | Gate to next state |
|-------|---------|----------|--------------------|
| **Incubating** | Co-developed inside the private `/srv/cosmon/cosmon` workspace; not in the public kernel. | private workspace | — |
| **Catalog-candidate** | Acknowledged as a *potential* plugin; integrates only over an S1–S4 seam (no kernel code-link). | private workspace, seam-clean | passes **Gate 1 (Scope)** of ADR-126 as *"a peripheral/app, not the kernel"* — by construction true for every candidate. |
| **Publish-ready** | Cleared to leave for its own public home. | own repo, pending flip | passes **Gate 2 (Confidentiality, ADR-126 §4 welding test)** + has its **own LICENSE/NOTICE** (permissive unless it has its own copyleft reason) + **Noogram attribution** (ADR-128) + a documented integration seam. |
| **Published** | Public, in its own repo/crate, on its own clock. | public, independent | the ADR-127 deny-by-default membrane clears every shipping path. |

**The graduation gates are the existing frontier gates, applied per plugin
instead of per release.** A plugin "earns" publication by (a) being
seam-clean (S1–S4, no K1 violation), (b) passing the ADR-126 welding test for
confidentiality, (c) carrying its own licence + Noogram byline, (d) clearing
the ADR-127 membrane. No new gate type is invented.

**`cosmon-llama` + `cosmon-llama-sys` are the first worked example.** They were
extracted from the kernel (`task-20260622-eeb9`), are confidentiality-clean
(vendored llama.cpp, no operator identity), and are the operator's named
candidate for republication as **MIT/Apache llama.cpp inference plugins**. Their
recommended re-integration seam is **S1** (a `llama-server` the `openai`/`ollama`
adapter is pointed at via `base_url`) — which keeps Invariant K1 intact and the
MIT licence unconditionally honest. (A future Rust-native local-inference path —
the operator's stated direction, memory `project_cosmon_autonomy_local_first` —
is itself a catalog plugin under the same lifecycle, not a kernel feature.)

## 4. What this is NOT

- **Not a runtime plugin-loader.** No dlopen, no manifest registry, no daemon,
  no new state store. cosmon stays a stateless CLI over JSON files. A "plugin"
  is an independently-published artifact reached over an existing seam — not a
  module the kernel discovers and links at runtime. (Composability principle;
  ADR-016 layer boundary.)
- **Not a contradiction of "molecules + formulas are the only extension
  points."** That rule governs *in-repo* extension of cosmon's own behaviour. A
  catalog plugin is *out-of-repo* and integrates as an external process/service;
  the two statements live on different sides of the kernel boundary. The
  "catalog" is a **publishing-lifecycle** concept, not a second extension
  mechanism bolted into the core.
- **Not a relitigation of the v0.1.0 scope line.** ADR-126 §7 owns that. This
  ADR governs what happens to the *out-of-scope* crates afterward.
- **Not new code.** The extension point is the set of existing seams (§2c); the
  one residual (§2e) is deferred to ADR-079's already-named `Spawn` trait, drawn
  only when a second real caller demands it.

## 5. Consequences

- **Positive.** The 28 crates get a future instead of a tombstone; "private"
  finally means one thing (incubating-in-private), with a defined path to
  public. The licence story becomes *enforced by architecture* (§2d) rather than
  asserted by policy. The kernel's "zero plugin dependency" claim (a delib-187a
  truth-in-claims concern) is now a named invariant (K1) with a CI shape.
- **Negative / cost.** Each graduation is real per-plugin work (extract repo,
  LICENSE/NOTICE, Noogram byline, seam doc, ADR-127 clearance). The S2 residual
  (§2e) means a brand-new spawn substrate still edits the kernel until the
  `Spawn` trait is drawn.
- **Anti-regression.** Invariant K1 should gain a `cargo deny`-style check
  ("no `members` crate links a catalog member") alongside the existing
  licence-direction gate — filed as a `temp:warm` follow-up, not built here.
- **Coherence checklist (this ADR is docs-only):** stateless ✔ · idempotent ✔ ·
  regime-aware ✔ (changes no command) · single perimeter ✔ (adds no verb) ·
  no daemon / no new extension mechanism ✔ (reuses existing seams) ·
  network-boundary doctrine preserved ✔.

## 6. Distribution deployment shape — the three integration flags

The seam model (§2c) answers *how a plugin couples to the kernel* (over a
process/network boundary). It left open *how an assembled kernel+plugin actually
runs* — the **distribution** question. Mapping the first real distribution (the
*avatar*: kernel + the `llama` plugin) onto the model surfaced three gaps —
exactly the kind the catalog ADR exists to close. They were raised cosmon-ward
from smithy (`task-20260624-107f`) rather than patched silently. The canonical
treatment lives in [`docs/distribution-mechanism.md`](../distribution-mechanism.md)
(the distribution profile schema + the assembler discipline); the design
resolutions are:

- **F1 — port wiring (no implicit `:8080`) — REQUIRED wiring invariant, not yet
  enforced (A1).** A port-binding plugin's `port` **must be** an always-explicit,
  required field in `distribution.toml`; the kernel **must** declare its reserved
  ports (`[kernel.reserved_ports]` — today `cosmon-rpp-adapter = 8080`); and the
  downstream assembler **must** validate port disjointness at build time. A
  kernel co-residing with an HTTP ingress has already spent `:8080`, so a
  server-plugin must never inherit it implicitly.
  > **Truth-in-claims (A1, architect/torvalds/adversary).** This is stated as a
  > **required contract on the downstream assembler, NOT an accomplished fact.**
  > Verified against `main`: cosmon has **no `distribution.toml` parser** in
  > `crates/`, no `[kernel.reserved_ports]` schema field, `render-baked-config.py`
  > parses-but-ignores `container_port`, and `build-distribution.sh` runs **zero**
  > port validation — so the disjointness check does not exist yet. Worse, the
  > live POC still ships the very collision it claims to have fixed: both the
  > `llama` plugin and `cosmon-rpp-adapter` default to `:8080`, and a second real
  > distribution builds **green** while racing two servers for one port at
  > runtime. The `:8080`→`:8081` move for the avatar's `llama` is the *intended*
  > resolution, not a landed one. **Tracked follow-up (`temp:warm`):** add the
  > `[kernel.reserved_ports]` table, make plugin `port` a required explicit field,
  > and gate disjointness at build time **before any real avatar build**. Source
  > of the flag and its resolution: `task-20260624-107f` (smithy-flags).

- **F2 — supervision (compose sibling is canonical).** A server-plugin (an S1
  process binding a port) runs as its **own sibling service in the compose
  stack**; the kernel **image stays single-process** (one PID 1, the cosmon
  ingress server). An in-image multi-process supervisor is a daemon-keeping-
  daemons-alive — exactly the supervision the stateless-CLI / no-daemon
  discipline refuses; compose already *is* the supervisor. An `in-image`
  supervisor remains a **downstream distribution-assembler's choice** (same
  directionality as the §2d code-link note), explicitly non-canonical and not
  shipped by cosmon. Sovereignty is a property of the network boundary, not the
  process table — a compose sibling on the same VM is as in-VM as a co-resident
  process.

- **F3 — `cosmon-rpp-adapter` is kernel-side, not a plugin.** The RPP
  (`cosmon-rpp-adapter`) is the **§8j ingress Port adapter** ([ADR-080](080-remote-pilot-port-https-oidc.md)
  / [ADR-117](117-rpp-central-security-capability.md)), an AGPL `members` crate
  in the published kernel closure. It is **not** a catalog plugin (it does not
  integrate over S1–S4; it *is* the kernel's own §8j boundary) and **not** a
  worker-spawn Adapter (ADR-079 — same word, different Port: ingress vs egress).
  It therefore carries **no `[[plugins]]` line**, and the manifest does not lie
  by omission — a plugin line would be the lie. Its `:8080` is precisely the
  reserved kernel port of F1: the kernel owns `:8080` because the *kernel*, not a
  plugin, ships the thing bound to it. (The `claude` binary baked alongside is
  the external *substrate* of the default worker-spawn Adapter — neither kernel
  crate nor catalog plugin, a baked runtime dependency a distribution may omit,
  as the local-default container does. It is declared explicitly in the
  `[[baked_substrate]]` taxonomy below — F4.)

- **F4 — the baked substrate is a third taxonomy class, declared explicitly
  (A4, adversary).** A distribution image bakes binaries that are **neither a
  `members` crate (kernel) nor a `[[plugins]]` entry (catalog)**. Today that is
  the **`claude` binary** — the external substrate of the default worker-spawn
  Adapter. The earlier manifest enumerated only what cosmon *considers* a plugin
  and was therefore silent about `claude`; a manifest that names the image only
  by its plugins **lies by omission**. The distribution profile schema gains a
  required **`[[baked_substrate]]`** array: every baked binary that is neither a
  kernel `members` crate nor a catalog plugin is named + versioned + licence-
  tagged, or explicitly marked `baked = false`. The manifest must enumerate the
  **image** (kernel crates + plugins + baked substrate), not only the cosmon-
  authored slice of it. Canonical schema: [`docs/distribution-mechanism.md`](../distribution-mechanism.md).

- **F5 — no private commit pin in this public ADR (amendment d).** This ADR text
  carries **no `cosmon-private` archive commit hash** — the interim plugin-source
  pin (`archive_rev`) lives only in the distribution example and the canonical
  mechanism doc, and must be **externalised to a gitignored symbolic ref**
  (resolved from a local file), never inlined into a public-audience ADR. Same
  externalisation discipline as the release denylist; this is the tree-side twin
  of ADR-133's pre-flip scrub of the treasure-map coordinate (ADR-133 §6, B2).
  A future reader of this ADR is handed the *model*, never the *coordinate*.

- **F6 — a distribution embeds the WHOLE kernel, never a selection (the
  indivisible-kernel rule).** A distribution image **builds and bakes the entire
  kernel — every one of its binary surfaces (`cs`, `cosmon-rpp-adapter`, and any
  future kernel binary) — never a hand-picked subset.** The kernel is one
  indivisible block; a `[[plugins]]` entry is a process-separated addition
  *around* that block, never a knob for swapping a kernel surface in or out. The
  POC drift that motivates this rule: a `Containerfile` that built **only `cs`**
  produced an image missing the kernel's own ingress surface
  (`cosmon-rpp-adapter`), and so could not honour the cosmon-server HTTP contract
  (`/api/healthz` + tenant API on `:8080`). It had silently shipped *part* of the
  kernel and called it the kernel. **An image either bakes the whole kernel or it
  is not a cosmon-server distribution.**

  > **Taxonomic consequence — `cosmon-rpp-adapter` is NOT `[[baked_substrate]]`
  > (refines A4 + F3 + F4).** A *kernel binary baked because it is part of the
  > kernel* is **kernel, delivered with the kernel** — its provenance is already
  > the kernel pin (`[kernel].rev`). It is therefore **neither** a `[[plugins]]`
  > line (F3) **nor** a `[[baked_substrate]]` line (F6): listing it as baked
  > substrate would double-count a kernel surface as if it were an external
  > dependency. The `[[baked_substrate]]` array (F4) stays useful only for
  > binaries that are **genuinely outside the kernel** — neither a `members`
  > crate nor a plugin — the canonical case being the external `claude`
  > worker-spawn substrate. So the three taxonomy classes a manifest enumerates
  > are: **kernel** (the whole 43-crate block, pinned once by `[kernel].rev`,
  > including `cosmon-rpp-adapter`), **plugins** (`[[plugins]]`, process-separated
  > around the kernel), and **baked substrate** (`[[baked_substrate]]`,
  > out-of-kernel binaries like `claude`). F3 and F6 are one fact seen twice:
  > rpp-adapter is kernel-side, so the kernel commit *is* its provenance.

  > **Corollary — a crate's version number is NOT a freshness signal.** The
  > avatar POC built an image whose `cosmon-rpp-adapter` reported `version 2.2.0`
  > even though the JWKS-fetch had already landed; the version field had simply
  > never been bumped (corrected to the `2.5.0` series in `task-20260627-9e4d`).
  > A version string is a *label a human maintains*, not a *measurement of the
  > bytes*. **The reliable, falsifiable signal that a pinned kernel tree contains
  > the JWKS-fetch is the presence of the file
  > `crates/cosmon-rpp-adapter/src/jwks_fetch.rs`** in that tree — not the
  > adapter's reported version. When pinning a kernel commit for a distribution,
  > grep for that file; do not trust the version number.

## 7. Chronicle hook

Worth an internal chronicle entry: *the integration boundary and the
licence boundary turned out to be the same line.* The open-core licence
partition (AGPL kernel / permissive frontier) was decided for *rent* reasons
(delib-ca76); the plugin boundary was decided for *coupling* reasons. They
landed on the identical seam — the process/network boundary — so a plugin is
permissive *because* it cannot code-link the kernel, and it cannot code-link the
kernel *because* that is what makes it a plugin. One line, two names. (Companion
to ADR-126 §10 *"'private' was one word doing two jobs"* — here, *two doctrines
were one seam.*)
