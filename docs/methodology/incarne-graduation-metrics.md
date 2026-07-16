# Cosmon-incarné — operator-felt graduation metrics

**Status.** Methodology note, v0. Canonical reference for how `cosmon-incarné`
rungs (Rung 0 → 1 → 2 → 3) graduate. Cited from the v0 baseline ADR
([`task-20260522-1f6b`](../adr/)) and from the formula READMEs of each
incarné organ (`peau-morning-digest`, `coeur-tick`, `voix-reply`).

**Parent.** `delib-20260521-955f` — *Architect cosmon-incarné v0*. See
synthesis §5.5 and `responses/{godin,kahneman,karpathy,wheeler}.md` at
`.cosmon/state/fleets/default/molecules/delib-20260521-955f/`.

**Scope (per operator).** All metrics below apply to **one** operator
(Noogram). Scaling to a multi-operator fleet is out of scope for v0 and
will require its own methodology revision.

## 1. Why this doc exists

The panel converged unanimously on convergence **C-8** of the synthesis:
*rungs are gated on operator behavior, not feature counts.* No graduation
by clock. No graduation by LOC. No graduation by "five organs landed."

The risk the panel named — kahneman's 2026-09-22 obituary — is that
without an explicit list of measurable proxies, the system silently
graduates *by either* of the two forbidden axes (clock, LOC) and ships
the v0 that kills itself by the end of summer. This document is the
load-bearing list of proxies that prevents that silent graduation.

It is read three ways:

- **By the operator**, at session-close or at temp-review, to verify
  none of the alarms have tripped and to decide whether a rung has been
  earned.
- **By any worker** writing or modifying an incarné organ — `briefing.md`
  must cite this document when claiming a rung gate has been met.
- **By a future `cs incarne-status` diagnostic** (out of scope here; see
  `idea-20260522-incarne-status` to be filed if appetite remains) that
  reports the current value of metrics 2–5 mechanically.

## 2. The five metrics

Each metric has a fixed shape:

- **Proxy** — what is measured (operator behavior, not feature output).
- **Threshold** — when the metric fires (or what value graduates a rung).
- **Source of truth** — which file / event / behavior is read. If a
  source does not exist yet, this is named as a dependency.
- **Action on trigger** — what the operator or the system does.
- **Falsifiable.** Always a clear yes/no determination from the source
  of truth. Never an interpretation.
- **Manual / automatic** — whether the metric can be read by a future
  `cs incarne-status` or requires operator self-report.
- **Citation.** Persona + response file at delib-20260521-955f.

### 2.1 — Pronoun test *(godin)*

| Attribute | Value |
|---|---|
| **Proxy** | The operator says *"cosmon thinks"* / *"cosmon decided"* / *"cosmon noticed"* (or any other agentive verb with "cosmon" as subject) 3+ times in a 7-day rolling window. |
| **Threshold** | ≥ 3 occurrences / 7 days. |
| **Source of truth** | Operator self-report at session-close + grep of an internal chronicle and `.cosmon/state/**/log.md` for `cosmon (thinks\|decided\|noticed\|wants\|chose\|saw)`. |
| **Action on trigger** | File a bead (`temp:hot` molecule, kind `signal`) named `sticky-pronoun-alarm-<YYYY-MM-DD>`. Surface it at the next `temp-review` sweep. The bead does **not** auto-demote any organ; it surfaces the question to the operator. |
| **Falsifiable** | Yes (count of occurrences vs 3). |
| **Per-operator** | Yes. The grep covers Noogram's lore; pronoun habits do not transfer. |
| **Manual / automatic** | **Manual** — the grep is mechanical but classifying false positives (quoted text, prose about another agent, conditional uses like *"if cosmon noticed"*) requires human judgement. Operator self-report is the primary signal. |
| **Citation** | godin, response §4 + §8 — *"Sticky risk monitored by the pronoun test — if the operator says 'cosmon thinks' three times in a week, file a bead."* |
| **What it gates** | This is a **sticky-autonomy alarm**, not a rung gate. The system can graduate to Rung 2 with this alarm clear and a rung-2 organ silent (no narrative coherence leaking). It cannot graduate to Rung 2 with this alarm firing. |

**Why this is the alarm and not a rung condition.** Godin's permission
ladder graduates *positively* (operator promotes a recipient) and the
pronoun test fires *negatively* (operator narrates cosmon as agentive).
A positive rung condition without a negative alarm yields the openclaw
failure mode — Tamagotchi compulsion misread as engagement. The
pronoun test is the kahneman-style pre-mortem evidence that the
narrative-coherence bias is forming.

### 2.2 — Silence-verdict rate *(godin)*

| Attribute | Value |
|---|---|
| **Proxy** | Rate of `0` verdicts (= *"silence me tomorrow"*) on morning-brief tiles. |
| **Threshold** | `> 2 / 7 days`. |
| **Source of truth** | `~/.cosmon/state/morning/verdicts.ndjson` — to be emitted by `peau-morning-digest` (sibling task `task-20260522-8bcd`). One row per operator gesture: `{ts, tile_id, organ, verdict: "0"|"1"|"2"|"3"|"later"}`. Until that ledger ships, this metric is **not yet readable**. |
| **Action on trigger** | The offending **organ** (column `organ` in the ledger) auto-demotes one rung. *In v0 this is a recommended manual demotion, not an automatic state transition* — the demotion enforcement is itself a Rung-2 decision (see §3.b below). |
| **Falsifiable** | Yes (count of `verdict=="0"` rows in a 7-day window vs 2). |
| **Per-operator** | Yes — single ledger, single operator. |
| **Manual / automatic** | **Automatic** once the ledger exists. The reader is a `cs incarne-status` query; no LLM, no judgement call. |
| **Citation** | godin, response §8 — *"Noise risk monitored by silence verdicts — if `0`-replies exceed 2/week, the relevant organ auto-demotes one rung."* |
| **What it gates** | Per-organ **noise alarm**. The expected steady-state is `0` rare (operator does not silence; the brief earns its slot). Sustained `0`s mean the organ is generating noise, not signal. |

**Dependency to surface for the operator.** This metric depends on
`task-20260522-8bcd` shipping the verdicts ledger. The note flags this
explicitly so workers writing `peau-morning-digest` know the ledger
emission is a load-bearing contract, not an accessory.

### 2.3 — Kill-switch dwell-time *(kahneman)*

| Attribute | Value |
|---|---|
| **Proxy** | Time elapsed since `~/.cosmon/autopilot.off` was last touched. |
| **Threshold (Rung 1 → 2)** | 30 days untouched. |
| **Threshold (Rung 2 → 3)** | 30 days untouched **and** ≥ 1 PEAU `temp:warm` promoted to `temp:hot` per week over the dwell window (kahneman §x — utility, not just absence of irritation). |
| **Source of truth** | `stat -f %m ~/.cosmon/autopilot.off` (Darwin) → mtime. If the file does **not** exist, dwell starts at the last `cs incarne-status --reset` (operator gesture; defaults to epoch if never reset). |
| **Action on trigger (30 d clear)** | Enables nucleation of the next rung's work. Does **not** auto-nucleate — it removes a precondition; the operator still has to declare the rung change. |
| **Action on trigger (kill-switch touched)** | Dwell resets to 0. The most recent touch is logged via `events.jsonl` row `kill_switch_touched` (depends on `task-20260522-069b` shipping the `initiator` field — see §2.5). |
| **Falsifiable** | Yes — file mtime is a primitive. |
| **Per-operator** | Yes — `~/.cosmon/autopilot.off` is per-operator by construction (lives in `$HOME`). |
| **Manual / automatic** | **Automatic.** |
| **Citation** | kahneman, response §10(x) — *"Rung graduation = kill-switch dwell-time, not feature counts."* |
| **What it gates** | The composite trust metric. This is the **load-bearing rung gate**. Every other metric is either an alarm (2.1, 2.2) or a confirmation (2.4, 2.5); only kill-switch dwell-time actually moves the rung counter. |

### 2.4 — Briefing cites hippocampe unprompted *(karpathy)*

| Attribute | Value |
|---|---|
| **Proxy** | A worker's `briefing.md` cites a hippocampe note (`~/.cosmon/identity/hippocampus/<member>.md`) without the operator passing `--var hippocampe_ref=...` at nucleation time. |
| **Threshold** | ≥ 1 occurrence per week for 2 consecutive weeks. |
| **Source of truth** | `grep -E "hippocampus/[a-z-]+\.md" .cosmon/state/fleets/default/molecules/*/briefing.md` — minus any molecule whose `variables` (in `state.json`) contains a `hippocampe_ref` key. The "unprompted" qualifier is *no hippocampe_ref variable was set at nucleation*. |
| **Action on trigger** | Confirms Rung 2 is **functional** (not merely shipped). Pre-condition for Rung 3 evaluation. Does not itself graduate a rung. |
| **Falsifiable** | Yes — grep result + variable inspection. |
| **Per-operator** | Yes — single-galaxy single-operator path. |
| **Manual / automatic** | **Automatic.** |
| **Citation** | karpathy, response §10(x) — *"R1 → R2 ... 2 weeks where worker briefing.md cites hippocampe notes — replies sound like you knowing the person."* |
| **What it gates** | Rung-2 activity check. The signal *"cosmon is felt as having a remembered identity"* is observable here, not at the operator surface (the operator does not introspect their own pronoun usage on a daily basis — that is a weekly grep). |

### 2.5 — Single ground-truth bit *(wheeler)*

| Attribute | Value |
|---|---|
| **Proxy** | At least one row in `.cosmon/state/events.jsonl` whose `initiator` field is **not** `Operator`. |
| **Threshold** | ≥ 1 row / 24 h. |
| **Source of truth** | `events.jsonl` — depends on `task-20260522-069b` shipping the `§-causal-attribution` invariant (initiator field with values `Operator \| Tick \| Ingest \| Worker \| Runtime`). Until that invariant lands, the bit is *inferred from the absence of a corresponding `cs <verb>` shell history entry* — coarse, not authoritative. |
| **Action on trigger (bit = 1)** | The on/off light is green. Cosmon is faintly alive. No further action — the bit is **read, not acted on**. |
| **Action on trigger (bit = 0 sustained 7 d)** | The on/off light is red. File a bead `aliveness-bit-zero-<YYYY-MM-DD>`. Investigate: is `cs patrol --propel` running? Is the LaunchAgent loaded? Has the cron been silenced? |
| **Falsifiable** | Yes (`jq '.initiator != "Operator"' < events.jsonl | grep -c true`). |
| **Per-operator** | Yes (per-galaxy `events.jsonl`). |
| **Manual / automatic** | **Automatic.** |
| **Citation** | wheeler, response §1 — *"Between t and t+Δt, with the operator's terminal closed and no `cs` command issued by a human, did a row appear in `events.jsonl` that was not a side-effect of a previously-issued human command?"* + §5 — *"missing invariant `§-causal-attribution`."* |
| **What it gates** | The **§1 aliveness bit**. Bit = 1 means cosmon is alive in the wheeler sense. Bit = 0 means cosmon is back to observer-locked. The bit is the cheapest possible discrimination between Rung 0 and Rung 1+; without it, every other metric is post-hoc rationalization. |

## 3. What this document does **not** do

This is a structural clarification. The methodology stops where the
enforcement begins.

### 3.a — It does not implement any metric

No code in this doc. The methodology is read by workers and the
operator; it is implemented in:

- The `peau-morning-digest` formula (verdicts ledger emission, metric 2.2).
- The `task-20260522-069b` invariant (`initiator` field, metric 2.5).
- A future `cs incarne-status` diagnostic (read-only, metrics 2.2–2.5).
- Operator session-close ritual (metric 2.1, manual).

### 3.b — It does not auto-demote organs

Metric 2.2 names *"the offending organ auto-demotes one rung"* as the
action on trigger. **In v0 this is a recommendation to the operator,
not an automatic state transition.** The demotion-enforcement logic is
itself a Rung-2 design decision: it requires a rung state machine, a
per-organ rung ledger, and an `events.jsonl` row typed
`OrganDemoted { from: Rung, to: Rung, reason }`.

This document names the metric; it does not enforce it. Filing the
enforcement as a Rung-2 task is the correct sequencing — premature
enforcement would re-introduce the cognitive debt kahneman's pre-mortem
warns against (*"the system pays itself out of the operator's attention
budget without permission"*).

### 3.c — It does not gate on feature counts

If a worker is tempted to write *"Rung 2 ships when VISAGE + HIPPOCAMPE
both land"*, that is a feature-count gate and is **forbidden** by C-8 of
the synthesis. The five metrics in §2 are the **only** legitimate gates.
A feature can land while the rung remains at the previous level; what
matters is the operator's behavior in response to that feature.

### 3.d — It does not survive a multi-operator scaling

Every metric is per-operator by construction (single chronicle, single
`autopilot.off`, single hippocampus tree, single `events.jsonl`).
Generalisation to a fleet is a separate methodology document; do not
extend this one.

## 4. Reading order at session-close

A future operator (or `cs incarne-status` after it lands) reads this
methodology in the following order:

1. **Metric 2.5 first** — *is the §1 bit green?* If no, the rung is
   Rung 0 regardless of every other metric. Investigate before reading
   further.
2. **Metric 2.3** — *what is the kill-switch dwell?* This is the load-
   bearing rung gate.
3. **Metrics 2.1 and 2.2** — *have either alarm tripped?* If yes,
   demotion is on the table; reading the positive rung gates 2.3–2.4
   without checking the alarms is the silent-graduation failure mode.
4. **Metric 2.4** — *Rung 2 activity check.* Only relevant if the rung
   is at 2 already; ignored at Rung 0/1.

This ordering matches the kahneman principle: alarms first, gates second,
features last. The system has no opinion on its own aliveness (godel's
G★); it publishes bytes and the operator reads them in this order.

## 5. Provenance

- **Parent deliberation.** `delib-20260521-955f` — *Architect
  cosmon-incarné v0* (8-persona panel, 2026-05-22). See `synthesis.md`
  §5.5 (the rungs) and §C-8 (the convergence this doc preserves).
- **Per-metric citations.** Each row in §2 cites the panelist response
  file under
  `.cosmon/state/fleets/default/molecules/delib-20260521-955f/responses/`.
- **Sibling tasks** (decomposition siblings of this molecule, also
  blocked-by `delib-20260521-955f`):
  - `task-20260522-1f6b` — v0 baseline ADR. Cites this doc.
  - `task-20260522-069b` — `§-causal-attribution` invariant (initiator
    field). **Load-bearing dependency for metric 2.5.**
  - `task-20260522-8bcd` — `peau-morning-digest` formula. **Load-bearing
    dependency for metric 2.2** (verdicts ledger).

## 6. Open question filed as a sibling

- The optional stretch goal — prototype `cs incarne-status`
  (or `cs sensorium --metrics`) reading metrics 2.2–2.5 mechanically — is
  filed as `temp:warm` molecule `idea-20260522-incarne-status` (to be
  nucleated when this methodology lands). Out of scope for v0; will
  graduate to a `task` molecule once metrics 2.2 and 2.5 have their
  source-of-truth ledgers shipped.

---

*This note exists so the system cannot silently graduate. If you find
yourself reading it and the rung counter has moved without an
operator-felt event triggering it, the methodology has failed and
should be revised before the next rung.*
