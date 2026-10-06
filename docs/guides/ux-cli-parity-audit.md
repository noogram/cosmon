<!-- SPDX-License-Identifier: AGPL-3.0-only -->

# CLI and UI parity audit

## Cross-galaxy event origin (issue #183)

| Capability | CLI | Native UI | Other CLI views |
|---|---|---|---|
| Attribute each JSON event in a cross-galaxy tail to its source | `cs tail --all-galaxies --json` adds `source_galaxy` without changing the authoritative ledger row | No cross-galaxy event-tail view audited | No matching multiplexed event stream audited |

The `source_galaxy` key is reader metadata for selecting the source ledger again; it
does not change the append-only event log.

## Spore admission (ADR-183)

| Capability | CLI | Native UI | Remote service |
|---|---|---|---|
| Inspect or run a work-type-constrained spore | `cs spore validate` and `cs spore run` take `--admission <FILE>` when the manifest declares admission. The preflight emits the work type, vehicle, pinned refs, paths, risk, gates, review resources and initial count; a refusal precedes allocation. | No spore admission control audited | No spore admission endpoint audited |

The terminal remains the admission surface for this change. A UI or remote
entry point must use the same versioned contract before it can allocate a DAG.

## Supervisor code-signing diagnosis

| Capability | CLI | Native UI | Remote service |
|---|---|---|---|
| Detect supervisor signatures that lose macOS TCC consent after rebuilds | `cs doctor supervision` reads the installed binary's identifier and designated requirement; its warning is also included in `cs doctor security` | No native diagnostic audited | No remote diagnostic audited |

The install script pins `com.cosmon.daemon-supervisor`. A persistent local
certificate is also required because ad-hoc signatures remain tied to the
binary's content hash. The doctor probe is read-only and does not grant Full
Disk Access or restart the LaunchAgent.

## Remote client upgrades (issue #149)

| Capability | Operator terminal | Native UI | Remote service |
|---|---|---|---|
| Reinstall without losing a login or profile edits | `install.sh` uses `cosmon-remote config init --report-created` and applies server defaults only to a newly created profile. `cosmon-remote --version` shows the source commit, and a missing-login error names the selected profile and expected credential location when derivable. | No equivalent installer or credential view audited | Serves the installer and binary; credential files remain on the client machine |

The credential key serialization is unchanged across the available source
history. The upgrade loss came from replacing the profile that carries the
issuer and client ID needed to address that key. No credential migration is
needed for this failure. An already reset profile lacks those inputs and must
be reconnected with `cosmon-remote login`; its credential filename cannot be
reconstructed from the one-way digest alone.

## Planned child model routing (issue #139)

| Capability | CLI | Native UI | Remote service |
|---|---|---|---|
| Preserve a planner's child model recommendation | `cs nucleate --var cosmon_model=<id>` persists the child's model choice. `cs tackle` and `cs run` resolve it above the formula-step pin; an explicit `cs tackle --model` wins. `cs run --affinity` groups by the same child choice. | No child model editor audited | Nucleation variables carry `cosmon_model`; in-process dispatch uses the same resolver |

The planner formula is a trusted shell surface. Its child adapter recommendation uses the existing `cs nucleate --adapter` pin; its model recommendation uses `cosmon_model` only when present.

## Frozen planner hand-off (issue #138)

| Capability | CLI | Native UI | Remote service |
|---|---|---|---|
| Show mission lineage without a completion prerequisite | `cs nucleate --decayed-from <mission>` records lineage; `cs deps --transitive` walks it; `cs observe` shows typed links and `cs peek` tree shows source and products. `--blocked-by` remains the pipeline order edge. | No equivalent lineage pane audited | `cs --json observe` exposes the typed link; no separate remote display audited |

The shipped planner formulas are a trusted shell surface and require operator review at harvest. A frozen planner exposes its first lineage child; later children still wait for their completed and integrated pipeline predecessors.

## Merge subject configuration (issue #140)

| Capability | CLI | Native UI | Remote service |
|---|---|---|---|
| Choose a per-galaxy merge subject | `cs done` reads `[project] merge_subject`; config load requires `{mol_id}`; `{title}` uses title or topic; unset preserves the existing merge subject | No matching configuration control audited | The shared harvest transaction uses the same galaxy setting; no separate remote control audited |

For example, `[project] merge_subject = "chore(merge): {mol_id} {title}"` in
`.cosmon/config.toml` selects a one-line subject. `{title}` is empty when the
molecule has neither a `title` nor a `topic` variable. The setting changes
only commits created by the merge strategy, including an automatically
resolved conflict. `ff-only` makes no merge commit.

`scripts/check-provenance.sh` recognizes this repository's default
`Merge branch 'feat/<mol_id>'` subject. A galaxy using another subject must
align its own provenance gate and subject hook with its template. This
repository leaves `merge_subject` unset, so its existing gate remains valid.

## Default model selection (issue #141)

| Capability | CLI | Native UI | Remote service |
|---|---|---|---|
| Select the default worker model | `cs tackle` probes `claude-sonnet-5-5` when no model is pinned; `claude-sonnet-5` remains an explicit pin and a historical realized-model id | No matching default selector audited | The adapter's absent-model default uses the same chain head |

## Context occupancy (issue #127)

| Capability | CLI | Native UI | Remote service |
|---|---|---|---|
| Show latest-turn context occupancy | `cs peek` TUI and `--snapshot` divide the last reported turn's input by the model window; unknown inputs show no percentage. `cs peek --json` has no context percentage and keeps its existing cumulative usage schema. IN/CACHED/OUT/RSN remain cumulative. | No matching gauge audited | No matching gauge audited |

The snapshot and TUI renderers share the worker energy observation. A model
window without a last-turn count is insufficient to infer occupancy.

## Model evidence coverage (issue #73, W5–W6)

| Capability | CLI | Native UI | Remote service |
|---|---|---|---|
| Qualify a pin that matches the last observed model when later coverage degraded | `cs peek` compact cell prefixes `!`; expanded peek and `cs observe` name `last observed; coverage degraded`. `cs --json observe` adds `realized`, `model_evidence` (receipt, reasons, counts and capture time) and `realized_disposition` | No rendered coverage warning audited | No corresponding remote JSON or web rendering audited |
| Keep absence states distinct | Peek and observe preserve unknown, silent, pending and unavailable realization states; CLI observe JSON preserves the same variants and `not_assessed` for legacy evidence | No matching visual audit | No equivalent evidence projection audited |

The warning describes the persisted assessment boundary. It does not measure
observer liveness or prove that the reported model executed. Web and native
renderers need a separate visual witness before parity can be claimed.

The CLI witness uses `cs observe` text and JSON over a persisted receipt; the
compact cell is checked from the same journal fold. A detached watcher test
also verifies that two capture processes retain the warning when a later
response reports the same model, across restart and replay. The terminal
buffer witness for narrow peek cells lives with the W5 surface tests. These
checks do not establish native or web visual parity. General grammar drift
inside a record treated as ordinary can still escape detection.

`cs harvest-authority` is an operator terminal surface for remote harvest trust administration, status, challenge construction and signed-grant import (issue #120 W6). No native UI counterpart is shipped; the RPP exposes the corresponding tenant reads and imports and a disjoint host-sealed administration route. A native UI must use the same public-root and grant validation rules before parity can be claimed.

`cs collaboration` is a host-local terminal surface that provisions, lists, shows and revokes collaboration bindings (issue #147 W2, `docs/specs/cross-machine-collaboration.md` §2). A binding ties one exact token identity to one work seat or pilot mission through an attachment with its own proof; the proof is written once to a new 0600 file and only its verifier stays on the host. No native UI counterpart is shipped and no RPP route exposes binding management: a remote request must never create, widen or revoke its own binding, so the gap is deliberate, not pending. The client keeps the proof through `cosmon-remote`'s credential store; no `cosmon-remote` command uses it until the work routes ship (W4).

## Remote harvest operator journeys (issue #120)

| Capability | Operator terminal | Native UI | Remote service |
|---|---|---|---|
| Select an explicit remote policy | `cosmon-remote harvest configure --policy scoped\|sealed\|disabled --admin-token-file <file>`; local `cs harvest-authority configure` | No equivalent control audited; use the terminal | Host-sealed compare-and-set admin route |
| Create or rotate a signer key | `cosmon-remote harvest init --admin-token-file <file> [--rotate-from <fingerprint>]` on the operator device; rotation signs with the current key, selected by `--current-key-file` when needed | No equivalent control audited; use the terminal | Receives the new public root and a current-key signature bound to tenant and epoch |
| Recover a lost signing key | Host `cs harvest-authority configure --policy sealed --public-key-file <file> --epoch <next> --local-reset` | No equivalent control audited; use the terminal on the host | No HTTP recovery route; local intent is recorded durably |
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

`GET /v1/ledger` (issue #184) is an adapter-only remote read of the tenant's event log, resumable from an opaque cursor. It adds no `cs` verb and no `cosmon-remote` subcommand: the CLI reads `events.jsonl` locally, and the thin client gains a `Client::ledger_stream` library method only. No native UI counterpart is shipped.

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
| Preserve work after worker purge | `cs purge <worker>` retains its Running molecule; `cs patrol --auto-collapse` reports the missing worker and holds the molecule for `cs tackle <molecule> --force` | No equivalent control audited | No equivalent control audited |
| Continue an interrupted in-process attempt | `cs tackle <molecule> --force --resume` restores the last complete checkpoint of an `openai`/`anthropic` attempt, keeps spent turns, tool calls and wall-clock time, and refuses with the reason when the evidence does not prove it safe | No equivalent control audited | No equivalent control audited |

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

## Whisper pane dialogue guard (issues #121 and #130)

| Capability | CLI | Native UI | Other CLI |
|---|---|---|---|
| Deliver a whisper through ordinary pane output, including status prose, an informational update banner, and earlier questions above the latest idle input field | `cs whisper` allows the active tail and records the delivery | No matching pane-dialogue guard audited | No matching pane-dialogue guard audited |
| Identify a live steerable worker by its built-in foreground-program signatures | `cs whisper` accepts `claude`, `claude*`, `node`, `<version>`, `codex`, and `codex*`; `cs whisper --help` lists the same set | No equivalent signature list audited | No equivalent signature list audited |
| Refuse a recognised money-stake or unknown confirmation/menu widget, including mid-line yes/no choices and risky prompts without a permission marker, and show the matched rule and tail lines | `cs whisper` exits 5 before persistence or paste; `cs whisper --help` states the policy | No equivalent refusal detail audited | No equivalent refusal detail audited |

Unknown here is a recognised widget or risky prompt whose choice cannot be inferred safely.
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
| Resolve a codex update menu and restart after installation | `[adapters.codex].update = "auto"` (default) accepts the update and re-tackles in place after the success notice; `skip` declines it; `operator` pages without a key | No update-policy control audited in the native apps | No equivalent action found |
| Dispatch an external CLI worker through a per-galaxy compatible gateway | `[adapters.codex]` and `[adapters.opencode]` carry `base_url`, `api_key_env`, and `default_model`; `cs tackle` injects native per-process provider configuration without writing either harness's global config | No gateway editor audited in the native apps | Status surfaces show the selected adapter/model; credential values remain absent |
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

## Durable adapter dispatch (issue #156)

| Capability | CLI | Native UI | Other CLI views |
|---|---|---|---|
| Dispatch a molecule through its durable adapter pin | Bare `cs tackle <id>` honours the adapter stamped by `cs nucleate --adapter`; an explicit `cs tackle --adapter` overrides it | No equivalent dispatch control audited | `cs run --resident` already prefers the durable pin over its run-wide directive |

## In-process failure after work (issue #150)

| Capability | CLI | Native UI | Other CLI views |
|---|---|---|---|
| Preserve work after an in-process loop error | `cs tackle` collapses with `agent_loop_failed`, retains the branch and worktree, and writes a partial synthesis; a pre-work failure rolls back | No equivalent recovery action audited | `cs status` and `cs peek` display the collapsed molecule; the branch needs an audit before removal |

## Formula-bound worker acceptance (issue #151, W2)

| Capability | CLI | Native UI | Other CLI views |
|---|---|---|---|
| Publish declared final text and advance only its current formula step | `cs tackle` and the detached local worker honor `steps.response_artifact`; publication precedes the transition and later steps remain pending | No formula-authoring or acceptance control audited | Status views read the ordinary current-step and completed-step state; no separate acceptance action exists |
| Refuse empty, stale, missing, unsafe, or ambiguous output | The worker run returns an error and the molecule stays recoverable on its current step | No equivalent refusal detail audited | Existing status views show the unchanged step |

The field is part of the formula contract, not a new lifecycle command. Tool
counts and provider-normal termination remain visible evidence but never grant
completion authority by themselves.

## Work-turn input on the messages direct arm (issue #151, W6)

| Capability | CLI | Native UI | Other CLI views |
|---|---|---|---|
| Deliver pending work evidence to an `anthropic` worker | `cs tackle --adapter anthropic` includes the member's pending `cs work` envelopes in its next request, as the `openai` arm does, and records a delivery receipt only after the request is answered | No equivalent delivery control audited | `cs work list` shows the delivery attempt per envelope |

No command or flag changes. The receipt shows that the bytes were in a
successfully answered request, not that the model used them.

## Seat-bound delivery and `cs collapse --with-seats` (issue #162)

| Capability | CLI | Native UI | Other CLI views |
|---|---|---|---|
| Deliver a work envelope only to the molecule its scope revision bound to the recipient seat | `cs work inbox`, the work hook and the turn-input adapters stop offering an envelope whose revision bound the seat to another molecule; no flag changes | No equivalent delivery control audited | `cs work list` keeps the envelope, with `recipient_bound: false` and a `recipient_not_bound` finding |
| Settle a declared work in one command | `cs collapse <owner\|member> --with-seats [--reason R] [--reason-kind K]` collapses every live seat through the ordinary collapse path, skips a completed seat and names `cs done <seat>`, merges nothing; exit is non-zero if any seat failed | No equivalent control audited | `cs work list <owner>` and `cs observe <seat>` show the result; no RPP route (the flag is not on the wire) |
| Find infrastructure-blocked collapses | `--reason-kind verification_blocked` is a documented convention; `cs errors --kind verification_blocked` lists them | No equivalent view audited | The label is the worker's claim, not merge-safety evidence |

## Blocked dependents (issue #118)

| Capability | CLI | Native UI | Other CLI views |
|---|---|---|---|
| Hold a dependent until its blocker completes | `cs tackle` refuses dispatch; `cs run --resident` and the frontier retain the pending dependent | No equivalent admission control audited | No equivalent admission control audited |
| Name dependents held by a collapsed or frozen blocker | Human `cs status` and `cs status <blocker>` name them; `cs peek` names the blocker on the dependent row and gives the recovery gesture | No equivalent detail audited | No equivalent detail audited |
| Diagnose a blocker absent from the resident project snapshot | `cs status` names each pending dependent and blocker in text and `missing_blockers` JSON, distinguishes a missing record from an out-of-project record, and names the re-nucleation gesture; `cs ensemble --json` includes referenced legacy blockers without `project_id` | No equivalent detail audited | No equivalent detail audited |

After a freeze, finish the blocker, then `cs complete` and `cs done` it. After a collapse, collapse the
pending dependent and re-nucleate it with a new `--blocked-by` edge. The native
and other CLI surfaces in this table remain unverified for this capability.
