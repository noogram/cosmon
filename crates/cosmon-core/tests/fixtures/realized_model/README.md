# Golden fixtures — realized-model capture (F-04)

Real captured session-log lines, anonymized, for the realized-model parsers in
`cosmon_core::model_realization`. Unlike synthetic `concat!` strings, these
carry the full field surface the producers actually write, so a schema drift
in either producer breaks the golden test instead of passing silently.

## Provenance

| File | Producer | Producer version | Captured | Anonymization |
|------|----------|------------------|----------|---------------|
| `claude-session.jsonl` | Claude Code session log (`~/.claude/projects/{proj}/{session}.jsonl`) | 2.1.195 | 2026-06-29 (extracted 2026-07-18) | `cwd` → `/work/tree`; `sessionId`/`uuid`/`parentUuid`/`requestId`/message `id` zeroed; `gitBranch` → `feat/example`; message `content` → `REDACTED`; `diagnostics` emptied. All other fields verbatim. |
| `codex-session.jsonl` | codex CLI rollout log (`~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl`) | codex_cli_rs 0.36.0 | 2026-02-14 (extracted 2026-07-18) | `payload.cwd` → `/work/tree`; `payload.id` zeroed; `git`/`instructions` redacted. All other fields verbatim. |

Anonymization procedure: mechanical key-replacement on the captured JSON
(`cwd`, ids, branch, free-text content); no structural key added, removed, or
reordered — the parsers must digest the real shape.

## Coverage notes

- `claude-session.jsonl` deliberately includes a **real non-`init` `system`
  line** (`subtype: "turn_duration"`) — the round-3 discriminant regression:
  only `subtype == "init"` system lines may contribute a realized model.
  Claude Code *session* logs (this producer) do not record a `system`/`init`
  line — that shape belongs to the `--output-format stream-json` stream; the
  init-line coverage therefore stays in the unit tests of
  `model_realization.rs` until a stream capture is added here.
- `codex-session.jsonl` includes the real `session_meta` line **without** a
  `model` key — the config-vs-realization regression: only `turn_context`
  (`payload.model`) names what ran.

Consumed by `crates/cosmon-core/tests/realized_model_golden.rs`.
