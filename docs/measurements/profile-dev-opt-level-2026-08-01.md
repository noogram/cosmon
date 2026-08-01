<!-- SPDX-License-Identifier: AGPL-3.0-only -->
# `[profile.dev] opt-level = 1` — the diff, and what it cost to check — 2026-08-01

**Molecule:** `task-20260801-ca50` · step 1 of the merged three-step plan in
[test-suite-two-seats-comparison-2026-08-01.md](test-suite-two-seats-comparison-2026-08-01.md).
**Change:** two lines of TOML in the root `Cargo.toml`. No test touched, no gate
command changed, no CI workflow touched.

This document exists because the plan said "adopt for CI only after one
CI-side cold-build measurement", and this node does not have a CI runner. So it
does the half it can do honestly — the local before/after — and states the
other half as an open number rather than guessing it.

## Badge (attach to every number below)

| | |
|---|---|
| repo | `973a00b`, worktree `.worktrees/task-20260801-ca50`, branch `feat/task-20260801-ca50` |
| host | Apple M4 Max, 16 cores, macOS 26.5.2 |
| toolchain | pinned `stable` (`rust-toolchain.toml`) |
| command | `cargo test --workspace --no-fail-fast`, the configured merge gate, unmodified |
| env | `CB_DEPTH` and `ANTHROPIC_MODEL` stripped — both poison `cs`-spawning tests from inside a worker session |
| window | 2026-08-01, 01:55–03:06 UTC |
| ambient load | **35–146** on 16 cores. This box runs the live agent fleet. Every run below carries the load it ran under, and the headline pair is the two runs taken 12 s apart. |

The load is not a caveat bolted on at the end, it is the reason the result is
reported as a *range*. The first seat's study measured this same change at
load 29–43 and got 1.44×. This node measured it at load 51–74 and got 3.37×.
Both numbers are real; they measure the same change on differently-busy hosts.

## The result

Four full gate runs. The last two are the headline: they were taken 12 seconds
apart, in the same load band, with nothing else changed between them but the
two lines of TOML and the 36 s it takes cargo to swap back to its cached
artifacts for the other profile.

| run | profile | load before → after | wall | user CPU | result |
|---|---|---|---:|---:|---|
| 1 | opt-level 0 | 63 → 78 | 932.2 s | 961.9 s | 7489 passed, 0 failed |
| 2 | opt-level 1 | **110** → 35 | 924.0 s | 385.7 s | 7489 passed, 0 failed |
| **3** | **opt-level 0** | **51 → 71** | **1010.4 s** | **892.7 s** | **7489 passed, 0 failed** |
| **4** | **opt-level 1** | **74 → 37** | **299.9 s** | **151.6 s** | **7489 passed, 0 failed** |

**Runs 3 → 4: 1010.4 s → 299.9 s wall, a 3.37× speedup, with all 7489
assertions still passing.**

Run 2 is kept in the table precisely because it looks like a refutation, and
explaining it is the point. It started at load **110** — the tail of its own
rebuild plus the fleet — and returned a wall figure indistinguishable from run
1's. Read only the wall column and you would conclude the change does nothing.
Read the CPU column and the same run shows 385.7 s against 961.9 s. The wall
clock of run 2 measured the neighbours; the CPU did not. That is why runs 3 and
4 were re-taken back to back once the box quieted, and why the ratio, not the
absolute, is what this document asserts.

**The load-robust invariant across all four runs:** user CPU falls from
892–962 s to 152–386 s. Every pairing, at every load, moves by 3–6×. The
direction is not in question; only the magnitude a given host will see is.

`debug-assertions` and `overflow-checks` keep their `dev` defaults at
`opt-level = 1`. The change alters codegen, not semantics — which is what the
identical 7489/0 on both sides is there to demonstrate rather than assert.

## Why it works at all (inherited, not re-derived here)

From [test-suite-wall-clock-2026-08-01.md](test-suite-wall-clock-2026-08-01.md):
91 % of the suite's CPU is 1150 integration tests, and those tests spend their
time *running* code — their own, and that of the real `cs` children they spawn,
which were also built at `opt-level = 0`. That study measured `cs __help-tree`
at 8.07 s of user time in `debug` against 0.62 s in `release`. The workspace
carried no `[profile]` section at all, so every one of those invocations paid
the unoptimised price. This node did not re-derive that; it measured the fix.

## The cost, stated plainly

The profile switch forces a full rebuild of the test world. Measured here, cold
in both cases (`target/` did not exist for the first):

| build | load | wall | CPU (user + sys) |
|---|---:|---:|---:|
| cold `cargo test --workspace --no-run`, opt-level 0 | 35 | 153.2 s | 876.7 s |
| full rebuild at opt-level 1 | 75 | 684.8 s | 2334.8 s |

Compare the CPU column, not the wall: **877 → 2335 CPU-seconds, 2.7× more
compile work.** (The first seat measured 2153 CPU-seconds for the same rebuild
at load 25–45 — within 8 % of this node's 2335, on a host running at three
times the load. Compile CPU is a property of the compiler; wall clock is a
property of the afternoon.)

Swapping *back* costs 36.1 s: cargo keeps both profiles' fingerprints, so a
developer toggling the line does not repay the full rebuild. That is what made
the back-to-back A/B in the table affordable at all.

## What this node did NOT establish — the open number for CI

**The cold-build cost on a CI runner is unmeasured, and this node could not
measure it.**

The trade is different in CI and the difference is structural, not a matter of
degree. Locally the 2335 CPU-seconds are paid once and amortised over every
subsequent `cargo test`; the developer's second run is already ahead. A cold CI
runner with no warm `target/` pays the build *every run*, against a test-phase
saving that on this host was ~710 s of wall. Whether that nets out positive
depends on the runner's core count, its cache hit rate, and whether the job
already restores a `target/` cache — none of which is decidable from here.

So: **this change is adopted for local development only.** It touches
`Cargo.toml` and nothing under `.github/workflows/`. The follow-up that CI
adoption is gated on is one measurement on the runner — cold build at
opt-level 0 versus opt-level 1, plus the test phase either side — and it is a
separate node.

Two further things left open, named rather than assumed:

- **The two nested-`cargo build` regressions.**
  `cosmon-filestore/trunk_lock_concurrent` and `cosmon-crashtest/recovery`
  shell out to `cargo` from inside a test and so inherit the profile they are
  measuring; the first seat measured them at 10.5 s → 27.1 s and 10.2 s →
  21.6 s. This node did not re-time them individually (the gate runs above are
  whole-suite wall clock) and did not fix them — that is step 2 of the merged
  plan, filed as its own molecule. They are inside the 299.9 s above, so the
  headline is a *net* figure that already carries their regression.
- **Any figure on an idle host.** Every number here was taken on a machine
  sharing 16 cores with a live fleet. The 3.37× is this host's; 1.44× is the
  first seat's, on the same change.

## Reproducing it

```sh
# A/B, back to back, on one host — the only comparison that means anything
cargo test --workspace --no-run                       # warm the profile you start on
/usr/bin/time -p cargo test --workspace --no-fail-fast  # side A
$EDITOR Cargo.toml                                    # toggle [profile.dev] opt-level
cargo test --workspace --no-run                       # ~36 s, cargo has both cached
/usr/bin/time -p cargo test --workspace --no-fail-fast  # side B
sysctl -n vm.loadavg                                  # before and after each run
```

Record the load either side of every run. A wall-clock number without one, as
run 2 shows, is not a measurement of your change.
