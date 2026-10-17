<!-- SPDX-License-Identifier: AGPL-3.0-only -->

# Client integrations: Cosmon in Herdr

An optional Herdr plugin and Python CLI for selecting molecules, viewing a
worker read-only, opening its existing Git worktree, and reading canonical
Markdown deliverables. Cosmon remains independent of Herdr. Removing the plugin
leaves worker processes, state, lifecycle and ordinary `cs` commands intact.

## Prerequisites

- Python 3.9+, Git, and a Cosmon build providing `cs terminal` v1 on `PATH`.
- Local tmux 3.2+ with read-only/ignore-size client flags (tested with 3.5a), on
  macOS or Linux for kernel process fingerprints. A single live window/pane
  and a creation-time terminal witness are required. Older workers without a
  witness display **unknown**; this tool never retrofits identity from labels.
- Herdr daemon CLI 0.9.5+ for the manifest/action/pane and argv PTY APIs. A
  running local Herdr session with an existing workspace for the chosen galaxy.
  The tested GPUI app is a separate executable from the daemon CLI.

There is no build step, dependency download, background service, startup hook,
web server or framework. Runtime state is not stored in the plugin source.

## Link, open, remove

From the Cosmon checkout containing this directory:

```sh
herdr plugin link "$PWD/tools/herdr-companion"
herdr plugin list --json
herdr plugin action list --plugin cosmon.client
```

Linking explicitly registers ordinary local code in Herdr's plugin registry.
It is an operator action; Cosmon never registers it automatically. The package
is not installed by a Cosmon upgrade. Keep a locally linked directory available
for later invocations; use a stable checkout, not a disposable worker worktree.

Inside a Herdr terminal workspace whose cwd belongs to the selected galaxy:

```sh
herdr plugin action invoke cosmon.client.open
```

The action opens a new **terminal tab** with a numbered selector. Choose a
molecule number, then `t` for its read-only terminal, `i` (**Interagir**) for
explicit human input/output, `w` for an existing
worktree shell, or `a` and an artifact number for a read-only text viewer.
`b` returns to the molecule list, `r` refreshes it and `q` exits. There is no
automatic refresh loop. No internal id needs to be copied during this flow.

The selector can also be opened explicitly:

```sh
herdr plugin pane open --plugin cosmon.client --entrypoint selector \
  --placement tab --workspace WORKSPACE_ID \
  --env COSMON_CLIENT_REPO=/path/to/galaxy
```

Plugin v1 exposes actions and terminal panes. The installed GPUI command
inventory does not expose a plugin action command; a native palette entry or
keybinding has not been qualified. Run the commands above in its terminal.
Worker views report to Herdr's native Agents list as **Cosmon**, with
semantic state **unknown**; the supported reader has no actual harness state
witness. Tab labels and report messages distinguish a view from a worker.
Molecule Running is never interpreted as Working. Daemon socket/PTY acceptance
is separate from a visual check of the GPUI rendering.

Close a read-only view using the normal tab close affordance or local
`q`, Escape or Ctrl-C. Interactive views detach with Ctrl-]; other keys,
including Ctrl-C, are forwarded to the exact worker. This detaches the view's client; it does not terminate,
freeze, complete or harvest the worker. The artifact viewer closes on Enter;
closing a worktree shell affects that shell only. Do not start a worker in
that shell and infer lifecycle integration.

To stop offering the plugin and remove its registration:

```sh
herdr plugin disable cosmon.client
herdr plugin unlink cosmon.client
```

For a GitHub-installed copy, `herdr plugin uninstall cosmon.client` is the
corresponding removal command. A local link needs only unlinking. Herdr does
not delete plugin user config/state; this plugin writes neither. No GitHub
installation/publication is performed or needed for the local path.

## Project and client context

The plugin uses the documented `HERDR_PLUGIN_CONTEXT_JSON.workspace_cwd` and
`focused_pane_cwd` fields, plus `HERDR_WORKSPACE_ID`. Git's worktree registry
resolves the canonical main checkout; it must contain `.cosmon/config.toml`.
Missing or contradictory project/workspace context is refused. The plugin
source cwd is never the default galaxy. An explicit `--repo` or
`COSMON_CLIENT_REPO` selects a validated root when context is unavailable.
`HERDR_BIN_PATH` selects the running Herdr CLI, and its status supplies the
actual socket address; no socket path is guessed. Commands use argv arrays.

All Cosmon calls pass explicit `--config ROOT/.cosmon/state` so an inherited
`COSMON_STATE_DIR` cannot redirect the selected project. `--cs` (or
`COSMON_CLIENT_CS`) can select a specific already-built Cosmon executable.
No command strips the runtime egress policy or changes shared Git config.

## Retained CLI

Global flags precede the subcommand. Commands return JSON; refusals exit 1.

```sh
python3 tools/herdr-companion/cosmon-herdr.py --repo /path/to/galaxy list --all
python3 tools/herdr-companion/cosmon-herdr.py --repo /path/to/galaxy show task-20990101-a001
python3 tools/herdr-companion/cosmon-herdr.py --repo /path/to/galaxy \
  open task-20990101-a001 --terminal --herdr-session NAME --workspace WORKSPACE_ID
python3 tools/herdr-companion/cosmon-herdr.py --repo /path/to/galaxy \
  open task-20990101-a001 --worktree --herdr-session NAME --workspace WORKSPACE_ID
python3 tools/herdr-companion/cosmon-herdr.py --repo /path/to/galaxy \
  open task-20990101-a001 --artifact synthesis.md --herdr-session NAME --workspace WORKSPACE_ID
```

`--artifact NAME --window WINDOW_ID` retains read-only GPUI code-tab opening;
set `--herdr-ui` to the actual GPUI executable. Without `--window`, artifacts
open in a new terminal text viewer with control bytes escaped. GPUI is not
required for that viewer. No canonical directory is served over HTTP.

`list` uses one-shot `cs observe`, never `peek` propulsion. Every action
re-observes the exact molecule id. Worktrees are joined by exact branch ref to
`git worktree list --porcelain -z`; no worktree is created or removed. This
locates the branch checkout, not the worker's current cwd. Conventional
`synthesis.md`, `frame.md`, `outcomes.md`, `plan.md`, `report.md`, `result.md`
and `responses/*.md` files beneath the public `molecule_dir` are offered;
symlinks are excluded. Binary/custom outputs need another reader. File
existence and molecule completion are not artifact acceptance.

## Public terminal contract

See [read contracts](../../docs/book/src/reference/read-contracts.md) for the
versioned JSON and attach argv. `show` includes that observation separately
from lifecycle/worktree/artifact status. A missing old CLI capability is
unknown; invalid versions, project/id mismatches and unsupported capabilities
are refused. Available observations carry an opaque expected-incarnation token.
The helper re-observes before opening and launches `cs terminal --attach
--expect` in a new argv PTY tab; it never attaches directly to cached addresses.

Cosmon then re-reads the exact process binding, checks platform PID start tokens
and executes a numeric server/session/pane guard inside tmux before attaching
with `-E -r -f ignore-size`. Session environment, worker size and ordinary
worker input are preserved. The read-only route exposes no input; lifecycle actions remain absent. This is
local same-user consistency, not an authorization boundary; the final
filesystem/kernel/tmux interval is not atomic, and user-controlled tmux hooks remain in force. See the contract for precise race
and platform limits. Native backends, remote SSH and other clients are outside
this package's acceptance.

## Verification

```sh
./scripts/no-pilot-env.sh python3 tools/herdr-companion/test_companion.py
./scripts/no-pilot-env.sh cargo test -p cosmon-cli --test terminal_contract
COSMON_CLIENT_RECEIPTS=/path/to/private/receipts \
  ./scripts/no-pilot-env.sh cargo test -p cosmon-cli --test terminal_contract \
  owned_herdr_client_acceptance -- --ignored --nocapture
```

The first uses recording providers and real Git worktrees for version,
capability, project/id, argv and context negatives. The second uses real tmux
launch witnesses, stale tokens, reused labels and changed kernel fingerprints.
The explicitly ignored third test uses real disposable tmux and Herdr socket/
PTY resources with no model credit; it registers only inside verified isolated
XDG config/state roots and removes its server/workspace afterward. It checks
plugin discovery, actual numbered selection, exact read-only client flags,
input/size/environment/state preservation, view close with worker alive, and
worktree/artifact navigation. It is not a rendered GPUI inspection or evidence
for cmux/Pi. Mutation receipts and release gates belong to the private run report.

Primary schema reference: [Herdr plugins v0.9.5](https://github.com/herdrdev/herdr/blob/v0.9.5/docs/next/website/src/content/docs/plugins.mdx).
Read-only client semantics: [tmux manual](https://man.openbsd.org/tmux.1#attach-session).

Read auditing: existing `cs observe` authorization instrumentation appends its
read decisions to `instrumentation/authz.jsonl`. View actions preserve molecule
lifecycle/process state and artifact bytes; this existing audit append is
retained and is not described as a zero-write filesystem operation.

The generic attachment requires Python 3 on macOS/Linux for its output-only
control-mode renderer. It launches a read-only/ignore-size tmux control client
without sending size announcements (`refresh-client -C`), including after an
ordinary controller resizes and detaches. Native ignore-size alone is not
sufficient when the viewer is the only client. The renderer samples guarded
visible-pane frames four times per second; it is not a lossless PTY byte stream
or scrollback viewer. Local q/Escape/Ctrl-C closes it and no keyboard bytes are
forwarded. Outer UI dimensions affect presentation only.

## Native Agents, explicit input and restore

Plugin 0.2 requires the corresponding Cosmon build for explicit interaction:

```sh
python3 tools/herdr-companion/cosmon-herdr.py --repo /path/to/galaxy \
  open task-20990101-a001 --interact --workspace WORKSPACE_ID
```

Opening a terminal runs a lightweight `view` wrapper, not an agent harness.
It sends bounded `pane report-agent --state unknown`, `report-agent-session`
and `release-agent` reports under its own source with label Cosmon. Reporting
failure does not interrupt the worker or view. This package cannot truthfully
report working/idle/blocked from its public observations, and deliberately
reports unknown. Reports do not supply authorization for input.

Interagir requests the separate `cosmon.terminal.interactive` capability and
revalidates project, molecule and incarnation. Literal bytes are hex encoded
before the guarded tmux queue; there is no shell interpolation. Dimensions
participate in tmux sizing and may resize the worker, including when clients
close. `-E` suppresses session environment updates. Both renderers sample the
visible pane four times per second; neither is a lossless PTY stream.

Herdr cold restore, when enabled, reconnects a **read-only** view to the
original token. It does not call agent start/resume and never launches another
harness. Interagir must be selected again explicitly. Missing/changed worker,
project or executable paths refuse restore. A linked plugin must remain at a
stable path; Herdr's plain-command, argument count/length and quoting limits
may prevent registration, which prints restore unavailable. Restore depends
on Herdr session persistence, the original local tmux server and launch
witnesses. Reported identity is a local consistency token, not a credential or
provider conversation identifier.

The real acceptance test also exercises native registration/release, explicit
input to a dummy process, environment and dimensions, cold restart with no
input authority, and stale refusal. Runtime socket/PTY evidence and rendered
GPUI inspection must be reported separately. See the primary Herdr contracts:
[adding support](https://herdr.dev/docs/add-herdr-support/) and
[Agents](https://herdr.dev/docs/agents/).

Herdr 0.9.5 exposes `agent_session` references only for its known official
agent sources. Cosmon keeps its own name/source: the worker consistency token
is carried in reconnect argv, not presented as a provider-native session.
Native registration and actual cold restore are separate acceptance checks.
