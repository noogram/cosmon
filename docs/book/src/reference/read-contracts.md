# Read contracts for external consumers

A program outside cosmon can read cosmon's state without importing any cosmon
code. Four artefacts make up that surface. Each carries a `schema_version`, and
this page lists the fields a reader may rely on.

| Artefact | Where | `schema_version` |
|---|---|---|
| Event log | `.cosmon/state/events.jsonl`, one JSON object per line | `1` |
| Molecule state | `.cosmon/state/fleets/<fleet>/molecules/<id>/state.json` | `1` |
| Ensemble | `cs ensemble --json` | `1` |
| Git conventions | branch and merge subject | not versioned |

## The rule

`schema_version` is an integer. Within one value a contract changes additively
only: a field may be added, and no field is removed, renamed or given a new
meaning. A change that would break a reader bumps the integer. Readers must
ignore keys they do not know.

A record or file with no `schema_version` was written before the stamp existed
and reads as version `1`.

A field that is going to leave the contract is first listed here as deprecated,
and stays readable for at least one release after that listing. It leaves only
with a `schema_version` bump.

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
| `session_presence` | `session_id`, `provider`, `role`, `worker_id`, `molecule_id`, `state`, `detail`, `ts` |

**Session presence.** `session_presence` is appended to the galaxy's
`events.jsonl` by `cs sessions hook run`, once per change of session state; a
hook that reports the state the session already holds appends nothing. `role`
is `pilot` or `worker`. `state` is one of `session_start`, `working`, `idle`,
`waiting_permission`, `idle_input`, `asking`. `idle_input` (an additive value)
is a session idle at its prompt after Claude's idle reminder, as opposed to
`waiting_permission`, a real permission prompt. `provider` (`claude`, `codex`),
`worker_id` and `molecule_id` are absent when unknown; `molecule_id` is set for
a worker session. `detail` is optional, redacted, at most 160 characters and
present only for a Claude `Notification` moment; a detail-only change does not
append another event. `operator_present` is a different event: it is per `cs`
call, not per session.

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

A molecule's branch is `feat/<id>`, and the merge commit subject is
`Merge branch 'feat/<id>'`. No state field names the branch; this convention
is the contract. Molecule ids match `[A-Za-z0-9_.:/@+-]{1,128}`.
