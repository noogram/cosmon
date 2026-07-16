# session-to-spark — convert session notes into spark molecules

> **Sibling formula of `whisper-to-spark`.** Where whispers arrive from
> Matrix (humans or peers talking TO cosmon), session notes come from
> the operator's own carnet (`cs session note`). Both become typed
> `spark` molecules via the same §8j ingress pattern — different port,
> same spark target.

## Why

A `cs session note` is a **carnet entry** — sealed by BLAKE3, meant to
record thinking in flight. But sometimes a note *is* a real task: "!I
should implement X", "TODO Y", a spark the operator wants dispatched.

Before this feature, the operator had to copy the note body by hand and
re-type it as `cs spark "…"`. `session-to-spark` closes that gap — two
ways:

1. **Prefix syntactic**: start the note with `!spark ` and the
   LaunchAgent picks it up on the next tick (every 5 min).
2. **Explicit promotion**: `cs session promote <note_ts>` turns any
   note — prefixed or not — into a spark right away.

## Command surface

```sh
# default: promote every !spark-prefixed note in the open session
cs session promote

# promote one specific note (regardless of prefix)
cs session promote 10:46:55

# promote several notes at once
cs session promote 10:46:55 11:02:30

# target a non-open session explicitly
cs session promote --session session-2026-04-22T10-31-31Z 15:44:43

# dry-run — show what would be promoted, change nothing
cs session promote --dry-run

# fully-qualified session@ts pair (when you know the full handle)
cs session promote session-2026-04-22T10-31-31Z@15:44:43
```

All flags:

| Flag | Default | Purpose |
|---|---|---|
| `<NOTE_TS>…` | — | Positional. `HH:MM:SS` (bound to `--session`) or `<session_id>@HH:MM:SS`. Repeatable. |
| `--session <ID\|PATH>` | open session | Restrict to one session. If none is open, defaults to scanning every session. |
| `--all-spark-prefix` | auto-on when no `<NOTE_TS>` | Promote every `!spark `-prefixed note. |
| `--dry-run` | — | Print plan, don't nucleate and don't write sidecars. |
| `--json` | — | NDJSON stdout (one line per note + one `tick_complete`). |
| `--tick-script <PATH>` | walk-up | Override the `scripts/session-to-spark-tick.sh` location. |

## The `!spark ` prefix

Typing `cs session note "!spark implement session-to-spark"` produces
this carnet line:

```markdown
## 14:03:22 — 

!spark implement session-to-spark
```

The LaunchAgent (when installed) scans every session every 5 minutes,
picks up the prefix, and emits:

- one `spark` molecule (`kind = idea`, tagged `temp:hot`,
  `source:session`, `stream:session-to-spark`,
  `session-note:<sid>@<HH-MM-SS>`)
- one sidecar at `.cosmon/state/sessions/.promoted/<sid>/<HH-MM-SS>.md`
  recording the spark id, promoted_at, and nucleon

The spark lands at the top of `cs inbox` under HOT. The operator
dispatches it with `cs tackle`, retags it, or collapses it.

## Idempotence

The tick is safe to re-run at any cadence:

- A note is promoted **at most once**. The presence of a sidecar under
  `.cosmon/state/sessions/.promoted/<sid>/<ts>.md` is the dedup key.
- Sessions are BLAKE3-sealed (§8b). **We never mutate the session
  file** — the seal would silently invalidate. Sidecars are the trace.
- Re-running with no new `!spark` notes is cheap (~10 ms per session).

## nucleon_id propagation

Each spark carries a `nucleon_id` variable identifying the author. It
is derived from:

1. The session frontmatter's `operator` field (the primary source).
2. If `operator` is a plain username, it is composed with the host
   (`operator@hostname`) to match the whisper convention.
3. `git config user.email` (fallback when the frontmatter is missing).
4. `$USER@$(hostname)` (last resort).

This preserves the §8j discipline: every cosmon-typed artifact knows
the human it came from.

## LaunchAgent

Install the background promoter (macOS only, every 5 minutes):

```sh
scripts/install-session-to-spark-launchagent.sh install
scripts/install-session-to-spark-launchagent.sh status
scripts/install-session-to-spark-launchagent.sh uninstall
```

Logs land at `~/.cosmon/logs/session-to-spark.{out,err}` — the output
is the tick's NDJSON, one line per processed note. `tail -f` follows
the flow.

The agent runs `scripts/session-to-spark-tick.sh --all-spark-prefix
--json`. For targeted promotion (`cs session promote <ts>`), the
operator invokes the CLI directly — the agent does not get in the way.

## Mechanism

The heavy lifting lives in `scripts/session-to-spark-tick.sh`. The
tick:

1. Walks up from `$PWD` (or `--cosmon-root <DIR>`) to find
   `.cosmon/state/sessions/`.
2. Enumerates every `session-*.md` (or just the one named by
   `--session`).
3. Parses the frontmatter `operator` and the `## HH:MM:SS — tag` note
   headings in one awk pass. Notes emit as ASCII-US-separated records
   (bash's `read` collapses consecutive tabs — US is safer).
4. For each note: checks the selection rule (prefix mode and/or
   explicit list), checks the sidecar, strips the `!spark ` prefix if
   present, then `cs --json nucleate spark --var ... --tag ... --no-parent`.
5. Writes the sidecar. Emits NDJSON per note.

`cs session promote` is a thin Rust wrapper that forwards operator
intent (the open session, the named timestamps, `--dry-run`) to the
tick script. It resolves the tick's location via:

1. `--tick-script <PATH>` explicit override.
2. `$COSMON_REPO_ROOT/scripts/session-to-spark-tick.sh` env hint.
3. Walk-up from `$PWD` looking for `scripts/session-to-spark-tick.sh`.

## iOS

The ios-pilot `Session` tab renders a `sparkles` button next to each
recent note; tapping it calls the cs-api `/session/{id}/promote`
endpoint (not yet shipped — the mock client exercises the UI path; the
live HTTP client returns `notImplemented` until the endpoint lands).
tenant_auditor's primary path on iOS remains Blink Shell → SSH → `cs session
note "!spark …"` on the mac, picked up by the LaunchAgent within one
tick.

## Relationship to whisper-to-spark

Both formulas materialize into `spark` molecules and share the
mechanical skeleton (shell tick + LaunchAgent + sidecar idempotence).
They differ in the **ingress port**:

| Dimension | `whisper-to-spark` | `session-to-spark` |
|---|---|---|
| Ingress | Matrix E2E inbox | Session carnet |
| File location | `.cosmon/whispers/inbox/<room>/*.md` | `.cosmon/state/sessions/session-*.md` |
| Archive | `.cosmon/whispers/sparked/<room>/` | `.cosmon/state/sessions/.promoted/<sid>/` |
| Selection | every admitted whisper | `!spark ` prefix OR explicit list |
| Author | Matrix sender | session operator |
| Tags | `source:whisper`, `stream:matrix` | `source:session`, `stream:session-to-spark` |

Downstream queries like `cs ensemble --tag temp:hot` see both streams
side by side; `--tag source:session` or `--tag stream:matrix` filters
to one channel.

## Troubleshooting

- **"scripts/session-to-spark-tick.sh not found"** — `cs session
  promote` walks up from `$PWD`. Either `cd` into a cosmon-like repo,
  or pass `--tick-script` or set `$COSMON_REPO_ROOT`.
- **Notes repeat in `cs inbox`** — if the idempotence sidecar got
  lost (`rm -rf .cosmon/state/sessions/.promoted`), the tick will
  re-nucleate. Collapse the duplicates with `cs collapse <id> --reason
  duplicate`.
- **Nothing is promoted** — by default the tick only acts on `!spark`
  -prefixed notes. Either add the prefix (`cs session note "!spark
  …"`), pass explicit timestamps to `cs session promote`, or use
  `--all-spark-prefix` (no-op when you already have it).
- **Empty spark body** — a note containing only `!spark` with no
  text is rejected (`note_rejected`, reason `empty_spark_body`).
  Add a body.

## References

- Formula: [`.cosmon/formulas/session-to-spark.formula.toml`](../../.cosmon/formulas/session-to-spark.formula.toml)
- Tick: [`scripts/session-to-spark-tick.sh`](../../scripts/session-to-spark-tick.sh)
- LaunchAgent installer: [`scripts/install-session-to-spark-launchagent.sh`](../../scripts/install-session-to-spark-launchagent.sh)
- Plist template: [`scripts/launchd/dev.noogram.cosmon.session-to-spark.plist`](../../scripts/launchd/dev.noogram.cosmon.session-to-spark.plist)
- Sibling formula: [`docs/guides/whisper-to-spark.md`](whisper-to-spark.md) (if present)
- Architectural invariants: `docs/architectural-invariants.md` §8b (seal is a trace), §8j (ingress)
- ADR-061 (pilot-session + nucleon_id)
- Chronicle: an internal chronicle
