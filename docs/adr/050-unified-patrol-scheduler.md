# ADR-050: Unified Patrol Scheduler

## Status
Proposed (2026-04-18)

## Context

Cosmon's host environment has accumulated a family of per-task `launchd`
plists that each answer the same shape of question: *"fire this command on
this cadence, unless a kill-switch says don't"*. As of 2026-04-17, three
such plists coexist on the operator's machine (plus a fourth template
delivered by `task-20260417-b87b`):

- `mailroom-executor-pulse.plist` — 6×/day, sends iMessages.
- `mailroom-executor-pulse-replies.plist` — every 5 minutes, polls replies.
- `cosmon-chronicle-lint-weekly.plist` — Sundays 09:00, runs the lint formula.

Each new patrol follows the same ritual: write an XML plist, figure out
`ProgramArguments`, pick a log path, add a bespoke lockfile convention,
`launchctl load`, document somewhere, repeat. Ten patrols would mean ten
horloges, ten log files, ten places to touch when standing the fleet
down. The marginal cost of adding a patrol is high enough that it biases
against having them at all — which is wrong, because patrols are cheap by
design. The substrate, not the concept, is expensive.

Parent idea `idea-20260417-b52d`
captured the observation, ran a three-step `idea-to-plan` formula, and
produced a `plan.md` with eight question-by-question decisions already
made. The child molecules (`task-20260417-59ff…e5ec`) implement the
scheduler. This ADR promotes the load-bearing decisions of that plan to
a binding, citable artifact so future patrols — and future galaxies —
can reference a stable path rather than archaeology through the parent
idea's evaluation.

The architectural question this ADR answers is narrower than the plan's:

> *Where does a unified patrol scheduler sit in cosmon's two-layer /
> three-regime model, what is the wire-level TOML schema v1, how is the
> kill-switch discipline enforced, and what migration policy is binding?*

Adjacent questions (binary vs subcommand placement, cron vs interval
parsing, log file naming) are resolved in the parent plan and implementation
tasks. This ADR fixes the architectural contract; the crates implement it.

### Starship as the reference shape

The parent idea cites [Starship](https://starship.rs) as the inspiration:
one binary, one TOML, modules that appear when the user edits config, no
re-install, no plist per module. The philosophy transfers cleanly:

- One file the operator edits to declare a new patrol.
- One binary the OS knows how to invoke.
- Zero build step between "I want a new patrol" and "it runs on the
  configured cadence".

The difference is that Starship is invoked *per prompt* (push model from
the shell) while a scheduler is invoked *per tick* (pull model from
launchd/cron). The rest of the discipline — strict TOML parsing,
conditional activation, fast cold start, modular extension — transfers.

## Decision

### 1. The scheduler is L2 Propelled, not L3 Autonomous

The scheduler sits in the **Propelled** regime of ADR-016's three-regime
model, not the **Autonomous** regime:

| Aspect | Scheduler's position |
|--------|---------------------|
| Clock locus | **External** (launchd/cron calls the binary every 60 s) |
| Trajectory | **Fixed at config load** (TOML declares what fires when) |
| Deliberation function | **None** (no runtime policy computes "what next") |
| Outlives a single decision? | **No** — each tick exits |

This placement is binding. It forbids three proximate mistakes:

1. **No resident daemon.** The scheduler does not own a long-lived
   process. It is a cron-triggered binary that loads, decides, dispatches,
   writes state, and exits. Cold start must stay under ~100 ms so a 60 s
   tick overhead is negligible. This preserves the "no daemon in core"
   invariant (CLAUDE.md §Architectural Discipline, ADR-016).
2. **No policy layer.** The scheduler does not compute what to fire next
   based on runtime state. It reads a static TOML and matches cadences
   against a clock. If patrol *ordering* or *dynamic dispatch* becomes a
   requirement, that belongs to the resident runtime (ADR-016 Phase 3+)
   via `cs run <dag>`, not to the scheduler. The scheduler stays dumb.
3. **No worker lifecycle.** The scheduler dispatches commands; it does
   not nucleate, tackle, or complete molecules itself. A patrol *may*
   invoke `cs nucleate … && cs tackle … && cs wait … && cs done …` as its
   command, but that is the patrol's payload, not the scheduler's
   responsibility. Merge-before-dispatch, worktree discipline, and
   lifecycle state remain in the transactional core.

The operator-facing read surface — `cs scheduler status` — is a thin CLI
over the scheduler's state file. It is read-only and lives in
`cosmon-cli` for vocabulary continuity. The scheduler binary itself lives
in a separate crate `crates/cosmon-scheduler/` so that the dependency
direction (`cosmon-scheduler` → `cosmon-core`, never the reverse)
mechanically prevents the scheduler from leaking back into the core.

### 2. TOML schema v1 is binding

The wire-level contract is fixed at v1 to decouple the scheduler binary
from the config file lifecycle. Pilots edit TOML without rebuilding; the
binary reads TOML without knowing which pilot wrote it.

```toml
# ~/.config/cosmon/patrols.toml — schema v1

[scheduler]
state_file            = "~/.cosmon/scheduler.state.json"
log_file              = "~/.cosmon/scheduler.log"
kill_switch           = "~/.cosmon/stand-down.lock"
tick_interval_seconds = 60

[[patrol]]
name                  = "string, unique"              # identity key
interval_seconds      = 300                           # XOR with `cron`
cron                  = "0 9 * * 0"                   # POSIX 5-field
command               = ["bin", "arg1", "..."]        # argv, never a shell string
working_dir           = "~"                           # optional, shell-expandable
env                   = { KEY = "value" }             # optional, merged on inherited env
kill_switch           = "~/path/to/lock"              # optional, per-patrol
log_file              = "~/path/to/log"               # optional, falls back to scheduler.log
dispatch              = "detached"                    # "detached" | "wait"
require_env           = ["HOME"]                      # optional, all must be set
timeout_seconds       = 3600                          # optional, metadata only
enabled               = true
```

**Parsing discipline (binding):**

- **Patrol level**: `deny_unknown_fields`. A typo in a field name is a
  fail-on-load error, not a silent ignore. Rationale: patrol declarations
  are load-bearing for side-effects; a misspelled `kill_switch` silently
  becoming "no kill switch" is a footgun we refuse.
- **Root level**: tolerant. Unknown top-level tables are ignored with a
  warning. Rationale: forward-compat across scheduler versions. An old
  binary reading a new TOML must not explode.
- **Cadence**: exactly one of `interval_seconds` or `cron` per patrol.
  Combining both is semantic soup; omitting both is unconfigured. Both
  cases are fail-on-load.
- **Cron dialect**: POSIX 5-field (`min hour dom month dow`). No
  6-field seconds column. The scheduler ticks at 60 s minimum; seconds
  are meaningless. Matches `crontab(5)` muscle memory.
- **`command`**: non-empty argv array. No shell string form. Complex
  invocations use `["/bin/bash", "-lc", "…"]` explicitly. This removes
  an injection surface and mirrors launchd's `ProgramArguments`
  discipline.
- **Name uniqueness**: enforced at load time. State-file entries are
  keyed by `name`; collisions would corrupt `last_fired_at` bookkeeping.

**Schema evolution**: v1 is additive-only going forward. Removing or
renaming an existing field requires a new `schema_version` key and a
successor ADR. The tolerant root-level parsing gives us the seam to
introduce `[scheduler] schema_version = 2` later without breaking v1
configs.

### 3. Kill-switch discipline is two-tier, file-based, fail-safe

Every tick checks two tiers in order:

1. **Global** — if `~/.cosmon/stand-down.lock` (from `[scheduler]
   kill_switch`) exists, no patrol fires. The tick logs
   `KILL_SWITCH_GLOBAL path=…` and exits clean. A single `touch` halts
   the entire scheduler without touching launchd.
2. **Per-patrol** — if a patrol's `kill_switch` field is set and that
   file exists, that patrol is skipped. Logged as
   `patrol=X skip=kill_switch path=…`.

**Invariants:**

- **Files, not commands.** A kill-switch is an atomic filesystem
  presence check. `touch` / `rm` / `[[ -f ]]`. Visible to `ls`,
  survives reboot, scriptable from a three-line hook, does not require
  a running scheduler to inspect. A "disabled state" in a database would
  require a tool and a running service to toggle.
- **Fail-safe direction**: the *presence* of a lockfile means "stop",
  not "go". If the operator's intent cannot be read (permission denied,
  symlink loop, disk error), the scheduler *skips* rather than fires.
  The question "did the scheduler fire it by mistake?" is more expensive
  than "why didn't it fire?".
- **Reuse of galaxy conventions**: the per-patrol kill-switch path lets
  each galaxy keep its existing lockfile convention. Mailroom's
  `~/.mailroom/stand-down.lock` continues to halt its patrols
  without retraining.
- **Race window is acceptable**: a lockfile appearing between the
  check and the spawn results in the patrol firing once more, then
  being honored on the next tick (≤60 s later). This bound is
  documented and accepted; adding in-process locking would not
  eliminate the race with external processes checking the same file.

### 4. Migration policy: progressive, reversible, chronicle-lint first

The migration from N plists to 1 LaunchAgent follows a strict order of
increasing blast radius. Each step is individually reversible in under a
minute.

| Order | Patrol | Blast radius | When |
|-------|--------|--------------|------|
| 1 | `cosmon-chronicle-lint-weekly` | 1 fire/week, no user-visible side-effect | Week 1 after scheduler lands |
| 2 | `mailroom-executor-pulse-replies` | 5-min cadence, reply polling | Week 2, after ≥1 clean week of #1 |
| 3 | `mailroom-executor-pulse` | iMessage dispatch (operator-visible) | Week 3, after owner review |

**Per-patrol migration protocol (binding):**

1. Add the patrol's `[[patrol]]` entry to `~/.config/cosmon/patrols.toml`.
2. Observe ≥1 successful fire via `cs scheduler status` and the patrol's
   own log.
3. `launchctl unload` the old plist.
4. Move the old plist to `scripts/launchd/archived/` — **never delete**
   during migration. Archived plists are the audit trail and the
   rollback artifact.
5. Document the cutover in the weekly chronicle if the patrol has
   operator-visible side-effects (mailroom pulse qualifies;
   chronicle-lint does not).

**Rollback protocol (binding):**

1. `launchctl load` the archived plist.
2. Set `enabled = false` on the `[[patrol]]` entry in the TOML.
3. Next tick (≤60 s) honors the disabled flag; archived plist resumes
   ownership.

Total reversal time: ~10 seconds. The two systems cohabit during the
overlap window because the new `[[patrol]]` is disabled and the old
plist is active; zero double-fire risk.

**Mailroom patrols (#2 and #3) are intentionally not nucleated as
implementation molecules at this ADR's writing.** They are operator-gated
(owner review for iMessage, observation window for replies). Nucleating
them prematurely would violate the "no untagged pending" rule of ADR-048.
They nucleate only after the chronicle-lint pilot (#1) proves stable
through one live Sunday fire.

### 5. Alignment with existing invariants

| Invariant | How this ADR respects it |
|-----------|--------------------------|
| No daemon in core (ADR-016) | Scheduler is cron-triggered, exits each tick. Lives in a separate crate with one-way dependency on `cosmon-core`. |
| One concept, infinite extensibility (CLAUDE.md §Composability) | Adding a patrol is one TOML entry. No new code, no new plist, no new command. |
| Control plane vs data plane (CLAUDE.md §Communication Model) | Scheduler is a clock, not a message bus. It dispatches commands; patrols communicate with the rest of cosmon (or any other system) via filesystem state just like workers do. |
| CLI-first for workers (CLAUDE.md) | A patrol that invokes cosmon uses the `cs` CLI. The scheduler is the invoker, not a cosmon internal peer. |
| Merge-before-dispatch | Applies to the patrol's payload (if it runs molecules), not to the scheduler itself. The scheduler has no molecule lifecycle. |
| Syzygie cross-galaxy | Each galaxy keeps its patrols in the shared TOML. A new galaxy contributes `[[patrol]]` entries, not plists. |

### 6. Re-evaluation criteria (binding flip-conditions)

This ADR expects revision — and names the triggers explicitly — if any
of the following become true:

- **Scale**: patrol count exceeds ~50 and aggregate tick time exceeds
  500 ms. At that point the cost of re-loading TOML and walking the
  patrol list each tick starts to matter. Successor ADR would evaluate
  resident mode with shared-memory cadence table.
- **Sub-minute cadence**: a legitimate patrol requires firing more
  often than once per minute. The 60 s tick becomes a floor the
  scheduler cannot cross. Successor ADR evaluates either (a) a
  resident scheduler with finer internal ticking, or (b) declaring
  sub-minute cadence out of scope and referring to the invoker's
  own ticking mechanism.
- **Ordering**: a patrol must wait for another patrol to complete.
  The scheduler has no notion of inter-patrol dependencies. Do not
  retrofit; that dependency is a DAG and belongs to `cs run <dag>`
  (ADR-016 Phase 3+). Successor ADR formalizes the boundary.
- **Hot-reload below tick-interval latency**: operator edits TOML and
  wants the change reflected in <60 s. Current design reloads on
  every tick, so changes take effect within one tick boundary. If
  a faster feedback loop is needed, consider a `SIGHUP` handler — but
  that pulls toward resident mode.

A single flip-condition firing is sufficient to reopen the question. The
scheduler's dumbness is the feature; we buy it back only when the
evidence demands it.

### 7. Non-goals (explicit, out of v1)

- **No `cs patrol` merge.** `cs patrol --propel` keeps its current
  operational role as the transport-layer watchdog for Propelled
  molecules (ADR-016). The scheduler is a sibling, not a replacement.
  Their confusable names are accepted; a future successor ADR may
  rename or merge if the overlap becomes painful.
- **No neurion integration.** The scheduler does not register its
  patrols in the nervous system registry at tick time. Documented as
  a future enrichment; belongs to a separate ADR when the observability
  need is real.
- **No Linux port.** Design is portable (cron entry replaces launchd
  plist, everything else is identical). Explicit Linux support ships
  when the fleet runs on Linux.
- **No log rotation.** Operator runs `newsyslog(5)` or `logrotate(8)`.
  Rotating logs from inside a 60 s-tick binary is a footgun the
  ecosystem already solves.
- **No in-process timeout enforcement.** `timeout_seconds` is metadata
  only. launchd or an external watchdog kills runaway processes; the
  scheduler just records the declared budget for `cs scheduler status`
  to surface.

## Consequences

**Positive:**

- Adding a patrol becomes one TOML entry. Removing a patrol becomes
  `enabled = false`. The substrate stops biasing against having
  patrols at all.
- One kill-switch (`~/.cosmon/stand-down.lock`) halts the entire
  fleet — reboot-survivable, scriptable, visible to `ls`.
- `cs scheduler status` answers "when did patrol X last run?" in
  <1 s without reading any log files.
- The "no daemon in core" invariant gains a second load-bearing example:
  a non-trivial scheduling surface, cron-triggered, exits each tick.
- Migration is reversible per-patrol in ~10 seconds. Pilot risk is
  bounded.
- Cross-galaxy discipline: mailroom, showroom, and any future
  galaxy contribute `[[patrol]]` entries to one TOML instead of
  forking plists. Syzygie-compatible by construction.

**Negative:**

- Two similarly-named surfaces coexist: `cs patrol --propel` (cosmon
  transport watchdog) and "patrols" (scheduler TOML entries). The
  confusion is real. Mitigation: keep both names distinct in
  documentation, never use them in the same paragraph without
  disambiguation, accept that a future ADR may rename.
- The scheduler is the third place where "what runs when" is declared
  (alongside `cron(8)` for user cron and `launchd(8)` for system-level
  agents). For macOS-only setups the picture is simpler — the scheduler
  replaces user-level plists — but operators using both `crontab` and
  the scheduler must check two places.
- Archived plists in `scripts/launchd/archived/` accumulate over time.
  After ~1 year with a stable scheduler, a sweep ADR may garbage-collect
  them; until then, their presence is the audit trail.
- The TOML schema v1 is frozen. Adding a cadence form (e.g., "at sunrise"
  or "every 3rd weekday") requires a schema bump and successor ADR.
  Accepted cost: the two cadence forms cover every observed patrol on
  the fleet today.

**Neutral:**

- The scheduler is not a molecule. It is adjacent infrastructure that
  *invokes* molecules (via patrol commands that run `cs nucleate … &&
  cs tackle …`). This is deliberate: the scheduler is a clock, and
  clocks are not in cosmon's domain. Molecules remain the only thing
  cosmon tracks.
- `cs scheduler status` is a read-only surface. A hypothetical
  `cs scheduler tick` or `cs scheduler reload` belongs to the scheduler
  binary itself, not to `cs`. This preserves the single-perimeter
  discipline of CLAUDE.md §Command perimeters.

## Implementation Sequence

The parent idea's plan lists six child molecules; this ADR codifies the
path without re-specifying them.

1. **Phase 0 (done)**: Scaffold `cosmon-scheduler` crate — TOML loader,
   tick skeleton, argv dispatch. Ships in `task-20260417-59ff` (merged
   2026-04-17).
2. **Phase 1**: Dispatch engine — cron parsing, interval logic,
   kill-switch checks, atomic state.json I/O. `task-20260417-572f`.
3. **Phase 2**: `cs scheduler status` subcommand (read-only).
   `task-20260417-eabd`.
4. **Phase 3**: LaunchAgent template + install/uninstall script.
   `task-20260417-6f83`.
5. **Phase 4**: Migrate `cosmon-chronicle-lint-weekly` plist (pilot).
   `task-20260417-e5ec`. Week 1 after Phase 3 lands.
6. **Phase 5 (deferred)**: Migrate mailroom patrols (replies then
   pulse). Nucleated only after Phase 4 proves stable through one
   live Sunday fire. Week 2–3.

**Gate to close this ADR**: Phase 4 and Phase 5 both done, one full
calendar month of clean runtime with all three original plists retired
to `scripts/launchd/archived/`, zero rollbacks recorded. At that point
the ADR's Status changes to **Accepted**; until then it is **Proposed**.

## References

- Parent idea: `idea-20260417-b52d`
  — full feasibility study and eight question-by-question rationale.
- [ADR-016](016-autonomy-regimes-and-resident-runtime.md) — two layers /
  three regimes; this ADR places the scheduler at L2 Propelled.
- [ADR-022](022-native-dag-scheduler.md) — native DAG scheduling is the
  *other* scheduler in cosmon's world; this ADR keeps inter-patrol
  ordering out of the patrol scheduler and references ADR-022 as the
  successor path if ordering becomes a requirement.
- [ADR-048](048-backlog-sanity-invariant.md) — "no untagged pending"
  rule; motivates deferring mailroom migration molecules until the
  chronicle-lint pilot succeeds.
- [Starship](https://starship.rs) — inspiration for one-TOML,
  strict-parse, zero-build-step configuration surface.
- `crates/cosmon-scheduler/` — the implementation seeded by
  `task-20260417-59ff`.
- `scripts/launchd/cosmon-chronicle-lint-weekly.plist` — pilot-migration
  target; representative of the plist form this ADR retires.
- CLAUDE.md §Architectural Discipline — "no daemon in core" invariant.
- THESIS.md Part V (Vocabulary) — physics naming; scheduler is a
  clock, not a deliberation function.
