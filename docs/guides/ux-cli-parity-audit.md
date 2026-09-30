<!-- SPDX-License-Identifier: AGPL-3.0-only -->

# CLI and UI parity audit

## Model evidence coverage (issue #73, W5)

| Capability | CLI | Native UI | Remote service |
|---|---|---|---|
| Qualify a pin that matches the last observed model when later coverage degraded | `cs peek` compact cell prefixes `!`; expanded peek and `cs observe` name `last observed; coverage degraded`. `cs --json observe` adds `realized`, `model_evidence` (receipt, reasons, counts and capture time) and `realized_disposition` | No rendered coverage warning audited | No corresponding remote JSON or web rendering audited |
| Keep absence states distinct | Peek and observe preserve unknown, silent, pending and unavailable realization states; CLI observe JSON preserves the same variants and `not_assessed` for legacy evidence | No matching visual audit | No equivalent evidence projection audited |

The warning describes the persisted assessment boundary. It does not measure
observer liveness or prove that the reported model executed. Web and native
renderers need a separate visual witness before parity can be claimed.

`cs harvest-authority` is an operator terminal surface for remote harvest trust administration, status, challenge construction and signed-grant import (issue #120 W6). No native UI counterpart is shipped; the RPP exposes the corresponding tenant reads and imports and a disjoint host-sealed administration route. A native UI must use the same public-root and grant validation rules before parity can be claimed.

## Remote harvest operator journeys (issue #120)

| Capability | Operator terminal | Native UI | Remote service |
|---|---|---|---|
| Select an explicit remote policy | `cosmon-remote harvest configure --policy scoped\|sealed\|disabled --admin-token-file <file>`; local `cs harvest-authority configure` | No equivalent control audited; use the terminal | Host-sealed compare-and-set admin route |
| Create or rotate a signer key | `cosmon-remote harvest init --admin-token-file <file> [--rotate-from <fingerprint>]` on the operator device | No equivalent control audited; use the terminal | Receives only the public root and epoch |
| Issue a molecule or mission grant | `cosmon-remote harvest grant --molecule <id>` or `--mission <id>`; `--export`, `--sign`, `--import` split the offline flow | No equivalent issuance flow audited; use the terminal | Challenge and verified grant-import routes; signing is local |
| Inspect policy and grant validity | `cosmon-remote harvest status [--molecule <id>]`; local `cs harvest-authority status` | No equivalent view audited; use the terminal | Tenant status route reports policy provenance and grant state |
| Close and optionally integrate a molecule | `cosmon-remote molecule done <id> --reason <text>`; local `cs done` | No equivalent control audited; use the terminal | Synchronous `done` route reports `merged` and any non-integration reason |

These terminal fallbacks are the available operator surface, not a claim of
native UI parity. The dedicated harvest scope comes from the tenant binding
only (OQ-1); a token claim alone is insufficient. On an explicit profile,
the sensitive override flags are refused pending exact ratification. No
private signing key enters the service or a worker through these commands.

## Harvest diagnostics and tracked state (issue #123)

| Capability | CLI | Native UI | Remote service |
|---|---|---|---|
| Explain a failed merge | `cs done` prints the merge error and dirty checkout paths; JSON includes `error` and `dirty_paths` | No equivalent diagnostic view audited | The harvest response retains the transaction error; no dirty-path projection audited |
| Finish a harvest in a project with tracked archive state | `cs done` commits eligible archive, event, and frontier changes after teardown and skips ignored molecule directories without an artifact warning | No equivalent action audited | The shared harvest transaction applies the same state commit |

The dirty-path list is a checkout observation at the failure report. It may
include state written while recording that failure.

## RPP identity discovery and quota (issue #45)

| Capability | CLI | Native UI | Remote service |
|---|---|---|---|
| Discover accessible noyaux | No matching command; local binding files remain readable | No equivalent audited | `GET /v1/noyaux` filters by token issuer, subject and audience |
| Inspect ingress quota | No matching command audited | No equivalent audited | `GET /v1/quota` reads the same issuer-subject bucket admission consumes |

The RPP route change has no CLI command behavior to mirror.

## Briefing confirmation and patrol transitions (issue #125)

| Capability | CLI | Native UI | Remote service |
|---|---|---|---|
| Read the latest briefing outcome and recovery gesture | `cs status <id>` and the `cs peek` briefing pane (`b`) show the typed delivery event and recovery gesture | No dedicated outcome view audited | A failed post-spawn confirmation returns `briefing_not_confirmed`; the typed outcome remains in the molecule event log |
| Nudge without a lifecycle transition | `cs patrol --nudge` leaves molecule status unchanged; `--auto-freeze` and `--auto-collapse` opt into transitions | No equivalent control audited | No equivalent control audited |

An absent delivery event remains absent. A recorded `session_gone`,
`unobservable`, `undelivered`, or `not_confirmed` outcome is never presented
as a delivered briefing.

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
