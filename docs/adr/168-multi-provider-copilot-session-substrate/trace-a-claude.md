# Trace A — Claude Code session (anonymised envelope)

Observed 2026-08-01, read-only. Source layout: `~/.claude/projects/<sanitised-cwd>/<session-uuid>.jsonl`.

Content (prompts, assistant text, tool payloads) is **not** reproduced: only the
envelope fields the co-pilot protocol would have to key on. Identifiers are
replaced by stable salted pseudonyms; the home prefix is `<HOME>`.

## Record-type histogram (n=135 lines)

| `type` | count |
|---|---|
| `assistant` | 50 |
| `user` | 31 |
| `last-prompt` | 11 |
| `mode` | 11 |
| `permission-mode` | 11 |
| `attachment` | 10 |
| `ai-title` | 10 |
| `file-history-snapshot` | 1 |

## Top-level key union

```text
aiTitle
attachment
cwd
effort
entrypoint
gitBranch
isSidechain
isSnapshotUpdate
lastPrompt
leafUuid
message
messageId
mode
origin
parentUuid
permissionMode
promptId
promptSource
requestId
sessionId
session_id
snapshot
sourceToolAssistantUUID
timestamp
toolUseResult
type
userType
uuid
version
```

## Envelope of one `assistant` record (content removed)

```json
{
  "type": "assistant",
  "uuid": "<uuid-7710d26c>",
  "parentUuid": "<uuid-7dcc7796>",
  "sessionId": "<claude-session-4940f28e>",
  "cwd": "<HOME>/galaxies/cosmon/.worktrees/task-20260731-0561",
  "gitBranch": "feat/task-20260731-0561",
  "version": "2.1.220",
  "requestId": "<req-aa1f56f3>",
  "timestamp": "2026-08-01T00:06:35.485Z",
  "message.model": "claude-opus-5",
  "message.usage": {
    "input_tokens": "<int>",
    "cache_creation_input_tokens": "<int>",
    "cache_read_input_tokens": "<int>",
    "output_tokens": "<int>",
    "service_tier": "standard"
  }
}
```

## Quota telemetry

No periodic quota record exists in this trace. A scan of the 40 most
recently modified Claude session logs on this host found `rateLimits` on **102 lines, all of them inside
`type:"system", subtype:"api_error"` records**, and `null` in every sampled case.
Claude publishes a limit only as the error that already happened.

## What the envelope does and does not give the protocol

- **Gives:** stable `sessionId` (filename *and* in every record), exact `cwd`,
  `gitBranch`, per-turn token usage, a `parentUuid` chain that orders the turns.
- **Does not give:** provider name, role, followed session, mission id,
  checkpoint, or any proactive quota signal.
