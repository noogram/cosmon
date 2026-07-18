# Diagnostic — `cargo test --workspace` exceeds the 600 s gate envelope

**Date:** 2026-07-18 · **Molecule:** `task-20260718-9d39` (round-3
realized-model) · **Trigger:** the round-2 re-pre-mortem
(`task-20260718-37fc`) observed `timeout 600 cargo test --workspace` exit 124
with zero failures and asked: *suite too big, or a slow/hanging test?*

## Verdict: the suite is too big for the envelope — no hang

Reproduced deterministically on 2026-07-18 (Apple Silicon dev machine, debug
profile). The run makes steady forward progress and every completed suite
passes; the wall-clock simply exceeds 600 s. One named slow test exists, but
it *passes* — it is the loudest instance of a structural pattern, not a hang.

## Measurements

| Observation | Value |
|---|---|
| `#[test]` markers across the workspace | 5 984 |
| Integration-test binaries (`crates/*/tests/*.rs`) | 206 |
| Cold build of the test profile alone | **7 min 21 s** (measured — already > 600 s before one test runs) |
| Warm run, 600 s budget | build 43 s, then ~72 suite results, 0 failures, killed mid-suite |
| `cosmon-cli/tests/energy_budget.rs` alone | **≈ 85 s**, passes (measured twice: 84.4 s / 86.6 s) |
| One `cs --json nucleate` subprocess (debug build, quiet temp dir) | ≈ 9 s wall / 3 s CPU |
| `cs --help` (binary startup) | 0.04 s |

## Root causes, in order of weight

1. **Subprocess-spawning integration tests on a debug binary.** Dozens of
   `cosmon-cli/tests/*.rs` binaries drive the real `cs` executable via
   `Command::new(env!("CARGO_BIN_EXE_cs"))`. A single `cs nucleate`/`cs
   evolve` invocation costs seconds (unoptimized debug codegen + real
   filesystem/git/event work), and tests chain 4–6 invocations —
   `energy_budget.rs` alone is ~85 s. Multiplied across the `cs`-driving
   binaries this dominates the budget. No single test hangs;
   `evolve_refuses_step_past_energy_budget_and_marks_stuck` (the one that
   trips libtest's "> 60 seconds" warning) completes and passes.
2. **Binary-count serialization.** `cargo test` runs the 206 test binaries
   one at a time (parallelism is only intra-binary), so per-binary startup
   and the slow `cs`-subprocess binaries serialize.
3. **Environmental gotcha (measurement, not suite):** any concurrently
   running `cargo` (clippy, check, another test) holds the artifact-dir file
   lock; the gate then burns its entire `timeout` budget printing `Blocking
   waiting for file lock` and exits 124 having run *nothing*. A 124 from the
   gate is only meaningful if the target dir was uncontended.

## Recommendations (for a follow-up molecule)

- **Adopt `cargo-nextest` for the test gate.** Binary-level parallelism
  collapses cause 2, and its per-test timeout (e.g. 120 s) converts any
  future genuine hang into a named failure instead of a silent gate 124.
- **Tier the gate**: `cargo test --workspace --lib` (fast, seconds) as the
  inner-loop gate; the `cs`-subprocess integration tier under nextest with a
  larger envelope (or a dedicated `timeout 1200`).
- **Trim per-invocation cost of `cs` in tests** where cheap: several tests
  re-spawn `cs` for steps that could share one invocation; and the ~6 s of
  non-CPU wall in a quiet-tempdir `nucleate` deserves its own instrumented
  look (suspect: filesystem walks / git probing).
- **Never run the gate concurrently with another cargo process** (cause 3).

Raw logs from the measurement runs are archived in the molecule directory of
`task-20260718-9d39` (`test-gate.log`, `energy_budget_timing.log`).
