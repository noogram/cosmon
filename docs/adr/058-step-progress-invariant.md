# ADR-058 — Step-progress invariant and re-prompt primitive

**Status:** Proposed (2026-04-20)
**Scope:** `I_StepProgress` liveness invariant, the `StepClock` (the
8th clock promised in ADR-052 Amendment 2026-04-20), the
`GhostKind::InferenceStalled` variant, the two-stage passive/active
detection protocol, the C-threshold policy, and the missing re-prompt
primitive that `cs tackle` writes but `cs evolve` has no counterpart
for. This ADR consolidates the five formal mechanisms the panel
produced into the authoritative specification of the
*inference-momentum-loss* pathology — the seventh ghost, the pane that
lives with a worker whose turn has ended.

**Parent task:** `task-20260420-8cef`
**Governing deliberation:** `delib-20260420-1b02`
— 9-persona panel (galileo, turing, knuth, shannon, hawking, torvalds,
jobs, feynman, carnot). Per-persona responses under
`responses/`.
**Fixture:** `idea-20260419-2d4e` in `/srv/cosmon/market-agents` — the
4-hour silent molecule. Preserved as an empirical test vector for
future regression detection; do not collapse.

**Binds:**
- [ADR-016](016-autonomy-regimes-and-resident-runtime.md) — regime
  vocabulary (Inert / Propelled / Autonomous). `I_StepProgress`
  applies in the Propelled regime; the auto-resume extension is
  deferred to P3-Autonomous.
- [ADR-038](038-whisper-perturbation-port.md) — the re-prompt
  primitive this ADR names is a specialization of the sixth channel
  (`cs whisper` / pilot→live-worker semantic text). ADR-058 answers
  *what the `cs evolve` counterpart writes on that channel*, and
  under which regime (galileo: *le canal est à moitié câblé*).
- [ADR-052](052-one-ledger-one-writer-one-witness.md) — extends
  it. `StepClock` is the eighth clock referenced by the Amendment
  2026-04-20 in ADR-052 §D1. `I_StepProgress` composes with I2
  (SingleWriterPerField), I7 (SingleEventWriter), I8
  (MeasurementEmission), I10 (SilenceIsSignal). Does **not** replace
  any of the ten named invariants; it adds a liveness clause.
- [ADR-055](055-cosmon-residence.md) — the eventual P3-autonomous
  resumer lives in the `residence` layer, not in the transactional
  core.
- [ADR-056](056-notary-protocol-v0.md) — seal-chain compatibility:
  the `StepClock` entries in `sealLog` are the same seals the notary
  signs, extended with an optional `kind: "resume"` tag
  (knuth §7).
- [ADR-057](057-genre-and-artifact-map.md) — the `I_StepProgress.tla`
  excerpt in `058-step-progress-invariant/` is genre
  *formal-spec*, not *architecture-decision*. Both live side by side;
  the genre map has no v0 distinction but the residence implied is
  identical.

## 1 · Context

### 1.1 · The fixture (galileo)

On 2026-04-19T18:53 UTC the operator ran
`cs nucleate idea-to-plan` → `cs tackle` on a 4-step molecule in the
`market-agents` galaxy. By 2026-04-20T16:07 UTC `cs evolve` had
recorded step 1 ("capture") as sealed in `sealLog[0]`, rewritten
`briefing.md` to display step 2, and bumped `current_step`. **Nothing
happened afterwards for four hours.**

The observational signature (galileo §1):

- `state.json` — `status: "running"`, `completed_steps: ["capture"]`,
  one briefing seal, no step-2 seal.
- tmux pane — last lines *"Step 1 captured. Now step 2 — evaluate
  feasibility."* followed by an idle `❯` prompt. The bypass-permissions
  banner still visible; no keystroke consumed.
- Worker process (PID 22635) — state `Ss+` (sleeping on stdin, session
  leader, foreground). Elapsed 02:42:48. Claude was not inferring; it
  had *finished its turn*.
- Filesystem — `briefing.md` mtime updated at the moment of the seal.
  Nothing ever read it back into the pane.

### 1.2 · Why the seven pre-existing ghosts do not cover it

The `GhostKind` enum introduced in ADR-052 D2 enumerates six pane-side
and ledger-side drift shapes:

| Ghost | ADR-052 | What it detects | Fires on 2d4e? |
|---|---|---|---|
| `DeadPane` | I4 | tmux dead, fleet Registered | **No** — pane alive |
| `VanishedWorker` | I3 | process gone, fleet Registered | **No** — PID 22635 alive |
| `UnHarvested` | I5 | `Completed` without merge | **No** — status is `Running` |
| `StaleProbe` | I10 | witness older than TTL | **No** — no probe has been taken |
| `UnnamedMerge` | I9 | merge without `cs done` | **No** — no merge at all |
| `PermissionPromptHang` (fa82) | — | Claude awaiting permission grant | **No** — no pending prompt |

None of them names the pathology. The worker was not crashed, not
ghost-merged, not blocked on a permission dialog — it had **cleanly
finished a narrative turn** on a descriptive phrase (*"Now step 2 —
evaluate feasibility"*) that *to the worker itself* read like a
transition, and there was no user-turn coming to wake it up.

Turing §6 named the pattern: the worker passed its own Turing test.
A judge reading only that tape would mark "continuing". The surface
form of completion coincided with the cessation of computation.

### 1.3 · The structural bug: the half-wired channel (galileo §4)

`cs tackle` injects the autonomous-mode prompt into the tmux pane as a
single user turn at spawn time. `cs evolve` rewrites `briefing.md` on
disk and appends a `BriefingSealed` event to `events.jsonl`.
**Nothing** translates a new seal into a new user-turn in the pane.
The write side of the briefing channel has no reader.

> *Le canal est à moitié câblé. `cs tackle` écrit une fois au spawn ;
> `cs evolve` n'a aucun contrepoint.*
> — galileo, synthesis §Insight 5

Feynman's 8-year-old (§1): *"Room done. Now the table."* The child
flipped the fridge-magnet himself but nobody told him to actually
start. Structural off-by-one between *cosmon advanced* and *worker
started next turn*.

### 1.4 · The channel-capacity bound (shannon)

Passive observation of the filesystem alone cannot distinguish a
stalled worker from a slow-but-progressing worker in the adversarial
chain-of-thought regime (shannon §5). Entropy table:

| Signal | I(S; Stalled) | Notes |
|---|---:|---|
| S1 seal-age | 0.05 bits | degenerate alone |
| S2 tmux mtime | 0.7 bits | discriminator when worker prints |
| S3 events Δ(T) | 0.8 bits | best when available; not universal |
| S4 CPU sample | 0.85 bits | highest raw I, high cost |
| S7 step vs completed | ≈ 0 | ledger consistency, not liveness |
| S8 briefing-hash vs seal | ≈ 0 | tamper channel, orthogonal |

MSS = `{S1, S2}` — sufficient in the non-adversarial regime
(≈ 1.1 bits, above the 1-bit binary threshold). **Worst case** in the
adversarial-slow-step regime: filesystem channel capacity = 0 bits.
No cleverness on `{S1, S2, S5, S6}` can separate stall from slow-step.
The channel does not carry the bit.

> *The MSS here is not a feature vector — it is a **protocol**: passive
> `{S1, S2}` until ambiguous, then ask. The measurement completes the
> channel.*
> — shannon, synthesis §Part 4

### 1.5 · Why the pathology is fixable (hawking)

The frontier between α (inference layer, Claude's turn) and γ
(cosmon filesystem) is **sharp on α and smeared to infinity on β/γ/δ**
(hawking §1). β/γ/δ only see α through its emissions; in the absence
of emission they extrapolate the last value forever. This is a
**cosmological horizon, not an event horizon**:

- Event horizon — causally forbidden, never fixable.
- Cosmological horizon — layers recede because their expansion rates
  decoupled. α stopped emitting; β/γ/δ tick on wall-clock. Close the
  horizon by *re-accelerating α* — a heartbeat, a re-prompt, a
  push-into-the-pane.

> *Cosmological horizons can be closed by changing the expansion
> rate.* — hawking §4

### 1.6 · The decidability horizon (turing)

Formally, the worker is an **oracle machine** (Turing 1938); internal
configuration is occluded. Only the tape is observable. Two
predicates:

- `LongRunning(m, t)` — ∃ `t' > t` where the worker autonomously
  emits a new tape event without external perturbation.
- `Stalled(m, t)` ≡ `¬LongRunning(m, t)`.

**Stalled is semi-decidable (co-r.e.), not decidable.** No finite
passive prefix separates "4 h on one token" from "decoding halted
forever" — Rice-adjacent over the oracle's future behaviour. The only
passively decidable predicates are `TmuxDead`, `ProcessDead`, and
`NoTapeProgress(m, Δ)`. The gap from `NoTapeProgress` to `Stalled` is
the halting-problem horizon — necessary for stall, never sufficient.

Active probing (re-prompting) lifts `Stalled` to semi-decidable: a
correlated output confirms running; silence is further evidence but
never certainty. The probe **is** the measurement, per ADR-052 I8
(MeasurementEmission). Cosmon does not solve halting; it detects the
silence after the descriptive phrase and **surfaces it to a pen that
can write**.

## 2 · Decision

ADR-058 composes **five formally independent mechanisms** into the
authoritative response to the inference-stall pathology. Each stands
alone, but together they form the full perimeter. The numbering
(M1–M5) is used throughout this ADR and tracks the panel provenance.

### D0 · Vision sentence

> **A cosmon molecule that stops emitting seals is detected by the
> kitchen clock, not by itself; and the re-prompt that wakes it is a
> pen held by an entity outside it.**

Sixteen words. Composes ADR-052's D0 (*Cosmon is a filesystem that
remembers which worker owns which decision, so no one — not even the
pilot — can answer in the worker's place*) with the eighth-clock
extension: the worker cannot certify its own *Presence*, which is why
silence is a signal, not a fact the worker asserts.

### D1 · Mechanism M1 — TLA+ `I_StepProgress` (knuth)

**Authoritative spec:** [`docs/specs/CosmonRun.tla`](../specs/CosmonRun.tla)
(extended 2026-04-20 by `task-20260420-ea09`).
**Excerpt for ADR reviewers:** [`058-step-progress-invariant/I_StepProgress.tla`](058-step-progress-invariant/I_StepProgress.tla).
**Model config:** [`docs/specs/CosmonRun_StepProgress.cfg`](../specs/CosmonRun_StepProgress.cfg).

`I_StepProgress` extends `CosmonRun.tla` with two variables —
`sealLog` (the per-molecule sequence of briefing-seal timestamps) and
`now` (a monotonic global clock bounded by `MaxClock` for TLC
finiteness) — and one observer action `MarkStalled(m)`.

The invariant itself, in TLA+ leads-to notation:

```
I_StepProgress ==
  \A m \in Mol :
    (mol_status[m] = "Running" /\ Silence(m) > T_STALL)
      ~> mol_status[m] \in {"Stalled", "Collapsed", "Frozen"}
```

with `Silence(m) = now - last_sealLog_entry(m)`.

The consequent is deliberately broadened beyond `Stalled` — operator-
initiated `Collapse` or `Freeze` are legitimate resolutions of a
detected stall (feynman's child test: the silent messenger is
noticed either by the kitchen clock *or* by the mother asking *"are
you asleep?"*). Without weak fairness on `MarkStalled` **and** `Tick`,
TLC produces starvation counter-examples (knuth §4, same shape as
ADR-052 I5 harvest-starvation). Both fairness conditions are required
— the `Spec` line in `CosmonRun.tla` encodes both.

**Test vectors (informative, from knuth §6).** Full traces in
`responses/knuth.md`.

| Trace | T_STALL | Behaviour | Verdict |
|---|---:|---|---|
| A — normal | 10 | `Silence` peaks at 4 across three seals | ✓ invariant satisfied |
| B — fixture 2d4e | 10 | `Tick×11` after `EmitSeal(step=1)`, no further seal | ✓ `MarkStalled` fires under WF |
| C — LongRunning at T − ε | 10 | `Silence` peaks at 9 | ✓ no false fire |

**Soundness of a future `Resume` action (knuth §7).** Because the ADR
couples detection (M1) with a re-prompt primitive (M5), the formal
correctness of the combined system requires `Resume` to be
ledger-written — a silent in-memory reset of the clock without
appending to `sealLog` produces a Sisyphus trace (re-prompt ∞,
invariant never violated, `Completed` never reached). See §D6 below
for the anti-Sisyphus K-bound.

**Do NOT run TLC in this ADR.** The mechanical validation belongs in
the CI pipeline feeding `docs/specs/VALIDATION-REPORT.md`. ADR-058
declares the spec; the runtime discipline — proposing mechanisms of
verification, not imposing them (architectural-invariants §8b) —
lives in the `tlc` gate child molecule.

### D2 · Mechanism M2 — `GhostKind::InferenceStalled` (turing)

A new variant of the `GhostKind` enum introduced in ADR-052 §D2. This
is the **seventh ghost** (hawking / turing naming; the sixth named in
ADR-052's five `GhostKind` + the prior `PermissionPromptHang` class
`fa82`).

**Formal predicate (turing §5).** An admissible state `s` exhibits
`InferenceStalled(m)` iff:

```
σ_tape(m, now_s) > T_step(formula(m), step(m))
  ∧ process_alive(m)
  ∧ tmux_alive(m)
  ∧ ¬Completed(m)
  ∧ mol_status(m) = Running
```

where

```
σ_tape(m, t) = t − max( last_sealLog_entry(m).t,
                        last_tool_call_timestamp(m),
                        last_pane_mtime(m) )
```

and `T_step(formula, step)` is a per-(formula, step) silence
threshold, seeded by Bayesian priors on tool-call cadence and
tightened by Shannon's σ from ADR-052 I10.

**Distinctness (one-line pattern-match, Rust-side).**

```rust
// extends the ADR-052 GhostKind enum in cosmon-core
pub enum GhostKind {
    DeadPane,              // I3  (ADR-052)
    VanishedWorker,        // I4  (ADR-052)
    UnHarvested,           // I5  (ADR-052)
    StaleProbe,            // I10 (ADR-052)
    UnnamedMerge,          // I9  (ADR-052)
    PermissionPromptHang,  // fa82 incident
    InferenceStalled,      // ADR-058 — the seventh ghost
}

impl RunState {
    pub fn ghost(&self, thresholds: &StallThresholds) -> Option<GhostKind> {
        match (&self.intent, &self.witness) {
            // ... prior ADR-052 arms unchanged ...
            (Intent::Run, Some(w))
                if w.sigma_tape > thresholds.t_step(self.formula, self.step)
                    && w.process == Liveness::Alive
                    && w.tmux == TmuxState::Alive
                    && !matches!(self.intent, Intent::Terminal(_))
                => Some(GhostKind::InferenceStalled),
            _ => None,
        }
    }
}
```

**What it is not.** Not `DeadPane` (pane lives). Not `VanishedWorker`
(process lives). Not `UnHarvested` (no `Completed` state). Not
`StaleProbe` (detection *is* the probe, not a cached observation).
Not `UnnamedMerge` (no merge at all). Not `PermissionPromptHang`
(nothing is pending).

The Rust-side landing of the variant is tracked under the Phase 1
polymer workstream (sibling task `task-20260420-ec4b`); ADR-058 is
the formal proof obligation that justifies the enum extension.

### D3 · Mechanism M3 — `StepClock` as the 8th clock (hawking)

ADR-052 names seven clocks: DAG bit, filesystem mtime, tmux
heartbeat, `events.jsonl` with `flock`, git, archive, RunState. The
`tmux heartbeat` is a β-clock (pty process alive) — **not an
α-clock** (inference alive). hawking §5 proves the missing clock is
an α-emission clock: a monotonic counter owned by the worker's
inference layer, witnessed in `events.jsonl` (I7). `StepClock` is its
name.

The ADR-052 Amendment 2026-04-20 (§D1) already reserves the phrase:

> *"eighth clock (StepClock)"* … *"operationalised by the liveness
> invariant `I_StepProgress`"*.

ADR-058 formalizes the reservation. Two readings of the extension are
equivalent and both are correct:

1. **Octave reading** — an eighth clock added to the seven.
2. **Completion reading** — the seventh clock (`RunState`) was
   missing one component; `StepClock` completes it by giving it the
   one thing `RunState` did not carry: proof that *this step* is
   alive, not just *this molecule*.

The octave reading is preferred in the polymer workstream (where
`StepClock` is a separate counter variable in `cosmon-core`); the
completion reading is preferred in the prose of ADR-052's
`RunState::witness` section (where the `sigma_tape` field is simply
added to `Witness`). Both descend to the same on-disk artifact — an
entry in `sealLog` appended on every successful `cs evolve`.

**`StepClock` generalizes.** Every autonomous layer in a
multi-layer agent system must emit its own liveness tick, because
the layers below it cannot observe its silence as anything but
continued activity. This is the primitive that lets cosmon host
non-Claude inference backends without re-deriving the freshness
calculus each time.

**Cross-link to ADR-052.** See ADR-052 §D1 last line (added in the
Amendment) and the footer of this ADR — ADR-052 receives a one-line
cross-link: *"See ADR-058 for the 8th clock StepClock."*

### D4 · Mechanism M4 — Two-stage protocol (shannon)

The passive MSS `{S1: seal-age, S2: tmux-mtime}` carries ≈ 1.1 bits
in the non-adversarial regime — enough to separate the binary
hypothesis in most cases. In the adversarial-slow-step regime
(chain-of-thought without tool calls), passive capacity = 0 bits:
the filesystem channel cannot distinguish stall from slow-step.

The two-stage protocol makes measurement *part of* the channel:

```
stage 1 — passive:    evaluate {S1, S2}
  if σ_tape < T_step  → LongRunning (no action)
  if σ_tape > T_step and unambiguous → InferenceStalled (surface ghost)

stage 2 — active probe (entered only on the ambiguity slice):
  emit RepromptProbed event (ADR-052 I8 compliant)
  re-prompt via the M5 primitive
  observe the pane for ≤ 60 s
  emit RepromptProbeResolved{outcome} event
  update witness.process and sigma_tape accordingly
```

**Key property.** Stage 2 is *not* a feature added to the classifier
— it is a **new channel opened** when the passive channel's capacity
is zero. shannon §Part 4:

> *When the channel has capacity zero, you must open a new channel.*

The probe completes the channel by forcing the worker to externalize
a bit. The bit emerges from a distribution the passive signals cannot
sample.

**Compatibility with ADR-052 I2 (SingleWriterPerField).** The probe
is not a worker self-certification of `Presence`. It is an external
action by an *observer* (patrol, pilot, or P3-autonomous daemon) that
writes a `*Probed` event; the worker's response (or silence) is a
second *external* observation. `Presence` remains probe-written, as
required by I2.

**Compatibility with ADR-038 (whisper).** The active-probe primitive
in stage 2 is a specialization of `cs whisper` — the sixth channel,
unbounded pilot→live-worker semantic text, Propelled regime only.
See §D7 for the full specification.

### D5 · Mechanism M5 — C-threshold policy (carnot)

A logistic classifier over the seven ADR-052 clocks (plus `StepClock`)
yields `C(m) = P(Stalled(m) | observable(m))`. Three bands:

| Band | Threshold | Policy |
|---|---|---|
| `C < 0.30` | `C**` | *"probably finishing, leave it alone."* No surface badge. Any noise in this band is free. |
| `0.30 ≤ C < 0.70` | — | **Human gate.** Surface ghost badge in `cs peek` (amber `●`). Operator decides. |
| `C ≥ 0.70` | `C*` | **Auto-fire** — eligible for a future P3-resumable re-prompt (deferred to the `residence` layer, ADR-055). In the current Propelled regime this band falls through to the human gate. |

**Cost justification (carnot §2–§3).**

- Operator attention is the exergy sink: 10 000 triggers/yr × 3 min ≈
  500 h/yr ≈ $50k loaded at $100/h.
- Dollar-cost of false re-prompts is negligible: $100/yr at 20% FPR,
  $500/yr even if all 10 000 triggers misfire.
- Bounding operator load to the `[0.30, 0.70)` band — ~30 % of
  triggers — gives ≈ 150 h/yr, a 3× reduction.
- Break-even C* is numerically 0.70 under the assumed cost curve;
  **per-formula tunable** (different formulas have different stall
  priors — see `carnot §6`).

**Carnot-like energy ceiling.** The seven ADR-052 clocks provide
≈ 13 bits of mutual information about `H = Stalled`, enough for ε ≈
1 % but not ε < 1 %. `StepClock` adds ≈ 3 bits — moving the ceiling
toward ε ≈ 0.1 %. This is the *only* reason the 8th clock is worth
adding; the seven alone are below the thermodynamic edge of
automation.

**Change the boundary, not the mechanism.** Do not tune the
classifier's internal precision. Move `C`; the clocks are already
good enough. *(carnot §7.)*

### D6 · Ledger-written `Resume` action — anti-Sisyphus K-bound

When the P3-resumable layer eventually ships (deferred — see §6
Non-goals), its re-prompt action **must** append an entry to
`sealLog[m]` with `kind = "resume"`:

```tla
Resume(m) ==
  /\ AutoResumeEnabled        \* gated constant, default FALSE
  /\ mol_status[m] \in {"Stalled", "Running"}
  /\ count_resumes_since_progress(m) < K
  /\ sealLog' = [sealLog EXCEPT ![m] =
        Append(@, [step |-> CurStep(m), t |-> now, kind |-> "resume"])]
  /\ mol_status' = [mol_status EXCEPT ![m] = "Running"]
  /\ UNCHANGED <<intent, witness, now>>
```

Two soundness conditions (knuth §7, synthesis §Part 5):

1. **Ledger-written.** `Resume` appends to `sealLog`, preserving I7
   (SingleEventWriter) and I8 (MeasurementEmission). Silent in-memory
   reset of the clock without ledger append creates the **Sisyphus
   trace** — re-prompt ∞, invariant never violated, `Completed` never
   reached. The `kind` tag distinguishes progress-seals from
   restart-seals.

2. **K-bound (anti-Sisyphus).** The guard
   `count_resumes_since_progress(m) < K` ensures a bounded number of
   resume attempts between two progress-seals. `K = 3` is the default.
   After `K` consecutive resumes without progress, the molecule is
   escalated to the human gate (band 0.30 ≤ C < 0.70) regardless of
   current `C`.

Until P3-resumable ships, `AutoResumeEnabled = FALSE` and the Resume
action is not reachable; the M1 invariant remains satisfied
vacuously. The spec is written now to forbid the Sisyphus shape by
construction when the capability lands.

### D7 · The missing re-prompt primitive — symmetrize the channel (galileo)

**Diagnosis.** `cs tackle` writes the briefing into the pane at spawn
time (one user-turn of text). `cs evolve` rewrites `briefing.md` on
disk and appends a `BriefingSealed` event. There is no counterpart
write to the pane. The write-side of the briefing channel has no
reader. Galileo §4:

> *The anomaly is structural, not temporal: the DAG's 1-bit-per-edge
> works; the filesystem data plane works; the propulsion channel is
> not idempotent across steps — it fires once at tackle, not on each
> seal.*

**Resolution.** Symmetrize the channel. The re-prompt primitive is
the `cs evolve` counterpart of the `cs tackle` initial injection —
it sends the *new* briefing into the pane as a new user-turn.
ADR-058 names this primitive. Its specification:

| Property | Value |
|---|---|
| Name | `cs patrol --nudge <mol_id>` (P1-safe, human-invoked). Future `cs resume --nudge` when P3-autonomous lands. |
| Direction | pilot → live-worker (same as `cs whisper`, ADR-038 channel 6). |
| Regime | **Propelled only.** Inert has no pane; Autonomous owns its own re-prompt loop. |
| Payload | Rendered `briefing.md` for the current step (not just `"continue"` — feynman §6 outcome (b) proves bare `continue` is insufficient when context has been lost). |
| Author | Human pilot (P2 manual) today. Future P3-resumable daemon in the `residence` layer (ADR-055). |
| Ledger | Emits `RepromptProbed{mol, at, step, origin, payload_hash}` on dispatch, `RepromptProbeResolved{mol, outcome}` on observation (≤ 60 s window). Both under I7 single-writer discipline. |
| Idempotence | The re-prompt is safe-to-retry within a formula step because `cs evolve` commits before advancing — the replay starts from the last commit (carnot §5 resumable). Outside a formula step, the K-bound applies (§D6). |
| Composes with | ADR-038 `cs whisper` (same channel, different authority in the header). ADR-052 I8 (MeasurementEmission — the probe event precedes the action). |

**Why this is the smallest change that works.** The panel converged:
the primitive missing from cosmon is not a detector (that is M1), not
a classifier (that is M5), not a ghost label (that is M2). The
primitive missing is a *writer on the pilot→worker channel that fires
on each seal*. Galileo's diagnosis is the lever; shannon's
two-stage protocol is the gate that controls when the lever is
pulled; carnot's C-threshold is the policy that decides *who*
pulls it.

### D8 · Cosmological-horizon framing (hawking) — prose

The frontier between α (Claude's turn) and γ (cosmon state) is not
an event horizon. An event horizon is causally forbidden —
information cannot cross it in principle, ever. The α/γ frontier is
**one-way transparent by default**: α sees everything about itself;
γ sees only the subset of α that α chose to externalize. When α's
emission rate drops to zero, the layers *recede from each other*
because their clocks decouple: γ ticks on wall-clock, α ticks on
token-emission.

Cosmological horizons can be closed by changing the expansion rate.
M5 (the re-prompt primitive) re-accelerates α's emission; the
layers re-entangle. The fact that the 2d4e molecule can be revived
by a single `tmux send-keys "continue" Enter` (feynman §6, validated
2026-04-20) is the empirical confirmation that this is not an event
horizon.

This framing is the prose answer to the question *"can cosmon in
principle recover a silent worker?"* — yes, because the horizon is
cosmological; the recovery primitive's name is M5.

### D9 · Inverted Turing test — prose (turing)

The worker passed its own Turing test. Its descriptive transition
phrase *"Now step 2 — evaluate feasibility"* was surface-
indistinguishable from a competent human's continuation. A judge
reading only the pane tape would mark "continuing". The worker did
not lie; it *narrated* itself into a position where the next token
was not forthcoming.

The question *"is it still thinking?"* is too meaningless to answer
from inside. Replace it with one cosmon can test: *"has the tape
moved recently enough given this step's prior?"* That is
`σ_tape > T_step`. Coherent surface form is not evidence of intended
continuation; silence is. ADR-052 I10 (SilenceIsSignal) names the
invariant; ADR-058 M1 names the action that responds to it.

## 3 · What ADR-058 does **not** do

Per the briefing, these refusals are part of the contract — each the
negation of a temptation the panel explicitly examined.

- **Does not resolve the halting problem.** `Stalled` is accepted as
  semi-decidable (co-r.e.). No finite passive prefix can decide it
  (turing §2, Rice-adjacent). Active probing lifts it to
  semi-decidable; certainty is not available.

- **Does not require a daemon in the transactional core.** The M5
  primitive is invoked by `cs patrol --nudge` (an external scheduler
  call on ADR-052 Layer A) in the Propelled regime. The P3-autonomous
  continuous-observer variant lives in the `residence` layer
  (ADR-055). ADR-016 L2 (no daemon in core) is preserved.

- **Does not require worker self-certification of Presence.** The
  heartbeat, if it exists, is an *α-emission of work* (observable
  side-effect of `cs evolve`), never a self-asserted *Presence label*.
  Shannon §6 + synthesis §D1: worker writes to `Presence` would
  violate ADR-052 I2 (SingleWriterPerField) — G1 forbidden
  self-assertion. `MarkStalled` is therefore an *observer* action,
  never a worker action.

- **Does not ship P3-resumable auto-fire.** Deferred to the
  `residence` layer (ADR-055). Until auto-fire lands with the full
  safeguard stack (ledger-written Resume, K-bound, resumable replay,
  empirically calibrated C-threshold), the C ≥ 0.70 band falls
  through to the human gate. P3-naive (no safeguards) is refused by
  the entire panel (synthesis §C3, unanimous — including carnot, who
  advocates auto-fire under safeguards).

- **Does not add a new CLI verb beyond what ADR-052 §D3 already
  renames.** The M5 primitive is surfaced through
  `cs patrol --nudge <mol_id>` (Propelled-regime extension of the
  existing patrol verb, idempotent, human-invoked) — not a new
  top-level verb. The polymer workstream's Phase 1 child ships this
  flag; no new verb is introduced.

- **Does not modify ADR-052.** The Amendment 2026-04-20 in ADR-052
  §D1 (the four-paragraph note) already reserves the vocabulary.
  ADR-052 receives one additional line at the bottom: *"See ADR-058
  for the 8th clock StepClock."* No content in §D1–§D7 of ADR-052 is
  rewritten.

## 4 · Consequences

### 4.1 · Positive

- **Names the seventh ghost.** `GhostKind::InferenceStalled` gives
  every future pane-still-alive, worker-still-alive, briefing-still-
  unconsumed molecule a single recognized label. The fixture 2d4e is
  no longer an untyped drift — it is a named pathology with a
  formally specified detection.
- **Closes the eighth-clock hole in ADR-052.** The Amendment
  2026-04-20 is operationalized; the `RunState` type in
  `cosmon-core` gains a `sigma_tape` field (or sibling `StepClock`
  counter, per the polymer workstream's choice of M3 reading) with
  no impact on existing invariants.
- **Makes the halting-problem boundary honest.** Cosmon no longer
  pretends to decide `Stalled` passively. The two-stage protocol
  (M4) writes the boundary into the code: passive detects unambiguous
  cases, active probing handles the ambiguity slice.
- **Bounds operator attention.** The C-threshold (M5) confines human
  intervention to the 0.30–0.70 band (~30 % of triggers), preserving
  the scarcest exergy in the system. The auto-fire band (C ≥ 0.70)
  waits for P3-resumable but is specified now so the landing is a
  drop-in, not a redesign.
- **Symmetrizes the propulsion channel.** `cs patrol --nudge` gives
  `cs evolve` the pane-write counterpart it always needed. The
  write-side of the briefing channel finally has a reader.
- **Cross-galaxy portability.** Every cosmon-hosted galaxy (mailroom,
  showroom, market-agents, …) inherits the invariant, the ghost
  kind, and the M5 primitive. The syzygie protocol (ADR-047 / the
  cross-galaxy chronicle pattern) will copy this ADR's summary to
  sibling galaxies in their next sync.

### 4.2 · Negative

- **Per-formula `T_step` calibration.** The silence threshold depends
  on formula and step — `idea-to-plan` step 2 has a different
  tool-call cadence than `deep-think` step 5. Calibration requires
  empirical priors. Mitigation: seed from Poisson fits on historical
  `events.jsonl`, refine Bayesianly (Banburismus — each quiet minute
  adds decibans of stall evidence). Default `T_step = 20 min` for
  Propelled Claude workers is the initial seed (turing §3).
- **Formal-spec maintenance cost.** The `I_StepProgress.tla` excerpt
  must be kept in sync with the canonical `docs/specs/CosmonRun.tla`.
  The CLI doc-sync discipline (feedback memory `cli_doc_sync`) is
  extended to formal specs: *any change to the TLA spec's
  StepProgress fragment updates the ADR-058 excerpt in the same PR*.
- **Human gate in the ambiguity band.** Until P3-resumable ships, the
  [0.30, 0.70) band remains 100 % human. At 10 000 triggers/yr × 30 %
  × 3 min this is ≈ 150 h/yr. Mitigation: the `cs peek` amber badge +
  single-keystroke `r` re-prompt path (jobs §) make each intervention
  a sub-30-second operation, not a full 3 min.
- **Two-stage protocol complicates `cs patrol`.** `cs patrol --nudge`
  must emit two events (probe dispatch, probe resolution) and wait
  ≤ 60 s between them. Mitigation: idempotent by design; a second
  invocation within the observation window is a no-op on the
  dispatch side.

### 4.3 · Neutral

- **No change to on-disk schema shape beyond ADR-052 D2.** The
  `Witness` struct gains `sigma_tape: Duration` (additive; preserves
  `#[serde(alias)]` migration discipline). `GhostKind` gains one
  variant (non-exhaustive enum — no breaking change for callers that
  match the existing variants).
- **No new dependency.** TLC already required by ADR-052's formal
  gate; the M1 extension reuses the same toolchain.
- **No mandatory CI change.** The `StepProgress` config is already
  validated alongside the other six ADR-052 configs in
  `docs/specs/VALIDATION-REPORT.md`. ADR-058 adds no new gate.

## 5 · Decomposition — phase-1 children (informative)

The implementation children are tracked outside this ADR in sibling
task molecules nucleated under the parent delib `delib-20260420-1b02`.
ADR-058 does not prescribe their order; the briefing names two
explicit non-ADR workstreams:

- `task-20260420-ec4b` — Rust-side `GhostKind::InferenceStalled`
  extension in `cosmon-core` + regression test against fixture
  `idea-20260419-2d4e` preserved state snapshot.
- `task-20260420-2814` — `cs patrol --nudge <mol_id>` implementation
  in `cosmon-cli`, composing ADR-038's whisper primitive; emits the
  `RepromptProbed` / `RepromptProbeResolved` events under I7.

This ADR is the formal specification that both workstreams reference.

## 6 · Non-goals

- **P3-autonomous auto-resume.** Deferred to ADR-055's residence
  layer once empirical C-threshold data is available.
- **Worker-emitted heartbeat as self-attestation of Presence.**
  Refused — violates ADR-052 I2 (shannon §6). The observable form of
  work emission (tool call, `cs evolve` commit, briefing seal) is
  the already-allowed α-emission and needs no new primitive.
- **Auto-tuning of `T_step`.** Seeded by Poisson priors; refined
  Bayesianly; not adaptive in v0.
- **A daemon in the transactional core.** Refused per ADR-016 L2.
- **Cross-galaxy chunk of the Resume action.** Deferred: `Resume`
  is always galaxy-local in v0.
- **A user-facing explanation of the Gödel / halting boundary.**
  Stays in the synthesis and in this ADR. The operator-facing
  surface is the amber `●` badge, the single keystroke `r`, and the
  one-sentence *"inference momentum lost — nudge or collapse?"*
  prompt.

## 7 · Mechanical validation status

The `I_StepProgress` property is checked mechanically in
[`docs/specs/CosmonRun_StepProgress.cfg`](../specs/CosmonRun_StepProgress.cfg)
against the existing `docs/specs/CosmonRun.tla` (extended
2026-04-20 by `task-20260420-ea09`). The validation run is recorded
in `docs/specs/VALIDATION-REPORT.md` as Model 7 — StepProgress.

The check confirmed: under `T_STALL = 3`, `MaxClock = 6`, one
molecule, WF on `MarkStalled` and `Tick`, the property holds and the
stepwise safety invariants I6, I7, I9 are preserved. Removing WF on
`MarkStalled` produces the expected starvation counter-example —
ADR-052 I5 shape (knuth §4).

ADR-058's formal content is therefore **TLC-clean**. The spec may
still be wrong (knuth's closing caveat: *"beware of bugs in the
above specification; I have only proved it correct, not run TLC"* —
quoted in the excerpt file). ADR-058 claims mechanical validation,
not deductive proof of correctness for the full cosmon system;
`P_external` (ADR-032) forbids the latter.

## 8 · References

- **Governing deliberation.**
  `delib-20260420-1b02`
  synthesis (9-persona panel: galileo, turing, knuth, shannon,
  hawking, torvalds, jobs, feynman, carnot). Per-persona responses:
  `responses/galileo.md`,
  `responses/turing.md`,
  `responses/knuth.md`,
  `responses/shannon.md`,
  `responses/hawking.md`,
  `responses/torvalds.md`,
  `responses/jobs.md`,
  `responses/feynman.md`,
  `responses/carnot.md`.
- **Fixture artifact.** `idea-20260419-2d4e` in
  `/srv/cosmon/market-agents` — preserved as an empirical test
  vector. Do not collapse. State snapshot at
  `.cosmon/state/fleets/default/molecules/idea-20260419-2d4e/` in
  that galaxy.
- **TLA+ fragments.**
  [`docs/specs/CosmonRun.tla`](../specs/CosmonRun.tla) (canonical,
  extended 2026-04-20).
  [`docs/specs/CosmonRun_StepProgress.cfg`](../specs/CosmonRun_StepProgress.cfg)
  (Model 7).
  [`058-step-progress-invariant/I_StepProgress.tla`](058-step-progress-invariant/I_StepProgress.tla)
  (ADR-reviewer excerpt).
- **Cosmon ADRs referenced.**
  [ADR-016](016-autonomy-regimes-and-resident-runtime.md) (regime
  vocabulary),
  [ADR-032](032-p-external-witness-axiom.md) (P_external — no
  system certifies itself),
  [ADR-038](038-whisper-perturbation-port.md) (whisper / sixth
  channel — the channel M5 writes on),
  [ADR-046](046-p-legibility-axiom.md) (P_legibility — the
  operator-facing badge grammar),
  [ADR-047](047-event-log-protocol-v0.md) (event log protocol —
  where `RepromptProbed` events land),
  [ADR-052](052-one-ledger-one-writer-one-witness.md) (the
  ten invariants; §D1 Amendment 2026-04-20 reserves the vocabulary
  this ADR implements),
  [ADR-055](055-cosmon-residence.md) (residence — where the future
  P3-autonomous re-prompt daemon will live),
  [ADR-056](056-notary-protocol-v0.md) (notary — seal-chain
  compatibility for the `kind: "resume"` tag),
  [ADR-057](057-genre-and-artifact-map.md) (genre — this ADR's
  formal-spec excerpt is classified as genre `formal-spec`).
- **Architectural invariants.**
  [`docs/architectural-invariants.md`](../architectural-invariants.md)
  §1 (no daemon in core — §D2 of this ADR cites it),
  §7e (DAG carries 1 bit, filesystem carries content — the
  diagnosis in §1.3 cites it),
  §8b (propose mechanisms of verification, do not impose them —
  the TLA+ approach in §2 D1 and §7 cites it).
