<!-- SPDX-License-Identifier: AGPL-3.0-only -->
# Test-suite speed — two independent seats, one verdict — 2026-08-01

Two workers studied the same question without seeing each other's worktree:
`task-20260731-44b0` (claude-opus-5) wrote
[test-suite-wall-clock-2026-08-01.md](test-suite-wall-clock-2026-08-01.md);
`task-20260731-817a` (gpt-5.6-sol) wrote
[test-suite-speed-second-seat-2026-08-01.md](test-suite-speed-second-seat-2026-08-01.md).
The independence was the point: agreement is evidence, and the one direct
contradiction between them is attributable.

## Where they agree, independently

- **Keep `cargo test --workspace` as the gate contract.** Both, explicitly.
- **nextest / process-per-test is an anti-lever here** — per-test rig work
  dominates, so multiplying processes multiplies exactly the wrong cost.
- **The same protected families must not be weakened**: both name
  `briefing_backstop_survival`, `zombie_prevention`, `consent_non_blocking`,
  `tag_single_writer`, the signal binaries — merged or not, they keep spawning
  real processes, real tmux, real PTYs.
- **Consolidation of integration binaries is worth doing** and its unit is the
  shared rig, not the crate.

## The contradiction, and how it resolves

The second seat measured sequential native launch/list at 247.2 s and treated
binary boundaries as the primary lever (modelled ratio 0.707). The first seat
measured the per-binary exec floor directly with a no-match filter: **11 ms
median, 4.2 s for all 289 binaries** — and its badge shows why the numbers
differ: the second seat ran at load 199–345, the first at 25–45. The 247 s
measured the neighbours, not the binaries. Where the seats measured the same
thing under comparable conditions (the consolidation ceiling), they agree:
pool emulation says **~2×**, not the 5–8× a CPU-division model suggests.

The second seat did not test the compiler profile at all — the first seat's
dominant lever (`opt-level = 1`, measured −31 % end to end on the real gate,
7473/7473 passing) was simply outside its search space. One seat's blind spot
is the other's headline; that is what the second seat was for.

## The merged plan (implementation molecules filed 2026-08-01)

1. `[profile.dev] opt-level = 1` — measured 412 s → 286 s locally; adopt for CI
   only after one CI-side cold-build measurement.
2. De-nest the two tests that run `cargo build` from inside a test
   (`trunk_lock_concurrent`, `recovery`) — the only regressions of step 1.
3. Consolidate the cosmon-cli rig groups (measured ceiling ≈ 1.9×), honouring
   both seats' protected lists and the 14 env-mutating files that cannot share
   a process.

Expected total: **412 s → ~150 s (≈ 2.7×)**, first 31 % from one line of TOML.
Falsifier (second seat's, kept): if the first consolidation step does not cut a
clean run by ≥ 20 %, stop the shared-rig work and re-trace.
