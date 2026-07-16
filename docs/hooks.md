# Cosmon Hooks

Hooks are external scripts triggered by Cosmon events. They receive event
JSON on stdin (one NDJSON line per event) and perform side-effects like
sending notifications.

## Telegram Outbound Hook

`hooks/telegram-notify.sh` sends Cosmon event notifications to a Telegram
chat via the [Bot API](https://core.telegram.org/bots/api).

### Prerequisites

1. Create a Telegram bot via [@BotFather](https://t.me/BotFather) and note
   the bot token.
2. Get the target chat ID (use [@userinfobot](https://t.me/userinfobot) or
   the `getUpdates` API method).
3. Ensure `jq` and `curl` are available on your system.

### Configuration

Set these environment variables:

| Variable | Required | Description |
|----------|----------|-------------|
| `TELEGRAM_BOT_TOKEN` | Yes | Bot API token from @BotFather |
| `TELEGRAM_CHAT_ID` | Yes | Target chat/group/channel ID |
| `TELEGRAM_PARSE_MODE` | No | Message format (default: `HTML`) |
| `TELEGRAM_SILENT` | No | Set to `1` for silent notifications |
| `COSMON_HOOK_FILTER` | No | Comma-separated event kinds to forward |

### Usage

Pipe Cosmon event output directly into the script:

```bash
# Single event
echo '{"kind":"worker_spawned","worker_id":"quartz","agent":"polecat"}' \
  | ./hooks/telegram-notify.sh

# Stream events from a molecule
cs observe --follow --json mol-abc | ./hooks/telegram-notify.sh

# Filter to only errors and terminations
COSMON_HOOK_FILTER="error_occurred,worker_terminated" \
  cs observe --follow --json mol-abc | ./hooks/telegram-notify.sh
```

### Supported Event Kinds

All Cosmon event kinds are supported:

- `worker_spawned` — worker process created
- `worker_terminated` — worker process stopped
- `molecule_dispatched` — molecule assigned to a worker
- `molecule_transitioned` — molecule lifecycle status change
- `step_completed` — molecule step finished
- `task_dispatched` — task sent to an agent
- `error_occurred` — error during an operation

Unknown event kinds are forwarded as raw JSON in a `<pre>` block.

### Exit Codes

| Code | Meaning |
|------|---------|
| 0 | All events sent (or no matching events) |
| 1 | Missing required environment variable |
| 2 | Telegram API request failed |
