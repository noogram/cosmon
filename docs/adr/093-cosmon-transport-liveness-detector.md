# ADR-093 — cosmon-transport liveness-driven readiness detector

**Status:** proposed
**Date:** 2026-05-14
**Decider:** Noogram
**Parent idea:** `idea-20260514-28f6`
  — capture + feasibility
  + plan.
**Authoring task:** `task-20260514-f356` (sole child of `idea-20260514-28f6`).
**Grandparent capture:** `idea-20260514-5a58`
  — the three-models triage that motivated this molecule.

**Related ADRs:**
[ADR-079](079-worker-spawn-port-and-adapter-contract.md) (worker-spawn
Port + Adapter vocabulary — this ADR refines a single Adapter's
internal classifier without touching the Port),
[ADR-038](038-whisper-perturbation-port.md) (Port vocabulary
origin),
[ADR-075](075-oracle-boundary-cs-tackle.md) (the `cs tackle` envelope
that drives `observe_spawn_postcondition`).

**Architectural invariants:** `docs/architectural-invariants.md`
§8j (Port boundary preserved — no new port method),
§8b (proposals of verification, never imposed enforcement —
seal-as-trace family).

---

## Context

`classify_output` in [`crates/cosmon-transport/src/readiness.rs`](../../crates/cosmon-transport/src/readiness.rs)
has been patched **three times in ~four months** to recognise a new
Claude Code TUI first-run surface (trust prompt early-2026, re-tuned
trust dialog 2026-04, v2.1.140 theme wizard 2026-05). Each fix is a
literal-string marker added to a fixed table; each passes its own
unit tests; none of them protect against the next vendor wizard. The
pattern is real, visible, and recurring at roughly quarterly cadence.

The failure mode at the operator level is acute. When a wizard
renders bytes that no marker in the table recognises, the classifier
returns `SessionStatus::Unknown`. The 2 s spawn postcondition
(`observe_spawn_postcondition`,
[`crates/cosmon-cli/src/cmd/tackle.rs:2282-2314`](../../crates/cosmon-cli/src/cmd/tackle.rs))
rejects `Unknown` and tears the worker down. The operator sees
*"session never produced live-claude output"* — the most misleading
possible diagnostic, because the session **did** produce live claude
output; cosmon just didn't recognise it.

The structural question — and the one this ADR answers — is whether
the classifier is the right abstraction or whether the marker table
is the right mechanism. The idea molecule evaluated three models
(A markers / B liveness / C version-aware) across six axes; the
feasibility step converged on a **hybrid B+A** verdict, deferred
behind two prerequisite migrations (Layer 1 markers,
`task-20260514-8f7a` — P0; Layer 2 image pre-seed,
`task-20260514-f988` — P1).

This ADR captures that verdict. It does **not** ship code; the
implementation is a separate molecule the operator schedules after
the two prerequisites have run in production for ≥1 Claude Code
release cycle.

## Decision

Adopt **hybrid B+A**: liveness for the spawn-postcondition gate,
positive markers for the action gates. Concretely:

1. **`SessionStatus` gains one variant:** `Alive(Aliveness)`, with
   `Aliveness ∈ { Booting, Producing, Stalled }`. The existing six
   variants (`TrustPrompt`, `Loading`, `Ready`, `Working`, `Blocked`,
   `Dead`, `Unknown`) are preserved unchanged.
2. **`observe_spawn_postcondition` extends its acceptable set** to
   include `Alive(_)` alongside `{Loading, TrustPrompt, Ready,
   Working, Blocked}`. The 2 s budget and 200 ms poll interval are
   **not** modified.
3. **`wait_ready` extends its match** with an `Alive(_)` arm that
   continues polling (same shape as today's `Loading | Unknown`
   arm). The 30 s budget is **not** modified. `Ready | Working`
   remain the only terminal states.
4. **`classify_output` is refactored to `classify_output_positive`**,
   returning `Option<SessionStatus>` (the positive-marker subset).
   The caller then promotes `None` to `Alive(Producing)` or
   `Alive(Stalled)` based on a poll-to-poll content delta.
5. **Liveness signal: content-hash delta** (Path I in the
   feasibility doc). `wait_ready` keeps `Option<(blake3::Hash,
   Instant)>` across iterations. A pane-buffer hash change within
   the liveness window (initial guess: 2× the poll interval) means
   `Alive(Producing)`; no change means `Alive(Stalled)`. **No new
   `TransportBackend` port method.**
6. **The action-gate markers stay.** `Ready`, `TrustPrompt`,
   `Blocked`, `Working` are positive contracts the caller acts on
   (send prompt, auto-Enter). Liveness cannot infer these.

The structural shift is therefore narrow: we decouple **"is the
binary alive and rendering?"** (now a liveness signal) from
**"is the binary at the idle REPL or a known modal?"** (still a
positive marker). The spawn gate uses the first; every other call
site keeps using the second.

## Consequences

### Positive

- **The third-strikes pattern stops at the spawn gate.** Any future
  Claude Code first-run wizard that renders any bytes within 2 s
  passes the postcondition without a cosmon patch. The dominant
  vendor pattern — theme picker, welcome screen, changelog modal,
  permissions tutorial — fits this case by construction.
- **Honest failure diagnostics.** When the spawn gate fails under
  the new regime, it fails because the pane bytes did *not* change
  in 2 s. The operator can trust the diagnostic: the binary really
  is dead, not "we forgot a marker".
- **Marker family shrinks.** The "first-run wizard" sub-family
  (`FIRST_RUN_THEME`, `FIRST_RUN_WELCOME`, any quarterly successor)
  becomes optional and can be retired in a follow-up PR after the
  wizard corpus passes.
- **No port-trait churn.** `TransportBackend` is unchanged.
  Downstream backends (mock + tmux today; future Docker / SSH /
  podman) need no implementation work for liveness — the delta lives
  in `wait_ready`'s loop, not in the port.
- **Strict superset.** Every existing call site keeps working
  unchanged. The new `Alive(_)` arm is additive; no caller is
  forced to handle it differently from `Loading`.

### Negative

- **State in `wait_ready` and `observe_spawn_postcondition`.** Both
  functions go from stateless-across-iterations to keeping one
  `Option<(blake3::Hash, Instant)>` local. Tests for liveness
  become two-snapshot tests, not one-string tests.
- **Per-poll cost.** Content-hash of ~30 lines (< 1 KB) at 200 ms
  intervals via blake3 (~GB/s throughput) is well below noise, but
  the carry cost is non-zero and must be observed once.
- **Liveness window tuning.** Initial guess (2× poll interval = 400 ms)
  is plausible but unproven against real spawn traces. Wrong tuning
  produces `Stalled` false-positives during Claude's natural pauses
  or, in the other direction, `Producing` false-positives on pure
  spinner animation. Mitigated by the upper-bound 30 s budget in
  `wait_ready`, which catches any "alive but useless" wedge regardless
  of classifier verdict.
- **`Unknown` semantics narrow.** `Unknown` remains a useful signal
  for *"pane has no recognised markers AND bytes did not change"*
  (stale or empty content). Implementors must NOT collapse `Unknown`
  into `Alive(Stalled)` — the distinction is the "we saw zero
  progress" signal.

### Neutral

- The action-gate markers (`TRUST_PROMPT`, `TRUST_PROMPT_ALT`,
  `READY_PROMPT`, `READY_TYPE`, `TOOL_USE`, `THINKING`,
  `TOOL_PERMISSION`, `YES_NO_PROMPT`, `LOADING`) keep their existing
  semantics and tests.
- The MCP / CLI surface is unaffected. `cs tackle`, `cs observe`,
  and `cs done` are oblivious to the classifier change.

## Alternatives considered

### Alternative A — status quo (marker-only)

Keep the marker table; add a new constant for every new vendor
wizard. **Rejected.** This is the current state and the recurrence
cost is the problem this ADR is solving. The fix arrives reactively,
after the V2 stack falls over.

### Alternative B-port-method — tmux `pane_last_activity`

Add `fn last_activity(&self, id: &WorkerId) -> Result<Instant, …>`
to `TransportBackend`, implemented via tmux's `#{pane_last_activity}`
format. **Rejected.** The mechanism is vendor-coupled to tmux (no
SSH / podman / Docker backend has an equivalent); the port grows for
zero gain over content-hash; and the second backend that would need
the method does not exist today. Content-hash (Path I) keeps the
port honest and the cost is negligible.

### Alternative C — version-aware classifier

Read `claude --version` at spawn, branch the marker table by
major.minor. **Rejected.** Better-organised coupling is still
coupling. The structural answer is decoupling. C also doubles the
test matrix and inherits `claude --version`'s own output as a
contract that could shift shape.

### Alternative D — raise the spawn budget

Move the 2 s postcondition window to 5 s / 10 s / 30 s.
**Rejected.** Bumping the budget hides the classifier question.
The 2 s number is right; the question was always about what we
accept *within* those 2 s. This ADR re-shapes the acceptance set,
not the window.

> **Postscript — 2026-06-02 (task-20260602-ef26): Alternative D
> partially reversed by cold-container evidence.** The reasoning above
> held for warm-host spawns, where the first frame renders well inside
> 2 s. It did **not** hold for the Tenant-Demo AWS tenant: on a *cold*
> container, claude's first TUI frame — including the trust prompt
> *"Quick safety check"*, which **is** already a recognised marker
> (`TRUST_PROMPT_ALT`) — renders *after* 2 s. So the postcondition
> timed out, tore the session down before it could be inspected, and
> surfaced as `503 tackle_unavailable` at the adapter. tenant auditor
> confirmed live: warming claude first makes tackle pass, but racy near
> the 2 s boundary ⇒ pure timing, not detection nor auth. The classifier
> was never the problem here — the marker existed; the window was too
> tight to *see* it render. Fix: the default window is widened to 12 s
> and made env-configurable (`COSMON_SPAWN_POSTCONDITION_SECS`); the
> second-stage `wait_ready` budget was already 30 s, so only this first
> proof-of-life gate was hardcoded too tight. A debug
> `COSMON_SPAWN_NO_TEARDOWN=1` flag now lets operators keep the carcass
> pane for inspection. This refines, not refutes, the ADR's thesis: the
> acceptance set was right; the window must clear a cold first-frame
> render, which 2 s does not guarantee.

## Migration plan — 4 PRs, no flag day

Each step is reversible on its own; no breaking change at any step.

| PR | Owner molecule | Scope | Status |
|----|----------------|-------|--------|
| **1** | `task-20260514-8f7a` (Layer 1, P0) | Add `FIRST_RUN_THEME` / `FIRST_RUN_WELCOME` markers; classified as `Loading`. Spawn gate accepts → wizard passes. | **Gate for V2.** Already nucleated, already plan-approved. **This ADR does not touch this PR.** |
| **2** | `task-20260514-f988` (Layer 2, P1) | Smithy-side image pre-seed: the wizard never renders in V2-built images. Layer 1 becomes belt-and-suspenders. | Parallelisable with PR 1. **This ADR does not touch this PR.** |
| **3** | _separate task-work, operator-scheduled_ | Implement this ADR. Add `Alive(Aliveness)`. Refactor `classify_output` → `classify_output_positive` (returns `Option<SessionStatus>`). Thread liveness state through `wait_ready` and `observe_spawn_postcondition`. Existing tests pass unchanged. New tests cover wizard corpus (see Falsifiability) + edge cases (empty pane, spinner-only, locked tmux). | **The ADR-grade decision.** Operator schedules after PR 1 + PR 2 run in production for ≥1 Claude Code release cycle. |
| **4** | _later, optional_ | Retire `FIRST_RUN_THEME` / `FIRST_RUN_WELCOME` (and any quarterly successors), because `Alive(Producing)` now absorbs them. Re-run the wizard corpus to confirm nothing regresses. | Routine test-validated cleanup; no decision left to make. |

PR 3 is the only ADR-grade decision. PRs 1, 2, 4 are routine.

## Risks (copied from feasibility, abridged)

1. **"Liveness is too generous."** A wedged Claude in an infinite
   loop that animates a spinner still classifies as
   `Alive(Producing)`. Mitigation: the 30 s upper-bound budget in
   `wait_ready` catches the wedge — same shape as a missing marker
   today, same diagnostic. The classifier moves the lower bound
   (2 s spawn gate), not the upper bound.
2. **"Bytes change but it's just a spinner."** Spawn gate only
   needs evidence-of-life within 2 s; a spinner satisfies that
   contract truthfully. The follow-up `wait_ready` gate already
   distinguishes `Working` (via `⏺` / `Thinking` markers) from
   non-progress, so a pure spinner times out at 30 s exactly as
   if a marker were missing.
3. **"`capture_output` itself blocks."** A wedged tmux socket
   hangs both classifiers equally. Out of scope.
4. **First-time-through has no baseline.** Conventionally treated
   as `Producing = true` (any output is proof of life), consistent
   with `observe_spawn_postcondition`'s existing
   "at-least-one-observation" semantics.
5. **TUI redraws.** Claude overwrites earlier lines; content-hashing
   the raw pane buffer captures redraws too — which is the correct
   semantics for "is the process alive and rendering?".

## Falsifiability — the wizard corpus

The implementor of PR 3 must run **both** classifiers (current A and
new B-hybrid) against the following corpus, and the new classifier
must pass **all** cases including #8 (the failure that motivated
this molecule):

| # | Surface | Source |
|---|---------|--------|
| 1 | Trust prompt (`Quick safety check`) | `readiness.rs:212-225` |
| 2 | Blocked tool-use permission | `readiness.rs:227-242` |
| 3 | Idle REPL (`❯ `) | `readiness.rs:252-256` |
| 4 | `Type your message` welcome | `readiness.rs:258-262` |
| 5 | Working (`⏺ Reading file`) | `readiness.rs:264-268` |
| 6 | Thinking | `readiness.rs:270-274` |
| 7 | Loading | `readiness.rs:276-280` |
| 8 | v2.1.140 first-run theme wizard | parent capture §"Problem"; `smithy/docs/ops/2026-05-14-tenant-demo-v2-cycle-vie-validation.md` Gap #4 |
| 9 | Hypothetical v2.2 surface (synthesised) | To be authored against the v2.1.88 source pattern (e.g. 4-step plugin gallery, telemetry consent modal). |

**Pre-prediction (recorded here as the falsifiable claim):** Model A
passes 1-7, fails 8, and will likely fail 9. Model B-hybrid passes
1-9. If the prototype contradicts this prediction, **this ADR is
overturned** and the implementor must escalate the discrepancy
before merging.

## Schedule trigger

The operator schedules PR 3 only after:

1. PR 1 (`task-20260514-8f7a`) has merged and run in production.
2. PR 2 (`task-20260514-f988`) has merged and pre-seed is active.
3. At least one full Claude Code release cycle has passed under
   the markers-only regime (Layer 1 + Layer 2). The cycle gives
   us empirical signal on how often the wizard actually renders
   in V2 traffic — which informs whether liveness is even worth
   the carry cost.

No calendar date appears in this ADR by design — the operator
controls the moment of conviction; the engineering substrate stays
ready in writing. This is consistent with the project's
**"no temporal gates in deliberations"** discipline.

## Anti-patterns (do not regress here)

- **Do not** add an `Alive` variant without the delta machinery.
  The name alone changes nothing; the failure mode is still
  `Unknown` after 2 s.
- **Do not** make `Alive` the catch-all that replaces `Unknown`.
  `Unknown` keeps semantic value for "bytes did not change AND no
  positive marker matched" (stale / empty pane).
- **Do not** thread `pane_last_activity` (or any tmux-specific
  field) through `TransportBackend` without a second backend that
  needs it. Path I keeps the port honest.
- **Do not** raise the 2 s spawn budget or the 30 s readiness
  budget. Both numbers are right; the classifier is the question.
- **Do not** ship PR 3 before PRs 1 + 2 have landed and a release
  cycle has elapsed. Doing so creates a merge maze and skips the
  empirical signal.

## References

- **Parent idea** —
  `idea-20260514-28f6` capture,
  feasibility,
  plan.
- **Grandparent capture** —
  `idea-20260514-5a58`
  §"Why this is recurring (the structural lesson)".
- **Sibling tasks** —
  `task-20260514-8f7a` (Layer 1 markers, P0),
  `task-20260514-f988` (Layer 2 image pre-seed, P1).
- **Code under decision** —
  [`crates/cosmon-transport/src/readiness.rs`](../../crates/cosmon-transport/src/readiness.rs)
  (the `markers` module, `classify_output`, `wait_ready`),
  [`crates/cosmon-cli/src/cmd/tackle.rs`](../../crates/cosmon-cli/src/cmd/tackle.rs)
  (`spawn_claude`, `observe_spawn_postcondition`).
- **Memory** —
  `~/.claude/projects/-Users-you-galaxies-cosmon/memory/feedback_no_temporal_gates_in_deliberations.md`
  (informs the absent-ship-date discipline of the Schedule trigger
  section).
