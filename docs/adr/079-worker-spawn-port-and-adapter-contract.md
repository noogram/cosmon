# ADR-079 — Worker-Spawn Port and Adapter Contract

**Status:** proposed
**Date:** 2026-04-26
**Decider:** Noogram
**Parent deliberation:** `delib-20260426-6b52`
("Onboarder d'autres harnesses agent dans cosmon" — 5-persona panel:
wheeler, torvalds, karpathy, feynman, niel). Wheeler vocabulary triage
in §1, reframed question in §2, T3 recommendation at end of
`responses/wheeler.md`.
**Authoring task:** `task-20260426-bb95` (Path-1 child #1 of the parent
deliberation).

**Binds:**
[ADR-038](038-whisper-perturbation-port.md) (the **Port** vocabulary this
ADR re-uses verbatim),
[ADR-072](072-session-route-formula-and-sidecar-invariants.md) (the
**Tier** vocabulary, scoped to routing — preserved here, *not* migrated),
[ADR-016](016-autonomy-regimes-and-resident-runtime.md) (the
*Resident Runtime* — the only legitimate user of the word `runtime`),
[ADR-075](075-oracle-boundary-cs-tackle.md) (the envelope every
worker-spawn inherits at `cs tackle` time).

**Architectural invariants:** `docs/architectural-invariants.md` §8j
(ingress bindings — every Port is one), §8k (UX surfaces are a Port
adapter family).

---

## Context

The deliberation `delib-20260426-6b52` opened with the question
"comment onboarder d'autres harnesses agent ?" Wheeler's first move
(§1, vocabulary triage) was to refuse the question on its own terms:
*harness* is not a primitive of cosmon. It is a colloquial role-word.
Drawing a trait, running an experiment, or shipping a wrapper before
fixing this would smuggle the wrong primitive into every later
artifact (the trait would be `WorkerAdapter` — a pleonasm; the
experiment would benchmark "harnesses" — an undefined unit; the
wrapper would be a "harness multiplex" — a metaphor stacked on a
metaphor).

The synthesis (§C2 — *routing cascade ≠ worker cascade*; §D1 — *what
is Step 1?*; §T3 — *vocabulary commit now, regardless of branch*)
recorded panel-unanimous agreement that this ADR is the cheapest
unblock available. It costs nothing, it fixes nothing in code, and it
prevents every later deliberation on this topic from re-litigating
the basic words.

This ADR is one page. It contains no code, draws no trait, names no
second harness, prescribes no benchmark.

## Decision

Cosmon commits **{ Worker · Port · Adapter · Tier }** as the
load-bearing four-word vocabulary for the worker-spawn boundary.
Everything else is prose.

### 1. Committed vocabulary (the four primitives)

| Word | Definition | Origin |
|------|-----------|--------|
| **Worker** | The molecule's executor — a typestate already in `MoleculeData` (`WorkerId`). One molecule, one Worker, one worktree, one termination. | pre-existing in `cosmon-core` |
| **Port** | The typed perimeter through which a class of perturbation crosses the cosmon boundary. The worker-spawn path is **a Port**; whisper is **another Port**; ingestion is **another Port**. A Port is named, scoped, and authorised. | [ADR-038](038-whisper-perturbation-port.md) |
| **Adapter** | The concrete realisation of a Port for one external substrate. `claude.rs` is **an Adapter** (the Claude Code CLI). Future Codex / DeepSeek / Aider would be other Adapters of the same Port. | [ADR-038](038-whisper-perturbation-port.md) |
| **Tier** | A graded fallback layer in a *speculative cascade*. Stays scoped to its existing perimeter (routing — ADR-072 §4-tier output; ADR-078 §4-tier input). May be re-used **inside** a single Adapter (an Adapter that itself cascades providers internally). **Does not migrate into the worker conversation.** | [ADR-072](072-session-route-formula-and-sidecar-invariants.md) |

### 2. Demoted to prose roles (allowed in prose; banned in code, trait names, crate names, ADR titles)

- **`harness`** — colloquial for "the thing wrapping the model". Use *Adapter* in code; *harness* may appear in chronicles, briefings, and operator-facing prose to name the substrate (e.g. "the Claude Code harness" = "the Claude Adapter and its TUI conventions").
- **`agent`** — overloaded across the industry. Allowed in prose ("the agent reads `briefing.md`") but never in a type, trait, file, or crate name within the worker-spawn perimeter.
- **`runtime`** — reserved for the *Resident Runtime* (ADR-016 Phase 3+). A Worker is not a runtime; an Adapter is not a runtime. The word `runtime` in cosmon code refers to one specific future component, full stop.

### 3. Retired words (do not use, in code or in prose)

- **`engine`**, **`driver`**, **`shell`** — generic mechanism nouns; replace by *Adapter* (or by the precise verb: *spawn*, *observe*, *terminate*).
- **`harness-multiplex`**, **`model-multiplex`** — metaphors stacked on a metaphor. The thing they were trying to name is *a Port with multiple Adapters*.
- **`WorkerAdapter`** — pleonasm: an Adapter for the worker-spawn Port is just *the Adapter*. The trait, **if** ever drawn, is `Spawn` or `WorkerSpawnPort`. It is not drawn by this ADR.

### 4. The Port — typed signature (no Rust code)

The **worker-spawn Port** is the only place in cosmon where a process
is spawned to advance a molecule. Its typed signature, at the conceptual
level:

- **Inputs.** `MoleculeId`; the worktree path (a `Path`, post-`cs tackle`);
  the path to the sealed `briefing.md` (BLAKE3 in `MoleculeData.briefing_seals`);
  the ADR-075 envelope (oracle clearance / permission handle / quota
  capability) inherited from `cs tackle`.
- **Outputs.** A *worker handle* that can be (a) **observed** — its
  liveness queried — and (b) **terminated** — its process tree released.
  No further capability is required of a handle by the Port itself.
- **Authority.** The Port is the **only** sanctioned route for a process
  to begin advancing a molecule. Anything else is a structural breach
  (file a bead). The Port carries the ADR-075 envelope through and **does
  not synthesise authority of its own**.

### 5. The Adapter contract — what every Adapter must do

Per the parent synthesis §C3 (panel-unanimous), every Adapter of the
worker-spawn Port realises exactly four obligations:

1. **Reads `briefing.md`** as its task input. (No alternative input
   format. The seal in `MoleculeData.briefing_seals` is the verifiable
   contract.)
2. **Has a writable `MOLECULE_DIR`** as its workspace. (Any artifact —
   markdown, JSON, screenshots — lands here.)
3. **Runs in the molecule's worktree with `cs` on PATH** as its `cwd`.
   (Walk-up discovery from the worktree is how the Worker calls
   `cs evolve` / `cs complete`.)
4. **Eventually terminates.** (Idempotent termination — calling
   *terminate* twice is calling it once.)

Anything else — `permission_mode`, TUI status enums
(`Loading/TrustPrompt/Ready/Working/Blocked`), JSONL parsers, whisper
scroll mechanics, token-accounting probes (`claudion`-style) — is
**per-Adapter / per-harness sibling concern**, **not** part of the Port
contract, and **not** an obligation any other Adapter is bound to honour
(synthesis §C4).

### 5a. Delivery integrity — formula-owned behavioural guards

The sealed `briefing.md` binds a Worker to faithful delivery and preserves
the ward signal across Adapter substitution. An Adapter MUST deliver the
sealed formula/briefing instruction without elision, weakening, or hidden
override. The Adapter is **never** the semantic judge of the requested work:
permitted behavioural variance belongs only to explicit boundaries written by
the Formula in the relevant step.

A behavioural guard is visible, step-owned, and identified. It states five
portable clauses: (1) positive mode/scope, (2) forbidden actions, (3) bounded
output/artifact, (4) confidentiality, and (5) a no-silent-deviation rule.
When a guard is observed breached or cannot be met, the Worker records a ward
event containing `formula`, `version`, `step`, `adapter`, `model`, `guard_id`,
`observed_action`, `confidentiality_impact`, and the briefing `seal` or trace.
The record makes deviation attributable without moving policy into an Adapter
or creating a new extension point.

### 6. §-leaks in propulsion and whisper — the Adapter-level fix

ADR-038 §5 (propulsion) and §6 (whisper target check) both gate on
`pane_current_command == "claude"`. This hard-codes a specific Adapter
into two channel implementations.

The fix is **Adapter-level, not channel-level**. Each Adapter declares
its **pane signature** (the set of `pane_current_command` values that
identify a live worker of that Adapter) at registration time. The
propulsion and whisper Ports check the calling Worker's Adapter against
the Adapter's registered signature — not against a hard-coded literal.
No channel renaming, no new ADR-038 channel; only an Adapter registration.

This is the exit criterion for "you can register a second Adapter
without editing `whisper.rs` or `propel.rs`". It is named here for the
record, *not* implemented here. Implementation is downstream of this
vocabulary commit.

## Consequences

**Positive.**
- Every later deliberation on worker-spawn (the Codex experiment
  `task-20260426-caae`; the branch decision `task-20260426-ca4d`; any
  subsequent trait or wrapper) shares one referent for *Worker*, *Port*,
  *Adapter*, *Tier*. Karpathy's "tight 5-method trait", Torvalds's
  "30-LOC `harness` field", Niel's "polyglot wrapper", and Feynman's
  "shell-script experiment" become *comparable proposals on the same
  vocabulary*.
- ADR-038 (whisper) and ADR-072 (routing) gain a sibling that re-uses
  *Port* and *Tier* without overloading them.
- The pleonasm `WorkerAdapter` and the metaphor stacks
  `harness-multiplex` / `model-multiplex` exit the design conversation.

**Negative / accepted.**
- Some prose retraining cost: chronicles and briefings will continue to
  use *harness* as a colloquial substrate name. That is allowed; the
  ban is only on code, trait names, crate names, and ADR titles.
- ADR-038 §5/§6 carry a known §-leak (hard-coded `"claude"` literal)
  until the Adapter-registration fix lands. The leak is documented
  here; no patch is required to sign this ADR.

**Structural.**
- The four-word set is closed for this perimeter. Adding a fifth
  primitive (e.g. *Substrate*, *Backend*, *Provider*) requires a
  successor ADR, not a silent introduction.
- *runtime* remains exclusively bound to ADR-016. Re-using it for an
  Adapter (e.g. "the Codex runtime") is a structural breach.

## Alternatives considered

- **Skip the vocabulary ADR; draw the trait first** (rejected by
  panel-unanimous T3). The trait would need a name; that name would
  smuggle the unfixed vocabulary forward. Cheaper to spend one page now.
- **Promote *harness* to a primitive** (rejected — synthesis §1, §C2).
  *Harness* names a colloquial role, not a typed perimeter. The typed
  perimeter is *Port*; the substrate is *Adapter*. Promoting *harness*
  would force a redundant level on every type.
- **Migrate *Tier* into the worker conversation** (rejected — synthesis
  §C2). *Tier* names a fallback cascade. Worker selection is per-molecule
  at `cs tackle` time, not fall-through-routed mid-execution. *Tier*
  stays in its routing home (ADR-072 / ADR-078) and is re-used only
  *inside* an Adapter that internally cascades providers.

## Invariants

**Preserved.**
- ADR-038 *Port* / *Adapter* — used here verbatim.
- ADR-072 / ADR-078 *Tier* — scoped to routing; not migrated.
- ADR-016 *runtime* — reserved.
- §8j (ingress bindings) — the worker-spawn Port is one such binding.

**Newly inscribed.**
- The four-word closed set { Worker · Port · Adapter · Tier } for the
  worker-spawn perimeter.
- The four-obligation Adapter contract (`briefing.md` · writable
  `MOLECULE_DIR` · `cs` on PATH in worktree · idempotent termination).
- Delivery integrity: formula-owned, visible behavioural guards preserve the
  sealed briefing's signal across Adapter substitution; the Adapter is never
  its semantic judge.
- The retirement of `engine`, `driver`, `shell`, `harness-multiplex`,
  `model-multiplex`, `WorkerAdapter`.

## References

- Parent synthesis: `delib-20260426-6b52/synthesis.md` §C2, §C3, §C4, §D1, §T3.
- Wheeler response: `delib-20260426-6b52/responses/wheeler.md` §1, §2, §3, §4.
- Port + Adapter precedent: [ADR-038](038-whisper-perturbation-port.md).
- Tier precedent (routing): [ADR-072](072-session-route-formula-and-sidecar-invariants.md), [ADR-078](078-session-route-for-utterances.md).
- Resident Runtime (the word `runtime`): [ADR-016](016-autonomy-regimes-and-resident-runtime.md).
- Envelope at `cs tackle`: [ADR-075](075-oracle-boundary-cs-tackle.md).
- Behavioural-guard source deliberation: `delib-20260715-bce7`
  (wheeler / torvalds / janis / jobs).
- Authoring task: `task-20260426-bb95` (Path-1 child #1).
