# ADR-053: Cosmon Daemon Supervisor — Unified Long-Running Process Supervision

## Status
Accepted (2026-04-19)

Supersedes: the ad-hoc fan of per-daemon LaunchAgent plists previously
installed in `~/Library/LaunchAgents/` (`com.you.notification-bot`,
`com.you.notification-bot`, `com.you.emacs-daemon`,
`com.you.zotero-mcp`, `com.you.almanac`, `com.you.archive-service`,
`com.noogram.dashboard`).

Parent artifacts:
- Idea: `idea-20260419-25fd`
- Feasibility: `idea-20260419-25fd-cosmon-daemon-supervisor-feasibility.md`
- Plan: `idea-20260419-25fd-cosmon-daemon-supervisor-plan.md`
- Pilot chain: `task-20260419-{bed0, e17a, f7b6, b31b, 5ad4}` (T1 → T5).

Related ADRs:
- [ADR-016](016-autonomy-regimes-and-resident-runtime.md) — autonomy regimes
  (Inert / Propelled / Autonomous). This ADR inhabits **Autonomous**.
- [ADR-050](050-unified-patrol-scheduler.md) — the sibling contract for
  tick-driven patrols. The supervisor is to daemons what the scheduler
  is to patrols: one TOML, one LaunchAgent, N entries.

## Context

The operator's machine had accumulated a plist per long-running
Cosmon-managed daemon. As of 2026-04-19 that meant seven individual
LaunchAgents, each with its own ritual — XML plist, `ProgramArguments`
array, log-path conventions, bespoke kill-switch, `launchctl load`
sequence, documentation elsewhere. The marginal cost of *running*
another daemon was high enough to bias against having one at all, even
though the concept (a Cosmon-owned resident process) is cheap and
frequently useful.

The pathology mirrors the one `cosmon-scheduler` (ADR-050) resolved for
**tick-driven patrols**: many identical rituals answering the same
shape of question — *"keep this process alive, unless a kill-switch
says don't"*. The answer is the same shape of solution: one TOML, one
LaunchAgent, one binary that reads the TOML.

The architectural novelty is limited to the execution mode. Where the
scheduler is **tick-driven** (cron / interval cadence, spawn-and-forget
each tick), the supervisor is **event-driven** (file-watch on the
TOML + `SIGCHLD` + throttle timers) and keeps its children alive with
`KeepAlive` semantics. Both run as resident `LaunchAgent`s under the
same `stand-down.lock` convention.

The architectural question this ADR answers is:

> *Where does a unified daemon supervisor sit in cosmon's two-layer /
> three-regime model; what is the wire-level TOML schema v1; what is
> the kill-switch / respawn / reload discipline; and what migration
> policy is binding?*

Eight design questions (Q1–Q8) were raised during feasibility; each one
is answered below with its rationale. This ADR promotes those answers
from the feasibility doc to a binding, citable artifact.

## Decision

### 1. The supervisor lives in L3 Autonomous, not L2 Propelled

The supervisor sits in the **Autonomous** regime of ADR-016's three-regime
model. Its clock is internal: file-watch events and child-exit
notifications drive state transitions, not a 60 s external tick. This
makes the supervisor the first load-bearing Autonomous inhabitant in
the cosmon codebase (ahead of the future `cs run <dag>` resident
runtime).

| Aspect | Supervisor's position |
|--------|----------------------|
| Clock locus | **Internal** (`notify::Watcher` + `tokio::signal::unix::signal(SIGCHLD)` + throttle timers) |
| Trajectory | **State-driven** (each `SIGCHLD`, each file event, each throttle deadline moves the child state machine) |
| Deliberation function | **None** (no runtime policy chooses which daemon to run; TOML declares the set) |
| Outlives a single decision? | **Yes** — by construction |

This placement is binding. It forbids three proximate mistakes:

1. **No leak into the Transactional Core.** The supervisor binary
   (`cosmon-daemon-supervisor`) is *not* part of the `cs` CLI. `cs
   daemons {list,status,logs,reload}` is a stateless read surface over
   the state file the supervisor writes; it never spawns, never signals,
   never mutates. The L1 invariant (no daemon in core) is preserved:
   the core stays a git-like CLI that shells out to the resident
   supervisor via the filesystem (state file + TOML + touch-to-reload).
2. **No policy layer in the supervisor.** The supervisor does not
   compute which daemons to run based on load, time-of-day, or
   upstream state. It reads the TOML and keeps the declared set alive.
   Dynamic orchestration of long-running processes belongs to a future
   ADR if the need arises; the supervisor stays dumb.
3. **No molecule lifecycle.** The supervisor does not nucleate,
   tackle, evolve, or complete molecules. A daemon it supervises *may*
   be a cosmon client (e.g. the Flask dashboard reading `.cosmon/state/`),
   but that is the daemon's payload, not the supervisor's responsibility.
   The worker/human boundary (ADR-016 §Command perimeters) remains
   unviolated.

The operator-facing read surface — `cs daemons status` — is a thin CLI
over `~/.cosmon/daemon-supervisor.state.json`. Read-only, stateless,
safe to call concurrently. The supervisor binary itself lives in a
separate crate `crates/cosmon-daemon-supervisor/` so that the
dependency direction (`cosmon-daemon-supervisor → cosmon-core`, never
the reverse) mechanically prevents leak-back.

### 2. TOML schema v1 is binding

The wire-level contract is fixed at v1 to decouple the supervisor
binary from the config file lifecycle. Pilots edit TOML without
rebuilding; the binary reads TOML without knowing which pilot wrote
it.

```toml
# ~/.config/cosmon/daemons.toml — schema v1

[supervisor]
state_file  = "~/.cosmon/daemon-supervisor.state.json"
log_file    = "~/.cosmon/daemon-supervisor.log"
kill_switch = "~/.cosmon/stand-down.lock"

[[daemon]]
name              = "string, unique"                 # identity key
binary            = "/abs/path/to/binary"            # PATH-resolvable
args              = ["arg1", "..."]                  # optional argv tail
throttle_seconds  = 30                               # post-exit respawn delay
env               = { KEY = "value" }                # optional, merged on inherited env
log_stdout        = "/path/to/stdout.log"            # optional
log_stderr        = "/path/to/stderr.log"            # optional
kill_switch       = "/path/to/per-daemon.lock"       # optional, per-daemon
enabled           = true                             # default true
```

**Parsing discipline (binding), mirrored from ADR-050:**

- **Daemon level**: `#[serde(deny_unknown_fields)]`. A typo in a
  `[[daemon]]` field name is a fail-on-load error. A misspelled
  `throttlee_seconds` silently becoming "no throttle" is a footgun
  we refuse.
- **Root level**: tolerant. Unknown top-level keys are ignored so an
  older binary reading a newer TOML does not explode. Forward-compat
  is a property of the *file*, not of the crate.
- **Content identity**: two `DaemonSpec`s are the **same daemon** iff
  they share the same `name`; they are **unchanged** iff their BLAKE3
  content hashes match. Any difference in binary, args, env, log
  paths, throttle, enabled flag, or `kill_switch` triggers a
  `stop → spawn` cycle on hot-reload, not a silent behavior swap.
- **All validation errors in one pass**: duplicate names, empty binary,
  etc., are collected into a multi-line `ConfigError::Invalid` so the
  operator sees the full picture without whack-a-mole.

**Schema evolution**: v1 is additive-only going forward. Removing or
renaming a field requires a `schema_version` key and a successor ADR.
The tolerant root-level parsing is the seam.

### 3. Kill-switch discipline is two-tier, file-based, fail-safe

Matches ADR-050 exactly — the operator learns one convention and it
applies to both the scheduler and the supervisor.

1. **Global** — `~/.cosmon/stand-down.lock` (from `[supervisor]
   kill_switch`). Present → SIGTERM every child, do not respawn, park
   until it disappears. A single `touch` halts the whole fleet; a
   single `rm` resumes it. Shared with `cosmon-scheduler`: one lock
   disables both tick-driven patrols and event-driven daemons.
2. **Per-daemon** — optional `kill_switch` field. Present → SIGTERM
   that child alone, leave others running. Intended for galaxy-local
   conventions (mailroom's `~/.mailroom/stand-down.lock` keeps
   working).

**Invariants:**

- **Files, not commands.** Presence check via `path.exists()`. Visible
  to `ls`, survives reboot, scriptable from a three-line hook, does
  not require a running supervisor to inspect.
- **Fail-safe direction**: presence means *stop*. If the check fails
  for any reason (permission denied, disk error), the supervisor
  treats the child as *kill-switched* and does not spawn. Safer to
  under-run than to over-run.
- **Kill-switch precedence**: global takes priority over enabled flag
  takes priority over per-daemon. Encoded once in `policy.rs`,
  exercised by unit tests.

### 4. Respawn is fixed-throttle in v0 (Q2)

Each `DaemonSpec` has a single `throttle_seconds: u64` (default 30).
After the child exits the supervisor waits `throttle_seconds` before
respawning. A value of `0` means respawn immediately.

Exponential backoff, circuit breakers, and configurable healthchecks
are **out of scope for v0** (Niel's cheapest-first). This covers
~95% of crash-loop cases in practice: the throttle window is long
enough to absorb transient resource contention (DNS flap, port reuse
on restart, long-poll 409 Conflicts as seen with Telegram bots)
without hammering downstream services.

Re-evaluation trigger (see §7 below): if any supervised daemon
exhibits a crash pattern that a fixed throttle cannot dampen (e.g.
exponential load from a failing dependency), a successor ADR
introduces exponential backoff with opt-in via `backoff = "exponential"`.

### 5. Targeted diff reload (Q3)

On a `ConfigChanged` event the supervisor re-reads the TOML, computes
`reload::diff(old, new)`, and applies a partition:

- **spawn** — name present in `new` only.
- **kill** — name present in `old` only.
- **keep** — name in both, same BLAKE3 content hash.
- **changed** — name in both, different hash → stop the child, respawn
  with the new spec.

**Proptest invariant** (enforced): for any `old`, `new`, the diff is
a *partition* of `old ∪ new` — every name appears in exactly one of
the four buckets.

**Debounce**: 200 ms window on the `notify::Watcher` stream to absorb
editor save storms (most text editors emit 2–5 events per write). The
hash-based "changed" classification is the second line of defense:
even if duplicate `ConfigChanged` events slip through the debounce,
identical hashes classify as `keep` and the child is not restarted.

**TOML parse failure on reload is non-fatal**: the supervisor keeps
its current child set running, logs the error, and retries on the
next `ConfigChanged`. A typo does not crash the whole fleet.

### 6. SIGTERM → SIGKILL cascade on stop (Q5)

When a child must stop (kill-switch, removed from TOML, reloaded-with-
changes, supervisor shutdown), the cascade is:

1. Send `SIGTERM`.
2. Wait `DEFAULT_TERM_GRACE` (constant in `event_loop.rs`, 5 s).
3. If the child is still alive, send `SIGKILL`.
4. Reap via `wait()`, update state file.

This is the only irreversible failure mode identified in feasibility
R1: a mis-handled cascade can leak lockfiles, socket files, or
runtime state across reboots. The 48 h soak of `notification-bot`
(T4) was the first live validation. Subsequent migrations (T5, this
ADR) add five more daemons without changing the cascade logic.

Grace timeout is **not configurable per-daemon in v0**. If a daemon
legitimately needs longer than 5 s to stop (slow flush, distributed
coordination), v1 introduces `shutdown_timeout_seconds`.

### 7. One-by-one migration, archive never delete (Q6)

Migration from N plists to 1 LaunchAgent follows a strict order of
increasing blast radius. Each step is individually reversible in under
a minute.

| Order | Daemon | Blast radius | T5 date |
|-------|--------|--------------|---------|
| T4 | `notification-bot` | Telegram bot, long-poll, resumes on respawn | 2026-04-19 |
| T5.1 | `notification-bot` | Telegram bot (same pattern as tg-bot) | 2026-04-19 |
| T5.2 | `emacs-daemon` | Editor daemon, fast restart, no network state | 2026-04-19 |
| T5.3 | `zotero-mcp` | MCP server, SSE transport, in-memory cache | 2026-04-19 |
| T5.4 | `almanac` | MCP server (same shape as zotero-mcp) | 2026-04-19 |
| T5.5 | `archive-service` | MCP server, local SQLite-backed | 2026-04-19 |
| T5.6 | `noogram-dashboard` | Flask HTTP server, port-bound | 2026-04-19 |

**Per-daemon migration protocol (binding):**

1. Add the `[[daemon]]` entry to `~/.config/cosmon/daemons.toml` with
   `enabled = false` (staged, not yet spawned).
2. `launchctl unload -w ~/Library/LaunchAgents/<LABEL>.plist` — stops
   the existing daemon cleanly.
3. `mv ~/Library/LaunchAgents/<LABEL>.plist scripts/launchd/archived/<LABEL>.plist`
   — **never delete**. The archive is the rollback artifact.
4. Flip `enabled = true` in the TOML. Supervisor hot-reloads and
   spawns the daemon under its management.
5. Smoke-test (ping the port, send a message, open the editor, …).
6. Update `scripts/launchd/archived/README.md` inventory row.

**Rollback protocol (binding):**

1. Set `enabled = false` in `daemons.toml` (or remove the entry).
2. `cp scripts/launchd/archived/<LABEL>.plist ~/Library/LaunchAgents/`
3. `launchctl load -w ~/Library/LaunchAgents/<LABEL>.plist`

Total reversal: ~10 s per daemon. The two systems never cohabit
actively (unload + archive happens before `enabled = true`), so there
is zero double-spawn risk.

### 8. pid-alive healthcheck only in v0 (Q7)

The supervisor considers a child "alive" iff `wait()` has not yet
returned an exit status. No HTTP probe, no file-based heartbeat, no
stdout-pattern match. If a daemon wedges (deadlock, TCP stall,
infinite loop) but its process is still alive, the supervisor does
not intervene — the operator notices through the daemon's own
side-effects (missed messages, stale HTTP responses).

This is a deliberate v0 scope restriction: pluggable healthchecks
require a shape decision (HTTP probe? file mtime? log line match?
command exit code?) that has no obvious best answer without real
failure data. v1 will propose a `[[daemon.healthcheck]]` stanza
informed by whatever failure modes the first year of supervision
surfaces.

### 9. Peers in v0 (Q8): supervisor and scheduler both under launchd

`cosmon-scheduler` (tick-driven) and `cosmon-daemon-supervisor`
(event-driven) are **siblings**, both installed as LaunchAgents:

| Label | Mode | Cadence | State |
|-------|------|---------|-------|
| `com.cosmon.scheduler` | Propelled | `StartInterval=60` | ~/.cosmon/scheduler.state.json |
| `com.cosmon.daemon-supervisor` | Autonomous | `KeepAlive=true` | ~/.cosmon/daemon-supervisor.state.json |

They share the global kill-switch (`~/.cosmon/stand-down.lock`) and
the `~/.cosmon/` directory, but otherwise run independently. This is
the smallest blast radius — if either misbehaves, the other keeps
working.

The Resident Runtime ratified by
[ADR-095](095-resident-runtime-ifbdd-path.md) is installed as a third
supervised daemon under this canal (`cosmon-runtime` block in
`daemons.toml`); see ADR-095 §5 and
[`docs/guides/install-resident-runtime.md`](../guides/install-resident-runtime.md)
for the binding and the template block.

v2 may revisit supervisor-owns-scheduler (i.e. add the scheduler as
a `[[daemon]]` entry in `daemons.toml` and remove
`com.cosmon.scheduler.plist`). Not done now because:

- The scheduler runs fine alone.
- Nesting would conflate two different clocks (external 60 s tick vs
  internal file-watch) in one supervised child, complicating the
  cascade semantics.
- `KeepAlive` wrapping a `StartInterval` process means every scheduler
  exit respawns immediately, bypassing the 60 s tick floor.

A successor ADR is the right place for that change.

### 10. Q1: New crate `cosmon-daemon-supervisor` (not a scheduler module)

The supervisor lives in a dedicated crate rather than a submodule of
`cosmon-scheduler`. Reasons:

- **Different execution mode** (event-driven vs tick-driven) →
  different test surfaces, different port traits.
- **Different lifetimes** (long-lived binary vs one-shot tick binary)
  → different composition-root shape.
- `cosmon-scheduler` is already ~2.5 kLOC; bundling the supervisor
  would push it past 5 kLOC and dilute both responsibilities.
- Separate crate preserves the "one crate, one role" discipline.

### 11. Alignment with existing invariants

| Invariant | How this ADR respects it |
|-----------|--------------------------|
| No daemon in core (ADR-016) | Supervisor is in `cosmon-daemon-supervisor`, a separate crate; `cs` CLI is stateless read-only over the state file. |
| One concept, infinite extensibility (CLAUDE.md §Composability) | Adding a daemon is one TOML entry. No new code, no new plist, no new `cs` command. |
| Control plane vs data plane | DAG (control) is not touched. The supervisor is a process-lifetime surface on the filesystem (data plane): reads TOML, writes state.json, spawns children. Not a message bus. |
| CLI-first for workers | Workers do not interact with the supervisor. The operator does, via `cs daemons`. |
| Merge-before-dispatch | N/A — supervisor is not on the molecule DAG. |
| Syzygie cross-galaxy | Mailroom, showroom, and any future galaxy contribute `[[daemon]]` entries to the one `daemons.toml`. `kill_switch` per daemon lets galaxy-local lockfile conventions survive unchanged. |

### 12. Re-evaluation criteria (binding flip-conditions)

This ADR expects revision — and names the triggers explicitly — if any
of the following become true:

- **Crash pattern fixed throttle cannot dampen**: a supervised daemon
  crashes with a cadence that hammers a downstream dependency
  regardless of `throttle_seconds`. Triggers exponential-backoff
  successor ADR (v1 enhancement to Q2).
- **Wedge failure mode surfaces**: a daemon stays `alive` per
  `wait()` but produces no useful work (TCP deadlock, infinite loop).
  Triggers healthcheck successor ADR (v1 enhancement to Q7).
- **Scale**: daemon count exceeds ~30 and reload latency exceeds
  ~500 ms. At that point hashing every spec on every reload starts
  to matter. Successor ADR evaluates lazy diffs or per-section
  hashing.
- **Linux port**: the fleet runs on Linux. The core is already
  port-abstracted; systemd adapter is a natural addition. Successor
  ADR picks between user-level systemd units, podman-style
  containerization, or keeping the supervisor and skipping systemd.
- **Shared-runtime ambition**: the resident runtime
  (`cs run --resident`, ratified by
  [ADR-095](095-resident-runtime-ifbdd-path.md)) is now an installed
  supervised child of *this* supervisor (entry `cosmon-runtime` in
  `daemons.toml`; install canal binding at ADR-095 §5). The
  flip-condition fires if the runtime grows enough overlap with the
  supervisor that the parent-child relationship becomes awkward —
  e.g., the runtime begins watching files the supervisor already
  watches, or the supervisor learns to plan DAG dispatch. Successor
  ADR revisits Q8 in that case.

A single flip-condition firing is sufficient to reopen the question.
The supervisor's dumbness is the feature; we buy it back only when
the evidence demands it.

### 13. Non-goals (explicit, out of v1)

- **No exponential backoff.** Fixed `throttle_seconds`, period.
- **No healthchecks.** pid-alive is the healthcheck.
- **No cgroup / resource limits.** launchd `SoftResourceLimits` was
  not used by any of the migrated plists; adding it now would be
  premature.
- **No log rotation.** Operator runs `newsyslog(5)`.
- **No Linux port today.** Core is portable; adapter ships when
  needed.
- **No merge with scheduler.** Peers in v0; successor ADR if painful.
- **No GUI supervision.** `cs daemons list` + log files suffice.

### 14. Triage of disabled / unknown plists

The feasibility doc flagged three categories of plists that were not
in the migration scope but needed a decision:

- `com.clawmetry.*.plist.disabled` (3 plists, trailing `.disabled`
  suffix — launchd ignores them). **Decision: leave as-is.** They are
  archived in place by filename convention; deleting them would lose
  the rollback path for a pre-cosmon telemetry experiment. Not
  adopted into `daemons.toml`.
- `dev.noogram.mailroom.notes-u2-probe.plist`. **Decision: leave
  as-is.** Short-lived probe agent; owner (mailroom) manages its
  own lifecycle. Outside the supervisor's responsibility until the
  galaxy asks to adopt it.
- `ai.openclaw.gateway.plist.disabled`. **Decision: leave as-is.**
  Disabled by owner; not a cosmon-managed daemon.
- Google and Grammarly plists. **Decision: never touched.** Vendor
  software; out of operator's ownership.

None of these become `[[daemon]]` entries. The supervisor manages
exactly the seven daemons listed in Q7's migration table.

## Consequences

**Positive:**

- Adding a daemon is one TOML entry. No XML, no `launchctl load`, no
  new Rust code. The substrate stops biasing against running
  long-lived Cosmon processes.
- One kill-switch (`~/.cosmon/stand-down.lock`) halts *both* the
  scheduler and the supervisor — reboot-survivable, scriptable,
  visible to `ls`. Unified halt for the whole fleet.
- `cs daemons list`/`status` answers "is X running? since when? how
  many respawns?" in <1 s without log archaeology.
- The "no daemon in core" invariant gains a load-bearing counter-
  example: a non-trivial resident surface that lives *outside* the
  core, never leaks back, and is reachable only through the
  filesystem boundary (state file, TOML, `stand-down.lock`).
- Migration is reversible per-daemon in ~10 s. Pilot risk bounded.
- ADR-050 (scheduler) and this ADR (supervisor) share vocabulary,
  TOML conventions, kill-switch semantics, archive-never-delete
  discipline. The operator learns one shape, applies it twice.

**Negative:**

- The supervisor is the first truly-resident binary cosmon owns. The
  Autonomous regime moves from "future" to "today"; reviewers must
  check future cosmon PRs for regime leaks (i.e. tick-style logic
  slipping into the supervisor, or supervision-style logic into
  the scheduler).
- Two similarly-named TOMLs coexist (`patrols.toml`, `daemons.toml`).
  Mitigation: vocabulary discipline ("patrol" = cron, "daemon" = keepalive)
  and distinct paths; the schemas share no tables.
- Archived plists accumulate in `scripts/launchd/archived/`. After
  ~1 year a sweep ADR may GC them; until then, the audit trail is
  their value.
- TOML schema v1 is frozen. Adding a field requires a schema bump
  and successor ADR. Accepted cost.
- A daemon wedged but not crashed (deadlock) is invisible to the
  supervisor. Operator notices via the daemon's own side-effects.
  v1 healthchecks close this gap when real data arrives.

**Neutral:**

- The supervisor is not a molecule. It is adjacent infrastructure that
  keeps declared processes alive. Molecules remain the only thing
  cosmon tracks.
- `cs daemons status` is read-only. A hypothetical `cs daemons stop`
  or `cs daemons restart` would belong to the supervisor binary
  itself (via touch-reload or `SIGHUP`), not to `cs`. Single-perimeter
  discipline preserved.
- The install script (`scripts/install-daemon-supervisor.sh`) is the
  only path that writes to `~/Library/LaunchAgents/`. No other cosmon
  code mutates the launchd domain.

## Implementation Sequence (done at time of merge)

1. **T1 (done)**: Scaffold `cosmon-daemon-supervisor` crate — config,
   model state machine, reload diff, policy, ports/adapters skeleton
   (`task-20260419-bed0`, merged).
2. **T2 (done)**: Real adapters + event loop — `tokio_process`,
   `notify_watcher`, `filestore`, signal cascade, double-spawn
   guard (`task-20260419-e17a`, merged).
3. **T3 (done)**: `cs daemons` CLI + install script + plist template
   (`task-20260419-f7b6`, merged).
4. **T4 (done)**: Migrate `notification-bot` + 48 h soak
   (`task-20260419-b31b`, merged; chronicle
   "Un seul concierge pour six appartements").
5. **T5 (this ADR)**: Lock Q1–Q8 in binding form, migrate
   `notification-bot`, `emacs-daemon`, `zotero-mcp`, `almanac`,
   `archive-service`, `noogram-dashboard` (`task-20260419-5ad4`).

## Rollback (whole-ADR)

If the supervisor is retired as a concept, the full reverse is:

1. `scripts/install-daemon-supervisor.sh uninstall` — stops the
   supervisor, unloads `com.cosmon.daemon-supervisor.plist`.
2. For each daemon in `scripts/launchd/archived/`:
   `cp <label>.plist ~/Library/LaunchAgents/` +
   `launchctl load -w ~/Library/LaunchAgents/<label>.plist`.
3. Set every `[[daemon]]` to `enabled = false`, or delete
   `~/.config/cosmon/daemons.toml`.

Total: N × 10 s, where N is the number of archived plists. The
supervisor's TOML-based declarations and the archived plists are the
two halves of a symmetric undo pair; the design guarantees the
reversal path exists by construction.
