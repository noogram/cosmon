<!-- SPDX-License-Identifier: AGPL-3.0-only -->

# Optional Cosmon views in Herdr

Run this companion explicitly from an existing galaxy checkout. Python 3
standard library, Git and `cs` suffice for observations; Herdr is optional.
Nothing is installed or enabled. Removing this directory leaves Cosmon intact.

```sh
python3 tools/herdr-companion/cosmon-herdr.py list
python3 tools/herdr-companion/cosmon-herdr.py list --all
python3 tools/herdr-companion/cosmon-herdr.py show task-example-0001
```

Output is JSON. `list` uses the one-shot `cs --json observe` list, and `show`
uses its detail form. Selection requires the complete, exact molecule id;
Cosmon's prefix convenience is refused. The reader never invokes `peek`
(whose normal loop can propel work), patrol, lifecycle verbs or terminal input.
It stores no cache and parses no private state files.

The view separates molecule status, unknown terminal presence, existing Git
worktree and canonical deliverables. It timestamps the observation; the facts
can change after the read. `completed` does not establish a live process,
artifact acceptance or harvest. A branch supplied by `cs observe` is joined
by exact ref equality to `git worktree list --porcelain -z`; no path is guessed
from an id. This identifies the branch checkout, not the worker's current cwd.
Absent or ambiguous mappings stay unavailable; nothing creates a worktree.

`molecule_dir` from `cs observe` supplies the canonical artifact residence.
The reader exposes existing regular `synthesis.md`, `frame.md`, `outcomes.md`,
`plan.md`, `report.md`, `result.md` and `responses/*.md`, and lists missing
conventional root files separately. Symlinks are excluded. These are file
observations, not a new artifact manifest or a success verdict. Custom and
binary outputs are outside this first companion's discovery set.

## Open an explicit view

Herdr's daemon CLI (`herdr`, tested against 0.9.5) and Herdr GPUI (`Herdr`,
tested against 20261016.1 protocol 3) are distinct executables. Find the local
executables and inspect their help; `--herdr` and `--herdr-ui` accept executable
paths. These flags and `--repo` precede the subcommand. No shell text is built
from paths, labels or ids; all external calls use argv and a bounded wait.

Use `herdr --session NAME workspace list` to choose an **existing local**
session and workspace. Opening a worktree creates only a new shell tab there:

```sh
python3 tools/herdr-companion/cosmon-herdr.py open task-example-0001 \
  --worktree --herdr-session NAME --workspace WORKSPACE_ID
```

The command re-observes the molecule and Git mapping before calling
`herdr --session NAME tab create --workspace WORKSPACE_ID --cwd PATH --no-focus`.
It does not use Herdr's worktree creation/removal machinery. Closing this shell
tab removes the view and its own shell; it cannot harvest or stop a Cosmon
worker. Do not start a worker inside the view and infer lifecycle integration.

Use `Herdr window list --json` to choose an explicit GPUI window, then open a
deliverable read-only in a code tab:

```sh
python3 tools/herdr-companion/cosmon-herdr.py open task-example-0001 \
  --artifact synthesis.md --window WINDOW_ID
```

This calls `Herdr code open PATH:1 --window WINDOW_ID --json`, never `code edit`
or `browser open`. A code tab does not serve the canonical directory over HTTP.
The companion has no close hooks, background refresh, worker backend or mutation
controls. Herdr unavailability/refusal returns JSON `availability: unavailable`
and exit 1, without falling back to a different window or spawning a worker.

## Terminal capability gap

`open ID --terminal` currently returns unavailable. The supported
`cs observe` detail has `worker`, `branch` and `molecule_dir`; it has no
transport backend, socket/session address or worker incarnation binding.
The `ensemble` worker `live`/`effective` fields also do not supply an attach
address. A worker label, pane label or derived tmux session name cannot establish
that association. This increment deliberately leaves terminal presence unknown
and never calls tmux or a production pane. An explicit public transport
observation is required before attachment can be implemented safely.

No core CLI, API schema, generated help or default transport changes. The
authority boundary follows ADR-095 amendment A3–A7, ADR-080 and ADR-168.

## Verification

```sh
python3 tools/herdr-companion/test_companion.py
```

Subprocess tests use real disposable Git worktrees and recording Cosmon/Herdr
executables to witness exact selection, safe argv, missing data and refusals.
These tests are not GPUI acceptance or a native worker qualification. A real
UI smoke test must use only its own named disposable workspace/panes and report
its actual outcome separately.
