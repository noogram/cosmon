# ADR-038 — `cs whisper` as a perturbation port (the 6th channel)

**Status:** Accepted (2026-04-14)
**Scope:** `cs whisper` CLI verb, pilot→worker semantic injection, communication
taxonomy.
**Parent:** deliberation
`delib-20260414-b8e2`
(11-persona panel: wheeler, einstein, torvalds, tolnay, turing, feynman, jobs,
godel, hawking, jr, godin).
**Binds:**
[ADR-016](016-autonomy-regimes-and-resident-runtime.md) (regime boundaries —
the primary structural constraint this ADR invokes),
[ADR-003](003-multi-channel-nervous-tissue.md) (channel taxonomy).

## Context

A long-running deliberation generates field observations *after* the worker
has started: a related incident lands in another repo, a nearby molecule
collapses with a finding, the pilot reads a synthesis mid-flight and wants
to inject a caveat. The existing five channels do not cover this.

| # | Channel | Direction | Payload | Authority |
|---|---------|-----------|---------|-----------|
| 1 | neurion | query R/W | structured SQL | registry truth |
| 2 | DAG | molecule→molecule | ~1 bit (done/not) | authoritative ordering |
| 3 | filesystem | broadcast | files | authoritative content |
| 4 | artifact chain | sequential | markdown files | proof of work |
| 5 | propulsion | pilot→worker | 0 bytes (wake) | no semantic content |

To re-nucleate is too expensive (context loss, re-planning). To wait for the
worker to finish and then feed the observation to a downstream molecule
defeats the reason the pilot noticed *now*. The gap is a **pilot→live-worker
semantic channel**.

On 2026-04-14, the capability was exercised empirically — before any code
change — via raw `tmux load-buffer` + `paste-buffer` + `send-keys Enter Enter`
into the deliberation's worker pane. The injection arrived in the worker's
context window and visibly shaped the synthesis. The channel existed
mechanically; it lacked a name, an API, and a regime scope.

This ADR formalizes, names, and scopes it. Implementation shipped in commit
`de2579d` (`feat(cli): implement cs whisper v0 — perturbation port`).

## Decision

Cosmon ships `cs whisper` as a **6th communication channel** — a
**perturbation port**, not a peer of the DAG or the filesystem.

### Scope and authority

| Axis | Value |
|------|-------|
| Direction | **pilot (human) → worker (live)** — unidirectional |
| Payload | unbounded natural-language text |
| Substrate | tmux `load-buffer` + `paste-buffer` + Enter×2 into the worker pane |
| Authority | **advisory only** — cannot abort, cannot modify `state.json`, cannot modify the DAG |
| Regime | **Propelled only** — forbidden in **Autonomous** (ADR-016 Phase 3+), meaningless in **Inert** |
| Caller | **Human pilot only for `cs whisper`.** Workers use the separately authorized ADR-182 advisory contract for peer evidence; they do not gain permission to invoke this steering command. |
| Delivery | Sender-side submission is observable; context delivery and recipient consumption require distinct evidence. This command does not currently establish those stronger facts. Semantic understanding is not implied by a receipt. |
| Target check | `pane_current_command == "claude"` — **fail-closed**, no `--force` escape in v0 |

ADR-182 proposes a separate, provider-neutral advisory evidence contract. It does not expand `cs whisper` into a peer control command. Messages cannot retask, interrupt or integrate a peer's work. The existing command's caller and regime restrictions remain in force unless separately amended.

### Persistence

- **Payload**: `.cosmon/state/fleets/<fleet>/molecules/<mol>/whispers/<ts>-<sha16>.txt`
  — the full text on disk, content-addressed by SHA-16 prefix for dedup.
- **Fact-log**: `.cosmon/state/fleets/<fleet>/molecules/<mol>/whispers.jsonl`
  — one line per whisper: `{ts, sender, payload_sha, byte_len}`. No body.
- **`events.jsonl` is NOT written.** Whispers are perturbations, not events.
  The control plane stays one-bit per molecule lifetime.

### Ontology

A whisper is a **speech act**, not a state transition. It does not advance
the molecule's lifecycle, does not appear in the typed-link graph, and does
not trigger `cs reconcile`. It is a signal on a sixth, separate plane:

```
┌─────────────┐        ┌───────────────────────┐
│ pilot (human) │── 6 ─▶│ live worker (Claude CLI) │
└─────────────┘  whisper└───────────────────────┘
                         ▲                 │
                         │ 5 propulsion    │ 3 filesystem
                         │                 ▼
                     ┌───────────────────────┐
                     │ .cosmon/ on disk       │
                     └───────────────────────┘
```

Channel 5 (propulsion) carries **0 bytes of payload** — a wake-up pulse.
Channel 6 (whisper) carries **unbounded semantic text** — a perturbation
of the oracle's context. The asymmetry is the headline, not a bug.

## Consequences

**Positive.**
- The pilot gains a co-creator affordance: mid-flight course correction
  without re-nucleation.
- The DAG is explicitly acknowledged as **incomplete-by-design** for
  pilot→worker semantic communication. This is a Gödel sentence: the DAG
  cannot express everything the operator needs to express to a live agent.
- Transport reuse: `cosmon-transport::send_input` handles tackle, propel,
  and whisper behind one abstraction. Cross-wiring fixes propagate for free.

**Negative / accepted.**
- **Idempotence is violated.** Speech acts are not idempotent; two identical
  whispers can produce different outcomes. This is a regime boundary signal
  — whisper lives outside the "twice = once" invariant that governs state
  transitions (einstein).
- **Delivery observations are limited.** Storage admission, transport submission, insertion into model input and an explicit recipient consumption report are separately observable protocol events when the relevant adapter supports them. No general receipt proves understanding or changed reasoning. Record only the stage actually observed; terminal paste alone is not proof of context delivery.
- **Attribution cost for verifiers.** A future verifier that compares a
  molecule's `synthesis.md` against its `prompt.md` will detect drift but
  cannot, from `events.jsonl` alone, attribute that drift to a whisper. The
  verifier must also read `whispers.jsonl`. ADR-037 (lineage conservation)
  applies: whispers are an additional lineage source the verifier must
  enumerate (godel).

**Structural.**
- The five-channel taxonomy is upgraded to six. Future ADRs and the
  handbook use the six-row table.
- `cs whisper` is Propelled-regime-scoped. When the resident runtime
  (ADR-016 Phase 3+) owns dispatch, the runtime MUST NOT use whisper as
  a scheduling primitive — autonomous re-propulsion goes through channel 5.

## Alternatives considered

- **Wheeler — `cs append-evidence`.** Reframe whisper as a late-binding
  write to channel 3 (filesystem) with a one-bit propulsion wake-up. Rejected
  on the turing/einstein counterargument: the injection arrives in the
  worker's context window *without* the worker choosing to re-read anything.
  The worker has no sovereignty over when it enters. Filesystem writes are
  read when the reader decides; whispers are heard when the sender decides.
  Semantically different channels.
- **Torvalds — 20-line shell-script v0.** Empirically sufficient, but misses
  the transport-reuse benefit and forces a shell-injection mitigation layer
  outside Rust's type system. Superseded by the binary v0 (commit `de2579d`).
- **Tolnay — `cs dev whisper` (unstable).** Compromise: ship under
  `cs whisper` (visible) with a first-use warning until v1 is declared stable.
  The incident that motivated the ADR also demonstrated the capability; hiding
  it creates silent nondeterminism (torvalds, jobs).
- **Wheeler/Feynman — typed event in `events.jsonl`.** Rejected: would
  pollute the control plane with speech-act payloads and couple the one-bit
  ordering channel to unbounded natural-language perturbations. The
  `whispers.jsonl` reflog preserves audit without that coupling.

## Invariants

**Preserved.**
- **Control via DAG.** Untouched — no whisper writes to the typed-link graph.
- **Data via filesystem.** Preserved — whisper payloads live on disk under
  the molecule directory, reachable by the same walk-up discovery as any
  other artifact.
- **Stateless CLI.** `cs whisper` is one-shot: validate → paste → log → exit.
- **Worker/pilot boundary.** `cs whisper` remains pilot-to-worker steering. Declared peer evidence exchange follows ADR-182 and does not reuse whisper as an implicit worker permission. Caller role, shared uid and process shape do not establish authorization; any claimed denial requires an effect-boundary check.

**Explicitly violated (by design).**
- **Idempotence.** Speech acts are not idempotent. This is documented, not
  hidden; it is the signal that whisper is a regime boundary channel, not
  a state transition (einstein).

## References

- Synthesis: `.cosmon/state/fleets/default/molecules/delib-20260414-b8e2/synthesis.md`
- godel response (incompleteness framing): `.../delib-20260414-b8e2/responses/godel.md`
- godin response (story + chronicle seed): `.../delib-20260414-b8e2/responses/godin.md`
- Chronicle: an internal chronicle
- Implementation: commit `de2579d` — `crates/cosmon-cli/src/cmd/whisper.rs`
- Regime boundaries: [ADR-016](016-autonomy-regimes-and-resident-runtime.md)
