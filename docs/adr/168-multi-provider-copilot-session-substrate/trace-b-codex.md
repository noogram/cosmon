# Trace B — Codex session (anonymised envelope)

Observed 2026-08-01, read-only. Source layout:
`~/.codex/sessions/<YYYY>/<MM>/<DD>/rollout-<ISO8601>-<session-uuid>.jsonl`.

Same redaction rule as Trace A.

## Record-type histogram (n=8440 lines)

| `type` | count |
|---|---|
| `response_item` | 4631 |
| `event_msg` | 3434 |
| `turn_context` | 317 |
| `world_state` | 38 |
| `compacted` | 19 |
| `session_meta` | 1 |

## `event_msg` payload subtypes

| `payload.type` | count |
|---|---|
| `token_count` | 1451 |
| `agent_message` | 690 |
| `user_message` | 311 |
| `task_started` | 306 |
| `task_complete` | 299 |
| `thread_settings_applied` | 298 |
| `web_search_end` | 38 |
| `context_compacted` | 19 |
| `patch_apply_end` | 11 |
| `turn_aborted` | 7 |
| `mcp_tool_call_end` | 4 |

## `response_item` payload subtypes

| `payload.type` | count |
|---|---|
| `reasoning` | 1320 |
| `message` | 1061 |
| `custom_tool_call` | 1029 |
| `custom_tool_call_output` | 1029 |
| `function_call` | 96 |
| `function_call_output` | 96 |

## `session_meta` envelope (values redacted)

```json
{
  "session_id": "<codex-session-71e49444>",
  "id": "<codex-thread-6d4e2172>",
  "cwd": "<HOME>/galaxies/cosmon",
  "originator": "codex-tui",
  "source": "cli",
  "cli_version": "0.145.0",
  "model_provider": "openai",
  "context_window": {
    "window_id": "<window-id>"
  },
  "history_mode": "legacy",
  "git": {
    "branch": "main",
    "commit_hash": "<commit-944797d8>",
    "repository_url": "<remote-e428058f>"
  }
}
```

## Quota telemetry — the asymmetry that decides M4

Codex emits `rate_limits` inside **every** `token_count` event
(1451 of them in this single session), unprompted:

```json
{
  "primary": {
    "used_percent": "<pct>",
    "window_minutes": 10080,
    "resets_at": "<epoch>"
  },
  "plan_type": "<plan>",
  "limit_id": "codex",
  "rate_limit_reached_type": null
}
```

`used_percent`, `window_minutes` and `resets_at` are present on the **co-pilot**
side. They are absent on the **primary** side (Trace A). The signal the mission
wants to trigger a takeover is published by the session that would receive the
authority, not by the session that would lose it.

## What the envelope does and does not give the protocol

- **Gives:** stable `session_id`, exact `cwd`, `workspace_roots`, git branch and
  commit, model, sandbox/approval policy per turn, cumulative token usage,
  proactive quota telemetry, explicit `compacted` records.
- **Does not give:** role, followed session, mission id, checkpoint — the same
  four gaps as Claude.
