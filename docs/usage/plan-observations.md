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
counters and the deprecated primary-only energy projection. C4 must use this
reader for canonical plan metadata. A new rate-limit object replaces the
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
This is a replaceable observation cache, not a second lifecycle ledger. C4
owns canonical `UsageObserved` emission after combining independent components.
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

Automatic launch enablement remains open: current dispatch does not establish
an effective-settings resolution contract. Neither peek nor ensemble enables
collection. Until a launch caller supplies that contract and an ordinary
worker response produces a supported reading, Claude live integration remains
unvalidated. C4 can render explicit unavailable state; C5 must keep this live
criterion open. Synthetic fixtures and a working pipe are not live evidence.

## CLI/UI parity and specification scope

No operator command, help, man page or existing UI projection changes here.
The internal hook is intercepted before ordinary CLI startup, like the briefing
receipt hook; it is not an operator verb. Existing help/man snapshots therefore
require no regeneration. Peek, snapshot, ensemble, JSON and canonical event
producer cutover remain C4 work. No molecule mutation or lifecycle transition
changes; the observation cache is out of band for `CosmonRun.tla`.
