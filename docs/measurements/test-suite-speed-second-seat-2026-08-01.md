# Cargo test workspace speed study — independent second seat

Date: 2026-08-01  
Build: `971f75c53ae018691339acd246a68f909fdd1845`  
Scope: study only; no test or product code changed.

## Verdict

Keep `cargo test --workspace` as the gate and consolidate integration test
source files into semantic harnesses. The first target should be approximately
70 integration harnesses instead of 237 integration files, while retaining all
44 doctest invocations and the process/signal/tmux isolation binaries listed
below. This takes the total gate from 333 executable invocations to roughly
165 (about 121 native libtest executables plus 44 doctest invocations).

The measured four-thread scheduling model over 1,377 tests in 99 fully traced
binaries reduced execution from 1,306.9 s to 924.0 s when tests were grouped by
crate: **70.7% of current time, a 29.3% reduction**. This is a ratio, not an
absolute forecast: the machine was heavily contended. Applying the ratio,
protected-test carve-outs, and the measured launch reduction to the supplied
clean baseline predicts **about 7.0 minutes local (conservative range 6.6–7.5
minutes)**. Applying the same conservative factor to the supplied 5.5–9 minute
CI range predicts **3.9–6.3 minutes**.

Shared rigs are a secondary optimization. A fresh tmux server/session lifecycle
cost 74.6 ms mean, while adding a session to an existing server cost 16.3 ms.
A minimal git init/commit/worktree rig cost 273.3 ms; adding a worktree to an
existing base repository cost 87.1 ms. Reusing a base saves about 58 ms per tmux
server and 186 ms per git rig, but does not approach the gain from removing
binary boundaries and exposing tests to libtest's four-way CI scheduler.

## Measurement badge and limits

Every number above and below has this badge unless stated otherwise:

| Field | Value |
|---|---|
| build hash | `971f75c53ae018691339acd246a68f909fdd1845` |
| device | Apple M4 Max, 16 cores, 128 GiB, Darwin 25.5.0 |
| target | isolated `/tmp/cosmon-817a-target` |
| test load | 7,158 native tests listed; supplied clean baseline is 7,473 tests in 333 invocations |
| sample | 289 native test artifacts (288 runnable harnesses plus one proc-macro artifact); 113 full binary traces; 1,377 per-test durations in 99 non-empty binaries; seam microbenchmarks n=10 (cs spawn n=20) |
| load | start 53/71/91; peaked above 345/264/199 during concurrent fleet activity |

The host load makes absolute full-test durations unsuitable as a clean baseline.
It does not invalidate binary counts, test counts, the setup/test separation, or
the four-thread scheduling ratio. The supplied 10-minute local and 5.5–9-minute
CI baselines are therefore the denominators for the expected-total forecast.
One binary, `consent_non_blocking`, failed its own 30-second deadline under host
starvation; that failure is retained in the raw data and was not retried away.
The final normal-layout workspace gate later passed both consent tests in 8.64 s,
refuting a product regression and confirming that the failed trace measured host
starvation.

### Verification cross-check

The complete `cargo test --workspace --no-fail-fast` contract passed in the
repository's normal target layout, including the protected process-group,
signal-race, zombie-prevention, consent, and single-writer families. An earlier
full run with `CARGO_TARGET_DIR=/tmp/cosmon-817a-target` passed every target
except two tests in `trunk_lock_concurrent`: those tests launch a nested Cargo
build and then look for its helper below the repository-default `target/`, so
the helper was not found. The same binary passed all three tests when run in the
normal layout. This is an isolated-target compatibility defect in the test
harness, not evidence against the product or the speed-study recommendation;
no fix is bundled into this study.

The hypothesis-killer was: if binary boundaries are not the lever, native
`--list` launch time should be small and grouping the same measured tests onto
four lanes should approach a ratio of 1. Instead, sequential native launch/list
took 247.2 s under contention and the grouping ratio was 0.707. Conversely, the
tmux-only shared-rig hypothesis is weakened by its measured 58 ms saving.

## Ranked binary table

`Setup` is measured external process/loader/harness time (`wall - libtest suite
time`). `Test` is libtest's internal suite time. Both are seconds from the
contended run, so rank and ratios matter more than absolute values. Expected
gain is for the indicated semantic group on a four-thread runner, not permission
to weaken that binary's assertions.

| Rank | Binary | Setup | Test | Consolidation candidate | Expected gain |
|---:|---|---:|---:|---|---|
| 1 | `mindguard_surface_visual` | 0.235 | 151.397 | CLI real-subprocess/state group, unique temp roots | 20–30% group tail reduction |
| 2 | `tag_single_writer` | 0.384 | 123.158 | **No parallel merge**; keep serialized/isolated | 0%; correctness boundary |
| 3 | `help_goldens` | 0.298 | 99.481 | read-only CLI help/surface harness | tail fill + one launch; retain real-bin smoke |
| 4 | `fleet_resolve` | 0.164 | 78.777 | CLI config/resolution harness | 20–30% group tail reduction |
| 5 | `test_migrate_genre` | 0.375 | 74.660 | shared git-base migration harness | 20–30% + ~0.186 s/reused git base |
| 6 | `demo` | 0.137 | 60.125 | CLI TTY/argument subprocess harness | tail fill; retain process boundary |
| 7 | `tracing_subscriber_installed` | 0.151 | 58.413 | CLI environment/subprocess harness | tail fill; child-local env only |
| 8 | `api_cli_coverage` | 0.173 | 50.464 | read-only CLI surface harness | high-confidence consolidation |
| 9 | `readme_cli_table` | 0.400 | 47.752 | read-only CLI help/surface harness | high-confidence consolidation |
| 10 | `recovery` | 0.198 | 43.883 | **No useful consolidation**; one long crash test | ~0%; investigate separately |
| 11 | `committee_seat_dispatch` | 0.277 | 43.136 | CLI git/config harness with unique roots | 20–30% group tail reduction |
| 12 | `briefing_backstop_survival` | 0.428 | 38.198 | **Protected isolation binary** | 0%; real process-group proof |
| 13 | `zombie_prevention` | 0.134 | 39.137 | **Protected isolation binary** | 0%; real tmux/process exit proof |
| 14 | `consent_non_blocking` | 0.250 | 30.061 | **Protected PTY/deadline binary** | 0%; failed under measured starvation |
| 15 | `local_model_selection` | 0.256 | 27.729 | config/library double for pure cases + one real-bin smoke | 20–30% group tail reduction |
| 16 | `tackle_inprocess_completion` | 0.359 | 23.563 | keep subprocess/wait protocol cases isolated | small launch-only gain |
| 17 | `tackle_inprocess_no_harvest` | 0.388 | 16.490 | keep lifecycle residue assertions isolated | small launch-only gain |
| 18 | `signal_cascade` | 0.202 | 6.089 | **Protected signal binary** | 0%; real SIGTERM→SIGKILL proof |
| 19 | `local_adapter_output_honesty` | 0.420 | 4.903 | CLI adapter-output harness | 20–30% group tail reduction |
| 20 | `sigint_race_suppresses_spurious_error` | 0.222 | 2.407 | **Protected signal binary** | 0%; real signal-killed subprocess proof |

The partial four-thread model's largest measured group changes were:

| Group | Current four-lane model | Consolidated | Reduction |
|---|---:|---:|---:|
| `cosmon-cli` (34 traced binaries) | 1,228.2 s | 854.3 s | 30.4% |
| `cosmon-daemon-supervisor` (7) | 11.1 s | 6.1 s | 45.0% |
| `cosmon-remote` (3) | 9.1 s | 7.5 s | 18.4% |
| `cosmon-agent-harness` (5) | 1.0 s | 0.4 s | 58.7% |
| all 99 traced non-empty binaries | 1,306.9 s | 924.0 s | 29.3% |

Raw tables beside this report contain all 289 launch/list rows and all 113 full
binary rows: `list-timings.tsv`, `run-timings-partial.tsv`,
`protected-timings.tsv`, `crate-integration-counts.tsv`, and
`direct-cs-test-files.tsv`.

## Consolidation design

Do not create one giant integration binary. Create semantic harnesses whose
fixtures and global-state requirements match:

1. `cosmon-cli`: read-only help/surface; config/resolution; state transitions;
   git/worktree operations; adapter/tackle subprocess; and separate protected
   PTY/tmux/process-group binaries. Target roughly 18–22 harnesses from 66 files.
2. `cosmon-rpp-adapter`: HTTP read routes, HTTP mutation routes, auth/OIDC,
   event streams, artifact/result serving, and a small subprocess/security
   group. Target 6–8 from 32 files.
3. `cosmon-runtime`: pure policy/guard tests, resident-loop fake-`cs` tests,
   real-`cs` protocol, and signal/process tests. Target 5–6 from 20 files.
4. `cosmon-core`, `provider`, `remote`, `transport`, and `thin-cli`: group by
   existing shared fake server/fixture; retain live/ignored and fixed-global
   tests separately. Target 25–30 harnesses from their 67 files combined.

This lands near 70 integration harnesses. Implement with a harness `main.rs`
whose `mod` declarations point to the existing source modules, so assertions
remain byte-for-byte reviewable during the mechanical first step. Resolve
module-name collisions explicitly. Only after that green change should shared
rig fixtures be introduced.

For shared rigs, use one uniquely named tmux server per harness and one unique
session per test. Use one immutable committed git base per harness and a fresh
branch/worktree per test. Never share mutable molecule state, worktree paths,
session names, environment variables, current directory, signal handlers, or
ports. A fixture drop guard must clean its own namespace only. Tests mutating
process-global environment or handlers must use a lock or remain isolated; do
not rely on `--test-threads=1` for the whole workspace because that gives back
the intended parallelism.

## Real `cs` inventory and lighter doubles

There are **56 integration binaries containing 303 tests** with a direct
`CARGO_BIN_EXE_cs`/`cargo_bin("cs")` dependency (65 direct source call sites).
This is the defensible static count of tests living in binaries that spawn the
real executable; helper indirection prevents claiming that all 303 individually
execute it on every branch without an exec-trace probe.

A lighter double is appropriate for:

- help-tree, README table, command registry, and generated-reference assertions:
  call the Clap command factory/library representation, retaining one real
  executable smoke test for argv/exit/stdout wiring;
- pure config/model/fleet resolution: call the command handler against injected
  filesystem/config ports, retaining walk-up and environment-boundary smokes;
- deterministic state migration/projection logic: use in-memory or temp-backed
  ports, retaining one real subprocess test per public command for exit status,
  locking, and on-disk compatibility.

Do not use a lighter double for claims about process lifetime, process groups,
signals, PTYs, tmux hooks, detached children, executable discovery, environment
inheritance, locking between processes, or crash recovery. The domain core is
already I/O-free; the double belongs at its existing injectable ports, not as a
second imitation of `cs` behavior.

## Tests whose isolation must not be weakened

These may be placed under a common directory for discoverability, but they must
remain separate libtest executables or explicitly serialized namespaces with the
same real OS/process assertions:

- `briefing_backstop_survival::{the_backstop_keeps_pressing_after_its_caller_is_killed,
  a_backstop_that_gives_up_leaves_an_annotated_record}` — measured 38.2 s suite;
  the first proves survival of a real process-group SIGKILL.
- `pane_died_hook::{pane_died_emits_worker_exited_and_projects_dead_witness,
  kill_dash_nine_writes_worker_exit_and_stales_process_status,
  pane_died_triggers_harvest_which_purges_fleet_when_completed}` (ignored in the
  default gate, real tmux when explicitly run).
- `purge_stale_tmux::purge_reclaims_worker_after_tmux_session_killed`.
- `zombie_prevention::cs_tackle_refuses_to_lie_when_claude_exits_nonzero` —
  measured 39.1 s suite.
- `tmux_parallel_send::send_input_does_not_cross_wire_under_parallelism`
  (ignored in the default gate; intentionally tests concurrency).
- `signal_cascade::{shutdown_escalates_sigterm_ignore_to_sigkill,
  shutdown_polite_child_terminates_before_grace}`.
- `sigint_race_suppresses_spurious_error::{signal_killed_ensemble_yields_clean_shutdown_trace,
  signal_killed_done_subprocess_yields_clean_shutdown_trace}`.
- `consent_non_blocking::{consent_with_tty_stdin_and_captured_stdout_does_not_block,
  tackle_never_asks_a_consent_question}` (real PTY and deadline).
- `tag_single_writer::{single_writer_in_process_vs_subprocess_keeps_state_valid,
  sequential_in_process_then_subprocess_round_trips}`.
- the real-subprocess lifecycle families in `tackle_inprocess_completion`,
  `tackle_inprocess_no_harvest`, `refused_root_dispatch_leaves_no_residue`, and
  `restart_fidelity_no_neurion`.

## Recommended implementation sequence

1. Add a small measurement script that records binary wall, libtest suite time,
   test count, build hash, machine, and load. Keep `cargo test --workspace` as
   the invoked contract.
2. Mechanically consolidate the read-only CLI and RPP groups first. No fixture
   changes in the same commit. Validate that test names/counts and ignored flags
   are unchanged.
3. Consolidate the remaining safe groups, with unique ports/sockets/temp roots.
   Run the four-thread model and the real gate after each crate.
4. Introduce shared git bases, then shared tmux servers, as separate changes.
   Their measured ceiling is tens of seconds, so they should not block the
   higher-value consolidation.
5. Replace only pure command-handler invocations with port-level doubles and
   retain one real executable boundary test per command family. Recount the 303
   real-`cs`-linked tests after each change.

Success criterion: the same 7,473 tests, doctests, ignored status, assertions,
and protected real-process proofs pass through `cargo test --workspace`, with a
clean four-vCPU run at or below 7.5 minutes and a stretch target near 6.6
minutes. If the first consolidation does not reduce a clean run by at least
20%, the hypothesis is falsified and shared-rig work should stop pending a new
trace.
