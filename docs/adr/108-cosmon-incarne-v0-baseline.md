# ADR-108 — Cosmon-incarné v0 baseline

**Status:** Proposed (pending operator approval before any child task ships).
**Date:** 2026-05-22.
**Parent deliberation:** `delib-20260521-955f`
— *"Architect `cosmon-incarné v0` — l'extension à 5 organes qui rend
cosmon ressenti-comme-vivant à la openclaw, SANS dégrader ce qui fait
de cosmon une plateforme supérieure."*
**Panel that converged on the verdicts:** wheeler · jobs · torvalds ·
godin · kahneman · godel · karpathy · jr.
**Authoring task:** `task-20260522-1f6b` (child of `delib-20260521-955f`,
formula `task-work`).

**Binds:**
- [ADR-016](016-autonomy-regimes-and-resident-runtime.md) — three regimes
  (Inert / Propelled / Autonomous). v0 organs live in **Inert + Propelled
  only**. No autonomous outbound. No new daemon. No new core verb.
- [ADR-061](061-pilot-session-and-causal-closure.md) — pilot-session and
  causal closure (§8e). The joint-invariant violation noted in
  Consequences below is detected against this ADR.
- [ADR-066](066-ux-v2-substrate.md) — wheat-paste viewport substrate
  (§8k'). The vital strip ships **inside** `cs peek --snapshot` bytes;
  no SwiftUI primitive is added (jr).
- [ADR-082](082-architecture-baseline.md) — substrate-tier obligation.
  Cosmon must obey what it ships; v0 ships no new primitive, so the
  obligation is preserved by construction.
- [ADR-099](099-dispatch-site-stability.md) — dispatch-site stability.
  All v0 organs are LaunchAgent-scheduled `nucleate→tackle→wait→done`
  cycles; the dispatch site is unchanged.

**Sibling implementation children of `delib-20260521-955f`:**
- `task-20260522-069b`
  → [ADR-107 typed-provenance-prompt-context](107-typed-provenance-prompt-context.md)
  (combines wheeler's `§-causal-attribution` and godel's `§8m`).
  **Must land before any VISAGE worker-read path ships** (see
  *Consequences* below). Already merged on `main` as part of the
  delib-20260521-955f sibling cohort.
- `task-20260522-8bcd`
  → implement `peau-morning-digest.formula.toml` + LaunchAgent +
  kill-switch.
- `task-20260522-34e4`
  → implement `voix-reply.formula.toml` (draft-only, email-channel,
  permit-gated).
- `task-20260522-55a8`
  → `cs peek` vital-strip rendering + companion
  `ADR-NEXT-sensorium-strip`.
- `task-20260522-b2da`
  → idea: operator-felt graduation metrics methodology.
- `task-20260522-0f14`
  → ADR-107 incarne-tick-verb-deferred:
  `cs patrol --tick` vs `cs autopilot tick` verb consolidation deferred
  until the second CŒUR beat lands. Already merged on `main`.

---

## Context

Discussion with Heidi on 2026-05-22 reframed cosmon's position relative to
openclaw (`ex-Cline`, 285k★ on GitHub): **cosmon is a *système nerveux sans
corps*, openclaw is a *body without nervous system***. Openclaw ships a
single binary, a HEARTBEAT.md a human reads, a felt rhythm — and is
adopted by hundreds of thousands of operators. Cosmon ships a typed DAG,
event-sourced state, a federation of galaxies, ADRs that prove
their own consistency — and is adopted by one operator.

The operator's question, posed to the panel: *can cosmon grow a body
without losing the nervous system?* — and specifically, do so by
**formula + molecule + thin adapter on mailroom**, never by adding
new primitives to the transactional core. The deliberation
`delib-20260521-955f` ran an 8-persona deep-think panel
(`wheeler / jobs / torvalds / godin / kahneman / godel / karpathy /
jr`) on five proposed organs (`PEAU / CŒUR / VISAGE / HIPPOCAMPE / VOIX`)
and converged on the architecture this ADR ratifies. The convergence is
unanimous on the architectural template (C-1 through C-9 in the
synthesis); divergences (D-1 through D-7) were resolved by §5 of the
synthesis. This ADR publishes that decision in durable form so future
agents and humans can read the rationale without re-running the panel.

The panel was deliberately staged adversarially: jobs argued for
**deleting four of the five organs**, kahneman wrote the **obituary** of
v0 dated 2026-09-22, godel surfaced the **Gödel sentence** `G★(incarné)
= "This system is alive"` and the **joint-invariant violation** between
PEAU+HIPPOCAMPE+VISAGE. The architecture below survives those critiques
by adopting their counter-measures *as constraints*, not as features.

## Decision

Adopt **cosmon-incarné v0** as the minimum that flips the §1-bit
(wheeler) — *"between t and t+Δt, with the terminal closed and no `cs`
command issued by a human, did a row appear in `events.jsonl` that was
not a side-effect of a previously-issued human command?"* — while
preserving the architectural posture that distinguishes cosmon from
openclaw. The v0 baseline has **five inseparable parts**:

1. **One ritual** at the operator's kitchen window 07:15–07:35
   (godin's smallest viable audience: Noogram alone, phone face-down,
   espresso in hand). The whole architecture is optimised for that
   20-minute window.
2. **Two formulas land**:
   - `peau-morning-digest.formula.toml` (new; sibling
     `task-20260522-8bcd`).
   - `voix-reply.formula.toml` (new; **draft-only**; sibling
     `task-20260522-34e4`).
3. **One formula already shipped** (no changes in v0):
   - `curate-patrol.formula.toml` — the first beat of the cœur.
4. **Two file conventions** ship **without workers** (the deferred
   organs as cheap, queryable declarations — jobs/godel/jr converge):
   - `<galaxy>/.cosmon/identity/SOUL.md` — **operator-authored,
     worker-read-only**. Prepended to the worker prompt before
     `CLAUDE.md` if present and BLAKE3-seal-valid.
   - `~/.cosmon/identity/hippocampus/<member>.md` — **append-only**,
     operator- or worker-written. Read by workers for context.
5. **One visible surface change**: jr's *vital strip* (sibling
   `task-20260522-55a8` + companion `ADR-NEXT-sensorium-strip`) — one
   fixed 80-column line in `cs peek --snapshot`, between header and
   molecule list, rendering glyphs `~ * @ = > [off?]`.

**No new core primitives. No daemons. No autonomous outbound. No
compactor. No new `cs` verb.**

The synthesis §5.1–5.7 is the canonical record; this ADR cites it
section-by-section below.

---

## Composability discipline (§5.2 — torvalds + every persona)

Every organ in v0 collapses to the **same five-line shape**:

1. A **formula** under `.cosmon/formulas/<organ>-*.formula.toml`.
2. An **append-only NDJSON ledger** under
   `.cosmon/state/<organ>/<scope>.ndjson` *or* a markdown projection
   under `~/.cosmon/state/<organ>/<date>.md`.
3. A **thin MCP wrapper** (`sec_*` on the mailroom side) or
   `cs --json` reader.
4. A **LaunchAgent** calling `nucleate→tackle→wait→done`.
5. A `~/.cosmon/autopilot.off` **check at each step start**.

**Zero new core primitives.** This is the strongest convergence in the
panel (C-1) and is the *invariant* the ADR ships. The synthesis
explicitly rejects any later proposal that introduces a wire protocol,
mailbox, MCP push, or event bus between mailroom and cosmon (C-2).

Cosmon's existing communication model holds: **DAG = 1 bit per molecule
lifetime (done/not-done); filesystem = content**. v0 adds no new
channel; it adds new *files* on the existing data plane and new
*formulas* on the existing control plane.

## Filesystem-as-channel (§5.2 + C-2)

Inter-organ communication is exclusively by **file**, never by
in-memory message or RPC. Concretely:

- **PEAU → operator → VOIX.** PEAU writes
  `~/.cosmon/state/morning/<date>.md` (≤ 3 tiles) and a sibling
  `~/.cosmon/state/morning/<date>.drafts/<tile_id>.md` per tile that
  asks for a reply. The operator types `cs verdict <tile_id> 1` (or
  the panel-tap equivalent), which nucleates a `task` molecule that
  *reads the draft file* and waits for `cs evolve`.
- **PEAU dedup ledger.** `.cosmon/state/peau/seen.ndjson`, append-only,
  keyed by `(channel, msg_id)`. Idempotent re-runs by date — the
  formula is a no-op if the morning file already exists.
- **VOIX permit ledger.** `.cosmon/state/voix/permissions.ndjson` —
  one append-only NDJSON row per `(recipient, channel, scope,
  granted_until, granted_by, at)` grant. The `permit` step of
  `voix-reply` aborts if no row matches the proposed send.
- **CŒUR ledger (already shipped).** Whatever `curate-patrol` writes
  today; v0 does not touch the schema.
- **HIPPOCAMPE.** `~/.cosmon/identity/hippocampus/<member>.md`,
  append-only markdown (karpathy's choice over torvalds's NDJSON —
  "workers read markdown for free", C-6). One file per *noyau*
  member.
- **VISAGE.** `<galaxy>/.cosmon/identity/SOUL.md` per galaxy. Read at
  `cs tackle` time and prepended to the worker prompt before
  `CLAUDE.md` (only after `task-20260522-069b` lands — see
  *Consequences*).

When a future PR is tempted to add inter-organ messaging, the question
is: *which file on disk carries this payload, keyed by what?* If the
answer is "none — it should be a message", the PR is rejected (C-1 +
C-2).

## Jobs scope: what ships, what doesn't (§5.1 + §5.2)

Jobs's discipline (delete four organs) was not adopted whole, but its
*subtraction logic* shapes the v0 scope. After reconciling §5 with
D-1 (1-organ vs 5-organ tension), the synthesis lands on:

**What ships in v0 (operator-callable):**

| Surface | Source | Cite |
|---|---|---|
| `peau-morning-digest` formula + LaunchAgent | new | §5.2 |
| `voix-reply` formula (draft-only) | new | §5.2 |
| `curate-patrol` formula | already shipped | §5.2 |
| `SOUL.md` file convention (worker-read-only) | new | §5.2 |
| `hippocampus/<member>.md` file convention (append-only) | new | §5.2 |
| `cs peek` vital strip | new (sibling `task-20260522-55a8`) | §5.3 |
| `cs verdict <id> 1\|2\|3\|later` UX gesture | new | §5.1 |

**What does not ship in v0 (operator-asked-by-name only — godin Rung 3 or
later):**

- Any LLM-authored emotional surface in SOUL.md.
- Any VOIX outbound that bypasses operator review.
- Any HIPPOCAMPE compactor or destructive view-of-source.
- Any auto-promotion of children from `temp:warm` to `temp:hot`.
- Any new top-level `cs` verb (the `cs autopilot tick` vs
  `cs patrol --tick` question is deferred — see sibling
  `task-20260522-0f14`).

The synthesis principle: **ship the ritual; the organs will name
themselves** (jobs §5.1). Anatomy describes a body after it is alive;
it is not the way to make one alive.

## The permission ladder — Rungs gated on operator behavior (§5.5 — godin + kahneman + karpathy + wheeler)

Rungs graduate on **operator behavior, not feature counts, not LOC,
not calendar time**. This is unanimous (C-8). Different panelists
named the metric differently; the ADR adopts the conjunction:

### Rung 0 — today

Cosmon is silent unless spoken to. (Baseline.)

### Rung 1 — `peau-morning-digest` + `curate-patrol` + `voix-reply` draft-only

**What ships:** the three formulas above + the file conventions for
SOUL.md and hippocampus/ + the vital strip.

**Graduation (four-condition AND):**
1. Four consecutive mornings the operator opens
   `~/.cosmon/state/morning/<date>.md`.
2. The operator dispatches at least one verdict per morning.
3. `~/.cosmon/autopilot.off` is **not** touched during those four
   mornings.
4. **Pronoun test** (godin): the operator says *"cosmon noticed X"* /
   *"cosmon thinks Y"* zero times — once is fine, three times in a
   week is a sticky alarm (file a bead).

### Rung 2 — `SOUL.md` worker-read path + `hippocampus` read into briefings

**Prerequisite:** `task-20260522-069b`
([ADR-107 typed-provenance-prompt-context](107-typed-provenance-prompt-context.md))
has landed and is verified green by `scripts/architecture-audit.sh` (see
*Consequences*).

**What ships:** workers begin reading `SOUL.md` (prepended to prompt
before `CLAUDE.md`) and citing hippocampus content in their
`briefing.md`.

**Graduation (three-condition AND):**
1. Two consecutive weeks where a worker's `briefing.md` cites
   hippocampe content unprompted (karpathy's metric).
2. The operator catches themselves saying *"cosmon noticed X"* at
   least once — pronoun-test *passes positively* once; remains a
   sticky alarm if it crosses three in a week (godin).
3. `autopilot.off` touched zero times across the two weeks (kahneman's
   kill-switch dwell-time).

### Rung 3 — `voix-reply` permitted on iMessage / WhatsApp / Signal

**Prerequisite:** mailroom-side `sec_send_imessage` /
`sec_send_whatsapp` / `sec_send_signal` wrappers exist and are
operator-tested manually.

**What ships:** `voix-reply` accepts non-email channels via the same
draft-blocking discipline. Permit ledger
(`.cosmon/state/voix/permissions.ndjson`) gates per-recipient,
per-channel.

**Graduation (two-condition AND):**
1. The operator voluntarily appends a recipient to
   `voix-permits.toml` *and* uses the verdict-door from the phone
   before opening Ghostty.
2. Two weeks zero `cs revive` on curate collapses **AND** zero
   "I wish you hadn't sent that" chronicle entries.

**No rung skips.** No automatic graduation. If condition (n) fails
after candidate-graduation, the previous rung is the operating regime
until conditions re-establish.

The methodology note that operationalises the five proxy metrics —
pronoun test, silence-verdict rate, kill-switch dwell-time,
briefing-cites-hippocampe, single ground-truth bit — is sibling
`task-20260522-b2da`.

## Kahneman's counter-measures vs sticky autonomy (§5.4 + §5.7 — kahneman)

The pre-mortem (synthesis §1.5) dated cosmon-incarné's death
**2026-09-22**: not via any single organ failing, but via the aggregate
exceeding the operator's appetite for un-asked-for presence. Five
biases × five counter-measures, each promoted to a v0 constraint:

| Bias | Counter-measure | Where it ships |
|---|---|---|
| **PEAU / availability cascade** | Children land `temp:warm`, **never** `temp:hot`. `peau-decay` collapses untouched after 7d. **Workers cannot promote `temp:warm` → `temp:hot`** (operator only, enforced in `cs tag`). | `peau-morning-digest` formula; `cs tag` change in sibling task. |
| **CŒUR / rhythm capture** | Silent by default, threshold-only emission, no metronome. | `curate-patrol` (already shipped). |
| **VISAGE / narrative coherence** | **Operator-authored, worker-read-only. No `visage-curate` / `visage-revise` formula in v0.** | File convention only; the absence of a worker is the counter-measure. |
| **HIPPOCAMPE / anchoring** | **Append-only, BLAKE3-sealed, no compaction in v0.** Forgetting is a human gesture (`git rm`). | File convention only; no `hippocampe-compact` formula. |
| **VOIX / social presence** | Every outbound is a **Propelled-regime draft blocking on operator verdict (`cs evolve`)**. Never auto-send in v0. | `voix-reply` formula structure. |

Two cognitive debts are **eliminated by absence**:

1. No emotional surface in v0 — `SOUL.md` is declarative facts only,
   not LLM-authored prose.
2. Typed `incarnation.trigger` + `incarnation.confidence` provenance
   computed from triage scores, never from LLM self-assessment.

These are not policies the v0 promises to follow — they are absences
the v0 *cannot violate* because the violating code does not exist.

## File conventions: VISAGE (read-only) and HIPPOCAMPE (append-only)

The two deferred organs ship as **file conventions only**. No workers
write to them in v0 (godel's grounding rule — no organ modifies the
file that governs its own scope; C-5 + C-6).

### VISAGE — `<galaxy>/.cosmon/identity/SOUL.md`

**Shape.** ≤ 50 lines. Operator-authored. Declarative facts about the
galaxy's posture (register, recipient list, taboo topics, tone budget).
Disjoint from `CLAUDE.md` by construction: CLAUDE.md = DNA / doctrine;
SOUL.md = voice / posture.

**Lifecycle.** Created by the operator. Edited by the operator.
**Never edited by a worker.** No `visage-*` formula exists.

**Worker-read path.** `cs tackle` reads
`<galaxy>/.cosmon/identity/SOUL.md` if present and BLAKE3-seal-valid,
and prepends it to the worker prompt **before** `CLAUDE.md`. **This
path ships only after sibling `task-20260522-069b` lands** — see
*Consequences*.

**Seal.** BLAKE3-sealed at edit time (operator-driven, advisory; same
discipline as `prompt.md` / `briefing.md` seals — `cs verify` reports
mismatch, does not block).

### HIPPOCAMPE — `~/.cosmon/identity/hippocampus/<member>.md`

**Shape.** One markdown file per *noyau* member (tenant_auditor, Heidi, etc.).
Append-only. Schema-dumb on purpose (no typed Rust schema — C-6).

**Lifecycle.** Operator- or worker-written by append only.
**No compactor in v0** (kahneman + godel). Forgetting is a human
gesture or a *view*, never a destructive write to source.

**Worker-read path.** Workers may read the file for context in
briefings. **No worker writes to it without an explicit operator
gesture** in v0 (Rung 2 graduates the worker-read path; worker-write
remains Rung 3+ and not in scope here).

**Decay.** `git rm` (operator gesture). The carnet glyph `= N notes`
on jr's vital strip reads the file count and an optional `decay_at:`
front-matter per file.

---

## Inviolable constraints (from `frame.md` §4)

Verbatim from the deliberation frame, the v0 must honour the following.
The synthesis preserved each by construction; this ADR records the
preservation for the audit trail.

- **Composability.** No new core primitive. Formulas + ledgers +
  adapters only. (Honoured: §5.2 + this ADR §"Composability
  discipline".)
- **`main` est sacré.** Every organ ships behind a feature gate or as
  a formula opt-in; no surprise default behavior change. (Honoured: v0
  ritual is *opt-in by LaunchAgent installation*; the kill-switch
  `~/.cosmon/autopilot.off` halts everything.)
- **Kill-switch generality.** A single file
  `~/.cosmon/autopilot.off` silences every v0 organ.
  Kill-switch is polled **per-molecule, not per-session** (kahneman's
  counter-measure). Ordering preserves generosity (godin §1.4): silence
  VOIX first, CŒUR next, PEAU last — the operator wants to keep the
  ambient presence even when sending is too much.
- **Felt-life = relation.** The §1-bit (wheeler) is observable on disk
  and queryable; aliveness is a *relation* between the operator and
  the bytes, not an assertion the system makes about itself. The
  Gödel sentence `G★(incarné) = "This system is alive"` is decidable
  only by the operator, never by cosmon (godel §1.6).
- **Syzygie no-duplication.** Every v0 organ that reads or writes
  external signal does so through `mailroom` (`sec_today`,
  `sec_attention`, `sec_send_email`, …). No new ingress adapter is
  cloned into cosmon.
- **Survives `sleep`.** All v0 state is filesystem-resident. A
  machine sleep, a power cycle, or a `kill -9` of the LaunchAgent
  leaves the v0 ritual idempotent on re-run by date. (Lesson from
  2026-05-21 drain-worker: long sequential workers must persist
  incrementally; the v0 ritual is short and idempotent, so the lesson
  is honoured by structure.)

---

## What is forbidden in v0 (§5.7)

The forbidden list is part of the decision, not a footnote. Each item
is forbidden because allowing it would either (a) violate a panel
counter-measure, (b) introduce a new primitive, or (c) cross the
operator-gesture firebreak.

- **No `visage-revise` worker** — SOUL.md is operator-edited only
  (godel grounding rule, kahneman narrative-coherence counter).
- **No `hippocampe-compact` formula** — forgetting is a human gesture;
  events stay in `events.jsonl`; views may project but never
  destructively (godel C4 + kahneman anchoring counter).
- **No autonomous `voix-send`** — every outbound is a draft-file
  blocking on `cs evolve` (kahneman + karpathy + godin + jobs
  converge).
- **No `cs autopilot tick` new verb** — use `cs patrol` (torvalds
  §single-perimeter); the verb-consolidation decision is deferred
  until the second beat lands (sibling `task-20260522-0f14`).
- **No mailbox or MCP push from mailroom to cosmon** — filesystem
  ledger only (every panelist; C-2).
- **No worker promotion `temp:warm` → `temp:hot`** — operator only
  (kahneman counter-measure).
- **No animation in the vital strip without an operator gesture in
  the last hour** — jr's silence law.

---

## Consequences

### Positive

1. **The §1-bit becomes queryable, not inferred.** The cron-driven
   `cs patrol --propel` already flips it faintly (wheeler §1.1); v0
   *names and concentrates* the trickle into one ritual the operator
   actually opens.
2. **Cosmon retains the architectural advantage over openclaw.**
   Where openclaw is one binary + one HEARTBEAT.md + ad-hoc autonomy,
   cosmon-incarné is *formula + ledger + adapter* — the same
   composability template applied to felt-life. Adopting an organ
   does not adopt a daemon.
3. **The Gödel sentence is honoured.** *"This system is alive"* is
   decided by the operator reading the bytes, not by cosmon asserting
   it. CŒUR publishes; operator decides. Same shape as openclaw's
   HEARTBEAT.md works *because* it is read by a human (godel §1.6).
4. **Kahneman's pre-mortem is structurally pre-empted.** Each of the
   five biases is countered by an *absence* (no compactor, no
   visage-curate, no autonomous send, no worker promotion to hot, no
   LLM-authored emotional surface).

### Negative / risks accepted

1. **Joint-invariant violation (godel §1.6).** PEAU + HIPPOCAMPE +
   VISAGE *individually* preserve §8e (causal closure of
   pilot-cognition, ADR-061). **Jointly they break it** because
   SOUL.md is git-tracked but not events-typed — a shadow causal
   graph forms parallel to the typed DAG. **Remediation:** sibling
   `task-20260522-069b` landed
   [ADR-107 typed-provenance-prompt-context](107-typed-provenance-prompt-context.md)
   (combining wheeler's `§-causal-attribution` and godel's `§8m`)
   *before* the VISAGE worker-read path opens at Rung 2. The Rung 2
   prerequisite gate is the structural enforcement of this
   sequencing. (Sibling ADR-107 already merged on `main`.)
2. **`peau-morning-digest` could silently inflate `temp:warm` backlog.**
   Counter: `peau-decay` collapses untouched after 7d; the
   silence-verdict rate metric (sibling `task-20260522-b2da`) auto-demotes
   an organ that produces > 2 zero-reply mornings per week.
3. **SOUL.md drift between galaxies.** Per-galaxy (D-6 resolved
   per-galaxy, not per-fleet). The drift is intentional — different
   galaxies have different voices — but the operator carries the
   maintenance cost. If three galaxies ship and SOUL.md is identical
   across them, the convention has failed; we'll revisit.
4. **The vital strip on `cs peek` consumes one 80-column line of
   viewport real estate.** Accepted (jr's verdict — *stillness is
   the signal*; the line is byte-identical when nothing changes).

### Sequencing (forced by godel §5.4)

- Rung 1 may ship as soon as `peau-morning-digest`, `voix-reply`
  (draft-only), and the vital strip are merged.
- **Rung 2 may not ship until `task-20260522-069b` lands.** The Rung
  2 prerequisite is explicit and gated by `scripts/architecture-audit.sh`
  passing the new `§8m` witness.
- **Rung 3 may not ship until mailroom-side iMessage / WhatsApp /
  Signal wrappers exist and have been operator-tested manually.**

---

## Panel contributions (per axis)

Acknowledgement of which persona drove which architectural commitment:

- **Composability discipline = formula + ledger + adapter, no new
  core primitives.** torvalds §1.3, with every persona concurring
  (C-1, the strongest convergence in the panel).
- **Jobs scope (ship the ritual, defer the organs).** jobs §1.2.
  The synthesis adopted jobs's *subtraction logic* while preserving
  godel/jr's argument that the two file conventions (SOUL.md +
  hippocampus/) ship cheaply because they are zero-worker
  declarations.
- **Permission ladder = Rungs gated on operator behavior.** godin
  §1.4 (smallest viable audience, *people-like-us-check-on-each-other-
  in-the-kitchen*).
- **Counter-measures vs sticky autonomy (the pre-mortem).** kahneman
  §1.5. Five biases × five counter-measures, promoted from advisory
  to v0 constraint.
- **Grounding rule + Gödel sentence + joint-invariant violation.**
  godel §1.6. *No organ modifies the file that governs its own
  formula or its own scope; modifications surface as operator-gated
  atomic questions.*
- **Operator-felt translation + the cat analogy.** karpathy §1.7.
  *"Not Tamagotchi, the good house-cat in an old house — hears the
  door before you do, has its own rhythm, has a personality you've
  come to know, remembers which guests it likes, comes find you when
  something matters then leaves."*
- **Vital strip + the five glyphs `~ * @ = >` + stillness as
  signal.** jr §1.8. Structural inversion of openclaw's HEARTBEAT.md.
- **IT-FROM-BIT reframe and the §1-bit + initiator field.** wheeler
  §1.1. The single ground-truth metric for *"is it alive"* lives on
  the events ledger.

---

## Verification

The v0 baseline passes the following audit checks on merge:

- **`cargo check --workspace`** — green (no Rust code change in this
  ADR PR; sibling tasks must independently pass on merge).
- **`cargo fmt --all -- --check`** — green.
- **`cs reconcile --check`** — green (this ADR is a doc-only addition;
  `docs/adr/INDEX.md` regenerated in the same PR).
- **`scripts/architecture-audit.sh`** — green (no new INV exercised;
  this ADR proposes the §8m + §-causal-attribution invariants for a
  *sibling* ADR to land, not for this one).

---

## References

- Synthesis: `delib-20260521-955f/synthesis.md` §§ 5.1 – 5.7 (canonical decision
  record cited verbatim above).
- Frame: `delib-20260521-955f/frame.md` §4 (inviolable
  constraints).
- Per-panelist responses: `delib-20260521-955f/responses/` — wheeler.md / jobs.md / torvalds.md /
  godin.md / kahneman.md / godel.md / karpathy.md / jr.md.
- Related chronicles:
  - 2026-04-27 *embargo-before-send* (Dan pivot) — informs the
    VOIX draft-blocking discipline and the kitchen-window guard.
  - 2026-05-21 *drain-worker silent kill* — informs the *"survives
    `sleep`"* constraint by way of incremental persistence.

---

*— ADR-108, 2026-05-22.*

> *Cosmon will not become alive by becoming bigger. It will become
> felt-as-alive the morning the operator opens the lid, finds three
> true lines waiting, and the system does not say anything else.*
