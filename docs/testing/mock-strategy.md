# Mock strategy — spawn-boundary testing with fake binaries

**Status:** introduced 2026-04-16 (task-20260416-03b5) as the mechanical
counterpart to the `ef31` subprocess-boundary fix and the `delib-3879` panel
deliberation. Lives in `tests/fakes/` + `tests/harness/`.

## Why fakes, not mocks

The zombie-worker bugs that drove this work (task-4046, task-25c3) lived at
the **process boundary** between cosmon and its two subprocess dependencies
(tmux, the `claude` CLI). Trait-level mocks in Rust cannot reach that
boundary: they replace the Rust abstraction over tmux, not the
`tmux(1)` binary itself. Whatever contract the real binary enforces is
invisible to a mocked `TransportBackend`.

Feynman prototyped an `experiment.sh` that did reach the boundary — fake
binaries staged on `PATH` before the real ones. This document generalizes
that experiment into a matrix we can iterate on in seconds.

The trade-off versus TLA+ (which was briefly considered): TLA+ models are
only as good as their oracle. We do not yet know what tmux *actually*
promises under "session created but pane died during exec". The fakes let
us *learn* the oracle empirically, row by row, so that a future TLA+ model
can encode facts, not guesses.

## What boundary we test

| Binary        | What cosmon calls                                      |
|---------------|--------------------------------------------------------|
| `tmux`        | `new-session`, `list-sessions`, `list-panes`, `kill-session`, `has-session`, `capture-pane`, `send-keys`, `load-buffer`, `paste-buffer`, `delete-buffer`, `set-hook`, `wait-for` |
| `claude`      | arbitrary — invoked as the session's foreground command |

The fakes reproduce this surface with enough fidelity for `cs tackle` to
complete (no network, no real process forking by default), plus
**env-var fault injection** that flips individual contract bits. See
[`tests/fakes/fake-tmux/README.md`](../../tests/fakes/fake-tmux/README.md)
and [`tests/fakes/fake-claude/README.md`](../../tests/fakes/fake-claude/README.md)
for the exposed knobs.

## The 5-bit observation tuple

Each matrix row runs the full `cs nucleate && cs tackle` cycle against a
scratch cosmon project, then records five bits:

| bit | symbol | meaning                                                   |
|----:|:-------|:---------------------------------------------------------|
| 0   | **S**  | fake-tmux has a session file for this worker              |
| 1   | **P**  | fake-tmux reports `#{pane_dead}=0` for that session       |
| 2   | **B**  | git branch `feat/<mol-id>` was created on the scratch repo |
| 3   | **W**  | `.worktrees/<mol-id>/` was materialized                   |
| 4   | **F**  | `.cosmon/state/fleet.json` holds at least one worker entry |

The 5-bit pattern is the **residual lie** — the discrepancy between what
the five storage locations *claim* about a worker's existence. A healthy
spawn is `11111`. Partial tuples reveal the fault: `10111` under
`FAKE_TMUX_PANE_DEAD=1` says "cosmon committed its bookkeeping, but the
pane is already dead — classic zombie".

## How to read the matrix

```
healthy                      tuple=11111 expected=11111 exit=0 expect=0 → PASS
claude-hang                  tuple=11111 expected=11111 exit=0 expect=0 → PASS
tmux-pane-dead               tuple=10111 expected=10111 exit=0 expect=0 → PASS
tmux-new-fail                tuple=00110 expected=00110 exit=1 expect=!0 → PASS
```

- `tuple` is what we observed, `expected` is the oracle.
- `?` in `expected` means don't-care on that bit (used for "transport
  layer unavailable — any state is acceptable").
- The harness exits non-zero if any row diverges from expectation.

A row going from PASS to FAIL across builds is the regression signal. A
FAIL on main says the tacit contract changed — either fix the code, or
update the expected tuple and say *why* in the commit.

## How to add a row

1. Identify the boundary bit you want to flip (new env var in fake-tmux
   or new `FAKE_CLAUDE_MODE`).
2. Add a line to the `MATRIX=(...)` array in
   [`tests/harness/run_matrix.sh`](../../tests/harness/run_matrix.sh).
3. Run `bash tests/harness/run_matrix.sh` locally.
4. Set `expected=` to the tuple the healthy (pre-regression) codebase
   produces. Commit the row with a short explanation in the PR body.

The matrix was explicitly kept small and legible (one array, one bash
file) so the add-a-row cost stays under 10 lines.

## Non-goals

- **Not a replacement for zero-I/O unit tests in `cosmon-core`.** Those
  cover the domain state machines; the harness covers the boundary where
  state machines meet operating-system semantics.
- **Not a replacement for smoke tests against real tmux + real claude.**
  Those remain the final acceptance signal (see `tests/spec/`).
- **Not exhaustive.** 2^N combinatorics explode quickly; the matrix
  targets the rows we *expect* to break under known-failed invariants.
- **Not a statement about claude.** The fake is a placeholder for the
  upstream CLI's exit semantics — we do not model its protocol.

## Budgets and guardrails

- **Runtime budget:** full matrix < 90 seconds on a laptop, ~60 seconds
  on CI with cargo caches warm. Adjust by shrinking per-row readiness
  timeouts if it grows.
- **Determinism:** each row runs in its own scratch directory, own
  `FAKE_TMUX_DIR`, and unsets `COSMON_MOL_DIR` / `COSMON_PARENT_MOL_ID` so
  a harness invocation inside a cosmon worker does not cross-contaminate
  the scratch project.
- **Cleanup:** scratch dirs live under `$OUTDIR` (default
  `target/matrix-<pid>`). Artifacts are preserved on failure for
  post-mortem; delete the directory to reclaim disk.

## How this interacts with ef31 (the MVP fix)

`ef31` tightens the contract on the production side (stricter zombie
detection, more defensive spawn). This harness stays agnostic of `ef31`
details — it tests the contract observable from outside the binary. If
`ef31` changes the expected 5-tuple for a given fault, the matrix row's
`expected=` field is what gets updated, with a commit note pointing to
the ADR or chronicle that explains the new contract.

The harness is the long-lived part; `ef31` is a one-time ratchet.

## Future work

- **Forking mode** (`FAKE_TMUX_FORK=1`): actually exec fake-claude inside
  the session so `P` becomes a live-PID check, not a static claim. Adds
  per-row noise, so kept off by default.
- **Rust rewrite of the fakes**: if the bash grows past ~400 lines, port
  to a small Rust crate under `tests/fakes/`. Keep behavior bit-identical.
- **Feed into TLA+**: once the matrix stabilizes, the observed tuples
  become the transition relation in a TLA+ model of `cs tackle`.
