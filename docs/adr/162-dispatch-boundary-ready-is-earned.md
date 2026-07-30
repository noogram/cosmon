# ADR-162 — The dispatch boundary: `Ready` is earned, never inherited

**Status:** Accepted (2026-07-25). Records an invariant over code already on
the trunk; ships no new behaviour. Supersedes [ADR-093](093-cosmon-transport-liveness-detector.md)
(see §"Resolution of ADR-093" below).
**Date:** 2026-07-25.
**Decider:** Noogram.
**Authoring task:** `task-20260725-01c3`.
**Entry artefact.** Blocking gap #3 of the G11 closure verdict for
noogram/cosmon#20 (`bug-closure-20260725-8c79`): *"the structural clause is
unenforceable — `docs/architectural-invariants.md` contains zero occurrences of
`closed default`, `AwaitingHuman`, `shows_composer`, `composer evidence` or
`positive evidence`. The clause C2 was written specifically to bind future
changes, and it currently lives only in a private function's doc comment."*

**Architectural invariants:** ratifies
[§8v — Dispatch boundary: `Ready` is earned, never inherited](../architectural-invariants.md#8v-dispatch-boundary-ready-is-earned-never-inherited).

**Related ADRs:**
[ADR-093](093-cosmon-transport-liveness-detector.md) (superseded here),
[ADR-079](079-worker-spawn-port-and-adapter-contract.md) (the worker-spawn
Port/Adapter vocabulary this decision lives inside),
[ADR-075](075-oracle-boundary-cs-tackle.md) (the `cs tackle` envelope that
drives the spawn postcondition).

---

## Context

Claude Code's TUI paints `❯` in two structurally different places: at the head
of the composer's input line, and as the selection cursor of every menu it
draws. `classify_output` originally scanned the last few lines for that
character and called what it found `Ready`. Every blocking onboarding screen
therefore read as a worker accepting work, so `cs tackle` typed an 80-line
briefing into a two-option menu, exited 0, and left the molecule `running`
forever with nothing reporting a fault.

The repair was applied three times by *naming one more screen* — the trust
dialog (`TRUST_PROMPT`), the bypass-permissions consent
(`BYPASS_PERMS_WARNING`), the first-run theme wizard (`FIRST_RUN_THEME`). Each
patch shut one door and left the corridor open. The fourth door, reported
externally against signed `v0.3.0` as noogram/cosmon#20, was Claude Code's
login-method selector — simply the first screen nobody had named yet.

The fix that landed on `feat/container-worker-doors` does not name a fifth
screen. It inverts the default: `SessionStatus::Ready` is now *earned* by
positive evidence that the composer is on screen (`shows_composer`), and every
pane that cannot produce that evidence lands on `SessionStatus::AwaitingHuman`
or `SessionStatus::Unknown`, both of which the dispatch gate refuses. The G11
audit measured the difference and found it real: reverting `shows_composer` to
the pre-fix open default turns **ten** tests red, including both halves of the
frozen acceptance pair and all five escapes the two clean-room review rounds
found. A fourth marker would have flipped one. That mutation was re-run while
writing this ADR and reproduces exactly — 253 passed / 0 failed becomes 243 /
10; the mutant body and the numbers are recorded in §8v so a reviewer can
repeat the measurement rather than take it on trust.

**The gap this ADR closes is not in the code.** The rule that binds *future*
changes — clause C2 of the bug contract — lives only in the doc comment of a
private function, `shows_composer`. A reviewer cannot cite a private doc
comment; a reviewer can cite an invariant. A rule no reviewer can invoke is not
an invariant, it is an intention. This ADR turns the intention into an
invariant, and cites the tests that make it executable rather than declarative.

## Options Considered

### Option 1 — Record the invariant in `architectural-invariants.md` as a new ratified §8v, citing its pinning tests *(chosen)*

**Pros.** The rule becomes citable in review. It sits with the other structural
rules a contributor is told to read before changing command behaviour. Naming
the pinning tests makes the section executable: a reader can check the claim
rather than trust it, and a future change that breaks the rule breaks a named
test rather than a paragraph.

**Cons.** One more section in a 3,000-line document; the invariant file grows
whether or not anyone reads it.

### Option 2 — Leave the rule in the `shows_composer` doc comment and rely on `cargo doc`

**Rejected.** This is the status quo the G11 audit named as a blocking gap.
`shows_composer` is private, so the comment does not even appear in the
published rustdoc; a reviewer objecting to a future patch has nothing to point
at but a source file. Worse, the failure mode it guards against is *someone
adding a fifth marker* — a change that would never touch `shows_composer` at
all and would therefore never surface its comment.

### Option 3 — Enforce mechanically with a lint (grep-based CI gate forbidding new marker constants)

**Rejected.** The rule is not "no new marker constants" — `TRUST_PROMPT`,
`BYPASS_PERMS_WARNING` and the composer-footer markers are all legitimate and
still load-bearing. What is forbidden is *promoting a screen to `Ready` by
naming it*, which is a semantic property of the classifier's control flow, not
a lexical property of the constant table. A grep gate would ban the wrong
thing, and the tests already cover the right one — notably
`an_unrecognised_menu_is_not_ready`, which is red exactly when the closed
default is re-opened.

### Option 4 — Ship the ADR without resolving ADR-093's status

**Rejected.** ADR-093 stood at `proposed`, proposing a *different* mechanism
for the same goal, and its Context asserted as present-tense fact something no
longer true of this trunk (that an unrecognised wizard returns `Unknown` and is
torn down with a misleading diagnostic). Leaving two live records pointing at the same
seam is the surface-lie family this issue is about, one level up.

## Decision

**Ratify [§8v](../architectural-invariants.md#8v-dispatch-boundary-ready-is-earned-never-inherited)
as a ratified (not proposed) invariant**, with four members and, for each, the
test that pins it. The section is ratified rather than proposed because the
rule is enforced by the code and the test suite at this head — a `*(proposed)*`
tag would say, falsely, that it carries no test.

The four members, in the words the G11 verdict fixed them:

1. **`cs tackle` never briefs a pane that has not positively proven a
   work-accepting state.**
2. **`Ready` is earned by composer evidence, never inherited from a chevron.**
3. **An unrecognised screen is refused, not admitted.**
4. **`observe` and `await_live` remain two distinct questions** (contract
   clause C0).

The full statement, the seams, the pinning tests and the reviewer rule live in
the invariant section itself; this ADR does not duplicate them. Options 2, 3
and 4 above are the rejected alternatives.

## Resolution of ADR-093

**ADR-093 is marked `Superseded by ADR-162`.** The analysis behind that choice,
rather than `partially realised`, is recorded here because the honest answer is
mixed and the mixture is the point.

ADR-093 diagnosed the *right* pathology — a marker table patched quarterly,
where an unrecognised vendor wizard falls through to `Unknown` and the spawn
postcondition tears the worker down with the most misleading diagnostic
available (*"session never produced live-claude output"*, when the session did
produce live claude output). It proposed to fix it by making the classifier
*more admissive at the spawn gate*: a new `SessionStatus::Alive(Aliveness)`
variant, a `classify_output_positive` returning `Option<SessionStatus>`, and a
poll-to-poll content-hash delta as the liveness signal.

What shipped instead moved in the opposite direction at the *other* gate: it
made the classifier *less* admissive at dispatch. Clause by clause:

| ADR-093 decision clause | Status on this trunk |
|---|---|
| 1. `SessionStatus::Alive(Aliveness)` variant | **Not implemented.** `AwaitingHuman` occupies the role — "a frame was painted, and it is a question" — as a *static* verdict, with no `Aliveness` sub-state and no delta machinery. |
| 2. Spawn postcondition accepts the new variant | **Realised by other means.** `SessionStatus::AwaitingHuman.liveness() == Liveness::Live`, so a rendered unnamed screen passes the postcondition. |
| 3. `wait_ready` keeps polling on the new variant | **Realised by other means.** `SessionStatus::Loading \| AwaitingHuman \| Unknown` is one arm that waits and deliberately answers nothing. |
| 4. `classify_output → classify_output_positive: Option<SessionStatus>` | **Not implemented as a signature; realised in substance.** `shows_composer` is exactly a positive-evidence predicate, scoped to the one verdict that opens the dispatch gate. |
| 5. Content-hash liveness delta | **Not implemented, and no longer needed** for the motivating case. The evidence that a binary rendered is now static (`pane_painted_a_frame`), not temporal. |
| 6. "The action-gate markers stay" — `Ready` remains a positive marker | **Contradicted.** `Ready` is no longer a marker at all. This is the one clause the shipped fix reverses rather than reroutes, and it is why the status is `superseded` and not `partially realised`. |

Its `Alternative D` (raise the 2 s spawn window) was already reversed by its own
2026-06-02 postscript; the window is now 12 s and env-configurable via
`COSMON_SPAWN_POSTCONDITION_SECS`. Its migration plan PR 1 (the
`FIRST_RUN_THEME` / `FIRST_RUN_WELCOME` markers) landed and those markers are
still in the table, now buying a *richer* verdict (`Loading`, hence `Live` at
the spawn gate) rather than keeping a wizard out of `Ready` — that job belongs
to `shows_composer`. PR 3 and PR 4 of that plan are **withdrawn**.

One of ADR-093's anti-patterns is honoured on this trunk by different means and
deserves the credit: *"do not make the new variant the catch-all that replaces
`Unknown`."* `AwaitingHuman` and `Unknown` are distinct here, and the
distinction is load-bearing and pinned — see
`a_rendered_frame_without_a_chevron_is_not_nothing`.

**The residual ADR-093 covered and this ADR does not.** A pane that renders
*plain text only* — no box-drawing characters, no chevron, no recognised
marker: a bare login URL, a stack trace, a proxy error — still classifies
`Unknown`, is `Indeterminate` at the spawn postcondition, and is torn down.
ADR-093's content-hash delta would have called that pane alive. This
supersession does not claim to have covered it, and the case is not
hypothetical for long-tail vendor output. It is recorded here as a known
residual rather than silently inherited: if it bites in production, it is a new
ADR with fresh evidence, not a revival of the delta mechanism on the strength
of this paragraph.

## Consequences

### Positive

- The clause that was written to bind future changes can now be cited by a
  reviewer, which is the only thing that makes it binding.
- The invariant is executable, not declarative: every member names a test, and
  each named test was read against the assertion it is cited for while writing
  this ADR.
- Two live records no longer point at the same seam with different mechanisms.

### Negative — the risks

- **An invariant is only as strong as its weakest cited test.** If a future
  refactor keeps a test's name while hollowing out its assertion, §8v will look
  enforced and will not be. The mitigation is inside the tests themselves:
  `an_unrecognised_menu_is_not_ready` carries the instruction *"if a future
  marker ever claims this pane, replace the pane — never the assertion"*, and
  `await_live_refuses_a_worker_parked_on_a_menu` guards against passing
  vacuously by first asserting the mock worker is not `Dead`.
- **The closed default's price is a fleet that refuses a healthy worker if the
  composer's own signature moves.** Claude Code renaming its footer hint would
  make every worker unspawnable — a loud, total outage rather than a silent
  wrong dispatch. This trade is deliberate (a loud failure is the contract's
  C3), but it is a real risk with a real cost, and it is the reason
  `the_composer_is_still_ready` and
  `a_composer_showing_a_suggestion_is_still_ready` exist as price-of-the-fix
  guards.
- **~~The instrument that should prove all of this in a container cannot yet
  express a refusal.~~** *(closed 2026-07-25.)* Arm C now asserts the four
  post-conditions of a correct refusal instead of grepping a pane, and the
  bench went on to earn its keep immediately: with a working instrument it
  showed that the four members ratified here were **all green while the
  container still dispatched**. The diagnosis and the fifth member are in the
  postscript below.

### Neutral

- No code changes. No CLI or MCP surface changes. `cargo` gates are unaffected
  except by the documentation itself.

## Postscript — the fifth member (2026-07-25, `task-20260725-4a1f`)

This ADR ratified four members and claimed no new behaviour, which was true.
What it did not know is that the four were **not sufficient**: with the bench
repaired, arm C still showed `cs tackle` exiting 0 over a blocked pane, on a
binary carrying every fix named here.

Instrumenting the readiness loop (`COSMON_READINESS_TRACE`,
`cosmon_transport::readiness_trace`) settled it in two lines of trace. The pane
that decided the dispatch was never the login-method selector every test in this
ADR uses. For the whole 30-second window it was Claude Code's **first-run theme
wizard**, which `classify_output` correctly calls `Loading` — and the dispatch
gate was a **deny-list**: four named statuses refused, everything else collapsed
through `SessionStatus::liveness`. `Loading` was not among the four. The briefing
then went out and *answered the wizard*, advancing the pane to the selector,
which is why every capture taken after `cs tackle` returned showed a screen the
process had never classified.

So the door this ADR describes as shut by M1–M4 was in fact shut one layer too
high. The gate is now an allow-list — `readiness::dispatch_gate_liveness`, only
`Ready` and `Working` open it — recorded as **M5** in §8v with its two pinning
tests. The generalisation is the same one this ADR already argues, applied one
layer down: *a door is shut by a name; a corridor is shut only by a default* —
and there was a second default nobody had closed.

Nothing above is retracted. M1–M4 are still necessary, still enforced, still
pinned. The correction is that they were described as sufficient, and the
container was the only place that could say otherwise.

## Postscript — the sixth member, and the premise that expired (2026-07-30, `task-20260730-ec81`)

M1–M5 shut the corridor against screens this build had never seen. They said
nothing about the build's model of the screens it *had* seen going stale, and
five days later it had.

Claude Code 2.1.220 changed two things at once. It stopped **boxing** its
composer and started **ruling** it — one full-width `─` above the input line,
one below — so neither `shows_composer` disjunct matched. And it stopped hiding
the composer during a turn. Seven panes captured from a live session — one idle,
six four seconds apart mid-stream — all classified `AwaitingHuman`. The first
change made every non-bypass session undispatchable outright; the second made
`SessionStatus::Working` unreachable, and `Working` is `cs tackle`'s
briefing-submit loop's only early exit, so every dispatch paid its whole 90 s
budget. An external tester measured the flat 92/93 s against jobs of 32 s and
53 s.

The lesson is a different one from M5's, and it is worth separating. M5 was
about a *default* left open. This was about a *premise* that quietly expired:
"an input box at the bottom means idle" was true only while the prompt vanished
for the duration of a turn, and its whole truth came from the disappearance.
When the disappearance went away the rule kept running, still shaped like a
sound rule, now certifying the opposite of what it was written to certify. No
test noticed, because not one test in the suite held a frame the TUI had
actually painted — every pane in it was a string literal someone typed from a
description.

The repair is recorded as **M2**'s third disjunct and **M2b** in §8v. The
guard against the next expiry is `tests/fixtures/claude-tui-2.1.220/` — seven
real captures, asserted by `tests/claude_tui_2_1_220.rs`. A classifier verified
against a described TUI drifts on exactly the schedule this one did.

Nothing above is retracted.

## References

- **Closure verdict** — `bug-closure-20260725-8c79`, §"The named gaps" #3
  (this molecule) and #1 (the bench — now closed, see the postscript).
- **Issue** — noogram/cosmon#20, reported by an external tester against signed
  `v0.3.0`.
- **Code under decision** —
  [`crates/cosmon-transport/src/readiness.rs`](../../crates/cosmon-transport/src/readiness.rs)
  (`shows_composer`, `classify_output`, `SessionStatus::liveness`,
  `ClaudeTuiProbe::await_live`),
  [`crates/cosmon-cli/src/cmd/tackle.rs`](../../crates/cosmon-cli/src/cmd/tackle.rs)
  (the `await_live` verdict match that guards `send_input`).
- **Operator guide** —
  [`docs/guides/claude-worker-in-a-container.md`](../guides/claude-worker-in-a-container.md)
  (blocking gap #2 of the same verdict; its door-4 passage still teaches the
  pre-fix workaround at the time of writing).
