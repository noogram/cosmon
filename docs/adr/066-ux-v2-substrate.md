# ADR-066 — UX v2 substrate: wheat-pasted viewports, bounded-Δ coherence, apfel as ingress channel

**Status:** proposed (foundational — implementation deferred to sibling tasks)
**Date:** 2026-04-23

> **Slot reconciliation (2026-06-23, `task-20260622-e3c0`).** This ADR
> drafts the bounded-Δ invariant under the working number **§8f**.
> That number is now permanently the ratified "Inviolable two-plane
> rules" in `docs/architectural-invariants.md`; the bounded-Δ invariant
> was renumbered **§8t** (next free top-level slot) with no semantic
> change. Read every "§8f — bounded-Δ" below as **§8t**. The §8f–§8i
> reservation cited under "(2)" is likewise narrowed to **§8g–§8i**.
> See the slot-collision resolution note under §8t in the invariants
> file.
**Parent deliberation:** `delib-20260423-becf`
**Authoring task:** `task-20260423-de93`
**Panel that converged on it:** jobs · jr · feynman · wheeler · tolnay · torvalds · niel · einstein · turing

**Ratifies** three propositions surfaced in the synthesis §VII:

- **§8k' — cross-surface wheat-paste extension.** *"One canon, many
  wheat-pastings — across surfaces too."* (JR §I, C2, C8)
- **§8f — bounded-Δ surface coherence.** Per-observer Δ contracts
  replace simultaneity. (Einstein §I, C7, S4)
- **Apfel as channel on the operator's Nucléon** — NOT a separate
  Nucléon. One `cause: {kind, agent, channel}` field on session-notes
  carries the decidability. (Tolnay/Einstein/Turing C4 + C5 + C6)

**Implementation siblings (separate molecules — not covered by this ADR):**
- `task-20260423-e49e` — `cause: {kind, agent, channel}` field on
  session-notes + `via:` field on `nucleate` events.
- `task-20260423-aaef` — Souffleur (apfel chat panel) with
  `apfel-mac-local` §8j binding, read-only first.
- `task-20260423-21e0` — Skylight (per-galaxy whisper window).
- `task-20260423-f74e` — iOS parity (Inbox + Whispers in `ios-pilot`).
- **Future remediation bead** — convert existing
  `apps/mac-pilot/mac-pilot/PilotView.swift` and
  `apps/ios-pilot/ios-pilot/ContentView.swift` to `WheatPasteView`
  consumers. File at the moment the remediation is picked up; name it
  here only so future reviewers recognise the pointer.

**Related ADRs:**
- [ADR-016](016-autonomy-regimes-and-resident-runtime.md) — regime
  vocabulary. §8f explicitly forbids any transport mechanism that
  would require a resident daemon on the Transactional Core.
- [ADR-038](038-whisper-perturbation-port.md) — live-perturbation port.
  Non-substitute for §8f: whispers are one-bit pilot→live-worker
  perturbations, not a surface-sync channel.
- [ADR-061](061-pilot-session-and-causal-closure.md) — pilot-session +
  Nucléon. Apfel does **not** get its own `nucleon_id`; it is a typed
  channel on the operator's existing Nucléon.
- [ADR-062](062-quotaclock-9th-clock.md) — frontmatter convention
  imitated here; cost accounting via `via:` = `apfel` field on
  `nucleate` events.
- ADR-064 — §C4 wheat-paste
  precedent (postman's uniform stays outside the house) and §8j
  ingress-bindings pattern reused here.

---

## Context

The 2026-04-23 UX v2 deliberation asked nine personas the same
question: *how does cosmon grow beyond the menubar — to a full
macOS app, an iPad/iPhone parity, and an on-device apfel chatbot —
without splintering the operator's mental model?* Seven of the nine
converged on a structural answer that precedes any feature: **the
substrate must be named before the surfaces are multiplied.**

Three convergences dominated:

1. **`cs peek` is the cockpit; every other surface is a wheat-pasting
   of it** (Jobs + JR + Niel + Wheeler). The canonical rendering is
   the monospace raster emitted by the TUI; SwiftUI apps are
   *viewports*, not re-renderings. The operator's muscle memory is a
   first-class invariant — what works on the terminal must work
   byte-identically on the iPhone.

2. **Cosmon is already relativistic; admit it** (Einstein). The DAG
   carries 1 bit of ordering; the filesystem carries content; a
   hidden third "live sync" channel would be a lie or a daemon. The
   correct UX invariant is **bounded staleness**, per-observer Δ
   contracts, not simultaneity.

3. **Apfel is a tool of the operator, not a parallel cogniser**
   (Tolnay + Einstein + Turing). Billing apfel actions to a separate
   `nucleon_id` would split causal closure and forge an identity
   without a sealed carnet. Promotion is gated on T1–T7 of the
   canonical `nucleon-test` formula (Turing §II), not on intuition.

JR named a **structural breach**: the current SwiftUI apps
(`apps/mac-pilot/mac-pilot/PilotView.swift`,
`apps/ios-pilot/ios-pilot/ContentView.swift`) already render cosmon
state through a different visual vocabulary (`TabView`,
`RoundedRectangle`, `Label(systemImage:)`). They are second
renderings, not wheat-pastings. This ADR ratifies the invariant that
forbids new breaches and schedules the remediation of existing ones
in a separate workstream (see *Followups*).

The ADR does not ship a full remediation. It ratifies the vocabulary,
the invariants, and the first enforcement tool (`WheatPasteView` +
CI golden-snapshot test) so that the next three tasks
(`task-20260423-e49e`, `-aaef`, `-21e0`, `-f74e`) can build *on top
of* a sealed contract instead of drifting further from it.

---

## Decision

### (1) §8k' — Cross-surface wheat-paste extension

Add to `docs/architectural-invariants.md` as a rider of the existing
§8k wheat-paste rule (inherited from ADR-064 §C4 / JR's
*postman-uniform* frame):

> **§8k'. Cross-surface wheat-paste.** Every cosmon-facing surface
> — TUI (`cs peek`), menubar popover, full-window macOS app, iPad,
> iPhone, Souffleur (apfel chatbot panel), future Vision / Apple TV /
> e-ink / web mirror — is a **viewport over the canonical raster
> emitted by `cs peek --snapshot`**. A viewport MAY:
>
> - clip, scroll, or scale glyphs uniformly (Retina, accessibility);
> - tint for dark / light;
> - translate touch gestures into the keystroke vocabulary the TUI
>   already consumes (`j/k`, `b/e/s/l/n`, `+/-/=`).
>
> A viewport MUST NOT:
>
> - re-render the same state in a different visual vocabulary
>   (rich-text bubbles, force-directed graphs, native list cells,
>   rounded badges, system icons substituted for ASCII glyphs);
> - introduce per-surface affordances (segmented controls,
>   swipe-to-action, contextual menus, SF Symbols replacing glyphs);
> - cache or summarise `cs peek --snapshot` output through an LLM,
>   a Markdown renderer, a syntax highlighter, or any "nicifier".
>
> **Test of legitimacy.** A screenshot of surface A and a screenshot
> of the same molecule on surface B overlay glyph-for-glyph, modulo
> crop and tint. If they do not, the canon is broken — file a bead,
> do not patch the surface.
>
> **Enforcement.** A single Swift adapter — `WheatPasteView(snapshot:)`
> — is the only path through which a SwiftUI view may display cosmon
> state; a CI golden-snapshot test pins the byte-identical raster
> across all surfaces.

**Scope clause.** New surfaces (Souffleur panel, Skylight windows,
iOS parity views) comply day one. Existing SwiftUI apps stay as-is
under a grandfather clause; the remediation is scheduled as a
separate bead (see *Followups*). This honours JR's invariant
without blocking Torvalds's next commit (synthesis §III D2).

**Why this is §8k' and not a new §-primitive.** §8k (wheat-paste
monoculture, ADR-064 §C4) already states the rule for the
filesystem-to-UI direction. §8k' is its cross-surface specialisation
— same invariant, additional axis. It is not a new invariant class;
it is the projection of §8k onto the multi-surface case. (Same
pattern as §8j vs §8e–§8i in ADR-064.) The §8-series remains closed.

### (2) §8f — Bounded-Δ surface coherence

Add to `docs/architectural-invariants.md` as a new subsection (next
free slot in the 8-series after §8e), with the Einstein synthesis
§I verbatim as the justifying gedankenexperiment.

**Naming-slot collision (resolved 2026-06-23, `task-20260622-e3c0`).**
The existing §8j rider (ADR-064) reserved `§8f–§8i` for the phase-2 ADR
drafted in `idea-20260422-8ec9` (tackle exclusivity, delegation chain,
budgeted nucleation, dispatch acyclicity). §8f is now permanently the
ratified two-plane rules, so: this bounded-Δ invariant is renumbered
**§8t**, the phase-2 reservation is narrowed to **§8g–§8i** (tackle
exclusivity is already ratified prose — `cs tackle` is one node — and
claims no slot), and §8f is vacated by both drafts. The semantic
content never depended on the slot number.

> **§8f. Bounded-Δ surface coherence.** For any two surfaces S₁, S₂
> observing the same molecule `m` whose authoritative state is the
> file `<galaxy>/.cosmon/state/.../molecules/<m>/state.json` on host
> H, a write committed at S₁ at time t' is visible to a read at S₂
> at time t iff (t − t') ≥ Δ(S₁, S₂), where Δ is the sum of:
>
> (a) S₁'s flush-to-disk latency on H,
> (b) the network round-trip if S₂ ≠ H,
> (c) S₂'s polling cadence (0 if S₂ is a one-shot read).
>
> **No surface may cache `state.json` content beyond Δ without an
> explicit invalidation event.** The system makes no claim of strong
> consistency, simultaneity, or notification — only of **bounded,
> observable, falsifiable staleness**, where the bound is declared
> by the surface and verifiable by `cs verify --surface <name>`.
>
> **Live push is forbidden.** A WebSocket, SSE stream, push broker,
> or notification service would require a resident daemon on the
> Transactional Core, which is forbidden by ADR-016. "Moving the lie
> to a different layer" (Einstein §VIII) does not resolve the
> closure violation.
>
> **Manifest.** Each surface declares its Δ and its read path in a
> `coherence.toml` sidecar (path convention:
> `apps/<surface>/coherence.toml`, or
> `crates/<surface-backed-crate>/coherence.toml`). Fields:
>
> ```toml
> [surface]
> name         = "mac-pilot"
> read_path    = "cs --json observe"     # never direct .cosmon/ read
> poll_cadence_ms = 5000
> flush_budget_ms = 150
> rtt_budget_ms   = 50                    # 0 for same-host surface
> delta_max_ms    = 6200                  # flush + rtt + 2×cadence
> ```
>
> `cs verify --surface <name>` reads the manifest, exercises the
> declared read path once, and asserts that a write-then-read cycle
> observes the state within `delta_max_ms`. Mismatch exits non-zero.

**The justifying thought experiment (Einstein §VIII, verbatim):**

> *Pretend Tailscale RTT is 8s (operator on a sailboat, satellite
> link). What still works: every typed link in the DAG, every
> `cs nucleate`, every `cs done` — because each is a one-shot atomic
> CLI call against the local filesystem on H, propagated to the
> remote surface lazily. What breaks: anything that assumes "I
> clicked, therefore the other surface saw it." WebSocket push
> doesn't help — it just moves the lie to a different layer. What
> emerges as the fundamental invariant: the only consistent
> multi-observer view of cosmon is the filesystem at H, and every
> other surface is a delayed, possibly-stale window onto it;
> surfaces must declare and verify their Δ, not pretend Δ=0.*

**Canonical Δ budget (LAN/Tailscale, 2026-04).** poll_cadence ≤ 5s,
RTT ≤ 50ms, flush ≤ 150ms ⇒ worst-tolerated stale-read ≈ 6s. A
surface showing stale data > 15s after a confirmed landed write is a
bug, not a tradeoff.

### (3) Apfel as channel on the operator's Nucléon

Three co-operative decisions, none new ADR-material on its own, but
all required for the ADR to hold:

**(3a) No separate `nucleon_id` for apfel.** Until T1+T2+T4+T5+T6 of
the `nucleon-test` formula below pass, apfel is a **typed channel on
the operator's Nucléon**, not a parallel cogniser. Actions flowing
through apfel are billed to the human with `cause_kind =
oracle-suggestion` (or `transcription` for voice-to-cs). Implementation
is sibling `task-20260423-e49e`.

**(3b) §8j ingress bindings for apfel (substrate reuse, not a new
port class).** The §8j rule (ADR-064 §2 + §8j rider) already governs
any non-CLI spark source. Apfel binds as two ingress ports under the
operator's Nucléon:

- `apfel-mac-local` — on-device FoundationModels on macOS.
- `apfel-ios-local` — on-device FoundationModels on iOS (Apple
  Intelligence-eligible devices only: M1+, A17+).

Each port must implement the four §8j clauses (materialize-before-DAG
write, identity-mapped-to-nucleon, pre-admission rate limit, one-way
topology). The identity-mapping clause (§8j-a) is satisfied by
binding the port to the operator's existing Nucléon — no separate
`matrix-identity.toml`-equivalent file is admitted. The admission
boundary `apfel_event_to_spark` lives inside the Souffleur panel's
Swift code and is exercised end-to-end by the `nucleon-test` fixture
in §IV below.

**(3c) The `cause` field on session-notes and `via:` on nucleate
events.** Adds one enum to each timestamped note line and each
`Sparked` event:

```rust
pub struct SparkCause {
    /// How the cognition reached the DAG.
    pub kind: CauseKind,
    /// Non-operator agent (if any) that mediated the cognition.
    pub agent: Option<AgentId>,
    /// Transport channel.
    pub channel: CauseChannel,
}

pub enum CauseKind {
    /// Human typed directly into a cosmon CLI.
    Direct,
    /// Human spoke; tool transcribed and ran cs.
    Transcription,
    /// LLM (Souffleur, world-model) suggested; human accepted.
    OracleSuggestion,
    /// Tool acted on its own admission budget (never v0).
    Autonomous,
}

pub enum CauseChannel {
    Keyboard,
    Voice,
    Matrix,
    Webhook,
    AppleFoundationModels,
    // ... extend per substrate ...
}
```

Three values, one line per note, decidable by inspection. **This is
the smallest schema change that restores T5 (decidable authorship).**
Without it, the carnet is forever an opaque mixture of {human,
apfel-author, apfel-transcriber}. The implementation crate and the
`cs nucleate` / `cs session note` flag additions are
`task-20260423-e49e`.

### (4) The `nucleon-test` canonical formula (T1–T7)

Ratify the seven admission tests as the canonical gate for any
future substrate that claims to be a Nucléon. Captured verbatim from
Turing's verdict (`responses/turing.md`) so the test surface is
closed before more substrates knock:

- **T1 — Causal trace on disk.** Every action attributable to the
  candidate leaves a record under
  `.cosmon/state/{sessions,nucleons,whispers}/...` *before* the DAG
  mutates. Implements §8e + §8j-b (materialize-before-write).
- **T2 — Append-only + sealed.** The trace file is append-only and
  BLAKE3-sealed (either per-file or per-entry chained). Detect silent
  post-hoc edit, not a motivated adversary (ADR-058 *propose, don't
  impose*).
- **T3 — Observability from `.cosmon/`.** A verifier with read-only
  filesystem access can answer *"what did this Nucléon do this
  hour?"* without inspecting an external process scope (chat
  history, audio buffer, mic stream).
- **T4 — Stable continuity ID.** A `NucleonId` newtype, stable
  across sessions (host portability optional per §8j-a granularity
  rule), with an identity file at
  `.cosmon/state/nucleons/<id>/identity.toml`, sealed.
- **T5 — Decidable authorship at every spark.** Each `cs nucleate`
  causable to the candidate carries `author_nucleon_id` AND a
  `cause: {kind, agent, channel}` field. Without both, {human,
  apfel-transcribing, apfel-suggesting} are indistinguishable — Rice's
  theorem says we cannot recover authorship from output text alone.
- **T6 — Bounded ingress contract (§8j compliance).** If the
  candidate is non-CLI (voice, network, world-model API), it admits
  sparks through an `<substrate>_event_to_spark` admission boundary
  enforcing identity-mapping, materialize-before-DAG, rate-limit,
  one-way topology. Same reduction as the Matrix bridge.
- **T7 — Adversarial-distinguishability.** Given two carnets — one
  written by the candidate, one by a competent human — a third-party
  verifier reading only `.cosmon/` can score them independently of
  mechanism. The Nucléon is admitted iff its trace is **evaluable**,
  not iff it "thinks". (Imitation Game inverted: we are not asking
  *"is it intelligent?"* — we are asking *"can its causal
  contribution be audited?"*.)

**Apfel's current score: 0/7.** The test gate is not a punishment —
it is the reusable measurement instrument. Future substrates
(world-model à la LeCun-AMI, Noogram-self, biological nucleons via
Matrix) pass through the same seven tests, no new Tn per substrate.

The formula stub is captured in
`.cosmon/formulas/nucleon-test.formula.toml` so future candidates can
run `cs nucleate nucleon-test --var candidate=<substrate>` and get a
sealed verdict molecule.

### (5) `WheatPasteView` adapter (the enforcement tool)

The smallest Swift primitive that honours §8k' at the library layer:

```swift
import SwiftUI

/// The only SwiftUI primitive authorised to display cosmon state.
///
/// §8k' — every surface consumes `cs peek --snapshot` bytes and
/// wheat-pastes them in place. No re-rendering, no reformatting,
/// no per-surface vocabulary.
public struct WheatPasteView: View {
    /// Raw snapshot bytes emitted by `cs peek --snapshot`. The
    /// adapter MUST NOT parse, transform, or prettify them.
    public let snapshot: String

    public init(snapshot: String) {
        self.snapshot = snapshot
    }

    public var body: some View {
        ScrollView([.horizontal, .vertical]) {
            Text(snapshot)
                .font(.system(.body, design: .monospaced))
                .textSelection(.enabled)
                .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        }
    }
}
```

Lives at `apps/CosmonKit/Sources/WheatPasteView.swift`. Every new
SwiftUI view in Skylight (`task-20260423-21e0`), Souffleur
(`task-20260423-aaef`), and iOS parity (`task-20260423-f74e`)
consumes exactly this type. A CI grep (Phase 2) forbids any other
SwiftUI primitive (`Text`, `Label`, `TabView`, `RoundedRectangle`,
`systemImage:`) outside this adapter; grep lint is out of scope for
this ADR but scheduled as a Followup.

### (6) Coherence checklist (§5 of invariants) — passed by construction

| # | Question | Answer |
|---|----------|--------|
| 1 | Stateless? | Yes — no new command is introduced by this ADR. |
| 2 | Idempotent? | N/A (no new command). |
| 3 | Regime-aware? | Yes — §8f applies to read surfaces (Inert + Propelled observers); apfel as §8j port inherits its regime from the spark it admits. |
| 4 | Single perimeter? | Yes — `WheatPasteView` is the sole SwiftUI entry point. |
| 5 | Symmetric undo? | N/A for ADR text; Followup implementation inherits existing `cs nucleate` ↔ `cs done` symmetry. |
| 6 | Runtime-compatible? | Yes — a resident runtime sees pilot-sessions + apfel-sourced sparks as ordinary molecules with typed `cause`. |
| 7 | Worker/human boundary? | Respected — apfel is a typed channel on the operator's Nucléon; workers never spark through apfel. |
| 8 | Write/read asymmetry? | Preserved — `cs verify --surface` is a pure read. |
| 9 | Merge-before-dispatch? | N/A (no dispatch change). |
| 10 | CLI-first for workers? | Yes — `WheatPasteView` consumes `cs peek --snapshot` bytes; no MCP dependency. |
| 11 | Scope-bounded? | Yes — §8k' applies to *surfaces*, §8f to *observers*, apfel to *one operator's Nucléon*. All three finite. |
| 12 | Self-similar? | Yes — §8k' composes (adding an Apple TV surface = new viewport, same canon); §8f composes (adding an S₃ = one more Δ); apfel composes (adding a second substrate = one more channel enum). |
| 13 | Alphabet-Closure? | **Mandatory**: when sibling tasks land the Swift/Rust edits, the §8k' + §8f text edits in `docs/architectural-invariants.md` MUST land in the same commit. |

No existing invariant is contradicted.

---

## Consequences

### Positive

- **One canon, finite surfaces.** Operator muscle memory transfers
  verbatim across devices. (JR §I, C2.)
- **Live push is explicitly ruled out.** §8f removes the WebSocket /
  SSE / APNS temptation that contradicts ADR-016. Future "let's just
  stream molecules" proposals: *file → close → cite §(2)*. (Einstein
  C7.)
- **Apfel promotion is test-gated, not vibes-gated.** T1–T7 apply
  uniformly to humans (tenant_auditor, Tenant-Demo), LLMs (apfel, Claude),
  world-models, and hypothetical Noogram-selves. (Turing C3/C4.)
- **Decidability is bought with one enum.** `cause: {kind, agent,
  channel}` collapses the {human, apfel-author, apfel-transcriber}
  indistinguishability forever. (Turing S3.)
- **Existing-app remediation is scheduled, not imposed.** The
  structural-breach call on `PilotView.swift` / `ContentView.swift`
  is named; cleanup lives in a separate bead so the next commit
  still ships. (JR §III D2.)

### Neutral / accepted costs

- **The `WheatPasteView` adapter exists.** ~30 lines of SwiftUI
  boilerplate that every surface imports. The alternative (inline
  `Text(snapshot).font(.monospaced)` everywhere) would be a
  per-surface vocabulary — the exact thing §8k' forbids.
- **Each surface ships a `coherence.toml`.** One file, ~10 lines,
  per surface. Auditable by grep.
- **Apfel availability gating is runtime, not Cargo feature.** The
  Rust workspace stays pure; Swift handles the
  FoundationModels-available / not-available branch at runtime
  (hidden / disabled-with-explanation / visible). No new Cargo
  feature, no build-graph split.

### Negative (risks)

- **Grandfather clause temptation.** New PRs against the existing
  apps may cite *"stay consistent"* to skip §8k'. Countermeasure:
  CLAUDE.md amendment pointing reviewers here + Followup remediation
  bead before the 2026-07 review (mirrors ADR-064 seuil cadence).
- **Δ manifests can drift.** A surface may declare `delta_max_ms =
  6000` while actually polling every 30s. `cs verify --surface` is
  the countermeasure; run it in CI per surface.
- **T1–T7 are necessary, not sufficient.** Passing all seven admits
  *auditability*, not *quality*. Imitation Game inverted — must be
  restated at every promotion decision.

### Open

- Whether `cs verify --surface` should also be part of the default
  `cs doctor` sweep. Tentative yes, deferred to the command's
  implementation bead.
- Whether `cs peek --snapshot` should be an explicit flag (today
  `cs peek --no-tui --once` approximates it) or a parse-stable
  sub-mode of peek. Deferred to `cs peek` follow-up; the ADR treats
  *"canonical byte raster"* as the contract, not the flag name.
- Whether the future remediation bead should land as a single PR
  (Torvalds would say yes) or as per-surface PRs (JR's original
  aesthetic). Defer to the remediation bead itself.

---

## Alternatives considered

### A. Ratify §8k' without a grandfather clause (rejected)

JR's initial framing asked for immediate remediation of
`PilotView.swift` and `ContentView.swift`. The synthesis §III D2
rejected this because it would block Torvalds's first commit
(drill-down DAG view in `InboxView.swift`) on a ~400-LOC rewrite.
The grandfather clause honours JR's invariant for new surfaces
(where compliance is cheap) and schedules the cleanup for old
surfaces as a separate workstream (where compliance is structural).

### B. Admit live push via a narrow broker (rejected)

A server-sent-events endpoint on `cs-api` would shrink Δ to ~100ms
for the iPhone. Einstein rejected: this is a daemon in disguise —
it must be always-running to deliver. The correct move is to
accept bounded staleness and declare it (Δ=6s is honest); live push
makes the lie faster, not truer.

### C. Give apfel its own `nucleon_id` immediately (rejected)

Tolnay, Einstein, and Turing converged 3-ways on *no separate
nucleon*. An apfel `nucleon_id` with no sealed carnet is an identity
token without a body (§ADR-061 *"nucleon-id can be forged"*). Until
T1+T2+T4+T5+T6 ship, billing actions to the human with `cause_kind
= oracle-suggestion` is the honest representation.

### D. Collapse §8k' into §8k (rejected)

§8k already forbids SDK monoculture drift (ADR-064 C4); §8k' adds
the multi-surface axis. Collapsing would overload §8k with two
distinct claims (postman-uniform + viewport-only) and complicate
future citation. The prime-notation (§8k') is the convention for
*projection of an existing invariant onto a new axis*, already used
by §8j (§8e projection onto ingress ports); keeping the families
visible preserves the §8-series as a finite, readable map.

### E. Use a per-surface snapshot (rejected)

Each surface could define its own "canonical raster" (mac-pilot
renders differently from ios-pilot because the screens differ). JR
refused: this is exactly the breach §8k' is trying to close. The
canon is one bit-stream; viewport translation (crop, scroll, tint)
is allowed; re-rendering is not. The screen size difference is a
viewport differential (crop-on-iPhone), not a content differential.

---

## Scope and non-scope

**In scope.** Ratifying §8k' + §8f in the invariants file; naming
`WheatPasteView` and its location; declaring `nucleon-test` (T1–T7)
as the canonical gate; declaring apfel as a channel with `apfel-*`
§8j bindings; listing Followups.

**Out of scope** — each a sibling/downstream molecule: `cause`/`via`
field (`-e49e`), Souffleur (`-aaef`), Skylight (`-21e0`), iOS parity
(`-f74e`), existing-app remediation (future bead),
`cs verify --surface` command, `cs peek --snapshot` flag
materialisation. Workshop existence (§III D3) and `cs-api` keep-vs-kill
(§III D1) are parallel decisions, not covered here.

---

## Followups

Tracked as sibling or downstream molecules; none block this ADR.

1. **`task-20260423-e49e`** — `cause: {kind, agent, channel}` on
   session-notes + `via:` on `nucleate` events. Required by §(3c)
   and by T5 of `nucleon-test`.
2. **`task-20260423-aaef`** — Souffleur (apfel chat panel), read-only,
   §8j binding `apfel-mac-local`, consumes `WheatPasteView`.
3. **`task-20260423-21e0`** — Skylight (per-galaxy whisper window),
   consumes `WheatPasteView` day one.
4. **`task-20260423-f74e`** — iOS parity (Inbox + Whispers tabs in
   `ios-pilot`), consumes `WheatPasteView` day one.
5. **Future remediation bead** — convert existing `PilotView.swift`
   and `ContentView.swift` to `WheatPasteView` consumers; nucleate
   `temp:warm` before 2026-07 review (mirrors ADR-064 three-month
   seuil).
6. **`cs verify --surface` implementation bead** — shape, exit
   codes, manifest parsing.
7. **CI grep lint** forbidding non-`WheatPasteView` SwiftUI
   primitives outside the adapter (~10 LOC shell, complementary to
   the golden-snapshot test).
8. **`cs peek --snapshot` flag materialisation** — explicit flag or
   parse-stable wrapper over `--no-tui --once`; contract is the
   byte-identical raster.

---

## Acceptance

This ADR is **proposed**, not **accepted**. Operator ratification is
the explicit next step. Until ratified:

- Sibling tasks MAY proceed with briefing and scoping.
- Sibling tasks MUST NOT land code that introduces `WheatPasteView`
  as the *sole* SwiftUI entry point (grandfather clause applies
  only after ratification).
- §8k' and §8f are drafted in `architectural-invariants.md` but
  flagged `(proposed — ADR-066)` so no downstream code treats them
  as hard rules prematurely.

JR's closing line from the deliberation is this ADR's motto:
*"one canon, many wheat-pastings — across surfaces too."* The ADR
gives the rule its surface.
