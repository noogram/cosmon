# Probe sample file convention

A **probe** is a patrol declared in `~/.config/cosmon/patrols.toml` that
writes one numeric sample per fire to a TSV file on disk. The scheduler
reads that file on every tick and — if the patrol has a
`[patrol.sunset]` block — decides whether the signal has converged and
the probe can stop.

This document pins the file format so probes across the fleet agree with
the reader shipped in `crates/cosmon-scheduler/src/convergence.rs`.

## TL;DR

```tsv
# u2 — Apple Notes unique-count probe
# written by bin/notes-u2-probe on every tick
2026-04-19T14:00:00Z	apple-notes	1423
2026-04-19T14:05:00Z	apple-notes	1428
2026-04-19T14:10:00Z	apple-notes	1431
```

- One sample per line. No headers, no footers.
- Comments start with `#` and are ignored.
- Blank lines are ignored.
- The scheduler reads the **last whitespace-separated column** as `f64`.
- Append-only — never rewrite the file mid-campaign.

## Line grammar

| Line shape                            | How the reader treats it              |
|---------------------------------------|---------------------------------------|
| empty / whitespace-only               | skipped silently                      |
| starts with `#` (after trim)          | skipped silently (comment)            |
| last column parses as `f64`           | value appended to the sample series   |
| last column does not parse            | row dropped; `MalformedRow` warning   |

## File policy

| Condition                             | Outcome                               |
|---------------------------------------|---------------------------------------|
| file does not exist                   | empty series + `MissingFile` warning  |
| permission / I/O error                | empty series + `IoError` warning      |
| exists but has 0 data rows            | empty series + `EmptyFile` warning    |

Warnings are advisory — they never abort the tick. A probe whose file is
temporarily missing is treated as "not enough samples yet", not as a
hard error. That is the same discipline `cargo check` uses when a file
it cannot open: warn, keep moving.

## Required columns

Exactly **one required column: the last one**, containing a number.

Everything to the left is optional and ignored by the convergence reader.
Put whatever helps humans — a timestamp, a label, a unit tag — knowing
the scheduler will keep only the final field. That keeps the same reader
compatible with both one-column probes (`echo "$v" >> log.tsv`) and
structured multi-column logs (`printf '%s\t%s\t%s\n' "$ts" "$label" "$v"`).

## Header rules

**Do not write a header row.** There is no schema line. If you need to
annotate the file, use a comment block at the top:

```tsv
# probe:  u2-apple-notes
# column: count of unique notes at tick
# units:  integer >= 0
# started 2026-04-19T14:00:00Z by ~/bin/notes-u2-probe
2026-04-19T14:00:00Z	apple-notes	1423
```

Comments are also the right place to record the probe's intent: what
signal, what units, when it was started. The convergence reader skips
them, a human operator reads them.

## Atomicity

The reader re-reads the whole file on every tick, so partial writes are
cheap to handle but not free:

- **Always append a full line at a time.** `printf '%s\n' "$v" >> file`
  is safe on POSIX for lines under `PIPE_BUF` (4 KiB on Linux / macOS).
- **Do not truncate and rewrite.** A tick that hits the file mid-rewrite
  sees either the old file or the new one, both of which are valid —
  but the convergence metric will briefly see fewer samples and could
  stall on `min_samples`.
- **Do not use newline-less writes.** A row without a trailing `\n` is
  merged with the next row and the reader drops it as `MalformedRow`.

## Why "last column"?

Operator-facing logs usually follow the shape `<timestamp>\t<label>\t<value>`
— the value is the last field. Reading the last column makes the same
format work for both single- and multi-column probes, without forcing
every probe author to pick a fixed schema.

A future `metric = "colN"` selector on `[patrol.sunset]` can override
this default when a probe needs to track a non-terminal column.

## Example: u2 Apple Notes probe

The u2 probe counts unique Apple Notes titles every 5 minutes. It writes
to `~/.cosmon/probes/u2-notes.tsv`:

```tsv
# u2 — Apple Notes unique-count probe (idea-20260419-cb4e)
# columns: timestamp, label, unique_count
2026-04-19T14:00:00Z	apple-notes	1423
2026-04-19T14:05:00Z	apple-notes	1428
# …
2026-04-19T20:25:00Z	apple-notes	1502
2026-04-19T20:30:00Z	apple-notes	1502
```

The matching TOML declares a variance-threshold sunset: once the rolling
standard deviation over the last 20 samples squares to less than 0.02
(and we have at least 30 samples), the scheduler declares convergence,
runs `on_sunset` hooks, and sets `sunset_decided_at` so no more ticks
fire the probe:

```toml
[[patrol]]
name             = "u2-probe"
interval_seconds = 300
command          = ["/Users/you/bin/notes-u2-probe"]

[patrol.sunset]
strategy           = "variance-threshold"
sample_file        = "~/.cosmon/probes/u2-notes.tsv"
window             = 20
variance_threshold = 0.02
min_samples        = 30
on_sunset          = ["notify_telegram", "write_chronicle_stub"]
```

## Scheduler event schema

When the scheduler acts on a probe's sample file, it emits structured
NDJSON lines into
`<state_file>.events.jsonl` (sibling of `~/.cosmon/scheduler.state.json`).

| `kind`                         | When                                         | `detail` fields                 |
|--------------------------------|----------------------------------------------|---------------------------------|
| `patrol.sunsetted`             | convergence rule fires (once per lifetime)   | `reason`                        |
| `patrol.sunset_unload_failed`  | advisory `launchctl unload` failed           | `plist`, `error`                |
| `patrol.sunset_hook_failed`    | one of `on_sunset = [...]` hooks failed      | `hook`, `error`                 |

Every record also carries a top-level `ts` (RFC 3339 UTC) and `patrol`
(the patrol name). Readers that just want to display a line do not need
to interpret `detail` — it is typed per-kind so downstream tooling can
opt in.

Example stream after a probe sunsets:

```ndjson
{"ts":"2026-04-19T20:30:00Z","kind":"patrol.sunsetted","patrol":"u2-probe","detail":{"reason":"variance-threshold converged (σ² < 0.02, window=20, samples=78)"}}
{"ts":"2026-04-19T20:30:00Z","kind":"patrol.sunset_hook_failed","patrol":"u2-probe","detail":{"hook":"notify_telegram","error":"script exited non-zero: 2"}}
```

## `on_sunset` hooks

Declared in the TOML as a list of strings. Hook names are resolved at
dispatch time, not at parse time, so an unknown name only surfaces the
first time the sunset actually fires — the patrol stays valid.

| Hook name              | Feature flag (env var)             | Side-effect                                   |
|------------------------|------------------------------------|-----------------------------------------------|
| `notify_telegram`      | `COSMON_TELEGRAM_HOOK_SCRIPT`      | pipe an NDJSON envelope to the pointed script |
| `write_chronicle_stub` | `COSMON_CHRONICLE_FILE`            | append a Feynman-register line to the file    |
| `unload_launchd`       | (handled by `launchctl_plist`)     | no-op alias — kept for operator documentation |

All three are **no-ops when their flag env var is unset**, so the same
TOML can ship across machines without every host needing credentials.

## Failure modes you should never hit

- **Rewriting the file between ticks.** The reader assumes append-only.
  If you need a fresh campaign, point the probe at a new path.
- **Mixed units in the same file.** A single file is one series. Don't
  interleave `latency_ms` and `latency_s`.
- **Headers.** There are no headers. A header row will be read as a
  malformed data row (its last column typically does not parse as `f64`)
  and counted as a `MalformedRow` warning every tick until it rolls out
  of the window — confusing, noisy, and avoidable.
- **Blank samples.** If your probe has no datum this tick, write nothing,
  not a placeholder. `read_samples_tolerant` treats missing samples as
  "not converged yet", which is exactly right.

## Reference

- Rust types: `crates/cosmon-scheduler/src/convergence.rs`
  (`read_samples_tolerant`, `SampleRead`, `ConvergenceWarning`).
- TOML schema: `crates/cosmon-scheduler/src/config.rs` (`Sunset`,
  `SunsetStrategy`).
- Parent idea: `idea-20260419-cb4e` — probe-with-auto-sunset.
- Syzygie deliberation: `delib-20260419-d485`.
