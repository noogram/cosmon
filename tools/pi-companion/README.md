<!-- SPDX-License-Identifier: AGPL-3.0-only -->

# Client integrations: Cosmon in Pi

Optional native Pi extension, independently consuming Cosmon's public readonly
terminal v1 contract. Select a project and molecule, inspect its details, view
worker output and conventional Markdown deliverables, and locate an existing
worktree. Cosmon has no Pi dependency. This is a client, with no worker backend,
lifecycle commands, model tools or automatic model turns.

Built by Noogram — open agent infrastructure and AI tooling.

## Requirements and loading

Pi's official package is `@earendil-works/pi-coding-agent`, maintained at
[earendil-works/pi](https://github.com/earendil-works/pi). This extension targets
Pi **1.1.0**, Node.js 22.19+, Python 3.9+, Git and a local `cs` exposing terminal
v1. Its host imports use Pi's supplied coding-agent and TUI packages. No npm
runtime dependency, build step or provider credentials are needed by the client.
For output views, the public Cosmon prerequisites also apply: macOS/Linux,
tmux 3.2+ and a creation-time process witness. Legacy workers stay unknown.

Keep this directory in a stable checkout or copy the entire directory to a
location you own. A disposable worker worktree will disappear at harvest.
Load it for one invocation without installing or changing settings:

```sh
pi --extension /absolute/path/to/cosmon/tools/pi-companion
```

This is an ordinary Pi package: `package.json` declares its extension entry and
version `0.1.0`. It can be distributed as a directory or an npm tarball produced
with `npm pack --ignore-scripts` in this directory. No package is automatically
published or installed. The official [Pi package documentation](https://github.com/earendil-works/pi/blob/v1.1.0/packages/coding-agent/docs/packages.md)
describes optional persistent installation with `pi install /absolute/path` and
removal with `pi remove /absolute/path` (add `--local` to both for project scope).
Those commands change Pi settings only when you explicitly run them. To remove
one-invocation loading, restart Pi without `--extension`; remove your optional
copy only after closing its views.

## Commands

Run `/cosmon-project` and enter an absolute Git checkout path and that project's
`[project].project_id`. Alternatively provide JSON, which also allows explicit
fleet and executable selection; see [the v1 example](examples/project-v1.json):

```text
/cosmon-project {"root":"/absolute/path/to/galaxy","projectId":"example-project","fleet":"default","cs":"cs"}
/cosmon-list
/cosmon-select task-20990101-a001
/cosmon-details
/cosmon-output
/cosmon-artifacts
/cosmon-worktree
```

`/cosmon` aliases the active-first selector. `/cosmon-list all` includes history.
The default list includes pending/running/frozen molecules in the selected fleet.
Selection uses complete ids, never prefixes. Details, output, artifacts and
worktree commands also accept an explicit complete id. Close a custom view with
`q`, Escape or Ctrl-C. Details and artifact text scroll with up/down arrows.
Selection and refresh are explicit, with no background polling service.
Project selection is session-local and clears the previous molecule selection.

The worktree view displays a path joined against Git's registry and the observed
branch. Navigate with your existing terminal/editor; the extension launches no
shell and changes no Pi working directory. Artifact reads accept only conventional
Markdown deliverables in the public `observe` residence and `responses/*.md`,
with a 1 MiB limit, refusing symlinks. State JSON, transcripts and private store
files are not read. Terminal controls in labels/artifacts are removed.

## Output and lifecycle boundaries

Before output opens, the extension re-observes public terminal v1 and validates
its explicit integer version, kind, complete id, canonical project/state paths,
project id, fleet and readonly capability. Unknown/unavailable remain distinct;
a failed probe is unknown. Unknown versions/capabilities and cross-project
observations are refused. Additive fields are ignored. The incarnation token is
opaque and passed unchanged as one argv entry to:

```text
cs --config STATE_DIRECTORY terminal EXACT_ID --attach --expect TOKEN
```

Pi 1.1.0 has a custom component API, without a native terminal-tab or PTY API.
An owned Python standard-library PTY runs only this public attachment; a native
Pi component displays its output. Keyboard bytes and Pi resize events stay local.
The PTY uses a fixed local presentation size; the public renderer keeps the
worker's size unchanged. Output is sampled visible-pane text, without color,
lossless byte streaming or full scrollback. Long lines are clipped to Pi width;
the view shows the top rows of the sampled frame. It is not an interactive shell.
Stale attachment errors remain visible until you close the view and reopen.

Loading the extension starts no processes, timers, watchers or subscriptions.
Commands own bounded subprocesses; closing output or shutting down/reloading Pi
cleans up owned readers. Closing a view preserves the worker and its incarnation.
No worker input, environment update, resize or lifecycle mutation is performed.
The existing `observe` command may append its authorization audit. Completion,
terminal availability and deliverable acceptance remain independent facts.

## Verification

Dependency-free contract tests:

```sh
node --test tools/pi-companion/test_client.mjs
```

The existing Rust `terminal_contract` harness runs these tests when Node is
available; it reports a skip when absent. It also provides an explicitly ignored
real-runtime acceptance using only a disposable galaxy and dummy worker:

```sh
COSMON_PI_EXEC=/owned/prefix/node_modules/.bin/pi \
COSMON_CLIENT_RECEIPTS=/owned/private/receipts \
./scripts/no-pilot-env.sh cargo test -p cosmon-cli --test terminal_contract \
  owned_pi_client_acceptance -- --ignored --nocapture
```

Install the official **1.1.0** package in an owned test prefix/cache using the
existing npm policy and `--ignore-scripts`; never override release-age policy.
The real test uses Pi's supported isolated agent directory, offline mode, no
model/authentication and only native slash commands. It records its exact
version, real TUI transcripts, public CLI argv and worker-preservation checks.
Private observations and receipts belong outside the repository. Unit tests,
real Pi/TUI acceptance, GUI visual review and remote/backend qualification are
separate witnesses. No GUI, remote, native worker backend or interactive input
qualification is implied.
