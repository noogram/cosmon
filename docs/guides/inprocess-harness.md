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
