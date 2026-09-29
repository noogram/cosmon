# Plan observation contract

Plan percentages are account context. Neither an account reading nor a change
between two readings establishes the share consumed by one worker. The C3
adapters always leave worker attribution unavailable and supply no account
correlation identifier. Consumers must decline cross-worker quota totals.

## Sources

| Input | Allowlisted windows | Input unit | Reset |
|---|---|---|---|
| Claude Code status-line stdin | `five_hour`, `seven_day` | percent | `resets_at`, epoch seconds |
| Claude Code stream `rate_limit_event` | `unifiedWindows.five_hour`, `unifiedWindows.seven_day` | fraction | `resetsAt`, epoch seconds |
| Codex rollout `event_msg` / `token_count` | `primary`, `secondary` | percent | `resets_at`, epoch seconds |

The Claude status-line contract is documented at
<https://code.claude.com/docs/en/statusline>. Installed schema evidence was
checked against Claude Code 2.1.283. A schema establishes capability, not live
capture. Claude stream parsing does not claim that the tmux launcher consumes
that stream. Context occupancy and gateway spend limits are separate meters
and are deliberately excluded.

Only finite nonnegative numbers are accepted. Conversion to a fraction happens
once. Claude status-line quota values outside its documented 0–100 range are
malformed. Claude stream overage is preserved; no global upper clamp is applied. Missing resets
remain unknown; invalid resets invalidate that window. A valid sibling survives.
Claude's two named window durations are 300 and 10080 minutes. Codex durations
come from the event; `primary` does not necessarily mean five hours.

`cosmon_core::plan_observation::codex_plan` reads quota separately from token
counters and the deprecated primary-only energy projection. The canonical
usage probe uses this reader for plan metadata. A new rate-limit object replaces the
window set within its quota bucket. Distinct Codex `limit_id` values retain
separate window sets under hashed meter prefixes; a bucket hash is not an
account identity. Missing siblings are explicit; token-only records and null limits
do not erase or refresh the preceding quota sample. Invalid JSONL tails are
ignored. No observation invents a plan classification from missing fields.

## Time and persistence

`PlanObservation` carries the canonical `PlanUsage` and per-window absence
reasons. Codex event timestamps remain source timestamps. Claude status-line
and stream payloads do not establish a provider observation time. Capture time
is supplied separately. `refresh(now, max_age)` is pure: expired resets and old
samples become stale; missing or future source times remain unknown. A recent
status-line invocation alone cannot establish that its cached quota is fresh.

`PlanObservationStore` is injectable. `FilePlanObservationStore` uses atomic
replacement and per-worker/source locking within a caller-supplied attempt
root. Older concurrent captures cannot overwrite newer ones. Reads do not
create directories or update times. The root must identify the worker attempt;
reuse across attempts is not supported. Only sanitized metadata is persisted.
This is a replaceable observation cache, not a second lifecycle ledger.
`UsageObserved` is the canonical durable emission after combining independent
components.
Removing the attempt directory retires this cache.

## Explicit worker launch integration

`claude_statusline_overlay` accepts resolved effective settings and a quoted
collector command. It returns only the status-line override for a worker's
`--settings` carrier. It preserves the existing command's stdin, stdout and
status-line options. The internal `cs plan-observation-hook ROOT WORKER`
forwards stdin verbatim through a pipe to that command, while persisting only
allowlisted fields. With no existing command its output must be redirected to
`/dev/null`, as the composition function does. Persistence failure still passes
input to the original command. Oversized input passes through without capture.
Do not invoke the hook directly as a visible status line: its stdout is the
private input stream intended for the existing command.

The caller must supply effective settings after resolving all provider settings
layers. Unknown settings or an unsupported status-line shape return unsupported.
The adapter does not read user settings, install itself, or overwrite global
configuration. A launch caller may merge the returned `statusLine` member into
its worker overlay while retaining receipt hooks and harness settings.

Dispatch now composes the collector into its existing per-worker settings
overlay after reading the user and worktree settings files. It retains the
selected status command and options, and leaves the overlay unchanged if the
files cannot be read or a separate launch settings flag is present. The hook
stores each sample in the molecule's `plan-observations/` directory. Dispatch
retires a previous sample for the same worker name before launch, so a retry
cannot display the prior attempt's quota as its own. Peek and ensemble read
that store through the shared energy probe and refresh each window with the
same ten-minute age rule used for rollout observations. Account scope and
unavailable worker attribution remain intact. Reading never installs a hook.

The settings resolution covers local files known at dispatch, not remotely
managed policy. A synthetic hook sample establishes the wiring but is not a
live quota witness. Ordinary activity must produce a stored reading before
live collection can be claimed.

## CLI/UI parity and specification scope

The internal hook is intercepted before ordinary CLI startup, like the briefing
receipt hook; it is not an operator verb. Peek, snapshot, ensemble, JSON and
the canonical event producer are documented in
[the usage surface contract](usage-surfaces.md). No molecule mutation or
lifecycle transition changes; the observation cache is out of band for
`CosmonRun.tla`.
