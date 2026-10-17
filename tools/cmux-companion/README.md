<!-- SPDX-License-Identifier: AGPL-3.0-only -->

# Client integrations: Cosmon in cmux

An optional, standalone Python client for listing/selecting molecules, opening
a worker output view, opening an existing worktree shell and reading canonical
Markdown deliverables in cmux. It consumes the public Cosmon JSON contract
independently of the Herdr package. Cosmon has no cmux dependency.

Built by Noogram — open agent infrastructure and AI tooling.

## Requirements and installation

Python 3.9+, Git, a local `cs` providing terminal contract v1, and a running cmux
macOS app with socket API v2 and `workspace.create`. The CLI inherits cmux's
existing socket discovery/authentication; `--socket PATH` explicitly selects
another socket. A missing socket or denied access is reported unavailable.
This helper never changes socket permissions, settings, hooks or startup files.

Worker views also require the public `cs terminal` prerequisites: tmux 3.2+,
macOS/Linux process fingerprints, a single live window/pane and a witnessed
launch. A legacy launch without a witness stays unknown. The cmux app is a
local client, not a worker backend or a remote API implementation.

Use this directory from a stable Cosmon checkout; there is no build, package
manager, plugin registry or background service to install. Optionally copy the
directory to a location you own and keep its Python files with the README:

```sh
mkdir -p /path/to/client-tools
cp -R tools/cmux-companion /path/to/client-tools/cosmon-cmux
python3 /path/to/client-tools/cosmon-cmux/cosmon-cmux.py \
  --repo /path/to/galaxy --project-id YOUR_PROJECT_ID launch
```

The helper is not installed automatically by a Cosmon upgrade. Do not link to
a disposable worker worktree that will disappear at harvest.

From that checkout, with the expected project id read from `[project].project_id`
in the chosen galaxy's `.cosmon/config.toml`:

```sh
python3 tools/cmux-companion/cosmon-cmux.py \
  --repo /path/to/galaxy --project-id YOUR_PROJECT_ID list
python3 tools/cmux-companion/cosmon-cmux.py \
  --repo /path/to/galaxy --project-id YOUR_PROJECT_ID launch
```

`launch` opens a new cmux workspace containing the numbered selector. The new
workspace is created without stealing focus; select its **Cosmon selector**
entry in cmux. `select` runs the same selector directly in the current terminal.
The list defaults to pending/running/frozen entries in the selected fleet;
`h` toggles history, `r` refreshes, and `q` exits. Choose a molecule number,
then `t` for worker output, `w` for a worktree shell, or `a` plus an artifact
number. `b` returns to the list. Refreshes are explicit, not a polling service.

Each action creates a separate workspace, never replaces an existing surface
and never sends input to a worker. Close the worker view with local `q`, Escape,
Ctrl-C or cmux's normal close workspace action. Closing detaches only the view;
the worker continues with the same incarnation. Artifact views close on Enter.
The worktree shell is an ordinary interactive shell in an existing checkout;
commands you run there can edit files. It has no Cosmon lifecycle integration.

Uninstall by closing these client workspaces and removing only your optional
copy of this directory. There is no registration, generated configuration or
resident process to remove. Closing/removing the client never completes,
freezes, collapses, harvests or terminates a worker.

## One-shot commands and boundaries

Global options precede the subcommand. `--cs /absolute/path/to/cs` and
`--cmux /absolute/path/to/cmux` select existing executables; `--fleet` defaults
to `default`. No download or installation occurs.

```sh
python3 tools/cmux-companion/cosmon-cmux.py --repo /path/to/galaxy \
  --project-id YOUR_PROJECT_ID list --all
python3 tools/cmux-companion/cosmon-cmux.py --repo /path/to/galaxy \
  --project-id YOUR_PROJECT_ID show task-20990101-a001
python3 tools/cmux-companion/cosmon-cmux.py --repo /path/to/galaxy \
  --project-id YOUR_PROJECT_ID open task-20990101-a001 --terminal
python3 tools/cmux-companion/cosmon-cmux.py --repo /path/to/galaxy \
  --project-id YOUR_PROJECT_ID open task-20990101-a001 --worktree
python3 tools/cmux-companion/cosmon-cmux.py --repo /path/to/galaxy \
  --project-id YOUR_PROJECT_ID open task-20990101-a001 --artifact synthesis.md
```

Commands return JSON and refusals exit 1. Git's worktree registry resolves the
canonical project root, including invocation from a linked worktree. All Cosmon
calls pin `--config ROOT/.cosmon/state`; inherited state overrides cannot select
another galaxy. `list` reads versioned `ensemble` JSON and filters the fleet.
Details use existing `observe` JSON with exact-id equality, never prefix results.
As with other clients, `observe` appends its existing authorization audit;
lifecycle/process state and artifact bytes are preserved.

Before each action the helper re-observes terminal v1, validates its integer
version, kind, exact id, canonical project/state paths, caller-supplied project
id, fleet and capability. Unknown/unavailable remain distinct; neither permits
attachment. A missing/failed terminal CLI is unknown, never evidence of death.
Unsupported stamps/capabilities and cross-project observations are refused.
Additive fields are ignored. Views, lifecycle completion and artifact acceptance
are independent facts.

For worker output, a small local presenter inherits the new cmux PTY and runs
only the public attachment argv:

```text
cs --config ROOT/.cosmon/state terminal EXACT_ID --attach --expect OPAQUE_TOKEN
```

The fresh token is passed verbatim; addresses inside `identity` are never used.
On refusal the presenter keeps the error visible until Enter, because cmux
otherwise closes a workspace as soon as its initial command exits.
`cs` revalidates the incarnation at attachment, including a stale-token refusal
if the worker changed after the helper's probe. cmux's `workspace.create`
accepts a shell command string rather than an argv array: each argument is
POSIX shell quoted with `shlex.join`, and `initial_command` executes it in the
new workspace. No `send`, `send-key`, direct tmux attachment, shared shell
replacement or worker input action exists in the helper. This assumes cmux's
normal POSIX-compatible startup shell and trusted local executables/settings.

Worktrees are joined by the observed branch to a unique existing Git checkout;
no directory is created or guessed. Artifacts come from the CLI's canonical
`molecule_dir` in the validated state/fleet/id, limited to `synthesis.md`,
`frame.md`, `outcomes.md`, `plan.md`, `report.md`, `result.md` and
`responses/*.md`. Symlinks are excluded and terminal controls are escaped.
Custom/binary outputs need another reader. No private directory is HTTP served.

The generic output-only renderer samples visible frames at four Hz; it is not
a lossless PTY stream or scrollback reader. It sends no keyboard bytes or size
announcements to the worker. It preserves session environment and worker size.
The checks provide local same-user consistency, not an authorization boundary;
the final filesystem/kernel/tmux interval is not atomic and independent tmux
operations/hooks remain operator-owned. See the [public contract](../../docs/book/src/reference/read-contracts.md)
for exact race and platform limits.

## Verification and qualification

```sh
./scripts/no-pilot-env.sh python3 tools/cmux-companion/test_companion.py
./scripts/no-pilot-env.sh cargo test -p cosmon-cli --test terminal_contract
COSMON_CLIENT_RECEIPTS=/path/to/private/receipts \
  ./scripts/no-pilot-env.sh cargo test -p cosmon-cli --test terminal_contract \
  owned_cmux_client_acceptance -- --ignored --nocapture
```

The first checks the independent consumer with recording providers and real
Git. The opt-in third command owns a disposable galaxy and dummy tmux worker,
creates only new cmux workspaces, checks actual selector/terminal/artifact
rendered text, read-only/ignore-size/control flags, size/environment/input
preservation, worker survival after close and stale identity refusal. It closes
only its created workspaces. Private receipts retain failed runs as well as
successes. This is real socket/PTY acceptance, separate from visual UI review,
remote use, native backend qualification, Herdr or Pi acceptance.

Qualified on macOS with cmux 0.65.1 (build 110), socket v2: 13 real disposable
acceptance checks passed, including stale attachment refusal. This establishes
socket/PTY behavior, not a visual UI inspection or support for every cmux build.

Primary references: [cmux CLI contract](https://github.com/manaflow-ai/cmux/blob/main/docs/cli-contract.md)
(`initial_command`, socket RPC v2) and [cmux API](https://cmux.com/docs/api).
The installed cmux CLI, runtime capabilities and actual surfaces must agree;
documentation alone does not qualify a runtime.
