# Cardinal patrols — the autopilot primitive in practice

> **The primitive already exists.** `cs autopilot tick` was the name proposed
> in [idea-20260417-66d8](../../.cosmon/state/fleets/default/molecules) and
> in [task-20260428-7b63](../../.cosmon/state/fleets/default/molecules). It
> was **already built** under a different name: the `cosmon-scheduler` binary
> + `~/.config/cosmon/patrols.toml` + `cs scheduler status`
> ([ADR-050](../adr/050-unified-patrol-scheduler.md), implementation
> idea-20260417-b52d). Building a second `cs autopilot tick` command would
> duplicate that perimeter — a single-perimeter violation (see CLAUDE.md
> coherence checklist #4). This guide closes the real gap instead: it shows
> how the *existing* scheduler instantiates every cardinal use-case the
> autopilot brief named, and it documents the one missing affordance that
> shipped with it — `cs scheduler validate`.

The autopilot primitive is **declarative**: you add a `[[patrol]]` block to
`~/.config/cosmon/patrols.toml`, the scheduler re-reads the file on its next
tick (default 60s — no reload command, no signal), and the patrol fires on
its cadence. A patrol is **advisory by default**: it `cs nucleate`s a molecule
(producing a digest/triage artifact) and does *not* tackle it. A patrol that
tackles is an explicit operator choice — make the `command` a script that
calls `cs tackle`, and keep it out of this file until you mean it.

## Anatomy of the success criteria → where each one lives

| Brief criterion | Mechanism in the shipped scheduler |
|-----------------|------------------------------------|
| (i) add a patrol by editing the file | `[[patrol]]` block + hot-reload on next tick; `cs scheduler validate` pre-flights it |
| (ii) disable without deleting | `enabled = false` field (kept in-file for rollback/documentation) |
| (iii) audit trail | `~/.cosmon/scheduler.state.json` (last-fire, exit code, fire count) + `~/.cosmon/scheduler.log` |
| (iv) query the schedule any time | `cs scheduler status` (table) / `--json` (NDJSON) |
| (v) a crash does not crash the daemon | each patrol is a **detached subprocess**; the scheduler returns immediately — there is no daemon to crash, only a per-tick launcher |

## Pre-flight: `cs scheduler validate`

Before wiring a new patrol, lint the file. Zero side-effects — no state read,
no dispatch, no kill-switch touch — safe to run while the scheduler ticks:

```
cs scheduler validate                      # ~/.config/cosmon/patrols.toml
cs scheduler validate --config cand.toml   # a candidate file before promoting
cs scheduler validate --json               # CI / scripting
```

Exit `0` = valid, exit `1` = malformed or semantically invalid (every
offending patrol is listed in one pass). The CI-facing twin is
`cosmon-scheduler validate` (exit `2` on any config error), suitable for a
pre-commit hook on the dotfiles repo that owns `patrols.toml`.

## The five cardinal patrols

Copy the blocks you want into `~/.config/cosmon/patrols.toml` under the
`[scheduler]` header, then `cs scheduler validate`. Each is **advisory** —
it nucleates a molecule; the operator triages it from the inbox.

```toml
[scheduler]
state_file            = "~/.cosmon/scheduler.state.json"
log_file              = "~/.cosmon/scheduler.log"
kill_switch           = "~/.cosmon/stand-down.lock"
tick_interval_seconds = 60

# (i) cosmon-ward-mayor — daily triage of 'cosmon-ward' issues across
#     galaxies (task-20260428-d164). Advisory: nucleates a triage molecule;
#     the operator decides what to tackle.
[[patrol]]
name        = "cosmon-ward-mayor"
cron        = "0 6 * * *"            # every day 06:00
command     = ["cs", "nucleate", "cosmon-ward-mayor"]
working_dir = "/srv/cosmon/cosmon"
enabled     = true

# (ii) reading-club-tick — weekly fetch of new papers cited in active Zotero
#      collections. Advisory: produces a digest of new references.
[[patrol]]
name        = "reading-club-tick"
cron        = "0 8 * * 1"           # Mondays 08:00
command     = ["cs", "nucleate", "reading-club-tick"]
working_dir = "/srv/cosmon/knowledge"
enabled     = true

# (iii) leaks-watchdog — daily scan of accord artefacts signed in the last
#       24h for confidentiality (D7 strict, post-signature). Advisory.
[[patrol]]
name        = "leaks-watchdog"
cron        = "0 7 * * *"           # every day 07:00
command     = ["cs", "nucleate", "leaks-watchdog"]
working_dir = "/srv/cosmon/accord"
enabled     = true

# (iv) backlog-frontier-rot — alert when a molecule has sat 'pending' > N days
#      with no temp:* tag (a dormant frontier). Advisory: nucleates a
#      temp-review sweep. Gated on an env var so the same file is portable.
[[patrol]]
name             = "backlog-frontier-rot"
interval_seconds = 86400            # daily
command          = ["cs", "nucleate", "temp-review"]
working_dir      = "/srv/cosmon/cosmon"
enabled          = true

# (v) digest-personnel — weekly synthesis of atomic decisions still pending
#     (the operator-facing inbox digest). Advisory.
[[patrol]]
name        = "digest-personnel"
cron        = "0 18 * * 0"          # Sundays 18:00
command     = ["cs", "nucleate", "digest-personnel"]
working_dir = "/srv/cosmon/cosmon"
enabled     = true

# A disabled patrol kept in-file for documentation / rollback (criterion ii).
[[patrol]]
name             = "legacy-noop"
interval_seconds = 3600
command          = ["true"]
enabled          = false
```

> The `cs nucleate <formula>` targets above assume a formula of that name
> exists in the target galaxy's `.cosmon/formulas/`. Until a formula ships,
> point `command` at any script that produces a digest — the scheduler does
> not care what the command *is*, only *when* to fire it. That is the whole
> composability point: a new patrol is a new line, never a new command.

## Safety: kill-switch and scope

- **Global stand-down**: `touch ~/.cosmon/stand-down.lock` — the scheduler
  skips every patrol at the next tick until the file is removed. No child is
  killed; already-firing patrols finish on their own.
- **Per-patrol kill-switch**: set `kill_switch = "~/.cosmon/skip-foo.lock"`
  on a single `[[patrol]]` to silence it alone.
- **Galaxy scope**: a patrol's reach is its `working_dir` + the `command` it
  fires. There is no separate `autopilot.toml` galaxy-enumeration file in the
  shipped design — scope is expressed per-patrol, which is simpler and keeps
  one file the single source of truth.

## Wiring the tick

One macOS `LaunchAgent` (or Linux cron line) invokes `cosmon-scheduler tick`
every `tick_interval_seconds`. Between ticks nothing runs — this honors the
"no daemon in core" invariant ([ADR-016](../adr/016-autonomy-regimes-and-resident-runtime.md)).
See [`scripts/install-scheduler.sh`](../../scripts/install-scheduler.sh) for
the template. Test a tick by hand without firing anything:

```
cosmon-scheduler tick --dry-run        # one FIRE/SKIP/SUNSET/INVALID row per patrol
```

## Runtime-independent stall detectors (CV-5)

`delib-20260608-6a5f` (the 3am-silence post-mortem) established that the stall
detectors **must run in the always-on `cosmon-scheduler` layer**, with zero
dependence on `cosmon-runtime`. A watchdog whose liveness is coupled to the
thing it watches is blind in exactly the worst case — runtime dead *and*
worker blocked. These three patrol flags are pure reads over state on disk
(`molecules` + `events.jsonl` + presence sidecars); they touch no transport,
no tmux, no `cs run` loop, so they fire even when every runtime is dead.

```toml
# --- livelock: circular blocked-on waits (Tarjan SCC over presence sidecars).
#     Reports / nucleates only on an actual cycle — low false-positive risk.
[[patrol]]
name             = "cosmon-livelock"
interval_seconds = 300
command          = ["/Users/you/.local/bin/cs", "patrol", "--livelock"]
working_dir      = "/srv/cosmon/cosmon"
log_file         = "~/.cosmon/logs/cosmon-livelock.log"
dispatch         = "detached"
enabled          = true

# --- event-age: the EXTERNAL-MODAL backstop. For every Running molecule,
#     ALERT (tiered by irreversibility) when its most recent event-log append
#     is older than 15 min. Keys on *any* event, so it catches a worker parked
#     at a Claude Code AskUserQuestion modal that emits no cosmon state at all.
#     Fold it into the existing cosmon-fleet-propel patrol's command:
#         command = ["…/cs", "patrol", "--propel", "--event-age"]
#     ⚠ The `--event-age` flag ships in THIS molecule (task-20260608-014f).
#     Do NOT add it to the live command until the post-merge `just install`
#     has refreshed ~/.local/bin/cs — an older binary rejects the unknown
#     flag and the patrol would error every tick. Apply after `cs done`.

# --- silence-detect: heartbeat-age sweep. ARMED-PENDING-PRECONDITION.
#     As of 2026-06-08 workers do NOT emit WorkerHeartbeat in practice (the
#     live event log holds a single heartbeat across 340k events). Enabling
#     this on the 60s tick today would flag every long-running molecule as
#     "never heartbeat" → flood `cs notify` and spuriously tag temp:frozen —
#     the precise alert-fatigue failure CV-6 warns against. Keep enabled=false
#     until heartbeat emission is universal; event-age is the backstop until
#     then. (Surfaced cosmon-ward from task-20260608-014f.)
[[patrol]]
name             = "cosmon-silence-detect"
interval_seconds = 120
command          = ["/Users/you/.local/bin/cs", "patrol", "--silence-detect", "--silence-after", "300"]
working_dir      = "/srv/cosmon/cosmon"
log_file         = "~/.cosmon/logs/cosmon-silence-detect.log"
dispatch         = "detached"
enabled          = false
```

> **Why staged, not a live worktree edit.** Wiring is a deployment step the
> operator (or the `cs done` post-merge hook) performs once the new binary is
> installed — a worker cannot safely flip `--event-age` on before the install
> that teaches `~/.local/bin/cs` the flag. The blocks above are the canonical
> source; copy them under the `[scheduler]` header and run
> `cs scheduler validate` before the next 60s tick picks them up.

## See also

- [ADR-050 — unified patrol scheduler](../adr/050-unified-patrol-scheduler.md)
- `cs scheduler --help` (the `IMAGE` / `WHEN TO USE` / `HOT-RELOAD` blocks)
- `cs daemons` — the sibling for **long-running** processes (the night
  watchman, vs the scheduler's alarm clock)
- [CHRONICLES.md](../lore/CHRONICLES.md) — "2026-06-05 — Le primitive déjà
  construit" (why this task shipped `validate` + this guide, not a duplicate
  `cs autopilot`)
