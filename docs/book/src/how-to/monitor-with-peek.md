# Monitor the fleet with cs peek

**Goal:** watch what your agents are doing (across one project or many) without
attaching to terminals or tailing raw log files. Cosmon's observability is a
**fractal portal, not a dashboard**: one tool, recursive, from a fleet overview
down to a single worker's live pane.

> Reach for these tools *before* `tmux`, `tail`, or `cat`. If `cs peek` cannot
> show you something, that is a gap to report, not a reason to go back to shell
> archaeology.

## The one tool: `cs peek`

```sh
cs peek
```

`cs peek` is the canonical fleet observation command: a TUI portal. The left
pane lists every worker; the right pane follows your selection. One keystroke
descends one level:

| Key | What it shows |
|-----|---------------|
| `j` / `k` | Move the selection down / up the worker list |
| `p` | The selected worker's live tmux pane: what the agent is doing right now |
| `b` | Briefing: the plan the worker is following |
| `l` | Log: its step-by-step history |
| `e` | Events: the raw event stream |
| `s` | Synthesis (for molecules that produce one) |
| `r` | Responses |
| `q` | Quit |

Press `p` to drop into any worker's output, `q` to come back up. That descend-
and-return is the whole model: you keep the fleet view while you inspect one
worker.

### Across many projects at once

```sh
cs peek --all
```

`--all` aggregates every tmux session and every `.cosmon/` on disk, so you get
the multi-project view from any directory.

## One-shot snapshots (for scripts and quick checks)

`cs peek` is the live portal. When you want a single printed snapshot instead
(in a script, a CI log, a quick glance) use these:

```sh
cs ensemble          # table of every worker: role, health, cost, molecule
cs status            # a quick DAG overview, like `git status`
cs pulse             # runtime-vitality reading: a tachometer + status lights
```

`cs ensemble --json` (and `--json` on the others) gives machine-readable output.
The `EFFECTIVE` / `LIVE` columns in `cs ensemble` are your health signal:
`healthy` + `working` is good; `suspect` / `stale` means a worker has stopped
progressing; take it to [Recover a crashed agent](./recover-crashed-agent.md).

## When something breaks

```sh
cs errors            # aggregate molecule-collapse events into one failure view
cs health            # read-only anomaly catalog across the fleet
```

`cs errors` answers "what is breaking the fleet, and which molecules are hit,"
with a `--since 7d` window and a `--kind` filter for a specific failure class.
`cs health` mutates nothing; it is the safe first look during triage. Add
`--all` to `cs health` to scan every project federation-wide.

## The live event stream

To follow events as they land, the raw NDJSON history a molecule writes:

```sh
cs tail --follow
```

`cs tail` is a `notify`-driven reader over the fleet's `events.jsonl`. `--follow`
stays attached and streams new events; `--all-galaxies` widens it across every
project (opt-in; cross-project reach is never implicit).

## Anti-patterns: do not do these

| Instead of… | Use… | Why |
|-------------|------|-----|
| `tmux attach` to a worker's session | `cs peek` + `p` | Attaching breaks the agent's rendering and confuses it. |
| `watch cs observe …` in a shell loop | `cs wait <id> &` | Hand-polling burns CPU and misses transitions between polls. |
| `tail -f` on `events.jsonl` | `cs tail` | Same stream, structured and fleet-aware. |
| `cat` a briefing from a random terminal | `cs peek` (`b`) | Loses fleet context. |

## See also

- [Running a fleet of agents](../tutorials/first-fleet.md): `cs peek` in a live run.
- [Recover a crashed agent](./recover-crashed-agent.md): acting on a `stale` worker.
- [Observability commands reference](../reference/observability.md): every flag.
