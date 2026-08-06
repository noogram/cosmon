<!-- SPDX-License-Identifier: AGPL-3.0-only -->
# Consolidating the cosmon-cli rigs — refuted, and where the clock went instead — 2026-08-07

**Molecule:** `task-20260801-5220` (TEST-SPEED step 3/3) · **Outcome:** the
planned change was built, measured, and **not shipped**.

Step 3 of the 2026-08-01 recommendation was: merge the 54 `cosmon-cli`
integration binaries that share a rig into ~4 harnesses, for a measured ceiling
of 1.9×. The step carried its own falsification clause, from the second seat's
success criterion: *if the first consolidation does not reduce a clean run by at
least 20%, the hypothesis is falsified and shared-rig work should stop pending a
new trace.*

It was built. It reduced a clean run by **5%**. This is the new trace.

## Badge

| | |
|---|---|
| repo | `59c7e9ea`, worktree `.worktrees/task-20260801-5220` |
| host | Apple M4 Max, 16 cores, macOS 25.5.0 |
| toolchain | pinned `stable`, `dev` profile — which now carries `opt-level = 1` (`a24e4081`) |
| window | 2026-08-07, 00:20–01:05 local |
| ambient load | 7–22 on 16 cores; every comparison below is back-to-back, alternating arms |
| worker env | `CB_DEPTH`, `ANTHROPIC_MODEL`, `COSMON_EGRESS_POLICY` stripped from every run — left in place they turn `cs`-spawning tests red for reasons that have nothing to do with the code |

## What was built

`autotests = false` on `cosmon-cli`, plus 15 explicit `[[test]]` targets: four
rig harnesses (`rig_cs` 32 suites, `rig_cs_git` 13, `rig_tmux_cs` 8,
`rig_tmux_cs_git` 4) and eleven protected suites left as their own executables.
Each harness is nothing but `#[path = "<suite>.rs"] mod <suite>;` lines, so the
suite sources never moved: assertions, fixtures, `insta` snapshot directories
and `include_str!` targets stayed byte-for-byte identical.

Both protected lists were honoured. Separate executables were kept for
`briefing_backstop_survival`, `zombie_prevention`, `pane_died_hook`,
`consent_non_blocking`, `realized_watch_reexec`, `purge_stale_tmux`,
`tackle_inprocess_completion`, `tackle_inprocess_no_harvest`,
`refused_root_dispatch_leaves_no_residue`, `restart_fidelity_no_neurion`, and
`tag_single_writer` (the one `cosmon-cli` file mutating the process-global
environment).

**Parity, checked rather than assumed:** the two arms list the *same 2171 test
names*, `diff`-clean once the harness prefix is stripped, with the same 6
ignored. The prototype is attached to the molecule under
`consolidation-prototype/`.

## Measurement 1 — the gain, three back-to-back pairs

Both arms are the same 70 (baseline) / 17 (consolidated) binaries executed one
after another with libtest's default thread count — which is exactly what cargo
does, and what a consolidated binary with N libtest threads does.

| pair | baseline | consolidated | reduction |
|---|---:|---:|---:|
| 1 | 55.26 s | 53.50 s | 3.2 % |
| 2 | 53.44 s | 51.22 s | 4.2 % |
| 3 | 56.27 s | 50.63 s | 10.0 % |

Mean **5.8 %**, against a 20 % threshold. And the figure is generous: 51
`insta` assertions in the consolidated arm failed fast (see *The cost side*
below), so that arm did strictly less work than the baseline it is being
compared to. The honest gain is smaller than 5.8 %, not larger.

## Measurement 2 — why: the premise expired

The 2026-08-01 study found `cosmon-cli` holding **86 %** of the suite's measured
CPU (1003 s of 1162 s). A full per-target sequential sweep of the workspace,
run today, disagrees. 242 test targets, 7382 tests, **309.0 s** total:

| crate | targets | tests | wall | share |
|---|---:|---:|---:|---:|
| cosmon-cli | 17 | 2333 | 56.0 s | **18.1 %** |
| cosmon-filestore | 3 | 102 | 42.8 s | 13.8 % |
| cosmon-rpp-adapter | 34 | 561 | 36.3 s | 11.7 % |
| cosmon-runtime | 22 | 151 | 31.0 s | 10.0 % |
| cosmon-daemon-supervisor | 8 | 76 | 23.2 s | 7.5 % |
| cosmon-state | 8 | 332 | 17.2 s | 5.6 % |
| cosmon-remote | 15 | 231 | 15.7 s | 5.1 % |
| cosmon-core | 19 | 1858 | 12.0 s | 3.9 % |
| cosmon-provider | 14 | 185 | 10.5 s | 3.4 % |
| cosmon-transport | 15 | 330 | 10.4 s | 3.4 % |

**86 % → 18 %.** A 1.9× ceiling on 86 % of the bill is worth having; the same
ceiling on 18 % of it is worth 8 % of the gate, and only if every second of that
18 % were consolidatable. It is not — see the next section.

The per-invocation cost of `cs`, which is what the study identified as the real
price, has collapsed:

| command | study, `debug` opt0 | study, `release` | today, `debug` opt1 |
|---|---:|---:|---:|
| `cs __help-tree` | 8.07 s | 0.62 s | **0.013 s** |
| `cs doctor leaks` | 10.30 s | 1.78 s | **0.82 s** |

*Measured.* Today's numbers are taken from a debug build. *Inferred, and the
inference is load-bearing:* codegen alone cannot explain them, because a debug
build at `opt-level = 1` is strictly slower code than the `release` build the
study timed, and today's debug is 47× faster than that release. So most of the
collapse is a **code** change, not the profile change of step 1. *Unknown:*
which commit. The leading candidate is `46c8eee1`, *"perf(event-log): checkpoint
the scan cursor so `cs` startup stops re-parsing the whole log"*, which landed
after the study's `971f75c` and addresses exactly the startup-scan cost the
study's own hypothesis-killer had wrongly exonerated. This was not bisected.

## Measurement 3 — what is left inside cosmon-cli is not consolidatable

Of the crate's 56.0 s, **34.7 s sits in three targets a rig harness cannot
touch**:

| target | wall | tests | why it cannot pool |
|---|---:|---:|---|
| `zombie_prevention` | 12.68 s | 1 | protected: real tmux + process-exit proof |
| `cs` (bin target) | 11.94 s | 1854 | not an integration test — in-`main.rs` unit tests |
| `tracing_subscriber_installed` | 10.11 s | 3 | wall-bound waiting on an unreachable `127.0.0.1:1`, already parallel across its 3 tests |

The remaining ~21 s is spread over 65 binaries averaging 0.3 s each. Merging
them is what bought the 3 s the measurement found. There is no 1.9× there,
because there is no longer enough serialised work to overlap.

## The cost side, since it is not zero

`autotests = false` is the load-bearing part of the design, and it is a
correctness hazard: a newly added `crates/cosmon-cli/tests/foo.rs` is then
**silently never built and never run** until someone remembers to edit
`Cargo.toml`. A test-speed change whose failure mode is *tests quietly stop
existing* is the same category of harm the two protected lists were written to
prevent — traded here for ~1 % of the gate.

Second, `insta` derives a snapshot's filename from `module_path!()`. Pooling a
suite renames every snapshot it owns: `ensemble_snapshot__x.snap` becomes
`rig_cs__ensemble_snapshot__x.snap`. That is 51 committed fixtures renamed in
`ensemble_snapshot` and `peek_snapshot` alone — a fixture change this mechanical
step explicitly excluded, and a permanent coupling of fixture names to harness
membership.

## Verdict and what to do instead

The hypothesis is falsified on its own stated criterion. **Do not consolidate
the `cosmon-cli` rigs, and do not start the shared-rig fixture work** that was
sequenced behind it — its measured ceiling was tens of seconds against a
premise that no longer holds.

The lever the same sweep hands over instead is step 2 of the 2026-08-01
recommendation, which was never implemented:

| target | wall | share of the whole gate |
|---|---:|---:|
| `cosmon-filestore/trunk_lock_concurrent` | 41.48 s | **13.4 %** |
| `cosmon-crashtest/recovery` | 5.75 s | 1.9 % |

Both still run `cargo build --example …` *from inside a test*
(`crates/cosmon-filestore/tests/trunk_lock_concurrent.rs:39`). `trunk_lock_concurrent`
is now the single most expensive target in the workspace — more than twice the
next one, and more than the entire consolidation was ever going to return.
Pre-building the helper as an artifact cargo already produces is a bounded
change to two files, and neither test's assertion (two real *processes*
serialising on the trunk lock) is weakened by it.

## Limits of this trace

The sweep is sequential per target, which is what cargo does, but it is not the
gate command: it excludes cargo's own up-to-date check and the 44 doctest
suites (~33 s, profile-insensitive). It was taken on one host under live fleet
load between 7 and 22, so the *ratios* carry the conclusions and the absolutes
do not. The `cs`-cost collapse is measured; its cause is inferred and not
bisected. The raw per-target table, both test-name inventories, and the probe
scripts are attached to molecule `task-20260801-5220` under `measurements/`.
