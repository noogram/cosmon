# ADR-122 — Auto-resume on Anthropic API stream-timeout in `cs patrol --propel`

**Status:** proposed
**Date:** 2026-06-05
**Decider:** Noogram
**Authoring task:** `task-20260605-b5fa`
**Source deliberation:** `delib-20260605-00db` (`deep-think` panel:
torvalds · tolnay · adversary · architect · feynman)
**Origin idea:** `idea-20260510-fde1` (idea.md + feasibility.md).

**Binds / cites:**
[ADR-016](016-autonomy-regimes-and-resident-runtime.md) (Inert / Propelled /
Autonomous regimes — this feature is **Propelled-only** and does **not** cross
into Autonomous),
[ADR-038](038-whisper-perturbation-port.md) (the propulsion/whisper channel
doctrine — owes a one-line footnote, §Consequences below),
[ADR-095](095-resident-runtime-ifbdd-path.md) (Resident Runtime — the future
L3 consumer of the pure sensor this ADR mandates).

**Downstream (blocked-by this ADR):** `task-20260605-043e` — the
implementation task. Its contract is this record; the ADR lands first
(merge-before-dispatch).

---

## Context

A Claude-Code worker spawned by `cs tackle` runs inside tmux. When the
Anthropic API stream times out, the pane prints a banner of the shape
`Stream idle timeout - partial response received` (also seen as the
`⎿  API Error … Stream …` family) and the worker **survives the process** but
**freezes at an empty `❯` prompt**: it has emitted no further tokens and is
waiting for input that will never come on its own. `cs patrol --propel`
observes the molecule as stale, but the existing `propel_stale_molecules`
path (`crates/cosmon-cli/src/cmd/patrol.rs:1104`) is **signature-gated** — it
refuses to send any keystroke into a pane whose foreground signature does not
match — so nothing recovers the worker. The only thing that works is a human
typing `continue ⏎`, i.e. `tmux send-keys 'continue' C-m`.

### The single finding that reorders the whole design

The feasibility study's safety argument rested on one sentence:

> *"require the marker **and** time-staleness … a thinking worker advances its
> clock, a wedged one does not."*

**This sentence was refuted by code inspection during the panel.** The
molecule's `updated_at` is stamped **only** at `cs evolve` step boundaries and
terminal transitions
(`crates/cosmon-cli/src/cmd/evolve.rs:542/857/898/928`,
`crates/cosmon-cli/src/cmd/patrol.rs:1271`) — **never mid-step**. A healthy
worker thinking for 25 minutes inside a single step is *time-stale by the same
clock* as a wedge. Therefore:

> **The time-staleness clause is NOT a safety guard.** It is, at best, a cheap
> cost pre-filter that narrows the candidate set before the expensive checks
> run.

The real discriminator is **pane byte-quiescence**. The Claude-Code TUI
repaints a spinner + token counter roughly every second while the worker is
alive, so a live worker is **never byte-static**, while a wedged one is frozen
indefinitely. Two pane captures a few seconds apart are byte-identical *only*
when the worker is genuinely stuck. This single fact is the spine of every
decision below: **quiescence, not the clock, carries safety.**

This also explains the worst false-positive class the feasibility table
missed: the detector's own design documents (idea.md, feasibility.md,
patrol.rs) **render the marker strings as content**. A worker authoring text
about the banner is long-running *and* shows the marker — exactly the
population a naïve `marker AND time-stale` predicate would mis-fire on. Only
position-bounding (the marker must be in the bottom region), exact-string
matching, idle-prompt shape, and motion (quiescence) separate the author from
the wedge.

---

## Decision

Auto-resume ships as a **pane-quiescence-guarded re-engagement gesture** folded
into `cs patrol --propel`. Each resolved choice below corresponds to one frame
question (Q1–Q5) of `delib-20260605-00db`.

### Q1 — Surface: config key inside `--propel`, no new CLI flag

Auto-resume is **not** a new command and **not** a new CLI flag. It is a
`[propel]` config key (e.g. `auto_resume`). A separate `--auto-resume` flag
would mint a **duplicate perimeter for an identical role** (`cs patrol` already
owns Propelled-regime maintenance) and a flag is a **permanent userspace
contract**; a config default is a one-line tunable. The config toggle **is**
the symmetric kill-switch (coherence-checklist §5). The `[propel]` section is
purely additive — `ProjectConfig::parse` has no `deny_unknown_fields` — so it
is a non-breaking surface change.

**Default value is conditional:**

- Default **ON iff** the quiescence-guarded predicate (Q3a, all of C1–C6)
  ships **and** the detector emits a per-check event (Q4a).
- Otherwise (e.g. a first cut that descopes quiescence) default **OFF**.

Rationale: a content keystroke into a *healthy* worker corrupts its turn, so
default-on is safe **only** if the predicate is actually safe **and** the
behaviour is loud. Flipping a permissive default-on with an unsafe predicate
would silently make every existing `cs run` user's patrol type keystrokes into
panes on a string grep — a behavioural breaking change. You can loosen a
conservative default later; you can rarely tighten a permissive one without
breaking someone.

### Q2 — Cue: `const RESUME_CUE = "continue"`, never configurable

The resume cue is a code constant living next to `PROPULSION_NUDGE`
(`crates/cosmon-cli/src/cmd/patrol.rs:1049`):

```rust
const RESUME_CUE: &str = "continue";
```

It is **never configurable**. `continue` is the only token a human has ever
verified to recover this stall; the worker's full context is still in its
buffer, so the gesture is "press *resume* on a machine that already knows what
it was doing." A configurable or richer cue is the single move that *would*
widen the propulsion channel toward unbounded semantics — and the richer,
unverified cue is exactly what `PROPULSION_NUDGE` already is, and it did **not**
recover the stall.

### Q3a — Predicate: the conjunction C1–C6, with quiescence as the guard

Auto-resume fires **only** when all six conditions hold (adversary's defended
predicate):

| | Condition | Role |
|---|-----------|------|
| **C1** | `now − updated_at ≥ S` (time-stale) | **cheap cost pre-filter — NOT the safety claim** |
| **C2** | the marker appears in the last ~K non-empty lines of the **visible** pane | position-bound (defeats the doc-author false positive) |
| **C3** | the marker matches the **exact banner string** only (not a loose substring) | precision (defeats the log/error reader) |
| **C4** | the bottom region is an empty idle `❯` input box — **not** a `y/n` or numbered-menu prompt | **menu-shape veto** (`continue` must never *select a default action*) |
| **C5** | **two pane captures Q seconds apart are byte-identical** | **pane quiescence — THE safety guard** |
| **C6** | idempotence on a dedicated `last_resumed_at` / `resume_count` | no double-fire |

**Record explicitly:** quiescence (C5), not the clock (C1), is what makes the
gesture safe. C4 (the interactive-menu veto) is **not optional polish** — it is
a distinct false-positive class where `continue` could confirm an unintended
default. The send mechanism inherits the guards: `send_input`'s `C-u`
clear-line is safe **only because** C4 + C5 guarantee nothing is mid-compose.

### Q3b / Q3c — Numbers (defaults, tunable)

| Symbol | Default | Meaning / constraint |
|--------|---------|----------------------|
| **S** | `600 s` | staleness pre-filter (C1). Cost pre-filter, not safety. The implementer **may** align it to the existing nudge `stall_timeout_minutes` budget but **must state in code which clock is used** (note: the nudge path keys on `last_progress_at`, not `updated_at`). |
| **K** | `8` non-empty bottom lines | scan window (C2), **reusing `input_still_pending`'s existing constant** — one bottom-zone convention, not two. |
| **Q** | `10 s` | quiescence interval (C5). |
| **W** | `60 s` | idempotence window (C6). |

**Non-negotiable:** capture the **visible pane** (`tmux capture-pane -p`),
**never** the `-S -` scrollback that `capture_output`
(`crates/cosmon-transport/src/tmux.rs:403`) uses — depth alone does not protect
you if the capture includes recovered history.

### Q4a — Markers: config-list, exact banner default, per-check event mandatory

Markers live in a config-list `stream_timeout_markers: Vec<String>`, reusing
the `WhisperConfig.allowed_commands` shape. The in-binary default ships the
**exact banner only**. **`API Error` is dropped** from the default set as
over-inclusive — it appears in healthy output (panics, stack traces, prose
*about* errors). Markers are **data** (config), not **code** (enum): Anthropic
owns the wording.

The **per-check event is non-optional** — the detector emits an event on
**every** check, both when it fires and when it does not. The load-bearing
hazard is a silent **false negative**: if Anthropic rewords the banner, a
silent detector goes quiet and a blind fleet is indistinguishable from a
healthy one. The loud per-check event is the same mechanism that makes
default-ON safe (Q1).

### Q4b — jsonl: pane-only live signal, session-jsonl test-oracle-only

v0 reads **only the pane** as a live signal. The Claude session jsonl is used
**only as a test oracle**, never as a live input. Reading the session jsonl as
a *live* signal is an **anti-corruption-layer violation** (patrol reaching past
the transport port into adapter-private, unversioned vendor state) and is "the
same bit twice" — the pane and the jsonl are two serializations of one
underlying event, not two independent measurements.

The broad-marker live-jsonl guard is a **deferred, separately-ADR'd escape
hatch** that **does not exist in v0** — the default marker set drops broad
markers anyway, so the broad-marker path is not reachable in v0. If a future
operator enables broad markers, *that* is when the live-jsonl question must be
re-decided, and because it crosses the layer boundary it is ADR-grade.

### Q5 — Doctrine: NO-BREACH of the propulsion-channel regime, with a bright line

Auto-resume is **NO-BREACH** of the ADR-016 regime model and the ADR-038
advisory / Propelled-only channel doctrine.

- **Payload is a red herring.** `continue` (one token) is *strictly more
  faithful* to the 0-byte channel ideal than today's `PROPULSION_NUDGE`
  paragraph. It is a **sharper stall sensor** feeding the **same fixed
  propulsion force** along the molecule's **unchanged** trajectory.
- **It is not Autonomous.** Per [ADR-016](016-autonomy-regimes-and-resident-runtime.md)
  §2, the Autonomous regime requires an **internal clock** *and* a
  **deliberation function** computing *which* of several next steps to take.
  This feature has neither: it reads the pane only to sharpen a **binary**
  stall/not-stall classification, then applies one fixed cue.

**Bright line (must hold in every future change):** the feature **breaches**
the instant it becomes a **marker → action dispatch table** — several markers
selecting among several recovery actions. *That* version computes *which*
action, which is L3 Resident-Runtime **policy**, not patrol, and demands a
**successor ADR**. v0's single-class → single-cue shape is the entire reason it
stays inside Propelled.

**Forward-compatibility requirement:** the detector must ship as a **pure
sensor function**, separated from the action and from the patrol loop, so that
the future Resident Runtime (ADR-095) can reuse it as a policy input with **no
migration debt**. [ADR-038](038-whisper-perturbation-port.md) already reserves
"autonomous re-propulsion through channel 5" as the legitimate path; this ADR
builds the sensor that path will consume.

---

## Implementation structure (binding constraint, not code)

The new detector **must reuse the `nudge_stalled_molecules` skeleton**
(`crates/cosmon-cli/src/cmd/patrol.rs:1239`) — it is **not** signature-gated
and already carries the idempotence pattern (check → send → stamp → persist).
It must **not** be built behind the signature-gated `propel_stale_molecules`
path (`crates/cosmon-cli/src/cmd/patrol.rs:1104`): **the signature gate is the
bug** that blocks recovery; do not build the new behaviour behind it.

Idempotence uses **dedicated** `last_resumed_at` / `resume_count` fields,
sharing the idempotence *helper* (`within_idempotence(last, now, window)`) but
**not** the storage with the nudge path. A shared field would let a nudge write
reset the resume clock (and vice versa), allowing either classifier to fire too
soon when both target the same worker in the same window.

---

## What this ADR does *not* do

- It does **not** write code. It is a decision record; `task-20260605-043e`
  implements it.
- It does **not** introduce a new CLI flag, command, daemon, or message
  channel.
- It does **not** add the broad-marker live-jsonl guard — that is a deferred,
  separately-ADR'd escape hatch absent from v0.
- It does **not** authorise a marker→action dispatch table — that is the bright
  line and would require a successor ADR (L3 policy).

---

## Consequences

- `cs patrol --propel` recovers a stream-timeout-wedged Claude-Code worker
  automatically, closing the gap where a survived-but-frozen worker stalls the
  pipeline indefinitely.
- The recovery is **safe by quiescence, not by the clock** — the design is
  immune to the "25-minute legitimate thinker" false positive that a
  time-only predicate would mis-fire on, and to the self-referential
  doc-author false positive.
- The behaviour is **loud**: a per-check event fires on every check, so marker
  drift (a silent false negative) surfaces instead of degrading the detector
  silently.
- The detector is a **pure sensor**, so the Resident Runtime (ADR-095) inherits
  it as a policy input with no migration debt.
- A genuine assumption is pinned for the implementer: *an alive worker
  repaints within Q seconds*. A test fixture must capture a real "thinking"
  pane twice and assert the bytes differ; if that ever fails, C5 is invalid
  and the predicate must be revisited.
- **Owed to [ADR-038](038-whisper-perturbation-port.md):** a one-line footnote
  recording that *channel 5 carries 0 bytes of operator-authored payload; the
  watchdog may emit fixed, code-owned re-engagement gestures (nudge,
  resume-cue).* This footnote is added by the implementation task in the same
  channel-doctrine spirit.

## References

- `delib-20260605-00db` — synthesis.md (full per-persona reasoning and the
  D1–D6 divergence resolutions behind each decision above).
- [ADR-016](016-autonomy-regimes-and-resident-runtime.md) §2 — the
  Inert / Propelled / Autonomous boundary this feature stays inside.
- [ADR-038](038-whisper-perturbation-port.md) — the propulsion/whisper channel
  doctrine and the reserved "autonomous re-propulsion through channel 5" path.
- [ADR-095](095-resident-runtime-ifbdd-path.md) — the Resident Runtime, the
  future L3 consumer of the pure sensor function.
- `crates/cosmon-cli/src/cmd/patrol.rs:1049` (`PROPULSION_NUDGE`), `:1104`
  (signature-gated `propel_stale_molecules` — the bug), `:1239`
  (`nudge_stalled_molecules` — the skeleton to reuse), `:1271`.
- `crates/cosmon-cli/src/cmd/evolve.rs:542/857/898/928` — the only `updated_at`
  stamp sites (the code fact that refuted the time-stale safety claim).
- `crates/cosmon-transport/src/tmux.rs:403` (`capture_output` — uses `-S -`
  scrollback; the detector must **not**).
