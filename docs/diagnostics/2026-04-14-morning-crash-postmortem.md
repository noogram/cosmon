# Post-mortem — Mac cold reboot, 14 avril 2026 (matin)

**Incident date:** 2026-04-14
**Reboot timestamp:** 07:33:26 UTC (09:33:26 CEST)
**Detection (post-reboot):** 07:40 UTC, uptime 9 min, load 26.28 / 170.92 / 125.15
**Machine:** MacBook Pro (Mac16,5 / T6041 / Apple Silicon) — macOS 15.7.3 (24G419)
**Author:** task-20260414-4315 (autonomous investigation)
**Related chronicle:** an internal chronicle

---

## 1. TL;DR

**Root cause (HIGH confidence):** a single Rust benchmark binary, `ga-bench`
(from the `wiki-genetic-algorithms` project, crate `ga-proof-search`, running
in worktree `task-20260414-9e74`), grew to **~400 GB resident pages** before
the crash and **~560 GB** after an automatic respawn on reboot. On a
finite-RAM laptop with 13 GB swap, this saturated RAM → compressor → swap,
froze userland, and required a hard power-button reset (`btn_rst`). There
was **no kernel panic**; the system simply stopped responding.

The 27 `cs run` runtimes, ~30 Claude workers, 1 348 total processes, and
~15 GB of Chrome renderers were **background noise**, not the proximate
cause — they elevated baseline pressure from comfortable to tight, but
`ga-bench` alone consumed more than the next **twenty** heaviest processes
combined.

---

## 2. Evidence

### 2.1 Shutdown signature

`/Library/Logs/DiagnosticReports/ResetCounter-2026-04-14-093326.diag`:

```
Date: 2026-04-14 09:33:26.16 +0200   (= 07:33:26 UTC)
Reset count: 0
Boot failure count: 1
Boot faults: rst btn_rst,btn_seq_reset timeout,dblclick_timeout target_off_restart
```

- **No `.panic` / `.ips` kernel panic** in either `/Library/Logs/DiagnosticReports/`
  or `~/Library/Logs/DiagnosticReports/`. The only kernel-side artifact is the
  ResetCounter above.
- `btn_rst` + `btn_seq_reset timeout` + `target_off_restart` is the signature
  of an SoC-level recovery triggered by power-button action (or by a hardware
  watchdog after prolonged non-response). XNU did not crash; it was *muted*.

### 2.2 Memory pressure — JetsamEvents

Three JetsamEvent reports bracket the incident:

| Local time | UTC | Free pages | Free MB | `largestProcess` | Largest rpages |
|------------|-----|-----------:|--------:|------------------|--------------:|
| 08:47:34 | 06:47 | 4 881 | 76 MB | `ga-bench` | **410 GB** |
| 09:57:08 | 07:57 | 10 261 | 160 MB | `ga-bench` | **559 GB** |
| 10:02:45 | 08:02 | (post-reboot) | — | `ga-bench` | — |

Pre-crash snapshot (`JetsamEvent-2026-04-14-084734.ips`):

- 1 348 total processes
- compressor: 4.7 M pages (~72 GB of compressed state)
- active + inactive + anonymous ≈ 88 GB of in-use memory on top of
  compressor
- `ga-bench` at **PID 68408** with **26 254 961 rpages × 16 KB = ~410 GB**.
  On a machine whose physical RAM is a fraction of that number, this value
  reflects the aggregate working set *before* the kernel reclaimed it —
  the process had been thrashing through swap/compressor for long enough
  to touch that many unique pages.

Top memory consumers aggregated by process name (pre-crash jetsam):

```
 410 077 MB   ga-bench              (1 process)
  14 814 MB   Google Chrome Helper  (70 renderers)
  12 439 MB   Code Helper (Plugin)  (57 instances)
   9 980 MB   com.apple.Virtualization.VirtualMachine (2)
   4 473 MB   Cursor Helper (Plugin) (13)
   3 943 MB   rust-analyzer         (3 workspaces)
   2 029 MB   WindowServer
   1 293 MB   Antigravity Helper
    ~1 GB    each: Claude, zotero, docker.backend, iTerm2
    40–50 MB each × ~35 `cs` CLI processes
    20–130 MB each × ~15 `node` (Claude Code worker) processes
```

The ratio **ga-bench : everything-else = ~6 : 1**. Eliminating every other
process would not have saved the machine; eliminating `ga-bench` alone
would have.

### 2.3 Post-reboot recurrence

07:57 UTC jetsam (24 minutes after reboot) shows `ga-bench` **back at
559 GB rpages**. The worker worktree at
`~/dev/projects/wiki-genetic-algorithms/.worktrees/task-20260414-9e74/` was
re-dispatched by the still-pending molecule, and the bug reproduced
immediately. Jetsam killed it in time this reboot, hence no second crash.

### 2.4 Current state (17:09 UTC, ~9 h post-reboot)

```
load averages: 50.06 24.47 13.90
vm.swapusage: 12 207 MB used / 13 312 MB total (92 %)
/System/Volumes/Data:  3.4 Ti used / 213 Gi free (95 %)
```

Memory pressure is **still high** and the disk is **near-full**. The
machine is one `ga-bench` relaunch away from a second incident.

### 2.5 What the evidence does **not** show

- No thermal shutdown — `pmset -g log` has no high-temp entries near
  07:30 UTC.
- No disk I/O error spike — we cannot verify inode exhaustion at crash
  time (logs rotated), but current `iused/ifree` are healthy (427k / 2.2G).
- No swapfile corruption — swap is live and serving reads/writes now.
- No `log show` output for panic/jetsam predicates because the incident
  is already outside the default live-log retention window; the only
  surviving structured evidence is the files listed above.

---

## 3. Timeline (reconstructed)

| UTC | Event | Evidence |
|-----|-------|----------|
| ~04:00–06:30 | Night runs: 27 `cs run` runtimes + ~30 Claude workers + multiple deliberations active. `ga-bench` launches under worktree `task-20260414-9e74`. | Mission context, worktree directory timestamps |
| ~06:00–06:47 | `ga-bench` accumulates resident pages faster than it releases them; compressor + swap fill. | JetsamEvent memoryStatus |
| 06:47:34 | First jetsam fires: free pages = 76 MB, `ga-bench` at 410 GB rpages. Kernel begins low-priority kills but system is already memory-thrashing. | `JetsamEvent-2026-04-14-084734.ips` |
| 06:47–07:33 | Userland becomes unresponsive (load-15m = 125 indicates sustained saturation over the preceding 15 min). Power button pressed (or SoC watchdog times out). | ResetCounter `btn_rst` |
| 07:33:26 | Cold boot. `Boot failure count: 1`. | ResetCounter timestamp |
| 07:33–07:40 | System boots; existing tmux runtimes auto-restart (launchagent or manual `just mission`). `ga-bench` is redispatched from its unresolved molecule. | Post-reboot uptime + fleet state |
| 07:57:08 | Second jetsam: `ga-bench` back at 559 GB. Kernel kills it in time; no second crash. | `JetsamEvent-2026-04-14-095708.ips` |
| 08:02:45 | Third jetsam — aftershock from the same process tree. | `JetsamEvent-2026-04-14-100245.ips` |
| 17:09 (now) | Machine operational; swap still 92 % used, disk 95 %. `ga-bench` no longer in process table. | Live `vm_stat`, `ps` |

---

## 4. Root cause analysis

### 4.1 Primary (HIGH confidence)

**`ga-bench` consumed unbounded memory.** The binary at
`wiki-genetic-algorithms/crates/ga-proof-search/src/bin/ga-bench.rs` runs a
GA benchmarking harness across *N* seeds × *K* benchmarks, caching each run's
JSON under `target/bench-cache/`. Hypotheses (not yet verified in code):

- **Per-seed populations retained in memory** rather than freed between
  seeds — the harness accumulates every generation's genome array across
  all benchmarks before writing the report.
- **No cap on population size × generations × seeds**: a "large" benchmark
  kind can blow the working set multiplicatively.
- **Unbounded cache growth in `target/bench-cache/`** contributes to disk
  pressure (consistent with /Data at 95 %).

To confirm the exact failure mode we would need to:

1. Run `ga-bench` in isolation under `/usr/bin/time -l` with a small seed
   set and watch `maximum resident set size` grow.
2. Instrument with `cargo run --release --bin ga-bench -- --seeds 42` and
   inspect heap profile (`heaptrack`, `instruments`, or `MallocStackLogging`).

### 4.2 Contributing factors

1. **No per-worker memory budget.** Cosmon's EnergyBudget tracks *tokens*,
   not *RSS*. A single misbehaving Rust worker can evict the entire system.
2. **Automatic respawn after crash.** The worktree molecule was still
   pending; post-reboot the runtime happily relaunched the same broken
   binary. The crash was not recorded as a "poison" event.
3. **27 runtimes × ~30 workers is near the tolerable limit.** Each Claude
   Code CLI is ~200 MB RSS + its Rust tooling (rust-analyzer per worktree,
   often 1.5–2 GB). Even without `ga-bench`, the laptop had ~40 GB of
   anonymous pages pinned by agent tooling — leaving little headroom for
   one rogue process.
4. **Disk near-full (95 %)** amplified the damage: compressor + swap fight
   for the same bytes, and APFS slows down dramatically past 90 %.
5. **Chrome + VS Code + Cursor + VMs together** held ~45 GB. These are
   "innocent" in the sense that they would never have triggered a crash
   alone, but they reduce the safety margin.

### 4.3 Ruled out (with given evidence)

- **Kernel panic** — no panic files, ResetCounter signature is button-reset.
- **Thermal** — no thermal events in `pmset -g log`.
- **Disk I/O fatal error** — APFS is healthy, no fsck failures at boot.
- **Network runaway** — not memory-relevant.
- **cs / cosmon infrastructure** — every `cs` process in the jetsam dump
  was < 50 MB; the fleet was a spectator.

---

## 5. Recommendations

### 5.1 Immediate (today)

- **Do not restart `ga-bench` until fixed.** Block the molecule or collapse
  the worktree. (Verified: no `ga-bench` in current `ps`.)
- **Reclaim disk.** `target/bench-cache/` and generic `target/` directories
  across the 14 projects repos probably hold tens of GB. `cargo clean` on idle
  worktrees.
- **Checkpoint swap.** Reboot the laptop once the current session closes;
  92 % swap usage is a soft sign the VM subsystem is still leaning on
  residue from the incident.

### 5.2 Short-term (this week)

1. **Fix `ga-bench`**: add a streaming/online aggregation path so only
   summary statistics per seed are retained, not full population histories;
   cap generations × population size; add `--max-memory-mb` guard rail that
   aborts if RSS crosses threshold (via `mach_task_basic_info` or
   `jemalloc`'s stats).
2. **Cosmon-side worker memory cap.** Add a `max_rss_mb` field to the
   worker spec; the transport (or a tiny sidecar) polls `ps -o rss=` and
   SIGKILLs a worker that exceeds the budget. Record the event as
   `WorkerOomKilled` so `cs resume` knows not to re-dispatch blindly.
3. **Concurrency ceiling.** Hard cap the number of simultaneously
   *Propelled* molecules per machine at something like
   `min(ncores / 2, 8)`. 27 runtimes × multiple workers each is beyond the
   laptop's design envelope.
4. **Load-avg kill-switch.** A `patrol --throttle` command that pauses new
   `cs tackle` dispatches when `load_avg_5m > 2·ncores` — prevents the
   self-reinforcing thrash we saw at 170.92.
5. **Disk hygiene.** CI-scheduled `cargo clean` on worktrees idle > 24 h;
   alert when `/` crosses 90 %.

### 5.3 Architectural (next ADR cycle)

- **Heavy workers belong in a VM or container**, not on the host. Define
  a "heavy" class of molecule (ML training, GA sweeps, long benchmarks)
  that the runtime dispatches only inside a resource-limited sandbox
  (Apple Virtualization Framework, Docker, or a dedicated remote box).
- **Crash-derived molecule poisoning.** If a worker is killed by the OS
  (exit code 137, or jetsam signature in the parent's log), the runtime
  should mark the molecule as `stuck(reason="oom")` rather than requeue.
  This prevents the post-reboot re-entry we observed.
- **Fleet-wide observability of RSS / load / swap.** `cs peek` already
  aggregates state; add a system-health panel so the operator sees "swap
  92 %, load 50" before deciding to tackle another molecule.

### 5.4 Decision (to confirm with pilot)

- **Accept the risk** of running everything on the laptop? → Only with
  items 2 (worker RSS cap) + 3 (concurrency ceiling) + 4 (kill-switch)
  implemented. The incident shows the current setup has *no margin*.
- **Change the architecture** toward a sandbox for heavy jobs? → Strongly
  recommended. One `ga-bench`-class process should never be able to down
  the machine that also hosts the orchestrator.
- **Test on a dedicated VM?** → Yes for `wiki-genetic-algorithms` and any
  benchmark-style workload. The laptop should remain the *pilot* surface
  (orchestrator + IDE + fleet of light workers), not the *heavy compute*
  surface.

---

## 6. Evidence gaps (for next incident)

We got lucky: Jetsam diagnostic reports and the ResetCounter survived
because they are persistent files. The *live* system log (`log show`) has
already rotated past the incident window, so subsystem-level signals
(`com.apple.SystemPower`, `kernel`) are unrecoverable for this event.

For next time, the pilot should capture, continuously:

- `sysctl vm.swapusage` and `vm_stat` every 60 s into a rolling file.
- `ps auxww | sort -nrk 6 | head -20` snapshot every 60 s.
- `log collect --output /tmp/sysdiag.logarchive --last 4h` at the first
  sign of trouble (before the crash) — this snapshots the kernel log in
  a form that survives reboot.
- A `patrol` skill that writes `load_5m`, `free_mb`, `swap_used_mb` into
  `~/.cosmon/health.jsonl` every minute.

---

## 7. Conclusion

The 14 April morning crash was a single-process memory-exhaustion event,
not a cosmon-fleet scaling failure. The fleet's role was to keep the
machine busy enough that there was no slack left to absorb the blast.

The principle to chronicle: **token budgets are not enough — a worker
without an RSS budget is a worker without a speed limit.** Cosmon tracks
cognitive energy (tokens, temperature) but delegates physical resource
accounting to the OS, and the OS's only recourse is to shoot the
process. In a cooperative multi-worker system, one rogue process is
enough to kill everyone else's work.

Next step is *not* a fix commit — it is a decision by the pilot on §5.4.
This post-mortem supplies the evidence.
