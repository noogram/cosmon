# Read contracts for external consumers

A program outside cosmon can read cosmon's state without importing any cosmon
code. Five artefacts make up that surface. Each carries a `schema_version`, and
this page lists the fields a reader may rely on.

| Artefact | Where | `schema_version` |
|---|---|---|
| Event log | `.cosmon/state/events.jsonl`, one JSON object per line | `1` |
| Molecule state | `.cosmon/state/fleets/<fleet>/molecules/<id>/state.json` | `1` |
| Ensemble | `cs ensemble --json` | `1` |
| Local interactive terminal capability | `cs --json terminal EXACT_ID --mode interactive` | `1`, distinct kind |
| Local worker terminal capability | `cs --json terminal EXACT_ID` | `1` |
| Git conventions | branch and merge subject | not versioned |

## The rule

`schema_version` is an integer. Within one value a contract changes additively
only: a field may be added, and no field is removed, renamed or given a new
meaning. A change that would break a reader bumps the integer. Readers must
ignore keys they do not know.

A historical record or file with no `schema_version` reads as version `1`.
The new terminal capability requires an explicit version and kind: missing or
unsupported stamps must be refused by clients.

A field that is going to leave the contract is first listed here as deprecated,
and stays readable for at least one release after that listing. It leaves only
with a `schema_version` bump.

## Client integrations: local terminal capability v1

`cs --config STATE_DIRECTORY --json terminal EXACT_ID` observes a bound worker
terminal without changing lifecycle or sending input. `STATE_DIRECTORY` is the
selected galaxy's state directory, not its configuration file. Exact molecule
ids are required; prefixes are refused. Existing `observe` output is unchanged.
Cosmon has no dependency on the clients consuming this contract. Existing
`observe` reads used for client details append authorization instrumentation to
`instrumentation/authz.jsonl`; lifecycle/process state and artifact bytes stay
unchanged. The new `terminal` command itself writes no persistent state.

Every observation has `schema_version: 1`, `kind: "cosmon.terminal"`, `id`,
`project_id`, `fleet_id`, canonical absolute `project_root` and `state_dir`, and
`availability`. Valid observations exit 0, including unknown/unavailable.
Invalid/missing ids, wrong projects and refused attachment exit nonzero and
report errors on stderr. Paths and process evidence are local and private;
clients should not publish their runtime observations.

| Availability | Fields and meaning |
|---|---|
| `available` | `capability: "read_only_ignore_size"`, `identity`, opaque `expected` token |
| `unknown` | `reason: "identity_unknown"` for a legacy/non-terminal launch without a witness, or `"backend_unavailable"` when the backend could not be read |
| `unavailable` | `reason: "no_process"`, `"identity_changed"` or `"ambiguous_terminal"`; no attachment capability |

Unknown/unavailable omit `identity`, `expected` and `capability`. Absence of a
successful probe never establishes worker death. Molecule completion, terminal
availability and deliverable acceptance remain separate observations.

Example (synthetic values, not a live receipt):

```json
{
  "schema_version": 1,
  "kind": "cosmon.terminal",
  "id": "task-20990101-a001",
  "project_id": "example-project",
  "fleet_id": "default",
  "project_root": "/example/galaxy",
  "state_dir": "/example/galaxy/.cosmon/state",
  "availability": "available",
  "capability": "read_only_ignore_size",
  "identity": {
    "backend": "tmux",
    "socket_path": "/example/tmux/socket",
    "server_pid": 1200,
    "server_start": 1000000,
    "session_id": 4,
    "session_created": 1800000000,
    "pane_id": 6,
    "pane_pid": 1300,
    "pane_start": 2000000
  },
  "expected": "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
}
```

`identity.backend = "tmux"` carries numeric session/pane ids, server/pane PIDs,
opaque platform launch fingerprints, tmux creation timestamp and exact socket
path. The spawning adapter captures the newly created target with
`new-session -P -F`; the optional `process.terminal` field persists it. No
retroactive witness is inferred from a label. Legacy launches, other backends,
unsupported platforms and failed optional witness writes remain unknown;
workers continue to run normally. Current kernel witnesses support macOS and
Linux; tmux server versions below 3.2 or unknown versions refuse the capability.
Backend read probes are bounded to three seconds. Terminal availability
authenticates the pane root process, not provider progress or business success. Only a single live window with a single pane is offered.

Clients must validate version, kind, selected project/state paths, exact id,
availability and capability. They ignore additive fields, refuse unknown
versions/capabilities, and re-observe immediately before opening a view. Treat
`expected` as opaque; do not hash identities or attach directly to their
addresses. Launch the public command with inherited PTY input/output and the
validated project cwd, using separate argv entries:

```json
["cs", "--config", "/example/galaxy/.cosmon/state", "terminal", "task-20990101-a001", "--attach", "--expect", "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"]
```

Both flags are required together. The token binds the project, state directory,
fleet, exact molecule and complete process record. Attachment re-reads that
binding, validates kernel fingerprints and guards server/session/pane numbers
inside tmux before `attach-session -E -r -f ignore-size`. It uses a separate
client, clearing inherited `TMUX`/`TMUX_PANE` only for this attachment process.
`-E` preserves session environment. An output-only renderer uses a read-only
control client without size announcements, because a sole ordinary ignore-size
client can still determine tmux's fallback size. No keyboard byte is forwarded;
outer UI dimensions affect only local presentation. Closing the client detaches its view. Opening/closing
never spawns, stops or harvests the worker. Local `q`, Escape or Ctrl-C closes the output-only view;
independent tmux operations and configured hooks remain operator-owned.

This is a same-user consistency check, not a security boundary. Filesystem
lifecycle reads and kernel probes cannot be atomic with tmux's queue. A
concurrent lifecycle clear after the last read remains possible. PID start
tokens are checked immediately before execution but cannot be evaluated by
native tmux formats: a server restart with colliding numeric identities and
creation timestamps in that final interval is not excluded. tmux hooks and
user configuration also remain operator-owned. The view is read-only while
its client flags stay read-only; it does not revoke the user's independent
ability to operate tmux.

The optional [Herdr plugin](../../../../tools/herdr-companion/README.md) and
independent [cmux tooling](../../../../tools/cmux-companion/README.md), and
[native Pi package](../../../../tools/pi-companion/README.md) consume this
contract. Each client's acceptance requires its own witnesses. Pi uses an owned
local PTY bridge and native component for output; it exposes no worker input or
backend. Its disposable Pi 1.1.0 TUI acceptance is distinct from GUI visual
review, remote use and worker-backend qualification. No remote API or native Cosmon UI
terminal route is added. Existing schemas and historical baselines are preserved.

The generic attachment requires Python 3 on macOS/Linux for its output-only
control-mode renderer. It launches a read-only/ignore-size tmux control client
without sending size announcements (`refresh-client -C`), including after an
ordinary controller resizes and detaches. Native ignore-size alone is not
sufficient when the viewer is the only client. The renderer samples guarded
visible-pane frames four times per second; it is not a lossless PTY byte stream
or scrollback viewer. Local q/Escape/Ctrl-C closes it and no keyboard bytes are
forwarded. Outer UI dimensions affect presentation only.

## Explicit local interaction

The omitted mode (or `--mode read-only`) retains the exact v1 kind and
output-only behavior above. `--mode interactive` is a separate typed port and
contract, `schema_version: 1`, `kind: "cosmon.terminal.interactive"`, with
`capability: "interactive_resize"` when available. Identity, availability,
project/fleet binding and expected-token semantics are the same. Observing this
capability sends no input. To request human input/output explicitly:

```sh
cs terminal EXACT_ID --mode interactive --attach --expect TOKEN
```

The input renderer attaches a separate tmux control client with `-E`, encodes
keyboard bytes as hex, and guards numeric server/session/pane identity on every
input, frame and resize in the server queue. Ctrl-] detaches locally; other
bytes, including Ctrl-C, go to the worker. Closing the outer pane detaches only
this client. No worker start/resume or lifecycle command is involved.

Interactive clients announce their dimensions with `refresh-client -C` and
participate in normal tmux sizing; opening, resizing and detaching may change
worker dimensions according to tmux window-size policy and other clients.
Session environment updates remain disabled by `-E`; this does not replace
the worker process environment or inject Herdr variables into it. The display
samples visible-pane frames at four Hz; it is not a lossless terminal byte
stream, scrollback browser, or guarantee of every full-screen application.
The same final-interval, tmux hook and same-user consistency limitations above
apply. Input authority is explicit local intent, never inferred from lifecycle
status, semantic status, or terminal availability.

The optional Herdr wrapper reports native label **Cosmon** with state
**unknown**: this reader has no supported actual harness semantic witness.
Molecule `running` never becomes `working`. Its tab/message describes a view,
not another harness. The `Interagir` selector action opens an explicit
interactive view. Read-only remains the default. Restore registers only the
wrapper's read-only reconnect argv pinned to project, exact molecule and
original token; it never launches a second harness or restores input authority.
The original plugin/script and Cosmon executable paths must still exist.
Herdr restore must be enabled and accept its bounded argv (plain PATH command,
no apostrophes/control bytes). Rejected paths are reported as restore
unavailable. Duplicate views may be opened, but never duplicate workers.

## events.jsonl

Every record has `type` (older lines may use `kind`), `timestamp` (ISO 8601
with zone), and `schema_version`. `seq` is present on records written by the
current writer.

**Molecule id.** The canonical key is `molecule_id`. Some event types also
spell it `mol_id` or `molecule`; those keys are kept as read aliases and are
deprecated as sources of the id. A reader takes `molecule_id` first and falls
back to the other two for older lines. Events about no molecule (for example
`energy_tick`) carry none of the three.

**Step evidence and completion summary.** `molecule_step_completed.evidence`
holds the text recorded with `cs evolve --evidence`, and
`molecule_completed.summary` holds the completion summary. Both are absent on
lines written before they existed, and `evidence` is absent when no text was
recorded. `molecule_completed.reason` stays as it was.

Fields by event type:

| Event type | Stable fields |
|---|---|
| `molecule_nucleated` | `formula_id`, `blocks` |
| `molecule_status_changed` | `from`, `to` |
| `molecule_transitioned` | `from`, `to` |
| `molecule_evolved` | `step`, `total` |
| `molecule_frozen`, `molecule_thawed` | none |
| `molecule_step_completed` | `step`, `total`, `evidence` |
| `molecule_completed` | `reason`, `summary` |
| `molecule_collapsed` | `reason`, `kind` |
| `merge_completed` | `result`, `branch` |
| `harvested` | `success` |
| `worker_spawned`, `adapter_selected`, `model_selected`, `model_observed` | `worker_id` or `mol_id`, `adapter_name`, `model` where the type has them |
| `energy_tick` | `worker_id`, `input_tokens`, `output_tokens`, `cost_usd` (legacy; current writers emit `usage_observed`) |
| `usage_observed` | `usage` (see below) |
| `session_presence` | `session_id`, `provider`, `role`, `worker_id`, `molecule_id`, `state`, `source`, `detail`, `ts` |

**Session presence.** `session_presence` is appended to the galaxy's
`events.jsonl` by `cs sessions hook run` or `cs sessions lifecycle`, once per
change of session state or source capability; an identical reading appends
nothing. `role`
is `pilot` or `worker`. `state` is one of `session_start`, `working`, `idle`,
`waiting_permission`, `idle_input`, `asking`. `idle_input` (an additive value)
is a session idle at its prompt after Claude's idle reminder, as opposed to
`waiting_permission`, a real permission prompt. `provider` (`claude`, `codex`),
`worker_id` and `molecule_id` are absent when unknown; `molecule_id` is set for
a worker session. `source` is `live_lifecycle`, `turn_end_only`, or
`unavailable`. `state` is absent when `source` is `unavailable`; absence is
unknown, never inferred idle. Events written before `source` existed decode as
`unavailable` while retaining their legacy `state`. `detail` is optional,
redacted, at most 160 characters and
present only for a Claude `Notification` moment; a detail-only change does not
append another event. `operator_present` is a different event: it is per `cs`
call, not per session.

**Provider coverage.** The state vocabulary is not a promise that every
provider can publish every state. Codex configured through its single `notify`
callback publishes `source: turn_end_only` and only `idle` at turn end. It does
not publish `session_start`, `working`, `waiting_permission`, `idle_input`, or
`asking`. Its local rollout files remain discovery and post-turn evidence, not
a live lifecycle stream.

A client that already owns a Codex app-server connection may forward only
`turn/started`, an `item/*/requestApproval` request, `turn/completed`, and
disconnect through `cs sessions lifecycle`. They map respectively to
`working`, `waiting_permission`, `idle`, and `source: unavailable` with no
state. The client assigns a sequence that is strictly increasing per provider
session and continues it across reconnects. Equal delivery is idempotent;
lower delivery is stale; neither refreshes presence nor overwrites a newer
reading. Before forwarding its first event, the connected client must establish
the session's presence row with `sessions attach` or a provider hook. Ingress
refuses to invent provider-session pid or cwd provenance. It preserves every
non-lifecycle presence field and refuses a row that names a provider other than
`codex`, while an absent provider on an existing row is initialized to `codex`.
It does not inspect conversation content, process age, quota, terminal contents,
or silence, and it does not make cosmon the owner of the app-server connection.

The anonymous local provider witness in
`crates/cosmon-core/tests/fixtures/provider-witness-2026-10-18.json` records
Codex CLI 0.163.0 with one isolated `gpt-6-luna` turn: actual
`turn/started`, command approval and `turn/completed` events were forwarded to
the CLI ingress, followed by an explicit transport disconnect. The recorded
readings are `working`, `waiting_permission`, `idle`, then unavailable with no
state. The owner of this disposable app-server connection assigned the sequence;
it was not a resident Cosmon broker. Replaying these observations tests the fold
and does not constitute another live witness.

The same Codex turn's tool subprocess exported matching `CODEX_THREAD_ID` and
`CODEX_SESSION_ID`, distinct from the freshly minted launch fallback. A hook
invocation with a payload constructed from those observed exports and an
interactive takeover check resolved the same presence row. This qualifies the
tool environment and shared resolution, not delivery of a configured `notify`
callback by app-server. The check refused because the disposable mission had no
lease; no grant or takeover was performed.

Claude Code 2.1.302 was probed with `--init-only`: a real `SessionStart` callback
exported `CLAUDE_CODE_SESSION_ID` matching its native payload, while
`CLAUDE_SESSION_ID` was absent. The supported launcher supplied a fresh marked
identity shared by that callback's presence row and its shell check. No Claude
model was called. A Claude **tool subprocess** environment remains unverified;
the callback export alone does not establish a native tool identity contract.
Keep using `cs sessions launch claude` for the shared launch fallback.

The presence snapshot exposed by `cs sessions list --json` carries the same
`lifecycle_source`, optional `state`, and the last accepted
`lifecycle_sequence`. A snapshot from before these fields existed reads as
`lifecycle_source: unavailable` with no invented sequence.

**Usage.** `usage_observed` carries one `usage` object per answered model
request, with its own `schema_version` (currently `1`) that moves independently
of the log's. The fields a reader may rely on:

- `usage.subject.worker_id` and `usage.subject.history.id`. The history id
  names one cumulative counter; `history.kind` is `known` when the id is
  usable and `legacy_unknown` otherwise.
- `usage.tokens.{input_tokens, cached_input_tokens, cache_write_tokens,
  output_tokens, reasoning_output_tokens}`, each `{"status": "measured",
  "tokens": N}` or `{"status": "unavailable", "reason": ...}`.
- `usage.tokens.model_segments`, either `{"status": "available", "value":
  [...]}` or `{"status": "unavailable", ...}`. Each segment has `model` and
  the same five counters as above, scoped to that model.
- `usage.api_equivalent` and `usage.plan`, which price and meter the same
  observation. Both can be `unavailable`.

The counters are cumulative over the history, not deltas. To total a worker's
usage, keep the last record of each history (the one with the highest `seq`)
and sum those; adding every record counts the early requests again. The same
rule per model applies to the segments: take the last record of the history and
read its segments. A record cannot name a molecule. Resolve `subject.worker_id`
with the `worker_spawned` event that carries both `worker_id` and `molecule_id`;
the molecule's own `process` record is cleared when the worker is torn down.
To fold per molecule, sum over the molecule's workers.

`unavailable` means the source did not report the value, and it is not zero. A
single request that reports no usage turns every category of its history
`unavailable` from then on, because the running total can no longer be known.
A reader must carry that through to its own total and not substitute `0`.

A reader detects log rotation by a size decrease or an inode change.

**`cs sessions seats --json`.** Requires the global `--json` flag and emits one `schema_version: 1` document rather
than the NDJSON used by `cs sessions list`. Each galaxy carries a readable
state (`ok`, `unreadable`, `no mission`, `ambiguous mission`, or `no trust
root`); each mission carries a verdict (`one`, `none`, `granted-unseated`,
`contested`, or `epoch conflict`). `seats` retain every presence claim with
its liveness and heartbeat, but only a current verified lease holder has
`standing: "holder"`. A primary claim at an epoch below the verified lease
epoch has `standing: "superseded"` and does not count toward `contested`; a
claim at or above the lease epoch, or without an epoch, is a `claimant` and does. A Codex claim without `native_id` cannot be a holder.
`--all-galaxies` uses the same perimeter enumeration as `cs tail` and derives
each entry with the same rules as the single-galaxy document. Each galaxy
carries `galaxy` (its name) and `root` (the project directory holding
`.cosmon`, or `null` when the state directory is not laid out that way).
A pilot session whose hooks are installed seats itself as a `copilot` of the
galaxy's sole mission at session start; `primary` comes only from a signed
grant.

**`cs sessions takeover challenge --json`.** Emits one `schema_version: 1`
document describing exactly one proposed transfer. Within version 1, changes
are additive under the rule above. The stable fields are:

- `schema_version`: the integer `1`.
- `challenge`: the structured transfer. Its fields are `mission_id` (the
  mission whose controls would move), `holder_session_id` (the exact session
  that would become primary), `epoch` (the next positive lease generation),
  `granted_by` (the operator label covered by the signature), and `ttl_seconds`
  (a non-negative integer, or `null` for no time limit).
- `bytes`: the exact UTF-8 text to write to the batch file and sign. It ends in
  a newline. A client must not reconstruct it from `challenge`, normalise it,
  or sign the JSON serialization. The grant verifier checks these exact bytes.

The command is read-only. Producing the document does not reserve its epoch;
if another grant advances that mission before this challenge is submitted,
the signature is stale and the grant is refused.

**`cs tail --all-galaxies --json`.** Each emitted JSON object retains the
event-log fields and adds `source_galaxy`, the name of the project whose ledger
supplied that row. The field is reader metadata rather than a mutation of the
authoritative ledger, so a consumer can use it to select the source for a
re-read without overwriting an event field. Fleet-local `cs tail --json` also
emits `source_galaxy` for a uniform stream shape.

## Reading the ledger over HTTP

`GET /v1/ledger` serves `events.jsonl` to a program that cannot read the file,
as a Server-Sent Events stream: it replays from a cursor and then follows the
log, from one reader over one file, so no line is missed or delivered twice at
the point where replay ends and live begins. It needs the scopes
`cosmon:events:subscribe` and `cosmon:molecule:read`, because
`molecule_step_completed.evidence` is free text a worker wrote.

**Cursor.** The `id` of each data frame is the resume cursor. It is opaque:
echo it back verbatim in `Last-Event-ID` or `?after=` (the header wins when
both are sent) and never parse it. It is not `seq`, which is absent on
lifecycle lines written by the file store and is not monotone where present.
Without a cursor the stream starts at the beginning of the log.

**Frames.** `ledger.epoch` comes first and names the log the cursors refer to.
`ledger.reset` means the log was replaced or shortened, or that a supplied
cursor could not be honoured (`reason`: `epoch_changed`, `truncated`,
`cursor_invalid`); the stream then replays from the start of the current log
and the reader must discard what it folded. Every other frame is one line of
the log: `event` is its type, and `data` holds `schema_version` (the line's own;
`1` for a line that carries none), `type`, `molecule_id`, `timestamp`, `seq` and
`mol_seq` where the line has them, and the stable fields of its type from the
table above. Unknown event names are to be ignored.

**What is not served.** Only the types in the table above, and only lines that
name a molecule (so not `operator_present` or `operator_signed`, which name
none). A line is never passed through verbatim. A type that is not in the table
is not served until it is added to it. `session_presence.session_id` is replaced
by `session_digest`, a stable digest that tells sessions apart. Worktree paths,
shell commands and harness-turn text are in no projection. `usage_observed` and
`energy_tick` name no molecule and are not served.

**Bounds.** A connection ends after 5,000 frames; reconnect with the last `id`.
A principal holds one ledger stream at a time.

## Reading the molecule collection over HTTP

`GET /v1/molecules` returns the tenant's state-only molecule index. Rows are
ordered by `(created_at, id)` ascending. Each row carries `id`, `formula`,
`status`, derived `phase`, `updated_at`, `kind` when recorded, `fleet`, step
counts, worker when assigned, sorted `tags`, `created_at`, `typed_links`, and
the optional state facts `merged_at`, `non_integration`, `last_progress_at`,
and `base_branch`. Token totals, model attribution, and energy are deliberately
absent; those remain on `GET /v1/molecules/{id}`.

Paging is opt-in. Without `limit`, the route returns the complete filtered
collection as before. With `limit=1..200`, `next_cursor` is the final molecule
id on a non-final page; echo it as `cursor` with the same filters and limit.
The `total` is the number of filtered rows before paging. A 500-row cold start
therefore takes three list requests at the maximum page size.

The weak `ETag` covers the projected page and its filters. Send it in
`If-None-Match`; an unchanged page returns a bodiless `304`. The
`ledger_cursor` is the ledger head read immediately before the state listing.
Open `GET /v1/ledger` from that opaque cursor for subsequent deltas. This is a
watermark, not a transactional snapshot: not every historical writer is known
to save `state.json` before appending its event. A consumer keeps the stream
and periodically revalidates the collection instead of treating one list plus
one stream as permanently gap-free.

## Reading one molecule over HTTP

`GET /v1/molecules/{id}` carries the persisted `base_branch` when `state.json`
has one and always carries the server-computed `branch`. The branch honours a
non-empty persisted `originating_branch`; older records without one fall back
to `feat/<id>`. Consumers use this field instead of reproducing that rule.

Two useful consumer values remain ledger folds rather than molecule fields:

- `ended_at` is the `timestamp` of the last `molecule_completed` or
  `molecule_collapsed` frame for the molecule in ledger order.
- `merge_result` is the `result` of the last `merge_completed` frame for the
  molecule in ledger order.

Before a matching frame, each value is absent rather than an empty string. A
`ledger.reset` clears both folds before replay. `merged_at` and
`non_integration` remain the authoritative persisted integration facts on the
molecule response; `merge_result` explains the latest merge attempt and does
not replace either one.

## state.json

Stable keys: `schema_version`, `id`, `status` (`pending`, `running`,
`completed`, `collapsed`, `frozen`), `variables`, `tags`, `typed_links`
(`rel`, `source`, `target`), `assigned_worker`, `created_at`, `tackled_at`,
`updated_at`, `merged_at`, `collapse_reason`, `collapse_reason_kind`,
`current_step`, `total_steps`, `formula_id`, `last_progress_at`,
`last_output_at`, and `process.adapter_name`, `process.model`,
`process.worker_id`, `process.worktree_path`. Keys may be absent when a
molecule has no value for them.

**Liveness.** `last_progress_at` and `last_output_at` are ISO 8601 timestamps
written by `cs evolve` each time a step completes. `last_progress_at` is also
advanced by `cs heartbeat --molecule`, so it records that a worker is alive.
`last_output_at` is never advanced by a heartbeat; it records the last durable
work product. A worker that is running with a recent `last_progress_at` and an
old `last_output_at` is alive and has produced nothing lately. Both are absent
before the first step completes, and a reader then uses `tackled_at` as the
start of the window.

**Worktree.** `process.worktree_path` is the absolute path of the directory
the worker was launched in: the molecule's worktree, or the project root when
the molecule was tackled with `--no-worktree`. It is set by `cs tackle` and
removed with the rest of `process` when the worker is torn down. A record
written before this field existed has none, and a reader falls back to
`<project root>/.worktrees/<id>`.

**Polymer.** There is no polymer field. Membership and order are the
`typed_links` of `rel` `blocks` (`target`) and `blocked_by` (`source`): the
two are written on both ends of an edge, so a molecule lists its upstream
molecules under `blocked_by` and its downstream ones under `blocks`. A polymer
is the set of molecules connected by those edges, and its order is the
partial order they define. A `decay_product` or `decayed_from` link records
lineage and does not order anything.

## cs ensemble --json

Stable keys: `schema_version`, and for each entry of `workers`: `name`,
`molecule`, `model`, `live`, `effective`, `ghost`, `molecule_health`.
Each entry of `molecule_states` carries `id`, `status`, derived `phase`,
`fleet`, `updated_at`, optional `kind`, `tags`, `blocked_by`, `typed_links`,
optional `merged_at`, `non_integration`, `last_progress_at`, `stuck_at`,
adapter fields, and optional `base_branch`.

## Presence records

`.cosmon/state/presence/<session_id>.json` holds the latest record of a
session. Besides `session_id`, `galaxy`, `heartbeat_at` and `headline`, it
carries `state` with the same values as `session_presence.state`; the key is
absent for a session that has not reported one. `galaxy` is the name of the
session's galaxy (the directory that holds `.cosmon/`). `headline` stays free
text, and a reader should use `state` instead of parsing it. `detail` is absent
except for the latest Claude `Notification` moment; when present it is redacted
and no longer than 160 characters. The file carries no `schema_version` and
follows the same additive rule.

## Git conventions

A molecule's branch is its non-empty persisted `originating_branch`, with
`feat/<id>` as the legacy fallback. `GET /v1/molecules/{id}` exposes the
resolved value as `branch`. The merge commit subject is `Merge branch
'<branch>'`. Molecule ids match `[A-Za-z0-9_.:/@+-]{1,128}`.
