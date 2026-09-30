<!-- SPDX-License-Identifier: AGPL-3.0-only -->

# CLI and UI parity audit

## RPP identity discovery and quota (issue #45)

| Capability | CLI | Native UI | Remote service |
|---|---|---|---|
| Discover accessible noyaux | No matching command; local binding files remain readable | No equivalent audited | `GET /v1/noyaux` filters by token issuer, subject and audience |
| Inspect ingress quota | No matching command audited | No equivalent audited | `GET /v1/quota` reads the same issuer-subject bucket admission consumes |

The RPP route change has no CLI command behavior to mirror.

## Durable task briefing (issue #124)

| Capability | CLI | Native UI | Remote service |
|---|---|---|---|
| Recover the task from a molecule directory | `cs nucleate` writes the topic and all bound variables into `briefing.md`; `cs tackle` points the worker to that file | No equivalent authoring surface audited | Molecule creation writes the same durable task section before dispatch |

The pasted worker prompt names `briefing.md` as the task source of truth and
does not repeat its topic or variables when that file has a Task section.

## Worker update recovery (issue #122)

| Capability | CLI | Native UI | Other CLI |
|---|---|---|---|
| Record an observed update offer or restart request on the molecule | `cs patrol` and the pane-death hook add `worker-update-offered` or `worker-restart-requested` | No equivalent marker audited | No equivalent marker audited |
| Reclaim a dead worker without changing its molecule | `cs purge <worker>` removes the worker entry and leaves a Running molecule available for `cs tackle <molecule> --force` in its worktree | No equivalent reclamation action audited | No equivalent action audited |

The marker records pane text, not the cause of a later process exit. The
existing unharvested-work guard can require `--allow-unharvested` before
reclamation; neither purge path collapses the molecule.

## Whisper pane dialogue guard (issue #121)

| Capability | CLI | Native UI | Other CLI |
|---|---|---|---|
| Deliver a whisper through ordinary pane output, including status prose and an informational update banner | `cs whisper` allows the captured tail and records the delivery | No matching pane-dialogue guard audited | No matching pane-dialogue guard audited |
| Refuse a recognised money-stake or unknown confirmation/menu widget and show the matched rule and tail lines | `cs whisper` exits 5 before persistence or paste; `cs whisper --help` states the policy | No equivalent refusal detail audited | No equivalent refusal detail audited |

Unknown here is a recognised widget whose choice cannot be inferred safely.
Unrecognised ordinary output is deliverable. The other audited surfaces
remain parity gaps for this refusal detail.

## Trusted shell review (issue #74)

| Capability | CLI | Native UI | Reveal CLI |
|---|---|---|---|
| Review a merged shell-surface diff, grant trust over the files on disk, then run the post-merge gate | `cs done <id> --review-shell` prompts for `trust <id>` and rolls back on decline | No equivalent review gesture audited | No equivalent review gesture audited |

Ordinary `cs done` still refuses a merge that makes B5 trust stale and points
to `--review-shell`. The operator gesture is available on the local CLI; the
harvest wire options have no equivalent interactive flag.

The following table records the operator-facing surface touched by issue #117.
It is scoped to the patrol dead-worker policy; it is not a full command inventory.

| Capability | CLI | Native UI | Reveal CLI |
|---|---|---|---|
| Configure the grace before a patrol dead-worker verdict | `cs patrol --dead-worker-grace-secs <seconds>`; default 120 | No control found in the native apps | No equivalent action found |
| Detect a codex launch menu before briefing delivery | `cs tackle --adapter codex` inspects the pane and records a blocking dialogue | No launch-menu status found in the native apps | No equivalent action found |
| Scan live panes for blocking dialogues | Every `cs patrol` run scans by default; `--auto-confirm-safe` remains opt-in | No dialogue scan control found in the native apps | No equivalent action found |

The dead-worker policy and dialogue scan are applied by the patrol command.
These native controls and Reveal CLI actions remain parity gaps under ADR-068.

## Scoped plan observations (issue #111)

| Capability | CLI | Native UI | Other CLI views |
|---|---|---|---|
| Read an attempt-scoped status-line plan sample beside API-equivalent USD | `cs peek` TUI, snapshot and `--json` use the shared worker probe; each window retains account scope, freshness and reset state | No separate native reader audited | `cs ensemble` and `--json` use the same worker probe; numerical worker attribution remains unavailable |

The dispatch overlay records a sample only after an ordinary status-line
callback. An absent sample remains unavailable on every surface.

## Resident config reload (issue #91)

| Capability | CLI | Native UI | Other CLI views |
|---|---|---|---|
| Continue after an edit to an unused adapter | `cs run --resident` reloads the config and records `config-reloaded` in its trace and summary | No equivalent control audited | No equivalent action audited |
| Halt when a running molecule's adapter settings change or its adapter is unknown | `cs run --resident` records `config-drift-halt` and exits with code 75 | No equivalent control audited | No equivalent action audited |
| Inspect the adapter recorded on a running process | `cs ensemble --json` includes `dispatched_adapter` separately from the durable `adapter` pin | No equivalent field audited | No equivalent field audited |

## Concurrent tackle claim (issue #119)

| Capability | CLI | Native UI | Other CLI views |
|---|---|---|---|
| Serialize a manual tackle and resident dispatch before model selection | `cs tackle` claims the molecule until the spawn verdict; a losing invocation exits non-zero and names the winning worker, adapter and selected model | No equivalent admission control audited | No equivalent admission control audited |

The loser receives a recorded model pin when one exists. If the winning
adapter chose its own default, the exact model is unrecorded and the error
says so.

## Blocked dependents (issue #118)

| Capability | CLI | Native UI | Other CLI views |
|---|---|---|---|
| Hold a dependent until its blocker completes | `cs tackle` refuses dispatch; `cs run --resident` and the frontier retain the pending dependent | No equivalent admission control audited | No equivalent admission control audited |
| Name dependents held by a collapsed or frozen blocker | Human `cs status` and `cs status <blocker>` name them; `cs peek` names the blocker on the dependent row and gives the recovery gesture | No equivalent detail audited | No equivalent detail audited |

After a freeze, finish the blocker, then `cs complete` and `cs done` it. After a collapse, collapse the
pending dependent and re-nucleate it with a new `--blocked-by` edge. The native
and other CLI surfaces in this table remain unverified for this capability.
