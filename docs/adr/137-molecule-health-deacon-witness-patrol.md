# ADR-137 — Molecule-health: realize the Gas Town deacon/witness/patrol pattern as a cosmon primitive

**Status:** Proposed — design + spec only. This ADR ratifies the *shape* of a
federation-wide molecule-health primitive (detect / remediate-without-interfering /
surface-cosmon-ward). It does **not** authorise the build; the build is a follow-up
DAG of typed molecules (see §11 phased plan). No `just install`.

**Date:** 2026-06-26
**Decider:** Noogram (operator proposal, 2026-06-26)
**Source molecule:** `task-20260625-e355` (📐 decision).

**Builds on (prior art — realize, do not reinvent):**
- Gas Town role taxonomy — the **Deacon** (town-wide watchdog, the only agent that
  receives heartbeats), the **Witness** (per-rig health monitor), the
  **SessionManager** trait (`start / handoff / checkpoint / detect_stale / recover`).
  See the internal archive notes on the Gas Town concept mapping (§1, §7)
  and the Gas Town / cosmon deep-dive (§1)
  (*"session death is the #1 failure mode"*).
- [`crates/cosmon-core/src/patrol.rs`](../../crates/cosmon-core/src/patrol.rs) —
  `PatrolReport` + `PatrolAction` types already exist; `run_patrol()` and the
  two-layer design are **absent**. This ADR specifies them.
- [`crates/cosmon-cli/src/cmd/patrol.rs`](../../crates/cosmon-cli/src/cmd/patrol.rs)
  — the live `cs patrol` with `--respawn / --propel / --nudge / --expire /
  --auto-collapse`. This ADR adds `--heal` as a sibling mode, not a new verb.
- `scripts/drainage-tick.sh` — a **shell prototype**
  of exactly this health-pass (lines 94–135). This ADR specifies how to retire its
  bespoke pane-grep health logic into the typed core.
- `delib-20260625-be1e`
  — the adversarial audit of that very prototype (panel: adversary · torvalds · godel ·
  architect). **Its central finding is the spine of this ADR** (§2).

**Cites / must comply with:**
- [ADR-016](016-autonomy-regimes-and-resident-runtime.md) — Inert / Propelled /
  Autonomous regimes; *no daemon in the transactional core*; the worker-callable vs
  human-callable perimeter.
- [ADR-095](095-resident-runtime-ifbdd-path.md) — RR-1..RR-5 obligations on any
  scheduled, state-carrying, goal-seeking loop (the Autonomous regime).
- [ADR-038](038-whisper-perturbation-port.md) — `cs whisper`, the 6th channel
  (pilot→live-worker advisory text). The whisper log is the **piloting signal** the
  healer must never override (§5).
- ADR-116 — phantom-`Running`
  molecules; the worker-liveness gap and the existing auto-freeze / harvest / resume
  machinery this primitive composes with.
- [ADR-062](062-quotaclock-9th-clock.md) — `Starved` status (external authority
  refused service); the quota/backoff dimension of the *overloaded* anomaly.
- [`docs/architectural-invariants.md`](../architectural-invariants.md) §8a–§8f
  (control-plane vs data-plane), §8d (`events.jsonl` is source-of-truth).

---

## 1. Context — what the operator does by hand

Today fleet health is a **human reflex**: the operator runs `cs peek`, eyeballs each
worker's pane, and applies a remedy by muscle memory — fire `Enter` on a worker whose
prompt pasted but never submitted; `cs done` a molecule that `cs complete`d but whose
session lingers; collapse-and-re-dispatch a session that went 401; back off a worker
hammering an overloaded API. This is the *witness/deacon* job, performed by a person.
It does not scale to 42 galaxies across 4 accounts, and every stalled-but-occupied
slot is fleet capacity doing zero work.

The conceptual framework already exists — Gas Town named these roles, `patrol.rs`
holds the report/action types, and `drainage-tick.sh` is a working (if defective)
shell prototype of the health-pass. **The gap is a typed, idempotent, regime-respecting
cosmon primitive that detects the catalog of anomalies, remediates the ones it safely
can without ever touching a worker a human is actively piloting, and escalates a
recurring anomaly-class to a cosmon-ward molecule instead of healing the symptom
forever.**

This is not greenfield and not a new concept. It is the *realization* of the deacon
and the witness, with the discipline the be1e audit taught us bolted in from the start.

---

## 2. The load-bearing lesson — read intent from the control plane, never from pane glyphs

The be1e panel converged, from four independent directions, on one defect and one cure.

> **adversary (SEV-1)** and **godel (finding I)** reached the identical conclusion by
> opposite roads — BSD-regex tracing and Tarski use/mention: the prototype's
> self-`cs done` guard `grep -qE 'cs done'` fires on any pane line that merely
> *displays* the string "cs done". The brief that *forbids* self-`cs done` prints the
> phrase five times and is pasted into the very pane the guard greps — **so the guard
> kills the worker that best demonstrates it understood the rule.** Stating the rule
> more clearly *enlarges* the false-positive set.

The chronicle-worthy compression: *a guard that recognizes its target by its name
arrests everyone who says the name.*

The escape is **stratification** (godel): decide the worker's *act* in the control-plane
metalanguage — `events.jsonl`, a real state transition, a `merged_at` the worker could
not have authorized — **never** in the rendered scrollback the object-language worker
controls. Pane text is an *adversarial channel*: the worker can print anything, including
the exact glyphs of the rule meant to police it.

This single principle governs the whole primitive:

| Anomaly | ❌ Glyph signal (forbidden) | ✅ Control-plane signal (authoritative) |
|---|---|---|
| worker about to self-`cs done` | `grep 'cs done'` in pane | *not detectable nor needed* — the orchestrator owns `done`; a worker physically cannot self-`done` if `cs done` refuses a worker-context caller (§4, ADR-016 perimeter). Delete the detector. |
| 401 / auth-dead | `grep '401'` in pane | adapter **exit code** / a `ProcessDied` event / heartbeat lease expiry + a typed `AuthFailed` probe — never a bare substring |
| completed-unharvested | — | molecule `status == Completed` in `state.json` (already control-plane) |
| crash-survived `Running` zombie | — | liveness-lease expiry (ADR-116), tmux session absent for a `Running` molecule |
| unsent paste / boot-stall | `grep '[Pasted text'` | the **transport's own** submit-verification (cosmon-ward `task-20260625-81b2`), not a downstream grep |

Pane capture is permitted **only** as a *last-resort, human-surfaced* diagnostic
(`cs peek` shows it to a person), never as the trigger for an autonomous mutation.
Every automated `PatrolAction` in this primitive is keyed off control-plane state.

---

## 3. Architecture — three layers, mapped to Gas Town

```
            ┌─────────────────────────────────────────────────────────┐
            │  cosmon-scheduler (patrols.toml)  — the alarm clock       │  L0 cadence
            │  fires `cs patrol --heal` on an interval. NOT in the core. │
            └───────────────────────────┬─────────────────────────────┘
                                        │ one-shot invocation
            ┌───────────────────────────▼─────────────────────────────┐
            │  cs patrol --heal   (transactional core, stateless)       │
            │                                                           │
            │  ┌──────────────┐   detect    ┌──────────────────────┐    │
            │  │  WITNESS      │────────────▶│  HealthReport         │   │  L1 detect
            │  │  pure scan of │   (no I/O   │  = PatrolReport +     │   │  (per-molecule
            │  │  state+events │    in core) │  [HealthFinding]      │   │   classification)
            │  └──────────────┘             └──────────┬───────────┘    │
            │                                          │                 │
            │  ┌──────────────┐   remediate            ▼                 │
            │  │  DEACON       │◀──── HealGuard ──── for each finding     │  L2 remediate
            │  │  idempotent   │      (§5: never touch a piloted mol)     │  (idempotent,
            │  │  actions      │────────────▶ apply PatrolAction          │   guarded)
            │  └──────────────┘                                          │
            │                                                            │
            │  ┌──────────────┐   surface-cosmon-ward                    │
            │  │  ESCALATOR    │──── if class recurs > threshold ───────▶│  L3 surface
            │  │  (ring buffer │     auto-nucleate/reference a typed      │  (root-cause,
            │  │  on disk)     │     cosmon-ward molecule                 │   not symptom)
            │  └──────────────┘                                          │
            └────────────────────────────────────────────────────────────┘
```

**Mapping to Gas Town:**

| Gas Town | Cosmon realization | Layer |
|---|---|---|
| **Witness** (per-rig health monitor, detects stalled/zombie polecats) | `cosmon_core::patrol::scan()` — pure function `(FleetSnapshot, EventTail, now) → HealthReport`. Per-molecule classification. | L1 |
| **Deacon** (town-wide watchdog, manages remediation) | the `--heal` apply-loop in `cmd/patrol.rs` — calls the existing perimeter-correct verbs (`cs complete`, `cs done`, `cs collapse`, transport-submit). | L2 |
| **SessionManager** (`start/handoff/checkpoint/detect_stale/recover`) | `detect_stale` = liveness-lease check (ADR-116, already partly built); `recover` = `cs patrol --respawn / --propel` (already built). This ADR wires them under one report, does not rebuild them. | L1/L2 |
| **mol-boot-triage** (Deacon watchdog for boot health) | the *unsent-paste / boot-stall* finding → delegated to the transport's own submit-verify (cosmon-ward 81b2), **not** re-grepped here. | L1 |

**Regime placement (ADR-016, non-negotiable):**

- The **scan + apply** (`cs patrol --heal`) is a **one-shot, stateless** invocation —
  it belongs to the **Transactional Core**, identical in shape to today's `cs patrol
  --propel`. *No daemon, no background loop, no in-process scheduler.* It reads state,
  computes a report, applies idempotent actions, exits.
- The **cadence** is owned by `cosmon-scheduler` reading `patrols.toml` — the *external*
  alarm clock, exactly as `--propel` and `galaxy-drainage` already are. The scheduler is
  a separate, opt-in binary; the core `cs` workflow never spawns it.
- The loop is **Propelled**, not Autonomous: it nudges/heals *existing* Propelled
  molecules; it does **not** dispatch new work, does **not** carry goal-seeking dispatch
  state. (That distinction is what got the drainage prototype a HIGH godel finding — a
  *dispatching* loop is the Autonomous regime and needs ADR-095 RR-1..RR-5. Healing is
  not dispatching; it stays in Propelled.)

---

## 4. The anomaly catalog (the Witness)

Each row: the anomaly, its **control-plane** detection signal (never a pane glyph), and
the perimeter-correct remedy. `N` thresholds are config in `patrols.toml`, with the
defaults below.

| # | Anomaly | Control-plane detection signal | Default remedy (§ = guard applies) |
|---|---|---|---|
| A1 | **unsent-paste / boot-stall** (prompt pasted, never submitted; slot occupied, zero work) | transport submit-verification reports input zone non-empty after tackle (the 81b2 fix); *or* molecule `Running` with `last_progress_at == tackled_at` and worker session alive but no `events.jsonl` growth for `boot_grace` (90 s) | **transport re-submits** (the 81b2 robust-submit, owned by `cs tackle`/`cosmon-transport`). The healer only *flags* if still stalled after re-submit — it does not re-implement the Enter-kick. |
| A2 | **self-`cs done` poised** (worker about to self-destroy — perimeter violation) | **none — structurally prevented.** `cs done` refuses a worker-context caller (walk-up discovery shows it's inside a worktree). No detector; the perimeter is the guard. | n/a — delete the prototype's pane-grep branch entirely (be1e B2). |
| A3 | **401 / auth-dead session** | adapter **exit code** (ADR-119 exit-code contract) / `ProcessDied` event / liveness-lease expired AND a typed auth-probe fails — **anchored**, never the bare substring `401` | `cs collapse --reason-kind process_death`; re-dispatch is the operator's/orchestrator's call with **per-account backoff** (§5, breaks the collapse→redispatch amplification loop, be1e B5). |
| A4 | **idle-after-complete** (`cs complete`d, session lingers, slot held) | molecule `status == Completed` AND tmux session still present | orchestrator-only `cs done` (harvest + teardown). Human-callable verb; the *scheduler* is a sanctioned non-worker caller (godel: runs from a sibling shell, not a worktree). |
| A5 | **idle-running zombie** (alive session, no progress > N) | `Running` AND `last_progress_at` older than the step's `timeout_minutes` budget (reuse the existing `--nudge` classifier) AND session alive | `cs patrol --nudge` (re-engage, references `briefing.md`), idempotent (no re-nudge within 60 s). Escalate to collapse only after `M` consecutive failed nudges. |
| A6 | **overloaded** (`API Error` / rate-limited / `Starved`) | adapter exit code / `Starved` status (ADR-062 QuotaClock) / a typed `RateLimited` event | **exponential backoff per account** — do *not* collapse, do *not* re-dispatch into the same wall. Mark `temp:frozen`-equivalent runtime hold; retry after cooldown. |
| A7 | **ghost-merge / silent-done** (branch merged or molecule archived without the authorized `done` path — `idea-20260509-e164`) | `merged_at` / archived set on a molecule whose `events.jsonl` has **no** authorizing `Done` event from a non-worker caller (stratified: a state the worker could not have authored) | **flag only — never auto-heal.** Emit a `GhostMerge` finding, surface to operator + cosmon-ward. This is an integrity alarm, not a slot-recovery action. |
| A8 | **completed-unharvested** | `status == Completed` AND `archived == false` (no live session needed) | orchestrator `cs done` (the §A4 harvest, decoupled from session presence). |
| A9 | **crash-survived `Running` zombie** (ADR-116: worker gone, `status` stuck `Running`, every downstream `cs wait` blind) | liveness-lease expired (ADR-116) AND no tmux session AND `status == Running` | `cs collapse --reason-kind process_death` (or auto-freeze per existing ADR-116 machinery) so downstream waiters observe a terminal state. |

**Note on A2/A7:** these are the two *perimeter/integrity* classes. Neither is healed by
a glyph match. A2 is prevented by the `cs done` perimeter itself; A7 is *flagged*, never
mutated. The healer's autonomous mutations are confined to A1, A3–A6, A8, A9 — all keyed
off authoritative state.

---

## 5. The no-interference-with-piloting guard (the hard part)

The deacon must **never** touch a worker a human pilot is actively driving — mid-whisper,
mid-legitimate-long-operation, mid-debug. Healing a piloted worker is worse than the
stall it cures: it destroys live human work and erodes trust in the whole primitive.
The guard is a **conjunction** — *all* clauses must pass before any autonomous mutation:

1. **No live pilot on this molecule.** If a pilot session is registered against the
   molecule in the presence registry (`.cosmon/state/presence/`), the molecule is
   **off-limits**. Piloted ⇒ skip.
2. **Whisper quiet-period.** If the molecule's whisper log (ADR-038) received a directed
   whisper within `pilot_quiet` (default **10 min**), skip. A recent whisper means a human
   is steering; the healer waits out the quiet period before considering the worker
   abandoned. (ADR-038: whisper is *advisory, Propelled-regime, human-pilot only* — its
   presence is the clearest "hands on the stick" signal we have.)
3. **Per-molecule kill-switch.** A `do-not-heal` marker — molecule tag `health:hold` OR a
   `<molecule_dir>/.no-heal` sentinel — exempts a specific molecule unconditionally. The
   operator sets it when they want to babysit a worker by hand.
4. **Global kill-switch.** `~/.cosmon/health.off` present ⇒ the whole `--heal` pass is a
   no-op (logs the skip, exits 0). Mirrors the `drainage.off` convention. Checked at the
   *top* of the pass *and* re-checked before each mutation (the pass may straddle an
   operator gesture).
5. **Backoff memory (idempotence across ticks).** Each remediation records its action +
   timestamp on the molecule (reuse `nudge_count` / a `last_heal_at` field). The same
   remedy is not re-applied within its per-class cooldown (nudge 60 s, collapse-redispatch
   per-account backoff, etc.). **Three consecutive failed remediations on one molecule ⇒
   stop healing it, flag for human** (the cross-galaxy three-strikes convention).

Clauses 1–2 are the *piloting* guard; 3–4 are *kill-switches*; 5 is *idempotence /
anti-thrash*. The guard is evaluated **per molecule, per finding** — a healthy decision
on molecule X never licenses an action on molecule Y.

> **Stratification reminder (§2):** the guard reads piloting from the **control plane**
> (presence registry, whisper log, tags, sentinel files) — never by inspecting a pane to
> guess "does this look like a human is typing?". The whole class of glyph-inference is
> banned, for the deacon as much as for the witness.

---

## 6. Cosmon-ward escalation rule (the Escalator)

Healing a symptom forever is a failure of advocacy. When an anomaly **class** recurs above
a threshold and has a plausible **upstream root cause**, the primitive must surface it as a
typed cosmon-ward molecule rather than kick the same worker every tick — exactly how the
operator surfaced the paste-submit root cause as `task-20260625-81b2` instead of accepting
the drainage script's forever-Enter-kick.

**Mechanism (stateless-friendly):** the healer keeps a small on-disk **ring buffer** of
recent findings per class (`~/.cosmon/health-ledger.jsonl`, append-only, bounded). On each
pass, for each class:

```
if findings_of_class(C) within window W  >=  escalate_threshold(C)
   and not already_escalated(C, cooldown):
       nucleate (or reference an existing) cosmon-ward molecule:
         kind = issue, tag temp:hot,
         title = "Cosmon-ward: <class C> recurs (<n>× in <W>) — upstream root cause"
         body  = the finding samples + the suspected primitive at fault
       record escalated(C) in the ledger  (so we don't re-nucleate every tick)
```

The ledger is **runtime sediment, not source-of-truth** — losing it only re-arms the
escalation, never corrupts state. Each class declares its own `escalate_threshold` and
suspected upstream in `patrols.toml`. Worked examples:

| Class | Threshold | Suspected upstream → cosmon-ward target |
|---|---|---|
| A1 unsent-paste | ≥3 in 1 h | `cs tackle` / `cosmon-transport` robust multi-block submit — **already filed** as `task-20260625-81b2`. The escalator *references* it (dedup), does not re-file. |
| A7 ghost-merge | ≥1 ever | `cs done` integrity invariant (`idea-20260509-e164`). One occurrence is enough — integrity alarms don't wait for a quorum. |
| A3 401 storm | ≥N per account per hour | account/quota routing or token refresh — escalate to the operator (auth is human-owned). |
| A9 crash-zombie | ≥3 in a day | the liveness-lease gap (ADR-116) — reference, don't re-file. |

**Dedup discipline:** before nucleating, scan for an open molecule with the same
cosmon-ward title-key; if present, append a finding-sample to it and bump its `temp` rather
than creating a duplicate. (Same anti-collision spirit as ADR-121.) Auto-nucleated children
are auto-tagged `temp:hot` per the CLAUDE.md decomposition rule.

---

## 7. The `cs` surface

**No new top-level verb.** Per the be1e architect verdict (*"scaffold the transport,
nucleate the judgment — no new primitive"*) and the composability principle, healing is a
**mode of the existing `cs patrol`**, and inspection is a thin read-only view.

- **`cs patrol --heal`** — run one detect→remediate→escalate pass. Composes with the
  existing flags: `--heal` implies the union of the targeted classes; `--stale-after`,
  `--no-tmux` (state-only, for tests) carry over. `--heal --dry-run` prints the
  `HealthReport` + the `PatrolAction`s it *would* take without mutating (the safe default
  for first operator trust). `--json` emits the report as NDJSON (agent-first).
- **`cs health`** (read-only alias / sugar) — `cs patrol --heal --dry-run --all` across
  every `.cosmon/` on disk: the federation-wide "what is anomalous right now" snapshot the
  operator currently assembles by hand with `cs peek`. Pure projection, zero mutation.
  Exit code: `0` all-healthy, `1` findings present (CI/monitor-friendly).
- **Scheduler patrol** — a `patrols.toml` entry firing `cs patrol --heal` on a cadence,
  gated by `~/.cosmon/health.off`. Ships **disabled by default**; the operator enables it
  per-federation once the dry-run output has earned trust.

**UX↔CLI parity (ADR-068):** `--heal`/`health` get a pilot-app counterpart (a "fleet
health" panel surfacing the `HealthReport` over the `cs peek --snapshot` raster) — filed as
a `temp:warm` follow-up in the same spirit, not built here.

---

## 8. What this primitive is NOT (scope fences)

- **Not a dispatcher.** It heals existing Propelled molecules. It never nucleates *work*
  molecules nor `cs tackle`s. (The drainage daemon's dispatch half is a *separate*,
  Autonomous-regime concern needing ADR-095 — explicitly out of scope here. This ADR is the
  *health-pass* half the be1e panel called "competent and worth keeping".)
- **Not glyph-driven.** No autonomous action is ever triggered by pane-scrollback text (§2).
- **Not a new state store.** The ledger (§6) is disposable runtime sediment; all truth stays
  in `.cosmon/state/` + `events.jsonl`.
- **Not a healer of integrity violations.** Ghost-merge (A7) is *flagged*, never *mutated* —
  the deacon recovers slots, it does not paper over a broken `done` invariant.

---

## 9. Coherence checklist (architectural-invariants.md)

1. **Stateless?** ✅ one-shot `cs patrol --heal`; cadence external (scheduler).
2. **Idempotent?** ✅ twice = once (backoff memory §5.5; dry-run default).
3. **Regime-aware?** ✅ Propelled (heals existing workers), never Autonomous (no dispatch).
4. **Single perimeter?** ✅ a mode of `cs patrol`; remediation calls existing verbs, adds none.
5. **Symmetric undo?** ✅ every mutation uses a reversible existing transition (`collapse`
   preserves variables+links; `done` is the sanctioned teardown). The one structure-lossy
   path (collapse severs DAG edges, be1e architect) is gated behind operator escalation, and
   a future `cs revive` (be1e B12) is referenced as the proper fix.
6. **Runtime-compatible?** ✅ when the Resident Runtime owns L3, `--heal` becomes a policy it
   calls; the pure `scan()` is reused verbatim.
7. **Worker/human boundary?** ✅ the deacon runs as a non-worker scheduler caller; it never
   self-dones a worker; A2 relies on the `cs done` perimeter, not a glyph.
8. **Write-read asymmetry?** ✅ `cs health` reads only; `--heal` writes; `--dry-run` separates
   them.
9. **Merge-before-dispatch?** ✅ n/a — no dispatch.
10. **CLI-first for workers?** ✅ no worker codepath; the primitive is operator/scheduler-side.

---

## 10. Realizing `patrol.rs` (the typed core)

The existing `PatrolReport` / `PatrolAction` gain a per-molecule classification layer
(sketch — final signatures land with the build molecule, kept I/O-free in `cosmon-core`):

```rust
/// A single anomalous molecule + its classified cause, keyed off control-plane
/// state — NEVER pane glyphs (ADR-137 §2).
pub struct HealthFinding {
    pub molecule_id: MoleculeId,
    pub class: AnomalyClass,          // A1..A9
    pub signal: ControlPlaneSignal,   // what state/event proved it (auditable)
    pub piloted: bool,                // guard §5.1/§5.2 — set ⇒ no autonomous action
    pub recommended: PatrolAction,    // the perimeter-correct remedy (or NoAction)
}

pub enum AnomalyClass {
    UnsentPaste, AuthDead, IdleAfterComplete, IdleRunningZombie,
    Overloaded, GhostMerge, CompletedUnharvested, CrashZombie,
    // (SelfDonePoised is intentionally absent — prevented by perimeter, §4 A2)
}

/// HealthReport = PatrolReport (fleet aggregate) + per-molecule findings.
pub struct HealthReport {
    pub patrol: PatrolReport,
    pub findings: Vec<HealthFinding>,
}

/// Pure: state in, report out. Testable without I/O (THESIS Part VII).
pub fn scan(fleet: &FleetSnapshot, events: &EventTail, now: DateTime<Utc>) -> HealthReport;
```

`scan()` is pure and property-testable (Stable-tier: proptest the classification +
serde roundtrips). The apply-loop and the escalator live in `cosmon-cli` (the I/O shell),
as `--propel`/`--nudge` already do.

---

## 11. Phased build plan (follow-up DAG — not built in this ADR)

Each phase is a typed molecule; ship in order, smallest-first, each independently mergeable.

| Phase | Ships | Retires / composes |
|---|---|---|
| **P1 — Witness (read-only)** | `cosmon_core::patrol::scan()` + `HealthFinding`/`AnomalyClass`/`HealthReport`; `cs health` (dry-run, `--json`, exit-code). **No mutation.** | First trustworthy artifact: the operator sees the catalog the same way `cs peek` shows it, federation-wide, no risk. |
| **P2 — Guard** | the §5 no-interference conjunction (presence + whisper quiet-period + per-molecule + global kill-switch + backoff memory). Unit-tested in isolation *before* any mutation is wired. | The guard exists before the deacon can act — the be1e lesson: build the brake before the engine. |
| **P3 — Deacon (safe classes)** | `cs patrol --heal` mutating only the *low-risk, reversible* classes: A1 (delegate to transport), A4/A8 (`cs done` harvest), A5 (`--nudge`), A6 (backoff). Each behind the P2 guard. | **Retires `scripts/drainage-tick.sh` lines 94–135** (the bespoke pane-grep health-pass) — replaced by the typed, stratified, guarded pass. The drainage script keeps only its *dispatch* half (the separate Autonomous concern). |
| **P4 — Deacon (collapse classes) + Escalator** | A3 (auth-dead collapse + per-account backoff), A9 (crash-zombie collapse, composing ADR-116), A7 (ghost-merge *flag*); the §6 ring-buffer escalator with dedup. | Composes ADR-116 machinery; references `task-20260625-81b2` & `idea-20260509-e164` rather than re-filing. |
| **P5 — Scheduler patrol + parity** | `patrols.toml` entry (disabled-by-default, `health.off` gated); the ADR-068 pilot-app health panel. | The cadence + the UI surface — last, once detect+remediate have earned operator trust via dry-run. |
| **P6 — `cs revive` (optional, proper fix)** | restore `Pending` from a collapsed molecule's preserved `state.json` (variables + typed_links survive collapse) — makes "collapse is recoverable" actually true (be1e B12). | Upgrades §9.5's structure-lossy caveat into a true symmetric undo. |

**Dependencies:** P2 blocks P3/P4 (guard before mutation). P1 blocks all (the report type
is the shared spine). P6 is independent and may land any time.

---

## 12. Consequences

**Positive.** The operator's by-hand `cs peek` reflex becomes a typed, auditable, idempotent
primitive that scales to the full federation; stalled-but-occupied slots are reclaimed without
human babysitting; recurring anomaly-classes escalate to their root cause instead of being
kicked forever; the be1e audit's hard-won lessons (control-plane stratification, the piloting
guard, class-aware policy) are encoded structurally rather than re-learned per incident.

**Costs / risks accepted.** A scheduled heal-pass is a step toward standing infrastructure —
mitigated by keeping the *cadence* external (scheduler, not core), shipping *disabled by
default*, and dry-run-first. The guard is a conjunction, so a *missing* piloting signal
(e.g. a pilot driving a worker with neither presence row nor recent whisper) could let the
deacon act on a piloted worker — mitigated by the per-molecule `health:hold` sentinel and the
operator-tunable quiet-period, and called out here so the operator knows the failure mode.
Glyph-inference is permanently foreclosed, which means some "obvious to a human eye" stalls
(a worker visibly waiting at a `?` prompt with no state signal) are *not* auto-healed — by
design: we accept missing a detectable-only-by-pane stall over the catastrophic false-positive
of killing a compliant worker (be1e SEV-1).

---

## 13. The chronicle seed

*A guard that recognizes its target by its name will arrest everyone who says the name.* The
fix for the drainage daemon's self-`cs done` bug was itself a self-`cs done`-class bug: the
operator replaced "a worker that *runs* `cs done`" with "a daemon that *greps* for `cs done`",
trading a bug a worker had to *choose* for one that fires on every worker that merely *reads
the rule*. The escape — and the spine of this primitive — is Tarski's: decide the act in the
control-plane metalanguage, never in the object-language glyphs the worker controls. The
deacon watches the state machine, not the screen.

(Worth an internal chronicle entry when this lands — flagged, not written, per the
chronicle discipline.)
