# ADR-072 — `session-route` formula, sidecar invariants, and the 4-tier routing cascade

**Status:** proposed (tactical — blocks v0 implementation of `session-route`)
**Date:** 2026-04-24
**Parent deliberation:** `delib-20260424-1b81`
**Authoring task:** `task-20260424-e857`
**Successor ADR:** [ADR-078](078-session-route-for-utterances.md) — lifts this output cascade to **input** (utterances) under the same `CognitiveResolver` / Tier discipline; inherits invariants I1–I4 verbatim.
**Operator amendments (binding):** `docs/delib-prep/2026-04-24-session-route-amendments.md`

**Implementation siblings (not covered by this ADR):**
- `task-20260424-a912` — ADR-B: `utterance` as first-class primitive (strategic, parallel, non-blocking)
- `task-20260424-59fd` — 11-note benchmark, operator blind-labelling
- `task-20260424-7335` — Impl: 4-tier cascade, sidecar writer, LaunchAgent, `cs session route` verb
- `task-20260424-69c1` — UX: verdict-door (markdown review file + SwiftUI view in parallel)
- `task-20260424-746e` — Rename sweep `digest` → `route`

**Related ADRs:**
- [ADR-047](047-event-log-protocol-v0.md) — event-log substrate
- [ADR-058](058-step-progress-invariant.md) — briefing seal model
- [ADR-061](061-pilot-session-and-causal-closure.md) — `pilot-session` kind, `nucleon_id` propagation
- [ADR-066](066-ux-v2-substrate.md) — wheat-paste UI substrate (mac-pilot / ios-pilot consume same bytes)
- [ADR-068](068-ux-cli-equivalence.md) — UX ↔ CLI capability parity

**Architectural invariants:** `docs/architectural-invariants.md` §8b (seal-as-trace), §8j (ingress bindings), §8f (data-plane discipline).

---

## Context

### The prefix seam

Today, `docs/guides/session-to-spark.md` ships the only mechanism that
lifts a `cs session note` into a typed molecule: the operator prepends
`!spark ` to the note body. A LaunchAgent scans every session every 5
minutes, recognises the prefix, and emits a `spark` molecule + sidecar
under `.cosmon/state/sessions/.promoted/<sid>/<HH-MM-SS>.md`.

The prefix is honest plumbing — it is BLAKE3-compatible (§8b: the
carnet file is never mutated, sidecars are the trace), it honours §8j
admission discipline (identity via session frontmatter `operator`,
causal closure via sealed file), and it is idempotent
(`.promoted/` sidecar is the dedup key).

What the prefix is **not** is a product. Every `!spark ` typed at the
keyboard is a moment in which the operator stops being a thinker and
becomes a categoriser. Every seam is a design failure. The symbol is
load-bearing only for the operator's keyboard — never for §8j, never
for §8b.

### The 11-note evidence

`.cosmon/state/sessions/session-2026-04-22T16-28-09Z.md` contains 11
notes written across 16 hours of cognition. **Zero** are prefixed with
`!spark `. The operator typed them as they arrived; the prefix
discipline lost to flow. Walked honestly:

| # | Note (excerpt) | Regex verdict | Human verdict |
|---|----------------|---------------|---------------|
| 1 | "Ou en est le developpement de cosmon?" | `\?$` → question | question |
| 2 | "dystopie: comment achète-t-on une baguette" | — | narrative/édito |
| 3 | "plusieurs idées" | — | **orphan (no route)** |
| 4 | "nommer la communauté Noogram … trouver un nom" | `trouver` → spark | spark |
| 5 | "définir un process d'onboarding: NDA, Yubi…" | `définir` → spark | **bundled** (3 sub-tasks) |
| 6 | "7 anneaux Tolkien pour les vetoers?" | `\?$` → question | idée-étoile |
| 7 | "github noogram-labs réservé … juridique?" | `\?$` → question | **polyvalent** |
| 8 | Multiplex de voix (200 words) | — | idée-étoile |
| 9 | "si validé dans l'ux, le multiplex…" | — | **refines #8** (relational) |
| 10 | "se renseigner sur la xbox portable" | `se renseigner` → spark | spark |
| 11 | "galaxie tenant-demo sur noogram ou noogram-labs?" | `\?$` → question | question |

Feynman's benchmark (panel) concluded: **5/11** deterministic,
**3–4** ambiguous even to the operator, **1** (#3) must stay
unclassified, **2** are multi-label. Shannon's entropy accounting
bounds the typology contribution at ≤ 2.58 bits per note against
body entropy H(body) ∈ [30, 2400] bits — roughly **1000:1**. The
typology is not the information; the body is.

### What needs to happen

The deliberation converged on five propositions (C1–C9 in
`synthesis.md`):

1. **Never mutate the carnet** — §8b compliance.
2. **Content-hash keying** — body-primacy makes zero-loss formal.
3. **Never auto-nucleate without operator consent** — staging, not
   pushing.
4. **One formula over existing molecules, no new daemon** —
   composability § respected.
5. **Confidence numbers never surface in UI** — translate to
   affordance, not a scalar.

The tactical ADR (this document) codifies those five propositions into
a concrete formula contract and the formal invariants that make "zero
information loss" testable.

### Operator amendments (binding, override where cited)

Four amendments on the post-synthesis verdict override parts of the
panel's convergence. They are reproduced here in abbreviated form;
the canonical statement lives at
`docs/delib-prep/2026-04-24-session-route-amendments.md`:

- **A1. Zero orphan.** `axes = ⊥` is **never terminal**. A note that
  fails confidence at tier *n* escalates to tier *n+1*. Tier 4 is
  human cognition — the note surfaces in the verdict-door with an
  explicit *"help me classify this"* affordance. There is no silent
  abandonment.
- **A2. Four tiers in v0.** The architecture ships **all four tiers
  on day one** — regex, local LLM (Apple MLX / llama.cpp), cloud LLM
  (Claude Haiku), human verdict-door. No progressive gating. The
  benchmark (`task-20260424-59fd`) calibrates **escalation thresholds**,
  not *which* tiers exist.
- **A3. Silence = alert.** `cs session review` is **never silent
  when idle**. It always surfaces a digestion counter
  (*"11 notes lues, 8 classées, 3 en attente, 0 en silence prolongé"*).
  A note that sits past tier 4 without operator verdict beyond a
  staleness threshold triggers a **push alert**.
- **A4. IFBDD (parallel code + vocabulary).** This ADR is drafted in
  parallel with ADR-B (`utterance` lift, `task-20260424-a912`). The
  two worktrees meet for a coherence rendezvous before merging to
  main; neither blocks the other.

Where the panel's synthesis and these amendments disagree, **the
amendments win**. Wherever this ADR invokes a decision absent from
the amendments, the panel's convergence holds.

---

## Decision

### 1. The `session-route` formula

Introduce a single formula `session-route` that, for every note in a
sealed or open session, produces exactly one sidecar file and — when
confidence warrants — exactly one staged molecule.

The formula replaces `session-to-spark` as the load-bearing path from
carnet to molecule. The `!spark ` prefix becomes a regex rule inside
tier 1; the older formula is deprecated on a migration window
(see §Relationship to `session-to-spark`).

**Composability discipline (CLAUDE.md §Composability Principle).**
`session-route` is a new formula over existing molecules — it does
not introduce a new daemon, a new MoleculeKind, or a new §8j port.
All four tiers write the same sidecar shape; the verdict-door
consumes the sidecar; existing molecule kinds (`spark`, `idea`,
`task`, …) host the staged proposals.

### 2. Pipeline (per note)

For each note `(ts, nucleon_id, body)` extracted from
`.cosmon/state/sessions/session-*.md`:

1. Compute `body_hash = BLAKE3(body)`.
2. Look up `.cosmon/state/sessions/.route/<sid>/<body_hash>.json`.
   If a sidecar exists with matching `(router_version, prompt_version)`,
   **skip** (idempotent dedup).
3. Enter the **4-tier cascade** (see §5). Each tier either emits
   sidecar with `axes` + per-axis confidences, or escalates on
   `max(confidence) < threshold`.
4. Tier 4 (human) always terminates — the sidecar carries
   `axes: null`, `decided_by: "tier4_pending"`, and the note is
   surfaced in the verdict-door as *needs-your-eye*. `axes = ⊥` is
   a **transient state only**; it is never written as the final
   decision (amendment A1).
5. Stage: if a tier ≤ 3 returns `max(confidence) ≥ staging_threshold`,
   `session-route` nucleates a molecule with `temp:proposed` tag
   carrying the proposed destination.
6. Emit a NDJSON line per note on stdout (agent-first CLI convention).

The pipeline is **pure**: given the same `(body, router_version,
prompt_version)`, the tier sequence and terminal verdict are
byte-identical (invariant I2, §Formal invariants).

### 3. Sidecar location and schema

**Location.** `.cosmon/state/sessions/.route/<sid>/<body_hash>.json`.
Content-hash keying (not `note_ts`) gives dedup across sessions
(Shannon: a body re-uttered in a later session resolves to the same
sidecar) and makes reclassification a pure recompute.

**Schema (mandatory fields — any future addition is additive):**

```json
{
  "note_id": "session-2026-04-22T16-28-09Z@08:42:35",
  "body_hash": "blake3:abcd…",
  "router_version": "route-v1-2026-04-24",
  "prompt_version": "p1",
  "axes": {
    "salience":      "hot",
    "addressee":     "self",
    "actionability": "task"
  },
  "confidences": {
    "salience":      0.92,
    "addressee":     0.85,
    "actionability": 0.88
  },
  "proposed_action": "nucleate_spark",
  "decided_by":      "tier1",
  "decided_at":      "2026-04-24T09:00:00Z"
}
```

Field semantics:

| Field | Role |
|-------|------|
| `note_id` | Human-readable handle (`<sid>@<HH-MM-SS>`) — for display only; dedup is by `body_hash`. |
| `body_hash` | `BLAKE3(body)` — the primary key (invariant I1). |
| `router_version` | Router identity — bumping it writes a **new** sidecar, never overwrites (invariant I3). |
| `prompt_version` | Tier 2/3 prompt identity — same append-only discipline as `router_version`. |
| `axes` | 3-vector per §4. `null` only in tier-4-pending transient state. |
| `confidences` | Per-axis scalar in [0.0, 1.0]. Agent-only — never rendered as a number in UI (C5). |
| `proposed_action` | Enum hint: `nucleate_spark` \| `nucleate_idea` \| `nucleate_question` \| `append_chronicle` \| `needs_your_eye` \| `skip`. Rendering layer only (see §4). |
| `decided_by` | `tier1` \| `tier2_local` \| `tier3_cloud` \| `tier4_pending` \| `tier4_resolved`. |
| `decided_at` | UTC ISO-8601. |

### 4. Axis vector schema (resolves D2)

**Persistence**: the 3-axis vector, per Wheeler's move. **Rendering**:
`proposed_action` as a human-readable destination, per Jobs (route
by destination, not taxonomy). The 6-bucket enum from the framing
document is rejected as persistent schema — it collapses Shannon's
distribution and prevents note #5 (bundled) and note #9 (refines #8)
from being represented faithfully.

**The three axes:**

| Axis | Values | Meaning |
|------|--------|---------|
| `salience` | `hot` \| `warm` \| `cold` \| `⊥` | How much attention does this deserve right now? `⊥` is transient only. |
| `addressee` | `self` \| `system` \| `audience` \| `other` \| `⊥` | Whom is the operator speaking to? `⊥` is transient only. |
| `actionability` | `task` \| `idea` \| `reflection` \| `narrative` \| `⊥` | What kind of move does this call for? `⊥` is transient only. |

Per axis, a confidence in [0.0, 1.0]. A tier's decision is
`max(confidences) ≥ threshold` — one axis at low confidence is
sufficient to escalate even if the others are high.

**The `⊥` state is transient.** Per amendment A1, `axes = ⊥` is
only written when a tier cannot resolve an axis and is about to
escalate. Writing a final sidecar with any `⊥` axis is forbidden
for tiers 1–3. Tier 4 resolves `⊥` to a concrete value as part of
the verdict-door interaction (operator picks the axis or explicitly
skips the note).

**Rendering hint — `proposed_action`.** The UI never shows axes; it
shows the destination.

| Axis triple | `proposed_action` |
|-------------|-------------------|
| `(hot, self, task)` | `nucleate_spark` |
| `(warm, self, idea)` | `nucleate_idea` |
| any `(*, *, reflection)` | `append_chronicle` |
| any `(*, audience, *)` | `nucleate_spark` (with audience tag) |
| any `(cold, *, *)` | `skip` (filed, not staged) |
| tier-4-pending | `needs_your_eye` |

The mapping is a rendering concern; the sidecar always carries the
axis triple plus `proposed_action` as a derived hint. A future UX
iteration may refine the mapping without bumping `router_version`
(because the axes — the canonical signal — remain unchanged).

### 5. The 4-tier cascade (amendment A2: all four tiers in v0)

All four tiers are **built, tested, and shipped in v0**. No tier is
deferred to a later milestone. The benchmark
(`task-20260424-59fd`) calibrates thresholds; it does not gate which
tiers exist.

| Tier | Cognition | Tool | Latency | Cost | Escalates on |
|------|-----------|------|---------|------|--------------|
| 1 | mechanical | regex / deterministic rules | ~10 ms | 0 | `max(confidence) < θ₁` |
| 2 | local AI | Apple Foundation Models (MLX) or `llama.cpp` + 3B model | ~500 ms | 0 | `max(confidence) < θ₂` |
| 3 | cloud AI | `cs ask haiku` (Claude Haiku 4.5) | ~200 ms | ~$0.001/note | `max(confidence) < θ₃` |
| 4 | biological | human verdict-door (operator) | N/A | operator time | **never escalates** — always terminal |

**Escalation rule.** A tier escalates when its resolver returns a
confidence vector whose max component is below the tier's threshold.
The thresholds θ₁, θ₂, θ₃ are **engineering constants** (not
product knobs — C5). v0 ships with placeholder values
(θ₁ = θ₂ = θ₃ = 0.75); `task-20260424-59fd` will measure the
operator's self-agreement rate on the 11-note corpus and propose
calibrated values in a follow-up ADR (the measurement — not a rebuild
of the cascade).

**Tier 4 is a normal stage, not an exception.** Per amendment A1,
tier 4 is the regular last-resort cognition. Entering tier 4 means
the sidecar is written with `axes: null`, `decided_by:
"tier4_pending"`, and the note is surfaced in the verdict-door as
*needs-your-eye*. The operator's decision resolves the axis triple
and promotes `decided_by` to `tier4_resolved`. There is no pressure
on the operator to act; the sidecar tracks the digestion counter
(§7) so the absence of action is visible.

**Tier 2 model choice is out of scope.** Apple MLX vs `llama.cpp` +
gpt-oss-3B vs another local stack is a separate ADR (see §What this
ADR does NOT decide). v0 implements a `Tier2Resolver` trait with
one in-tree default; swapping the backend does not require bumping
`router_version` unless the prompt changes.

**Stateless prompts for tiers 2 and 3.** Each call sees
`(note_body, 3 preceding notes, typology doc)`. No rolling context,
no RAG over prior verdicts — that is deferred to a future
optimisation and gated on residue measurement, not guessed.

### 6. Staging via `temp:proposed`

When a tier 1/2/3 returns `max(confidence) ≥ staging_threshold`,
`session-route` nucleates a molecule with:

- The appropriate kind (from `proposed_action` — `nucleate_spark`
  → `spark`, `nucleate_idea` → `idea`, etc.).
- Tag `temp:proposed` — **invisible to** `cs ensemble --tag temp:hot`,
  `cs ensemble --tag temp:warm`, and every other standard query.
- Tags `source:session`, `stream:session-route`,
  `session-note:<sid>@<HH-MM-SS>`, and `nucleon_id:<resolved>` per
  ADR-061.
- A back-link to the sidecar (`sidecar:<relative_path>`).

A staged molecule exists on disk but is not yet "real" backlog.
Three affordances on the verdict-door (§7) promote, dismiss, or
undo it.

### 7. Verdict-door contract (amendment A3: silence = alert)

**CLI surface.** `cs session review [<sid>]` — the user-facing
ceremony. Invoked explicitly by the operator, or opened
automatically on `cs session end --review`.

**Per proposed molecule, three affordances:**

| Verdict | Effect |
|---------|--------|
| `keep` | Drops `temp:proposed`, adds `temp:hot`. Molecule enters normal backlog. |
| `dismiss` | Collapses the staged molecule with reason `router_discarded`. Sidecar is untouched. |
| `undo` | Deletes the staged molecule. Sidecar is untouched. The raw note remains sealed in the carnet. |

No fourth button. If `keep` is later regretted, the regular
molecule lifecycle handles it (`cs collapse`, `cs thaw`).

**Never silent when idle (A3).** The verdict-door always shows a
digestion counter when opened:

```
session-2026-04-22T16-28-09Z — 11 notes
  ✓ 8 routed      (tier1: 5, tier2: 2, tier3: 1)
  ⧖ 3 waiting     (tier4_pending)
  ⚠ 0 stalled     (> 1h past tier 4 with no verdict)
```

**Prolonged silence triggers a push alert.** A tier-4-pending
sidecar that has not received a verdict within
`stale_threshold` (default: 1 hour; operator-overridable, engineering
constant otherwise) emits:

- A red line in `cs peek` (fleet view).
- A SwiftUI notification on mac-pilot (and iOS pilot once it lands
  — inherits via ADR-066 wheat-paste substrate).
- A log entry on `events.jsonl` (`RouteStaleAlert`).

The silence mechanism runs on the same LaunchAgent tick as the
router itself (no new scheduler).

**When no proposals exist.** The counter still shows
(*"11 notes, 11 classées automatiquement, 0 en attente"*). The
operator always knows the router is alive. This replaces the
panel's "silent when nothing is interesting" resolution (C8) —
amendment A3 overrides.

**Markdown + SwiftUI in parallel (amendment A4 corollary).** The
markdown review file (`.cosmon/review/YYYY-MM-DD.md`) and the
SwiftUI mac-pilot view consume the **same sidecars** — one source of
truth. Neither blocks the other.

### 8. Naming (resolves D3)

| Layer | Canonical name | Rationale |
|-------|----------------|-----------|
| Formula | `session-route` | Architectural verb — projection over immutable source. |
| Sidecar tree | `.cosmon/state/sessions/.route/<sid>/<body_hash>.json` | Mirrors `.promoted/` from `session-to-spark`. |
| User-facing CLI | `cs session review` | Human verb for the verdict-door ceremony. |
| Chore CLI | `cs session route <sid>` | Explicit one-shot re-routing (for `--rerun`, debugging). |
| Staging tag | `temp:proposed` | Invisible to `temp:hot`/`temp:warm` consumers. |
| Router identity | `router_version` | Append-only; bump writes new sidecars. |

**Forbidden vocabulary.** The word `digest` is **rejected at every
layer**. It imports the consumption metaphor — the carnet as food,
the router as stomach. Per Wheeler, that metaphor will breed §8b
violations in downstream code ("we digested the note" suggests the
raw body is consumed; §8b requires it to be immortal). `route` is
the projection metaphor that composes cleanly with §8b (the seal
is a trace, routing is a separate trace) and §8j (ingress is a
form, routing is a form).

The rename sweep (`task-20260424-746e`) removes `digest` from
framing docs, existing code, and conversation conventions. Any new
code that introduces `digest` in a public surface is a structural
breach — reviewers should file a bead.

---

## Formal invariants (Shannon, verbatim)

These four invariants make "zero information loss" testable in CI.
A `cs verify --route` command (scope of the impl task, not this ADR)
walks every sidecar and asserts all four.

### I1 — Body-primacy

> Every sidecar carries `body_hash = BLAKE3(body)`.

Given any sidecar, the original bytes are retrievable from the
sealed carnet by scanning for the matching hash. **Lost body is
unrecoverable; mis-labelled body is recoverable by grep + rehash
+ reclassify.** This is the Shannon-exact definition of "zero loss":
there exists a deterministic path from every downstream artifact
back to the original body bytes.

### I2 — Idempotent pure reclassification

> `(body, router_version, prompt_version) → sidecar` is a pure
> function. Re-runs with identical inputs produce **byte-identical**
> outputs.

Subsumes C6 (classifier/router version is first-class, replay is
free) and makes `cs session route --rerun` safe by construction.
The classifier is stateless; no rolling context, no session
memory, no operator-last-100-decisions inlined.

### I3 — Append-only (`router_version` bumps write new sidecars)

> A `router_version` bump writes a **new** sidecar file. Previous
> sidecars are never overwritten, never deleted by the router.

Sidecar history is itself a trace, mirroring §8b discipline. A
human operator may curate the sidecar tree (as with any file on
disk), but the router itself is append-only. `--rerun` under a
bumped `router_version` writes alongside the old sidecar; the
reader picks the newest by `router_version` precedence.

### I4 — Carnet untouched

> `session-*.md` is never opened for write by `session-route`.

§8b holds by construction. The BLAKE3 seal on the carnet remains
valid forever; only sidecars carry routing metadata. Violations are
detectable by `cs verify` (the briefing-seal mechanism extended to
carnet files) — this ADR does not ship the verifier, but its
discipline is compatible.

---

## Relationship to `session-to-spark` (deprecation path)

`session-to-spark` (today: `!spark ` prefix + LaunchAgent + `.promoted/`
sidecar) is **superseded** by `session-route`. The migration is
non-destructive:

1. **v0 ship:** `session-route` writes `.route/` sidecars. The
   `!spark ` prefix is preserved as one regex rule inside tier 1:
   a body starting with `!spark\s` maps to
   `(hot, self, task)` at confidence 1.0 and terminates at tier 1.
   Existing operator muscle memory keeps working.
2. **Migration window:** existing `.promoted/` sidecars from
   `session-to-spark` remain valid — their molecules are in
   `cs ensemble` as before. No migration script rewrites them.
   `session-route` writes its own sidecar tree (`.route/`);
   the two are independent.
3. **Deprecation signal:** `cs session promote` prints a
   deprecation notice on invocation (one quarter). It still works.
4. **End state (> 1 quarter):** `session-to-spark`'s LaunchAgent
   is removed; `cs session promote` collapses into a thin alias of
   `cs session review`. The `.promoted/` tree is kept (it is a
   trace, like `.route/`); no files are deleted.

**The `!spark ` prefix itself lives on** as a regex rule. An
operator who types `!spark buy milk` gets the same behaviour as
before (tier-1 terminal with confidence 1.0, staged spark molecule,
keep/dismiss/undo in the verdict-door). The prefix is honest
plumbing; honest plumbing is welcome wherever the operator naturally
reaches for it.

---

## What this ADR does NOT decide

This ADR is deliberately narrow. The following are **out of scope**
and handled by sibling molecules or future ADRs:

- **Tier 2 backend choice** — Apple MLX / Foundation Models vs
  `llama.cpp` + gpt-oss-3B vs another local stack. Separate ADR,
  informed by privacy, battery, and latency measurements.
- **Tier 3 prompt contents** — the exact few-shot template for
  Haiku. Lives in the impl task (`task-20260424-7335`); bumping it
  bumps `prompt_version` and triggers optional `--rerun`.
- **Threshold values θ₁, θ₂, θ₃** — placeholder 0.75 in v0;
  calibrated values follow `task-20260424-59fd` (benchmark) in a
  separate measurement ADR.
- **SwiftUI verdict-door layout** — shape, affordance order, badge
  design. Lives in `task-20260424-69c1` and must respect ADR-066
  (wheat-paste over `cs peek --snapshot`).
- **`utterance` first-class primitive lift** — ADR-B,
  `task-20260424-a912`. This ADR's axis-vector schema is compatible
  with a future `utterance` primitive (the schema does not name
  `session note` or `whisper` explicitly; it names `(ts, nucleon_id,
  body, provenance)`) — but it does not require the lift.
- **Matrix whisper routing through `session-route`** — whispers
  are a separate §8j port. Unifying the routers is a future
  consolidation, tracked implicitly by ADR-B.
- **Event-driven vs batch cadence refinement** — v0 ships the
  existing 5-min LaunchAgent cadence (same infrastructure as
  `session-to-spark`). `fswatch` debounce on the open session is a
  v1 addition, gated on operator complaint.

---

## Consequences

### What this ADR adds

- A new formula `session-route.formula.toml` with four steps (one
  per tier) plus sidecar write + optional staging.
- A new sidecar tree `.cosmon/state/sessions/.route/<sid>/<body_hash>.json`.
- A new staging tag `temp:proposed` (invisible to existing `temp:*`
  consumers; documented in CLAUDE.md §Molecule Temperature Tags as
  a router-managed state).
- A new CLI verb `cs session review` (user-facing) + `cs session
  route <sid>` (chore). Per ADR-068, both gain SwiftUI counterparts
  in mac-pilot / ios-pilot via the ADR-066 wheat-paste substrate
  in `task-20260424-69c1`.
- A new log event `RouteStaleAlert` on `events.jsonl` for
  amendment A3's silence-alert mechanism.

### What this ADR preserves

- §8b (seal is a trace) — invariant I4 makes this literal: no
  write to `session-*.md`.
- §8j (ingress) — the `session-route` formula does not add a new
  admission port; it promotes already-admitted carnet notes through
  tiers. §8j clauses (a-d) apply to the original note ingestion,
  not to this routing projection.
- §8f (data-plane discipline) — sidecars live on the data plane
  (filesystem), the DAG carries only 1-bit done/not-done signals
  between `session-route` steps.
- ADR-061 (`nucleon_id` propagation) — every staged molecule
  inherits `nucleon_id` from the session frontmatter (same rule as
  `session-to-spark`).
- ADR-068 (UX ↔ CLI parity) — every user-facing verb in this ADR
  has a SwiftUI counterpart planned (`task-20260424-69c1`).

### What this ADR supersedes

- `docs/guides/session-to-spark.md` as the load-bearing carnet→molecule
  path (enters deprecation window; see §Relationship above).
- The `!spark ` prefix as a user-facing gatekeeper (remains as a
  regex rule inside tier 1; no longer required).
- The 6-bucket typology enum as persistent schema (replaced by the
  3-axis vector; the enum lives on only as a rendering hint via
  `proposed_action`).

### What this ADR forbids

- Writing `axes = ⊥` as a final sidecar decision at tiers 1–3
  (A1 — must escalate).
- A silent-when-idle verdict-door (A3 — counter is always visible).
- Deferring any of the four tiers to a post-v0 milestone (A2 — all
  four ship together).
- Using `digest` in formula / CLI / sidecar paths (D3).
- Any classifier UI panel, rules editor, or confidence slider (C5
  from the synthesis).
- Auto-promoting `temp:proposed` to `temp:hot` without operator
  verdict (C3 — operator consent is invariant).

### Founding-thesis impact

None. The founding thesis (Parts I/II/III in `docs/founding/`)
does not name session notes, digestion, or routing. The ubiquitous
language gains three lowercase terms (`session-route`,
`sidecar.route`, `verdict-door`) but no new domain type. ADR-B
(parallel, `task-20260424-a912`) may propose an `utterance`
primitive that touches Part III; this ADR does not.

### Exit criteria verified

- [x] ADR file at `docs/adr/072-session-route-formula-and-sidecar-invariants.md` (next unused NNN after 071).
- [x] `docs/adr/INDEX.md` updated (see edit in same commit).
- [x] Shannon's four invariants cited verbatim (§Formal invariants).
- [x] Parent deliberation `delib-20260424-1b81` cited explicitly.
- [ ] `cargo check --workspace` — verified in §Step 2 (no code changes in this step; docs only).
- [ ] Commit on worker branch; merge via `cs done`.
