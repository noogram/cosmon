# Test-suite wall clock — where the 7 minutes actually go — 2026-08-01

**Molecule:** `task-20260731-44b0` · **Kind:** study, no fix bundled

> **Superseded in part, 2026-08-07.** Recommendation 3 (consolidate the four
> `cosmon-cli` rig groups) was built and measured, and returned 5% against its
> own 20% threshold: `cosmon-cli` had fallen from 86% of the bill to 18%. See
> `cli-consolidation-refuted-2026-08-07.md`. Findings 1–3 and both protected
> lists stand; recommendation 2 is now the largest single lever in the gate.
**Question answered:** *what would make `cargo test --workspace` significantly
faster without weakening what it proves?*

Every number below was produced on this machine during this study. Nothing is
extrapolated from a model of the suite; where a number could not be measured
under honest conditions, the section says so instead of guessing.

## Badge (attach to every number)

| | |
|---|---|
| repo | `971f75c` (v0.5.0), worktree `.worktrees/task-20260731-44b0` |
| host | Apple M4 Max, 16 cores, macOS 26.5.2 |
| toolchain | pinned `stable` (`rust-toolchain.toml`), default `dev` profile unless stated |
| window | 2026-08-01, 00:30–03:40 local |
| ambient load | **25–275** on 16 cores — this box runs the agent fleet, and neighbours moved during the study. Each run below carries the load it ran under. |

The ambient load is not a footnote, it is the first finding: an early
consolidation experiment measured a 1.16× gain and the honest reading of it was
*"the host had two spare cores to give"*, not *"the design does not help"*. Wall
figures are therefore only ever compared **back to back**, and CPU-time is used
wherever a load-robust quantity is needed.

## What the suite is made of

`cargo test --workspace` builds **289 libtest binaries** and runs **44 doctest
suites**; **7473 tests pass**, 0 fail.

| target kind | targets | tests | share of measured CPU |
|---|---:|---:|---:|
| integration (`tests/*.rs`) | 232 | 1150 | **91 %** (1054 s) |
| bin (`cs`, in-`main.rs` tests) | 13 | 1658 | 8 % (94 s) |
| lib (unit tests) | 43 | 4350 | **1.2 %** (14 s) |
| doctests | 44 | 116 | 33 s wall, profile-insensitive |

Read that table twice: **4350 unit tests cost 14 seconds; 1150 integration
tests cost 1054 seconds.** One integration test is worth ~280 unit tests.

Concentration is extreme. Of the measured CPU, the **top 10 binaries hold 67 %**
and the top 30 hold 94 %. `cosmon-cli` alone is **86 %** (1003 s of 1162 s).

## Finding 1 — the per-binary *fixed* cost is 11 ms, so binary count is not the bill

Measured by running every binary with a filter that matches no test
(`--exact __cosmon_nonexistent_test__`): exec + static init.

* median floor: **11 ms**
* all 289 binaries together: **4.2 s wall / 1.8 s CPU** — 0.3 % of the suite.

So the hypothesis "333 binaries × expensive setup" is **false as stated**. There
is no meaningful per-*binary* setup; there is expensive per-*test* rig work
(temp galaxies, git repos, tmux servers, real `cs` invocations) that happens to
be spread thin across many small binaries. That distinction decides everything
downstream: consolidation cannot save setup that does not exist — it can only
buy **parallelism**, because libtest runs the tests *inside* one binary on
threads while cargo runs the binaries themselves one after another.

## Finding 2 — the cost is unoptimised code, not process count

An instrumented probe (a counting shim installed over `target/debug/cs`,
recording every spawn, then removed) contradicted the intuition that these
tests are spawn-storms:

| binary | real `cs` spawns per run |
|---|---:|
| `cosmon-cli/cli` | 69 |
| `spec_audit_multi` | 9 |
| `doctor` | 9 |
| `fleet_resolve` | 8 |
| `opt_in_share` | 7 |
| `help_goldens` | 10 |

Nine spawns, 50 s of CPU. So the price is per *invocation*, and it is large:

| command | `debug` (opt-level 0) | `release` | ratio |
|---|---:|---:|---:|
| `cs __help-tree` | 8.07 s user | 0.62 s | **13×** |
| `cs doctor leaks` | 10.30 s user | 1.78 s | **5.8×** |

The workspace `Cargo.toml` carries **no `[profile]` section at all**, so every
test — and every `cs` child a test spawns — runs at `opt-level = 0`.

*Hypothesis killed on the way (recorded as data):* "`cs` startup is a fixed
multi-second cost" — refuted. `cs --version` is 0.01 s; `cs observe` against an
isolated `COSMON_STATE_DIR` is 0.04–0.09 s. The 3.2 s reading that suggested a
startup tax was `cs observe` walking up to the **live** galaxy, which no test
does.

## Finding 3 — the profile lever, measured end to end with the real gate

`[profile.dev] opt-level = 1`, then the contract command itself, twice, back to
back on the same host (debug artifacts stayed cached, so reverting cost 1.1 s):

| run | wall | suite self-time | doctests | result |
|---|---:|---:|---:|---|
| `cargo test --workspace --no-fail-fast`, opt-level 0 (load 29→26) | **411.7 s** | 383.5 s | 32.7 s | 7473 passed, 0 failed |
| same, opt-level 1 (load 43→35) | **286.0 s** | 266.1 s | 32.3 s | **7473 passed, 0 failed** |

**−126 s, 1.44×, for a two-line diff, with the same 7473 assertions passing.**
`debug-assertions` stays on at `opt-level = 1`, so no test is weakened: the
change alters codegen, not semantics.

Cost side, measured: the profile switch forces one full rebuild of the test
world — **295 s wall / 2153 s CPU**. On a warm developer machine that is paid
once; on a cold CI runner it is paid every run and must be measured there
before the change is adopted for CI. This study did not measure CI.

**Two tests get worse, and they are worth naming** (they are the only
regressions in the whole set):

| target | opt0 | opt1 | why |
|---|---:|---:|---|
| `cosmon-filestore/trunk_lock_concurrent` | 10.5 s | **27.1 s** | calls `cargo build --example trunk_lock_holder` *from inside a test* |
| `cosmon-crashtest/recovery` | 10.2 s | **21.6 s** | same shape — a nested cargo build |

A test that shells out to `cargo` inherits the profile it is measuring. After
the profile change these two become the two most expensive targets in the
suite; pre-building the helper (dev-dependency artifact, `build.rs`, or an
`examples` target cargo already builds) is the natural follow-up and is worth
roughly −20 to −35 s at opt-level 1.

## Finding 4 — the consolidation ceiling, measured rather than modelled

Merging K small binaries that share a rig into one binary converts K sequential
rig boots into K concurrent ones. The ceiling of that plan can be measured
without writing the merge: run the same binaries through a pool of N workers,
each binary `--test-threads=1`, so exactly N tests are in flight — which is what
one consolidated binary with N libtest threads would do.

All rows below: opt-level 1 binaries, 289 targets, ambient load 25–45.

| execution | wall | speedup vs sequential |
|---|---:|---:|
| sequential, libtest defaults (what cargo does) | 329.3 s / 311.3 s (two runs) | 1.00× |
| 16-way binary pool | **167.3 s** | **1.97×** |
| 8-way binary pool | 168.2 s | 1.96× |
| 16-way, excluding `trunk_lock_concurrent` | 162.3 s | 1.94× (vs 314.3 s seq) |
| 16-way on the opt-level 0 binaries | 183.2 s | 1.80× (vs 330.3 s seq) |

Three things to take from it:

1. Consolidation is worth about **2×**, not the 5–8× a naive "1162 s CPU ÷ 16
   cores = 73 s" model predicts. The suite does not have 16 cores' worth of
   demand at any instant.
2. **8 workers already saturate it** — width beyond 8 buys nothing, so the
   binding constraint is the tail (a handful of long, mostly-waiting tests), not
   the pool.
3. The same experiment run earlier at ambient load 100–200 returned 1.16×.
   Same code, same binaries. That number measured the neighbours.

This also explains the earlier nextest result quoted in the mission (18.6 min
local, ~6 h extrapolated on the runner): with an 11 ms process floor but heavy
*per-test* rig work, process-per-test multiplies exactly the cost that
dominates. The executor was never the lever.

## The table the mission asked for

Integration targets grouped by crate and by the rig they boot — this is the
consolidation unit, because a merged binary can only share a rig its members
agree on. Times are gate self-times from the two full runs above.

| crate | shared rig | binaries | tests | opt0 | opt1 | consolidation verdict |
|---|---|---:|---:|---:|---:|---|
| cosmon-cli | `cs` | 23 | 123 | 68.1 s | 30.4 s | **merge** — biggest single win |
| cosmon-cli | `cs`+git | 12 | 61 | 41.8 s | 26.0 s | **merge** — one temp repo factory |
| cosmon-cli | tmux+`cs`+git | 10 | 70 | 57.4 s | 25.8 s | **merge**, one tmux server per binary, session per test |
| cosmon-cli | tmux+`cs` | 9 | 34 | 51.9 s | 23.1 s | **merge**, same rig |
| cosmon-filestore | pure | 2 | 6 | 10.7 s | 27.3 s | do **not** merge — fix the nested cargo build first |
| cosmon-crashtest | pure | 1 | 1 | 10.2 s | 21.6 s | same |
| cosmon-runtime | pure | 16 | 32 | 11.9 s | 12.2 s | low value (12 s total) |
| cosmon-daemon-supervisor | pure | 6 | 15 | 7.8 s | 9.3 s | low value |
| cosmon-core | pure | 16 | 103 | 5.9 s | 4.4 s | no |
| cosmon-thin-cli | pure | 9 | 48 | 11.6 s | 4.2 s | no |
| cosmon-rpp-adapter | pure | 24 | 157 | 4.4 s | 3.9 s | no |
| cosmon-cli | `cs`, **env-mutating** | 1 | 2 | 14.2 s | 3.5 s | **blocked** — see below |

The four cosmon-cli rows are the whole story: **54 binaries, 288 tests, 219 s at
opt-level 0 / 105 s at opt-level 1**, all booting variations of the same three
rigs.

A shared-rig fixture (one tmux server per binary, one session per test; one
temp-galaxy factory instead of one galaxy per binary) is the natural companion
to the merge — but note what the floor measurement says: it buys **concurrency**,
not saved setup. The setup is per test either way.

## What must not be weakened

These are not candidates for a lighter double, whatever the merge plan does.

**Real process groups / signals — the isolation *is* the assertion:**
`cosmon-cli/briefing_backstop_survival` (the backstop must survive its caller
being killed), `cosmon-cli/zombie_prevention`, `cosmon-cli/pane_died_hook`,
`cosmon-cli/consent_non_blocking` (allocates a real pty),
`cosmon-cli/realized_watch_reexec`, `cosmon-runtime/sigint_race_suppresses_spurious_error`,
`cosmon-daemon-supervisor/{signal_cascade,double_spawn_on_restart,crash_loop_propulsion_down,spawn_failure_isolated}`,
`cosmon-core/autonomy_attacks`, `cosmon-rpp-adapter/v1_tackle_ceiling`.
They may be merged into a shared binary — threads do not weaken a process
group — but they must keep spawning real processes and real tmux servers.

**Process-global state — cannot share a process with anything, merged or not
(14 files):** every test calling `std::env::set_var` / `remove_var`:
`cosmon-cli/tag_single_writer`, `cosmon-thin-cli/operator_ux`,
`cosmon-remote/{credential_env,wire_contract,oidc_env_gate}`,
`cosmon-observability/canonical_snapshot`,
`cosmon-runtime/backlog_sanity_guard`,
`cosmon-rpp-adapter/{image_init_test,subprocess_env_hygiene}`,
`cosmon-transport/claude_integration`,
`cosmon-agent-harness/{exec_command_smoke,exec_command_egress,exec_command_netns_e2e,local_research_tools_smoke}`.
In a merged binary these race with their neighbours. They stay in their own
binary, or the env mutation is replaced by explicit injection first. (No test
mutates the process cwd — that hazard does not exist here.)

**`cosmon-filestore/trunk_lock_concurrent`** deserves its own line: it asserts
that two *processes* serialise on the trunk lock. An in-process double would
assert nothing.

## Recommendation, with the expected total

Ordered by measured gain per unit of risk. Each step's number is the measured
one, not a projection.

1. **`[profile.dev] opt-level = 1`** — **412 s → 286 s** measured end to end,
   7473/7473 still passing, two-line diff, no test touched. Adopt after one
   CI-side measurement of the cold-build cost (this study measured the local
   rebuild at 295 s wall / 2153 s CPU, paid once locally, per-run on a cold
   runner).
2. **De-nest the two tests that run `cargo build` inside a test**
   (`trunk_lock_concurrent`, `recovery`) — they are the only two regressions of
   step 1 and become the suite's two most expensive targets after it.
   Expect ≈ **−20 to −35 s**, landing the suite near **250 s**.
3. **Consolidate the four cosmon-cli rig groups** (54 binaries → ~4, shared-rig
   fixtures) — measured ceiling **1.9×** on top of the above, i.e. roughly
   **250 s → ~130–160 s**. Highest effort, and the env-mutating and
   process-group constraints above bound how far it can go.

**Expected total: 412 s → ~150 s (≈2.7×)**, of which the first 31 % is one
line of TOML.

What this study did **not** establish, stated plainly rather than assumed:
per-test timings inside the heavy binaries (libtest's `--report-time` is
nightly-only, and timing every test individually was not affordable in the
window); the CI-side cost of step 1; and any number at all on an idle host —
every figure here was taken on a machine sharing 16 cores with a live agent
fleet, which is why the ratios, not the absolutes, carry the conclusions.

`cargo test --workspace` remains the gate contract throughout: none of the three
steps changes the command, only what it compiles and how the tests are grouped.

## Reproducing it

The measurement scripts are small and were kept deliberately dumb (one process,
one number): a per-binary sweep recording exec floor, wall and child-CPU
(`RUSAGE_CHILDREN` deltas); a sequential control that does what cargo does; and
a pool runner that emulates a consolidated binary at N threads. The raw
per-binary tables and the two gate logs are attached to the molecule
(`task-20260731-44b0`).
