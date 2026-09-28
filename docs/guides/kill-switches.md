# Kill switches

cosmon stops autonomous activity when a file exists under `~/.cosmon/`.
Presence means stop; removing the file resumes. One file stops everything,
and three narrower files each stop one area.

| File | Scope | Stops |
|---|---|---|
| `stand-down.lock` | global | every component below |
| `health.off` | healing | `cs patrol --heal`, `cs patrol --propel-api-stall` |
| `autopilot.off` | autopilot | the nightly curate sweep (`scripts/curate-all-galaxies.sh`) and the step checks of the `curate-patrol`, `peau-morning-digest` and `voix-reply` formulas |
| `ask.off` | ask | `cs ask --execute` dispatch |

What `stand-down.lock` stops:

- `cosmon-scheduler` skips every patrol (ADR-050). Its path is the
  `[scheduler] kill_switch` config value, which defaults to this file.
- `cosmon-daemon-supervisor` SIGTERMs every child and does not respawn
  (ADR-053). Its path is the `[supervisor] kill_switch` config value, which
  defaults to this file.
- `cs patrol` performs no sweep at all: no respawn, propel, nudge, expire,
  orphan freeze, harvest, heal, API-stall propulsion or dialogue
  auto-confirm. It prints `stood_down` and exits 0.
- `cs ask --execute` records a `kill_switched` audit line and does not
  dispatch.
- The curate sweep and the autopilot formula steps abort.

`cs status` prints a `kill-switch` line naming every active file, and
`cs status --json` carries a `kill_switches` array with every file, its
scope, and whether it is active.

Two per-unit controls stay separate because they are not global: a
per-patrol `kill_switch` in `patrols.toml` and a per-daemon `kill_switch`
in `daemons.toml`. Per molecule, a `health:hold` tag or a `.no-heal`
sentinel keeps healing away from one molecule (ADR-137 §5).

The catalogue lives in `cosmon_core::kill_switch`; the `cs` side reads the
files through `crates/cosmon-cli/src/kill_switches.rs`.
