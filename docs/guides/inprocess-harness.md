# In-process harness: tool-shell environment and file-tool paths

Direct-API adapters run the agent loop inside the `cs tackle` process. The
`exec_command` tool starts a persistent `bash` for the model. This page records
what that shell can see.

## Environment

The shell does not inherit the harness process's environment. It is spawned
with `env_clear` and receives only the set built by
`crates/cosmon-agent-harness/src/tools/shell_environment.rs`, on first spawn
and on every respawn after the shell died or timed out. The parent environment
is not re-imported on respawn, so exports made inside a dead shell are gone.

| Group | Variables | Reason |
|---|---|---|
| Fixed | `PS1`, `PS2`, `HISTFILE`, `TERM` | Keep the sentinel protocol deterministic. |
| Toolchain, locale | `PATH`, `HOME`, `USER`, `LOGNAME`, `LANG`, `LC_ALL`, `LC_CTYPE`, `TMPDIR`, `CARGO_HOME`, `RUSTUP_HOME`, `RUSTUP_TOOLCHAIN` | Commands need to find tools. Copied by exact name when set. |
| Lifecycle | `COSMON_MOL_DIR`, `COSMON_PARENT_MOL_ID`, `CB_DEPTH`, `CB_SESSION_ROLE`, `COSMON_ARTIFACT_DIR`, `COSMON_EGRESS_REQUIRE_NETNS`, `COSMON_API_REQUEST` | The worker's `cs` resolves its own molecule and the spawn-depth guard still applies. |
| Egress | `COSMON_EGRESS_POLICY` | Emitted as the resolved policy token. An unset or corrupt parent value reaches the shell as `deny-external`. |

Everything else is absent: provider credentials under any name (the configured
variable name is never consulted), shell-startup variables such as `BASH_ENV`,
loader injection variables, agent-socket variables, and the Claude account and
model pins. The list is an allowlist, so a variable nobody has thought of is
excluded by default.

## What this does not change

The shell runs as the same user with the same filesystem access. It can read
files a credential lives in, and `cd /` works. Reducing the environment removes
the credentials the process holds in memory from the shell's reach; it is not
isolation. The only enforced boundary is the egress network namespace, which
blocks the wire, not file reads. See the module documentation of
`exec_command` for the full statement.

## Test

`crates/cosmon-cli/tests/harness_shell_environment.rs` seeds a default-named
credential variable, a custom-named one and `BASH_ENV` in the parent, then
probes by name (never by value) from the tool shell, before and after a
respawn.

## File-tool path authority

`read_file`, `edit_file`, `write_file`, `list_dir`, `grep` and `find_file` share
one policy, implemented in `crates/cosmon-agent-harness/src/tools/path_authority.rs`.

- A path is relative and contains no `..`; absolute and parent paths are refused.
- Reads, listings and searches use the canonical path, which must lie inside the
  canonical work root. A symlink whose target stays inside the root is read; one
  that leaves it, or that dangles, is refused. The search root of `list_dir`,
  `grep` and `find_file` is checked the same way.
- Writes may target a missing file, but not a symlink, and the deepest existing
  ancestor must canonicalize inside the root. Existence is probed without
  following links, so a dangling ancestor link is checked rather than treated as
  absent.
- Any failure to compute a canonical form refuses the operation (reads, writes)
  or skips the entry (walkers). Nothing falls through to an open.
- `list_dir` reports a symlink as an entry of kind `symlink` with no size and no
  target, and never descends into it.
- `write_file` stays create-only, and `edit_file` keeps its per-file all-or-nothing
  commit.

### Limits

The check and the later open are separate system calls. A process that swaps a
path component between them (for example the persistent shell, or another
process on the machine) can still redirect the open. Closing that needs
descriptor-relative access or an outer sandbox, which this change does not
provide. The shell is not subject to this policy at all: `exec_command` can read
and write anything the user can, as described above.

`crates/cosmon-agent-harness/tests/file_authority.rs` plants links to a synthetic
file outside a temporary root and drives every tool through the default
registry.

## Turn, tool and context budgets

The `openai` and `anthropic` direct adapters run the loop in-process. Its
ceilings are configured per adapter in `.cosmon/config.toml` and, optionally,
per exact model id:

```toml
[adapters.openai]
max_turns = 80            # model round-trips; default 30
max_tool_calls = 200      # cumulative tool dispatches; default 64
max_input_tokens = 120000 # estimated input ceiling; default 32768
max_tokens = 4096         # output bound sent on the wire; default: none (openai), 8192 (anthropic)

[adapters.openai.models."gpt-4o-mini"]
max_turns = 120           # wins over the adapter-level value for this model only
```

Precedence per field is the exact-model row, then the adapter-level field, then
the built-in default. Model ids are matched exactly; no capacity is inferred
from a name. A zero value, or an input ceiling plus output bound that overflows
`u32`, is refused at dispatch with the offending field named. An absent row
keeps the previous fixed behaviour.

`max_tokens` is sent as `max_tokens` on the chat-completions body and as
`max_tokens` on the messages body.

### Final request guard

When `max_input_tokens` is configured, the serialized request body is measured
immediately before each network call and refused with `ContextOverflow` if its
estimate exceeds the ceiling. The body includes the system text, messages, tool
arguments and results, and tool schemas, so a large tool argument from an
earlier turn counts. The check runs on every attempt, including a retry that
splices in a corrective message. A refused request sends nothing.

### Limits

- The estimate is one token per four bytes, rounded up. It is a heuristic, not
  a tokenizer, and non-ASCII text is over-estimated. The ceiling is not
  validated against the model's real context window.
- The guard is active only when `max_input_tokens` is set. The spine's own
  log-based check keeps using the default ceiling otherwise.
- The guard refuses; it does not select or defer request-only peer input, and
  it does not compact in response to a refusal. Peer-input selection under the
  cap, and the interactive session's budgets, are not changed here.
- Input and output allowances are not checked against a known model capacity.

## Work-turn input and molecule context

Both direct arms read the member's declared work roster (`work-ref.json`) at
each request. Pending envelopes are appended to a copy of the message log for
that one request, in each arm's native message shape; later turns do not repeat
them. The delivery receipt is written after the request returns: `submitted`
when the response decoded, `failed` when the HTTP call or the decode failed. It
proves inclusion in an answered request, not comprehension, exactly-once
transport, or task acceptance. A response cut off by the output limit still
leaves a submitted receipt and cannot complete the step.

Both arms set `COSMON_MOL_DIR` to the worker's own molecule directory before the
loop, so `cs` run from the shell tool addresses that molecule. Errors from a run
with peer input map to the same error classes and silent-failure telemetry as a
run without. A failure of the input source itself is reported as a tool I/O
error prefixed `turn input`.

`crates/cosmon-cli/tests/harness_work_turn_parity.rs` drives `cs tackle
--adapter anthropic` against a loopback server.

## Usage accounting

Both direct arms and the `local` floor report the token counters each answered
request carried. The provider decodes the response's `usage` block at the same
seam where it already captures the served model, and hands one per-request
sample to a usage sink. `cs tackle` attaches a `HarnessUsageRecorder`, which
folds the samples of one worker attempt into a cumulative history and writes
each step as a `UsageObserved` event: the same record, history identity and
availability vocabulary the other usage producers use. No schema or price table
was added.

What a record means:

- **Cumulative, per attempt.** The history id names the molecule, worker and
  that attempt's invocation, so a re-tackle starts a new history. Each record
  carries the running totals, never one request's delta.
- **Unknown stays unknown.** A category is a measured total only while every
  sample reported it. One response without usage, or with a value the schema
  cannot hold (negative, fractional, a string, a subset larger than its total),
  makes that category unavailable from then on. It is never counted as zero. The
  projection keeps the latest record of a history when it lost a category, so an
  earlier partial sum is not shown as complete.
- **Sample identity.** A sample is identified by its provider response id.
  Delivering the same response twice changes nothing and writes nothing. A
  response with no id gets a sequence key, so it cannot be replayed through this
  path.
- **Every answered request counts.** Output-limited, refused and incomplete
  responses keep the usage they carried. A request that produced no decodable
  body (transport failure, an HTTP error, a stream that broke before its last
  frame) produces no sample and leaves a gap that later samples cannot fill.
- **Models stay separate.** Counters are paired with the model the response
  reported. The requested model is not stored as the served one; when a response
  names no model the segment axis is unavailable.
- **Cost.** The API-equivalent amount comes from the bundled reference tariff
  through the existing valuation. An unlisted model, a missing model or a counter
  gap yields partial coverage or no amount; a gap never produces a complete
  total.

The chat-completions wire asks a streamed response for its final usage frame
(`stream_options.include_usage`) only when a sink is attached, so the default
request body is unchanged. The messages wire reports input excluding cache
traffic; the recorder's input total is the sum of fresh input, cache reads and
cache writes.

### Limits

Usage is what the provider reported, not a billing record. The reasoning subset
is reported only by the chat wire, and the cache-write subset only by the
messages wire. The accumulator lives in memory for the attempt: after a process
restart a new history begins, and resuming a history is part of the checkpoint
work, not of this unit. Records are written best-effort; a failed write is
logged and the next record carries the same totals.

## Turn and effect evidence

A direct-arm worker records what it requested, received and ran, so a killed
attempt can be rebuilt from disk. `cs tackle` attaches a journal to the `openai`
and `anthropic` arms; the records are ledger rows and the content is stored in
blobs. See [ADR-185](../adr/185-turn-and-effect-evidence-is-durable-before-it-is-acted-on.md).

What is written, in this order:

| Record | Written | Means |
|---|---|---|
| `attempt_started` | before the first request | ceilings, tool registry digest, briefing digest, adapter, requested model |
| `request_intent` | before the request | a request with no later outcome may have been billed |
| `inputs_selected` | before the request | key and digest of each work-turn block that rides on it |
| `request_failed` | after a failed request | the request is resolved, not possibly billed |
| `compacted` | before the request it preceded | tokens before and after, messages replaced |
| `assistant_received` | before any of its tools | the provider-native envelope is durable |
| `tool_intent` | before the effect | if it cannot be written the tool does not run |
| `tool_receipt` | before the next call | classified result and the result as appended |
| `checkpoint` | after the last receipt of a turn | the native log at a complete boundary |
| `terminal` | before the loop returns | disposition and the text received, possibly partial |

Rows go to the galaxy ledger as `harness_turn_recorded`, so `cs events journal`
projects them with the molecule's other rows. A row holds counts, call
identifiers and digests, never raw model or tool text. The content is in
`<molecule dir>/harness-turns/blobs/<sha256>`: written to a temporary file and
renamed, named by its digest, never rewritten, and checked for length and digest
on every read. The turn records carry the same history id as the attempt's
`UsageObserved` rows, in the same ledger.

Reading an attempt back is `cosmon_state::harness_checkpoint::load_attempt`. It
returns the pure reconstruction (spent turns and tool calls, delivered input
keys, assistants, completed calls, compactions, last checkpoint, terminal) with
two lists that matter for recovery:

- `unresolved_calls`: a tool intent with no receipt. The effect may have
  happened. It is not a failure and not a success.
- `unresolved_requests`: a request intent with no outcome. It may have been billed.

A blob that is missing, truncated or altered is listed in `blob_problems` next
to the records that survive; one bad blob does not discard the attempt.

A record that cannot be written stops the loop with `HarnessError::Evidence`
before the next side effect. When the failed record is a tool receipt the tool
had already run, and the error says the effect is unconfirmed.

### Test

```text
./scripts/no-pilot-env.sh cargo test -p cosmon-cli --test harness_checkpoint_crash
./scripts/no-pilot-env.sh cargo test -p cosmon-core --lib harness_turn
./scripts/no-pilot-env.sh cargo test -p cosmon-state --lib harness_checkpoint
```

The crash test re-executes itself as a child that exits abruptly at one seam,
then rebuilds the attempt from the ledger and the blob directory. Another test
runs `cs tackle` against a loopback responder and reads the same evidence back.

### Limits

- Resume is explicit and narrow; see [Resuming an interrupted attempt](#resuming-an-interrupted-attempt).
- An uncertain effect is reported, never repaired or repeated.
- The `local` floor runs a loop with no progress channel and writes no turn
  evidence. Interactive sessions do not write it either.
- A blob over 8 MiB is refused and stops the loop. Total retention per molecule
  is not bounded yet.
- Blobs hold the native conversation in clear text under the molecule directory.
  They are as private as that directory and are never published.
- The formula digest and a configuration digest are not pinned; the requested
  model, adapter, registry and briefing are.
- Usage and turn records now go to the galaxy ledger. Before this change the
  `openai` and `anthropic` arms wrote `UsageObserved` rows to the molecule
  directory's own event file, where the usage projections do not read.

## Resuming an interrupted attempt

`cs tackle <molecule> --adapter openai|anthropic --force --resume` continues the
molecule's latest in-process attempt from its last complete tool-result
checkpoint. Without `--resume` a dispatch starts a fresh attempt, as before.
`--force` is the existing gesture that thaws a frozen molecule; `--resume` adds
only the intent to continue. `cs resurrect` rebuilds a Claude session and never
continues an in-process attempt. A molecule that collapsed because its loop
failed is terminal and is not resumable.

Resume reads the durable evidence on disk and nothing else
(`cosmon_state::harness_checkpoint::load_resumable`, then the pure rules in
`cosmon_core::harness_turn::plan_resume`). It continues only when all of this
holds, and otherwise refuses with the reason and does nothing:

| Refused when | Why |
|---|---|
| a tool has an intent and no receipt | its effect is unknown; it is never repeated or assumed |
| a request has no recorded outcome | it may have been billed; it is not sent again |
| a completed call used `exec_command` | the shell's directory, exports and processes are not recorded |
| there is no complete checkpoint, or a turn began after it | nothing proves the effects after the boundary |
| the attempt already ended on a terminal response | there is nothing to resume |
| a pin differs: formula step, adapter, requested model, tool registry, briefing, worktree | a changed input is a new admission, not a continuation |
| the loop ceilings differ | budgets are spent, not refreshed |
| the wall-clock budget is already spent | the deadline only shrinks |
| a referenced blob is missing or altered, or the records break their ordering | the evidence is damaged |
| another `cs tackle` holds the molecule's loop | one owner per molecule (`harness-turns/owner.lock`) |

A continuation restores the native log from the checkpoint blob
(`MessageLog::decode_checkpoint`), starts at the next turn with the tool count
already spent, and runs with the wall-clock time the chain has left. It writes a
new `attempt_started` and then a `resumed` record naming the attempt and
checkpoint it continues, both before its first request. A refusal happens before
either is written, so the interrupted attempt stays the latest one. The restored
checkpoint is the baseline of a second interruption, so a chain of resumes
inherits every spent budget. Work already in the worktree counts as the
molecule's own output when the continuation is accepted.

Peer input blocks are request-only and are marked delivered once the response
returns. At a clean checkpoint every one of them was resolved, so none is
delivered twice. Completion is unchanged: a continuation that ends normally goes
through the same acceptance as a fresh attempt, and a terminal attempt is never
resumed, so a pending completion cannot advance twice.

To reconcile a refused attempt, inspect `harness-turns/` and the worktree, then
either finish the work by hand and collapse the molecule, or collapse it and
nucleate again. Nothing silently restarts it.

### Test

```text
./scripts/no-pilot-env.sh cargo test -p cosmon-cli --test harness_resume
./scripts/no-pilot-env.sh cargo test -p cosmon-core --lib harness_turn
```

The crash seams run the real loop in a child killed at a seam. The dispatch
tests run `cs tackle` against a loopback responder that reads the galaxy ledger
file at its exact path.

### Limits

- The tool-cycle detector restarts empty after a resume.
- Wall-clock time spent between the last record and the kill is not counted.
- A shell-using attempt is never resumable, however harmless its commands were;
  nothing can prove that.
- Only the `openai` and `anthropic` arms keep turn evidence. The `local` floor
  cannot be resumed.
