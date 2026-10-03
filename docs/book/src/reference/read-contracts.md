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
| `molecule_step_completed` | `step`, `total`, `evidence` |
| `molecule_completed` | `reason`, `summary` |
| `molecule_collapsed` | `reason`, `kind` |
| `merge_completed` | `result`, `branch` |
| `harvested` | `success` |
| `worker_spawned`, `adapter_selected`, `model_selected`, `model_observed` | `worker_id` or `mol_id`, `adapter_name`, `model` where the type has them |
| `energy_tick` | `worker_id`, `input_tokens`, `output_tokens`, `cost_usd` |
| `session_presence` | `session_id`, `provider`, `role`, `worker_id`, `molecule_id`, `state`, `ts` |

**Session presence.** `session_presence` is appended to the galaxy's
`events.jsonl` by `cs sessions hook run`, once per change of session state; a
hook that reports the state the session already holds appends nothing. `role`
is `pilot` or `worker`. `state` is one of `session_start`, `working`, `idle`,
`waiting_permission`, `asking`. `provider` (`claude`, `codex`), `worker_id` and
`molecule_id` are absent when unknown; `molecule_id` is set for a worker
session. `operator_present` is a different event: it is per `cs` call, not per
session.

A reader detects log rotation by a size decrease or an inode change.

## state.json

Stable keys: `schema_version`, `id`, `status` (`pending`, `running`,
`completed`, `collapsed`, `frozen`), `variables`, `tags`, `typed_links`
(`rel`, `source`, `target`), `assigned_worker`, `created_at`, `tackled_at`,
`updated_at`, `merged_at`, `collapse_reason`, `collapse_reason_kind`,
`current_step`, `total_steps`, `formula_id`, and `process.adapter_name`,
`process.model`. Keys may be absent when a molecule has no value for them.

## cs ensemble --json

Stable keys: `schema_version`, and for each entry of `workers`: `name`,
`molecule`, `model`, `live`, `effective`, `ghost`, `molecule_health`.

## Presence records

`.cosmon/state/presence/<session_id>.json` holds the latest record of a
session. Besides `session_id`, `galaxy`, `heartbeat_at` and `headline`, it
carries `state` with the same values as `session_presence.state`; the key is
absent for a session that has not reported one. `galaxy` is the name of the
session's galaxy (the directory that holds `.cosmon/`). `headline` stays free
text, and a reader should use `state` instead of parsing it. The file carries
no `schema_version` and follows the same additive rule.

## Git conventions

A molecule's branch is `feat/<id>`, and the merge commit subject is
`Merge branch 'feat/<id>'`. Molecule ids match `[A-Za-z0-9_.:/@+-]{1,128}`.
