# Lifecycle smoke test — hello-world cosmon in 30 seconds

**Status:** introduced 2026-04-16 (task-20260416-f026) on top of the
03b5 fake-tmux + fake-claude shims and the ef31 spawn postcondition.
Lives in `tests/harness/full-lifecycle-smoke.sh` + `tests/harness/smoke/`.

## Why this exists

Phase 2 Docker coverage (task-20260416-bc3f) validated the **Inert**
commands — `cs nucleate`, `cs observe`, `cs reconcile`, `cs tag`,
`cs ensemble`. It deliberately skipped everything **Propelled**
(`cs tackle`, `cs wait`, `cs evolve`, `cs done`) because those require
`tmux(1)` and the `claude` CLI, neither of which are available in a
sealed container. The fakes delivered by 03b5 unblock this gap:
fake-tmux provides the subprocess-boundary oracle, fake-claude provides
a catalog of exit behaviors.

This harness is the **canonical hello-world for cosmon's full pilot
cycle** — `cs init` → `cs nucleate` → `cs tackle` → `cs complete` →
`cs wait` → `cs observe` → `cs done`. In under 30 seconds, on any
laptop, with zero external dependencies, it proves the whole loop is
sound.

It is not redundant with `run_matrix.sh`:

|                      | `run_matrix.sh`                           | `full-lifecycle-smoke.sh`          |
|----------------------|-------------------------------------------|------------------------------------|
| Scope                | `cs tackle` boundary only                 | `cs init` → `cs done` end-to-end   |
| What it asserts      | 5-bit spawn tuple (residual lie)          | post-cycle invariants (no worktree, no branch, no tmux, merged) |
| Fault injection      | 21 rows across tmux + claude combinations | 3 targeted scenarios (exit-42, hang, segfault) |
| Runtime              | ~40s                                      | ~13s for the full suite            |

## Layout

```
tests/
├── fixtures/
│   └── hello.formula.toml          # minimal 1-step formula — zero deps
└── harness/
    ├── full-lifecycle-smoke.sh     # happy path
    └── smoke/
        ├── _assertions.sh          # shared assertion lib
        ├── fault-exit-42.sh        # claude dies with exit 42
        ├── fault-hang.sh           # worker never drives; cs wait must time out
        ├── fault-segfault.sh       # claude SIGSEGVs during startup
        └── run_all.sh              # aggregate runner (CI entrypoint)
```

## Running it

```bash
# Full suite (happy + 3 fault injections)
bash tests/harness/smoke/run_all.sh

# Just the happy path
bash tests/harness/full-lifecycle-smoke.sh

# One variant
bash tests/harness/smoke/fault-hang.sh
```

The harness resolves `cs` in this order:

1. `$CS_BIN` (explicit override)
2. `target/debug/cs` in the repo (preferred — tests the code you just changed)
3. `command -v cs` (installed binary)

Artifacts land under `target/smoke-*/` — one subtree per run, preserved
on failure for post-mortem. Delete the directory to reclaim disk.

## The happy-path architecture

```
┌──────────────────────────────────────────────────────────────┐
│ scratch tmpdir (isolated git repo, isolated .cosmon)         │
│                                                              │
│  cs init .                  — scaffolds .cosmon/             │
│  cp hello.formula.toml      — drops the smoke formula        │
│  cs nucleate hello          — creates a pending molecule     │
│                                                              │
│  cs tackle <id>             — creates worktree + tmux session│
│  ╰─▶ fake-tmux records a session                             │
│     fake-claude exits 0 (harmless)                           │
│                                                              │
│  cs complete <id>           — *harness* plays the "worker"   │
│  cs wait <id> --timeout 15  — must return immediately        │
│  cs observe <id>            — assert status=completed        │
│                                                              │
│  cs done <id>               — merge + teardown               │
└──────────────────────────────────────────────────────────────┘
```

The "worker" step is simulated by the harness calling `cs complete`
directly — fake-claude is a stub that exits 0 without driving anything.
In a real run, claude would receive the injected prompt and call
`cs evolve` / `cs complete` from inside the worktree. The harness
replaces that behavior for determinism.

Post-conditions asserted after `cs done`:
- `.worktrees/<mol-id>/` removed
- `feat/<mol-id>` branch deleted
- fake-tmux state directory empty (no leftover `.meta` files)
- molecule status = `completed` in `state.json`

## The fault variants — boundary sentinels

Each fault script is a single-scenario assertion of a structural
invariant. They exist so a regression in `tackle.rs`, `wait.rs`, or the
pane-liveness oracle cannot slip past CI.

| Script              | Fault injection                                  | Contract asserted                                     |
|---------------------|--------------------------------------------------|-------------------------------------------------------|
| `fault-exit-42.sh`  | `FAKE_CLAUDE_MODE=exit-42` + `FAKE_TMUX_PANE_DEAD=1` | `cs tackle` exits non-zero with postcondition diagnostic; molecule is not `active` |
| `fault-hang.sh`     | `FAKE_CLAUDE_MODE=exit-0`; harness never completes | `cs wait --timeout 5` exits with 124 within ~15s (kubectl-wait, not kubectl-watch) |
| `fault-segfault.sh` | `FAKE_CLAUDE_MODE=segfault` + `FAKE_TMUX_SESSION_EXITED=1` | `cs tackle` exits non-zero; `fleet.json` carries no live worker entry |

The `exit-42` and `segfault` scripts encode the post-ef31 contract:
**tackle must not return 0 when claude failed to produce live output**.
Pre-ef31 these scripts fail — that's the point. The harness is the
ratchet that prevents task-4046 / task-25c3 from coming back.

## Pre-flight: build the cs binary

Most scripts prefer `target/debug/cs` over the installed binary. If you
changed cs and haven't rebuilt, you'll silently test the stale installed
binary.

```bash
cargo build --bin cs -p cosmon-cli --locked
bash tests/harness/smoke/run_all.sh
```

CI does this automatically (see `.github/workflows/ci.yml`,
`smoke-lifecycle` job).

## Runtime budgets

| Scope               | Target  | Rationale                                   |
|---------------------|---------|---------------------------------------------|
| Happy path alone    | <15s    | Deliberate: make `run` feel instant         |
| Full suite (4 scripts) | <60s | Fits comfortably inside a CI step           |
| Any single assertion | <5s    | If a single check crosses this, it's a bug  |

If the suite grows past ~2 minutes, shrink `COSMON_READINESS_TIMEOUT_SECS`
and `--timeout` bounds before adding more coverage. Long suites are
unused suites.

## Extending — how to add a new fault scenario

1. Pick the fault: a new `FAKE_CLAUDE_MODE` in `tests/fakes/fake-claude/README.md`,
   a new `FAKE_TMUX_*` env var, or a novel combination.
2. Copy `tests/harness/smoke/fault-hang.sh` (the simplest script) as a
   template. Rename it `fault-<short-name>.sh`.
3. Set the fault env vars, call `smoke_bootstrap_project`, nucleate,
   tackle, then assert what the contract says must hold.
4. Append the script path to `SCRIPTS=(…)` in `smoke/run_all.sh`.
5. Run `bash tests/harness/smoke/run_all.sh` locally. Iterate until the
   new script joins the green rows.

Add-a-scenario cost target: under 40 lines of bash plus one line in the
runner. Keep the scenario **single-purpose** — a script that asserts
three unrelated things is three scripts waiting to happen.

## Non-goals

- **Not a test of claude's prompt semantics.** fake-claude is a
  stand-in; we do not model what Anthropic's real CLI outputs.
- **Not a full tmux conformance test.** fake-tmux covers the subset
  cosmon actually calls. Real tmux edge cases belong in Docker-based
  integration tests.
- **Not a Rust unit-test replacement.** Domain state machines are tested
  in `cosmon-core`; this harness tests the **shell**-level pilot
  protocol that the Rust test framework cannot reach.

## Why the harness drives `cs complete` instead of fake-claude

fake-claude is stateless and has no access to `COSMON_MOL_DIR` in the
general case (env is consumed by tmux before fake-claude runs).
Asking fake-claude to "drive the molecule to completion" would
require reimplementing the in-worker lifecycle — defeating the
minimal-shim design principle.

Instead, the harness plays worker: after `cs tackle` has set up the
tmux session and the worktree, the harness calls `cs complete` from
the project root. This is exactly the protocol a real claude worker
would execute. We are testing the state-machine contract, not
claude's behavior.

## Interplay with `run_matrix.sh` and ef31

- `run_matrix.sh` (03b5) — probes the **spawn** surface with a 5-bit
  residual-lie tuple. Good at detecting whether cosmon's bookkeeping
  contradicts reality at the moment of tackle.
- `full-lifecycle-smoke.sh` (this file, f026) — probes the **whole
  pilot loop** end-to-end. Good at detecting whether the cycle can
  actually close (merge happens, worktree vanishes, branch disappears,
  tmux goes quiet).
- ef31 tightens the production-side contract (`observe_spawn_postcondition`).
  Its MVP fix lets `fault-exit-42` and `fault-segfault` pass — they
  assert exactly what ef31 promises.

Together they cover the full `cs tackle` → `cs done` journey, with
different magnifications.
