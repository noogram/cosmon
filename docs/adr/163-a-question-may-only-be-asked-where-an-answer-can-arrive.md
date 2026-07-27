# ADR-163 — A question may only be asked where an answer can arrive

**Status:** Accepted (2026-07-27).
**Date:** 2026-07-27.
**Decider:** Noogram.
**Authoring task:** `task-20260727-2133`.
**Entry artefact.** The fifth door of noogram/cosmon#20 — measured in the
external tester's container image against `cs 0.3.0`, with the
container-real-mission harness. Unlike the first four, this door is ours: no
adapter, no vendor TUI, no third-party onboarding screen. Cosmon's own
first-run consent prompt hung the dispatch.

**Architectural invariants:** ratifies
[§8w — A question may only be asked where an answer can arrive](../architectural-invariants.md#8w-a-question-may-only-be-asked-where-an-answer-can-arrive).

**Related ADRs:**
[ADR-162](162-dispatch-boundary-ready-is-earned.md) (the same corridor seen
from the other side: what cosmon may *believe* about a worker's screen),
[ADR-075](075-oracle-boundary-cs-tackle.md) (the `cs tackle` envelope this
decision removes a blocking call from).

---

## Context

### What was measured

A real dispatch, with a real credential present, ran for the full 240s
timeout and died at `rc=124` having spawned nothing. The molecule stayed
`pending`; no tmux session was ever created. `/out/tackle.out` contains, in
full:

```text
Acceptez-vous de partager des informations avec les développeurs cosmon ?
Les bundles seront chiffrés age, seul le mainteneur Noogram pourra les lire.
Modifications à votre projet : aucune trace de cosmon dans vos commits. [o/N]
>
```

That is `cosmon_cli::cmd::opt_in_share::ensure_consent`, reached from the
first line of `cs tackle::run` in a galaxy with no recorded decision.

The differential was measured on both halves, in the same image:

| invocation | result |
|---|---|
| `docker exec` **without** `-t` | prints `opt-in-share: stdin non-tty — refus par défaut enregistré`, exits 0 |
| `docker exec` **with** `-t` | the prompt renders and blocks until the timeout |

A `-t` allocation is not exotic here. The container guide tells the human to
use `docker exec -it` for the credential login; reusing that same invocation
shape for the mission is the obvious next gesture.

### The defect

`ensure_consent()` gated on `io::stdin().is_terminal()`. That predicate
answers *"is a terminal attached?"*. The question that actually matters is
*"can a human see this and answer it?"* — and the two come apart in exactly
the case that hangs. An orchestrator captures the child's stdout
(`TACKLE_OUT="$(cs tackle …)"`) while stdin remains the inherited TTY. The
question is then printed into a variable nobody reads, on an input nobody is
watching. No keystroke can arrive, and no output can warn.

That is a **mute hang**: not a crash, not a refusal, not a timeout with a
diagnosis. It is structurally identical to the four doors of
noogram/cosmon#20 — cosmon certifying as healthy a process that cannot make
progress — except that here cosmon is not the observer that got it wrong. It
is the one holding the door shut.

### Why a wider guard was not enough on its own

Fixing the predicate makes the prompt harmless. It does not make the
*placement* correct. Dispatch is precisely the path an orchestrator wraps in
command substitution; any blocking question placed there is a latent hang
waiting for the next caller who redirects one more stream than the guard
happens to test. Two questions, two answers.

## Decision

### D1 — The guard tests answerability, not attachment

A first-run question may be asked only when **both stdin and stdout are
terminals**. If stdout is captured or redirected, the question is invisible
and auto-declines down the exact path a missing TTY already took. The
recorded-decision short-circuit is unchanged; the no-TTY path's behaviour and
its operator-facing wording are unchanged byte-for-byte.

Seam: the private `can_ask_interactively` predicate in
`crates/cosmon-cli/src/cmd/opt_in_share.rs`, consulted by both
`ensure_consent()` and `run()`.

**stderr is deliberately excluded from the conjunction.** It is the channel
the auto-decline trace is written on, and supervisors routinely redirect it
to a log file while leaving the interactive pair intact. Folding it in would
suppress a question the operator can see and answer perfectly well. The
prompt is written to stdout; stdout is therefore the stream whose visibility
is load-bearing, and stderr's state says nothing about it.

### D2 — The consent question leaves the dispatch path

`cs tackle` no longer calls `ensure_consent()`. The question now fires from:

* **`cs init`** — the explicit, once-per-galaxy interactive moment. Suppressed
  under `--json`, so a question can never land in the middle of a
  machine-read document.
* **`cs opt-in-share`** invoked alone — the command whose entire purpose is
  this decision.

Nothing on the dispatch path may block on a human. This is not a statement
about consent; it is a statement about `cs tackle`.

The behavioural cost is nil: no code path reads the consent record yet, and a
missing record already means deny-by-default. An operator who never runs
`cs init` interactively is in exactly the state the record would have
described.

### D3 — An auto-decline is never silent

When the question declines itself, `warn_skipped_on_stderr` writes a trace
naming what happened and the explicit remedy
(`cs opt-in-share --accept | --decline`) — but only when stdout is *not* a
terminal, i.e. only when the normal rendering could not have been seen. On a
real terminal the operator has already read the decision and a second copy is
noise.

The stdout wording now names which half of the pair was missing:
`stdin non-tty` (preserved verbatim from before) or `sortie capturée`. The
old message would have been a lie in the new case.

### D4 — The regression test asserts the property, not the artefact

`crates/cosmon-cli/tests/consent_non_blocking.rs` allocates a real pty and
runs the real `cs` binary in the failing geometry — terminal on stdin,
captured stdout — then requires the process to *terminate* within a deadline,
polling `try_wait` rather than blocking on `wait` (a blocking wait against a
hung child is the bug reproduced inside the harness).

A test that asserted only the contents of `consent.toml` would pass against
the broken build: the broken build also writes a declined record. It writes
it after somebody types into a terminal nobody is watching — or never. Only
*terminates on its own* separates the two builds. Verified by reverting the
guard to `stdin().is_terminal()`: the test fails on the 30s deadline with the
diagnosis quoted.

A second test pins D2 from the other end: `cs tackle` on a *fully*
interactive pty — the friendliest possible case for a prompt — must ask
nothing and write no consent record.

## Consequences

* One fewer place where cosmon can block on a human. The dispatch path is now
  free of interactive questions by rule, not by accident of stream wiring.
* The container corridor of noogram/cosmon#20 loses its fifth door.
* A future first-run question inherits the guard by reusing
  `can_ask_interactively`, and inherits the placement rule from §8w.
* The prompt is now reachable in fewer situations. That is the point: the
  situations removed are the ones where it could not be answered.
