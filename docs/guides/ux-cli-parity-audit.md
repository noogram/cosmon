<!-- SPDX-License-Identifier: AGPL-3.0-only -->

# CLI and UI parity audit

## Cross-machine work exchange (issue #147 W4)

| Capability | Local CLI | Remote CLI / API |
|---|---|---|
| Inspect roster and revision | `cs work list` | `cosmon-remote work list`; `GET /v1/work/{owner}` |
| Send bounded evidence | `cs work send` | `cosmon-remote work send`; `POST /v1/work/{owner}/messages` |
| Peek without delivery observations | `cs work inbox --peek` | `cosmon-remote work inbox --peek`; `GET /v1/work/{owner}/inbox` |
| Pull at an explicit safe point | `cs work inbox` | `cosmon-remote work inbox`; `POST /v1/work/{owner}/inbox` |
| Report considered, deferred or rejected | `cs work ack` | `cosmon-remote work ack`; `POST /v1/work/{owner}/messages/{key}/ack` |

The host derives the seat from the operator-provisioned binding and checks
its attachment proof; the request never selects a sender. Unknown request
fields are refused. Every response names its schema version, server receipt
time and scope revision. Inbox messages and outgoing acknowledgment receipts
remain separate; a result is a new message linked by reply, never an ack.
Writes recheck binding revision and revocation at commit. Declaration and
binding management stay host-local. No collaboration route starts a command,
molecule or worker, changes a lifecycle, or acquires a lease. W4 provides no
session routes, background outbox or wake-up channel.

The client profile pins the existing deployment URL, issuer and client ID,
plus the host-issued public attachment ID (`config set attachment ID`).
`cosmon-remote work store-proof` reads the host-issued secret proof from stdin
and stores it in the private credential backend for that profile; the proof
never belongs in argv. A profile's issuer and client ID identify that private
credential slot even when using a static bearer. Every work request sends
`cosmon-attachment` and `cosmon-attachment-proof` alongside the bearer. The
scope revision comes from `work list` and is explicit on send, inbox and ack.
A retry keeps the same message key and request bytes; there is no local or
background fallback when the server lacks the route.

The transport canon
classifies these typed adapter routes as `adapter-only`: the local work
commands are outside the thin CLI's `#[verb]` registry. See the
[frozen contract](../specs/cross-machine-collaboration.md).

## Worker host selection

| Capability | CLI / configuration | Native UI / remote API |
|---|---|---|
| Choose worker hosting | `[transport] backend = "tmux"`, project > existing global configuration > tmux; unknown/Herdr/Zellij refused before resource creation | No new selector, worker backend, or control route |
| Read launch provenance | `cs --json observe ID` adds optional `worker_transport`; new Codex Exec pins persist host/source and creation-time terminal identity | Shared observe JSON includes the optional field; terminal clients keep using `cs terminal` |
| Control existing Exec worker | Terminal views and forced purge use the durable incarnation; harvest validates launch-bound triggers and preserves OS receipt semantics | Existing explicit terminal attachment only; no new lifecycle authority |
| Reclaim a stale Exec pin | `cs purge WORKER --force` removes the fleet entry when the recorded server is provably gone or its terminal incarnation is confirmed changed; reports `tmux_killed=false` and `terminal_cleanup`, never kills a same-name replacement; transport errors retain the entry | No new native UI control; command JSON carries the cleanup reason |
| Stop a supervised worker (ADR-188) | `cs purge WORKER --force` and `cs done` stop a launch with `worker_exit_receipts` through its supervising parent, addressed by launch identity only; the terminal is torn down only after a terminal receipt; any refusal (`unreachable`, `identity_mismatch`, `already_exited`, `unsupported`) warns, performs no fallback termination and leaves the launch unresolved; purge JSON adds `supervised_stop`, done reports `stopped_supervised_worker` or a warning | No new native UI control or remote stop route |
| Resume a new Exec pin | Interactive resume/whisper, freeze/thaw, resurrection and forced re-tackle refuse before effects because no Exec continuation contract is qualified | No equivalent continuation support added |

The qualified dispatch path is Codex Exec on tmux, with macOS runtime
evidence. Legacy Interactive and old processes retain their historical paths.
Client integrations named below do not constitute alternative worker hosts.
See [configuration](../project-config.md#transportbackend--worker-hosting).

## Client integrations

| Capability | CLI | Herdr companion | Native UI / remote API |
|---|---|---|---|
| Observe exact worker terminal incarnation | `cs --json terminal EXACT_ID`, versioned available/unknown/unavailable contract | Consumes public capability; never reads private state JSON or infers a target from labels | No new route or control |
| Attach a read-only, size-neutral worker view | `cs terminal EXACT_ID --attach --expect TOKEN`, re-read and tmux in-server identity guard, output-only control reader with `-E -r -f ignore-size` | New argv PTY tab invoking the same public command; close detaches the view | No new route or control |
| Explicit exact-worker human input/output | `cs terminal EXACT_ID --mode interactive --attach --expect TOKEN`; separate port/kind; guarded hex input and participating dimensions, `-E` | Optional Herdr Interagir action; native Cosmon view reports unknown, restores token-pinned read-only | No new route or control |
| Select molecule, worktree and deliverable | Existing `observe` plus exact Git branch join and canonical molecule_dir | Optional manifest action/pane and numbered selector; worktree shell tab and read-only artifact pager, retained CLI/GPUI code-open mode | No change |

The kernel has no Herdr or other client dependency. Optional launch evidence on
`process.terminal` is captured at spawn; missing legacy witnesses remain unknown.
Molecule status and deliverable acceptance do not establish terminal presence.
Input is available only through the explicit interactive action. No lifecycle
authority, worker backend or default transport change is added. Read [the contract](../book/src/reference/read-contracts.md) and the
[plugin instructions](../../tools/herdr-companion/README.md) for compatibility,
installation, local consistency limits and actual verification coverage. Herdr
socket/PTY acceptance is distinct from rendered GPUI acceptance and from other
clients. The independent [cmux tooling](../../tools/cmux-companion/README.md)
now consumes the same v1 contract: active-first numbered selection and fresh
workspaces for worker output, an existing worktree shell and canonical artifact
text. cmux socket v2 `workspace.create` uses a POSIX-quoted initial command;
it has no argv PTY API. No existing surface receives worker input. Installation
is an explicit local directory invocation with no registry or global hooks.
The independent [Pi package](../../tools/pi-companion/README.md) uses native
slash commands and TUI components for explicit project/molecule selection,
details, readonly output and conventional artifacts. Worktree navigation displays
a verified existing path without launching a shell or changing Pi's cwd. An owned
Python PTY bridge invokes the public opaque-token attachment because Pi 1.1.0 has
no native PTY/tab API. Input and resize stay local; close/shutdown detach owned
readers. Real Pi 1.1.0 TUI/dummy-worker acceptance passed without model calls;
GUI visual review, remote and worker-backend support are not qualified. No core
command, JSON contract or remote classification changes for this consumer.

## Explicit empty handoff scopes (issue #209)

`cosmon-rpp-adapter trust converge` and server boot use the same bootstrap:
`scopes = []` in `[binding]` or `[[bindings]]` creates `[scopes] allowed = []`
and refuses token-carried scopes. Omission retains the default grants and
warning. Existing bindings remain operator-owned and are preserved on drift.
The operator `nucleon render`, admin provisioning and federation paths retain
legacy empty-list rendering without `[scopes]`. No native UI control, remote
route or CLI flag changes; generated help and OpenAPI are unchanged.

## Collapsed persistence and terminal command refusals (issue #207, D8)

| Capability | CLI | Native UI | Shared storage |
|---|---|---|---|
| Refuse stuck/evolve on terminal work before energy or event mutation | `cs stuck` uses the shared stuck operation; `cs evolve` refuses Completed/Collapsed even at zero energy | No new control | Regular and durable FileStore saves refuse leaving Collapsed under one serialized check/rename; metadata updates retaining Collapsed and Completed → Frozen remain legal |
| Refuse stale dispatch/gate/acceptance results | Runtime dispatch claim and rollback, gate completion and worker acceptance compare live lifecycle and revision before saving | No new control | Conditional writes use the same innermost writer lock; dispatch and fleet locks are not reacquired |

The writer lock is `CosmonPath::MoleculeWriteLock` at
`molecule-write-locks/<mol>.lock` under the state root, owned by FileStore and
classified as an advisory lock in `cs paths --writes`. It is keyed by molecule
id across legacy-to-fleet migration. Lock order is dispatch (if held), then
fleet (if held), then the writer lock; a save never reacquires either outer
lock. A save retaining Collapsed must retain its embedded receipt as well. The JSON
state schema and collapse receipt format are unchanged.

## Conditional, idempotent collapse — local surfaces (issue #207, ADR-187)

| Capability | CLI | Native UI | Remote service |
|---|---|---|---|
| Read the revision a conditional collapse compares | `cs --json observe <ID>` carries `revision` (`b3:<hex>`, BLAKE3 of the canonical state JSON, computed on read, never stored) | No revision display audited | Not yet: `GET /v1/molecules/{id}` gains the field and a strong ETag in the next increment |
| Collapse only the state the caller confirmed, once, under a caller key | `cs collapse <ID> --reason R --key K --expect-status S --expect-revision REV [--approval-digest D] [--not-after T]`; `--json` prints `outcome`, `digest`, `receipt`; refusals exit 16, 20–25 by outcome | No equivalent control audited | Not yet: `PUT /v1/molecules/{id}/collapse-receipts/{key}` and `cosmon-remote` flags are the next increment; the existing POST is unchanged and unconditional |
| Read back a lost response | `cs collapse-receipt <ID> <KEY>` answers `committed` (with the receipt), `reserved_not_applied`, `inconsistent` (exit 26) or `unknown`; a key bound to another molecule answers `idempotency_key_reused` (exit 20) without disclosing it | No equivalent view audited | Not yet: `GET /v1/molecules/{id}/collapse-receipts/{key}` in the next increment |
| One collapse operation and one lease guard | `cs collapse` and MCP `cosmon_collapse` (stdio and the RPP `/mcp` connector) call `cosmon_state::ops::collapse::execute`; the lease is read from the state dir being mutated | — | MCP over `/mcp` is a network caller: refused on a leased mission in v1. The V1 POST route still runs without the guard until its keyed replacement lands |

`cosmon_collapse` changed behaviour with the shared operation: collapsing an
already-collapsed molecule is now an idempotent no-op (`already_collapsed:
true`) as with `cs collapse`, instead of an error, and the collapse now writes
`log.md` and the `EventV2` records, releases the process record, and is
refused on a leased mission.

## Codex live lifecycle observations (issue #193)

| Capability | CLI | Native UI | Remote service |
|---|---|---|---|
| Forward an observed Codex app-server lifecycle event without transferring connection ownership | `cs sessions lifecycle --session SID --sequence N --event <turn-start\|approval-request\|turn-complete\|disconnect>` folds a client-owned monotone sequence and updates only lifecycle fields plus heartbeat | Pupitre may own the connection and invoke the same ingress; no native lifecycle rendering is audited | No resident broker or remote lifecycle route is added |

Presence and ledger consumers distinguish `live_lifecycle`, `turn_end_only`,
and `unavailable`. Disconnect is unavailable with no state, not idle. The
client sequence continues across reconnects; duplicate and stale deliveries
are no-ops. The legacy Codex `notify` callback remains a compatible
turn-end-only source. Ingress refuses a presence row owned by another provider;
an absent provider on an existing row is initialized to `codex`. Before its
first forwarded event, the connected client must establish a row with
`sessions attach` or a provider hook; lifecycle ingress refuses to invent
provider-session pid or cwd provenance.

## Checkpoint draft publication and holder selection (issue #196)

| Capability | CLI | Native UI | Remote service |
|---|---|---|---|
| Publish a versioned hand-over draft and retrieve the current lease holder's hand-over | `cs sessions checkpoint publish --from-file <PATH\|->` accepts only `cosmon.checkpoint-draft/v1`, stamps provenance from the verified lease, and refuses unknown or authority fields. `cs sessions checkpoint show --mission M --of-holder --json` returns `{checkpoint, selected_by}` and admits only the newest record with the verified holder's exact session and epoch. | No checkpoint draft editor or holder-scoped reader audited | No remote checkpoint route audited |

The holder selector does not infer authority from publication order: an absent
lease and an absent exact match both retain the wrapper and return a null
`checkpoint`. Inline checkpoint publication remains available for compatibility;
new stored records carry `cosmon.checkpoint/v1`, while legacy records missing
that field read as v1 without rewrite.

## Interactive provider launch identity (issue #195)

| Capability | CLI | Native UI | Remote service |
|---|---|---|---|
| Launch a pilot whose hooks and tool subprocesses share one identity | `cs sessions launch <claude\|codex> -- <ARGS>` mints a fresh anonymous `COSMON_SESSION_ID` and equal `COSMON_SESSION_LAUNCH_ID` marker before starting the provider executable | No provider launch control audited | No provider launch endpoint added |

Codex tool subprocesses use native `CODEX_THREAD_ID` / `CODEX_SESSION_ID`
identity before a launch marker. Both exports must agree when present, and a
Codex callback's `thread-id` must agree with them; disagreement refuses the
identity and publishes no row. Presence, takeover checks and lifecycle guards
share this resolution. Workers retain their dispatch identity.

Every launch replaces inherited pilot identity and clears inherited Codex native
ids, so the provider supplies its own identity. The fresh anonymous marked id is
the fallback when native environment identity is unavailable, including the
supported Claude tool boundary; a Claude native tool binding has not been
verified. Provider arguments pass without shell interpolation and the provider
retains its exit status. Hooks must already be installed separately. Unmarked
callbacks without native environment identity use Codex `thread-id` or Claude
`session_id`, ignoring stale inherited pilot ids; this callback fallback alone
provides no tool identity parity. A user hook outside a Cosmon galaxy succeeds
without writing state.

## Remote bearer hand-off (issue #188)

| Capability | CLI | Native UI | Remote service |
|---|---|---|---|
| Supply a current bearer to a local API consumer | `cosmon-remote token --confirm` prints the credential saved by `login` after resolving it through the same single-writer refresh path used by authenticated commands. It refuses without explicit confirmation and does not accept process-supplied bearer overrides. | No matching credential-export control audited | No new route; the bearer remains a client-side credential |

The command writes only the bearer to stdout so a local consumer can read it
directly. The confirmation acknowledges that stdout may be retained by a shell,
redirect, or process supervisor. Two simultaneous calls converge through the
credential store's refresh protocol; only one refresh grant is permitted.

## Linux user-service distribution (issue #143, W6)

| Capability | CLI | Native UI | Remote service |
|---|---|---|---|
| Install, inspect, reload, or uninstall the supervisor and scheduler as user services | No new `cs` verb or flag. The public installer's opt-in `--with-services` route installs service helpers with `status`, `reload`, and `uninstall` operations | No service-management surface audited | No service-management route; host service ownership stays outside the tenant API |

This delivery changes installer scripts, unit templates, and documentation, not
the `cs` command surface. Generated help, manual, reference, and route snapshots
are therefore not applicable. The existing runtime and scheduler `cs` commands
retain their prior parity status.

## Cross-galaxy event origin (issue #183)

| Capability | CLI | Native UI | Other CLI views |
|---|---|---|---|
| Attribute each JSON event in a cross-galaxy tail to its source | `cs tail --all-galaxies --json` adds `source_galaxy` without changing the authoritative ledger row | No cross-galaxy event-tail view audited | No matching multiplexed event stream audited |

The `source_galaxy` key is reader metadata for selecting the source ledger again; it
does not change the append-only event log.

## Ledger-derived pilot seats (issue #197)

| Capability | CLI | Native UI | Remote service |
|---|---|---|---|
| Inspect pilot authority across galaxies | `cs sessions seats --json [--all-galaxies]` emits one versioned document; verified lease authority and presence claims remain distinct | No native seat dashboard audited | No remote route; the local lease and presence files are host-bound |

The CLI never derives authority from heartbeat recency. Multiple primary
claims remain visible, including stale claims after a transfer.

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
| Select the default worker model | `cs tackle` probes `claude-sonnet-5-5` when no model is pinned; `claude-sonnet-5` remains an explicit pin and a historical realized-model id. `COSMON_MODEL_PROBE_TIMEOUT_SECS` sets the positive, bounded Claude availability-probe budget (default 60 seconds). | No matching default selector audited | The adapter's absent-model default uses the same chain head |

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


## Conditional remote collapse (issue #207 B2)

| Gesture | CLI | HTTP / remote client | Native UI |
|---|---|---|---|
| Confirm status and revision | `cs observe --json` | `GET /v1/molecules/{id}` includes raw-state revision and strong ETag | No confirmation control audited |
| Collapse the confirmed revision | `cs collapse --key --expect-status --expect-revision` | Keyed PUT and matching `cosmon-remote molecule collapse` flags; nine ADR-187 outcomes, no POST fallback | No keyed control audited |
| Recover a lost response | `cs collapse-receipt <id> <key>` | Tenant-scoped GET and `cosmon-remote molecule collapse-receipt` | No receipt view audited |

Both HTTP collapse routes refuse leased missions through the shared authority.
The stale-writer limitation in ADR-187 remains until the separate D8/B3 change lands.
## Supervision portable des workers

| Capability | CLI | Native UI | Remote service |
|---|---|---|---|
| Opt-in durable direct-child exit evidence | Galaxy configuration `[adapters.codex] worker_exit_receipts = true`, with explicit `mode = "exec"` and `update = "skip"` or `"operator"`, applies only to POSIX Codex tmux Exec launches through `cs tackle`. Interactive opt-in is refused before dispatch effects; default legacy Interactive is unchanged. Other adapters refuse the option. | No toggle or backend qualification in this increment. Read existing canonical observations; no new lifecycle authority. | No new API option or worker backend. |
| Internal waiting parent | Hidden `cs worker-supervise` consumes the launch prepared by `cs tackle`; it is not a user dispatch entry point. | No equivalent command. | Not exposed. |
| Conservative receipt consumption | Hidden `cs harvest --worker-launch` pins the hook trigger to one launch. Validated child wait/exec evidence is distinct from legacy pane status; unknown blocks automatic harvest and redispatch. | No new harvest authorization. | Existing authorization remains required. |

See [worker exit receipts](worker-exit-receipts.md) for launch residence,
signal/TTY policy, explicit unknown states, qualification and limitations.

Pinned Exec safety also survives a missing process snapshot: positive dispatch
history refuses legacy freeze/thaw/resume, input and teardown; fleet probes
report unknown without adopting a same-name terminal. Genuine legacy fleet-only
records retain their established behavior.

A legacy dispatch that reuses a worker name for a different molecule does not
erase the old molecule's positive hosting history. Molecule-addressed controls
and propulsion preserve that distinction; the new legacy molecule retains its
existing path.

Harvest of a molecule without any recorded worker/session/fleet assignment
or historical tackle fleet record
performs no transport or fleet teardown inferred from its id. Legacy fleet-only
assignments still select their recorded worker name; unavailable history never
grants a legacy control target.

## Supervised Herdr worker hosting

`cs tackle` accepts `[transport] backend = "herdr"` for Codex Exec with durable
exit receipts, using an explicitly selected existing protocol-22 socket.
Interactive and other adapter requests are refused. The host is saved on the
process: changing configuration does not migrate existing tmux or Herdr workers.
`cs observe`, fleet probes, forced purge and harvest follow that saved host.
Herdr stops use the supervisor channel exclusively (ADR-188); terminal close
operations remain unsupported. The UI inherits these CLI constraints rather
than adding a separate launch or stop path. Terminal attachment remains a
separate capability; this increment qualifies worker hosting, not a new Herdr
read-only attachment client. See [configuration](../project-config.md).

## Single-pass peek (2026-10-23)

| User intent | CLI | UI | Remote |
|---|---|---|---|
| Print one fleet baseline without a terminal | `cs peek --once` selects the non-TUI path; `--no-tui` alone still follows by default | TUI remains the interactive default | No matching change audited |

## Archived-only observe (2026-10-23)

| User intent | CLI | UI | Remote |
|---|---|---|---|
| Find a molecule absent from live state | `cs observe <id>` identifies an exact or unique-prefix archive match and names `cs archive show <full-id>`; exit status remains 1 | No matching change audited | No matching change audited |

## Remote evolve, resurrect, deps and prime (pilot plan 0001, lot E1, 2026-10-28)

| User intent | CLI | Remote CLI / API | UI |
|---|---|---|---|
| Advance a step | `cs evolve <id> --evidence … --formula …` | `cosmon-remote molecule evolve <id> --evidence …`; `POST /v1/molecules/{id}/evolve` (formula resolved from the tenant project) | No matching change audited |
| Revive a wreck | `cs resurrect <id> [--dry-run]` | `cosmon-remote molecule resurrect <id> [--dry-run]`; `POST /v1/molecules/{id}/resurrect` | No matching change audited |
| Show blocking, lineage, citation links | `cs deps <id> [--transitive]` | `cosmon-remote molecule deps <id> [--transitive]`; `GET /v1/molecules/{id}/deps` | No matching change audited |
| Re-inject the current step context | `cs prime <id>` | `cosmon-remote molecule prime <id>`; `GET /v1/molecules/{id}/prime` | No matching change audited |

Each route calls the operation its verb calls (`cosmon_state::ops::{evolve,
deps, prime}`; `cosmon_runtime::resurrect` plus the library executor for
`resurrect`). Remote differences, all deliberate: no `--formula` path (the
formula is the tenant project's); a step declaring a shell verification is
refused `501`; cross-galaxy links in `deps` are listed, not resolved;
`resurrect` needs the spawn scope and the per-noyau ceiling. `cs prime`'s help
text, which described a config self-check the code never performed, now
describes the propulsion reminder it prints; `cs evolve` is no longer on the
operator-only list (ADR-080 §5.6).
