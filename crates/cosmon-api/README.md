# cosmon-api

Local HTTP adapter (`cs-api`) for the `cs session` CLI. Ships the tuyau
that native pilots (Mac menubar, iOS/iPad) use to open / annotate /
close an operator carnet without shelling out directly.

`cs-api` is **not** a cosmon runtime. It is a thin HTTP facade:
every request shells out to `cs session …`, and the filesystem under
`$COSMON_STATE_DIR/sessions/` (default `~/.cosmon/state/sessions/`)
remains the source of truth.

## Install

```sh
cargo build --release -p cosmon-api
# or from the workspace root:
cargo install --path crates/cosmon-api
```

## Usage

```sh
cs-api --help
cs-api                                 # bind 127.0.0.1:4222
cs-api --bind 127.0.0.1:4242           # alternate port
cs-api --cs-path /opt/cs/bin/cs        # non-standard cs path
cs-api --cosmon-state /path/.cosmon/state
cs-api --galaxies-root /Users/you/galaxies
cs-api --verbose                       # debug logging
```

### Flags

| Flag | Default | Purpose |
|------|---------|---------|
| `--bind <ADDR>` | `127.0.0.1:4222` | Socket to listen on. Use `0.0.0.0:4222` only behind Tailscale (see security below). |
| `--cs-path <PATH>` | `which cs` | Absolute path to the `cs` binary. |
| `--cosmon-state <PATH>` | inherit | Override `$COSMON_STATE_DIR` for child `cs` processes and for `/inbox` / `/whispers` scans. |
| `--whispers-inbox <PATH>` | `<cosmon-state parent>/whispers/inbox` | Override where `/whispers` reads markdown files from. |
| `--galaxies-root <PATH>` | `$HOME/galaxies` | Parent directory scanned by `/galaxies`. |
| `--verbose` / `-v` | off | Enable `debug`-level tracing. |
| `--version` | — | Print `cs-api` version and exit. |

## Endpoints

All responses are `Content-Type: application/json`.

### `GET /healthz`

```json
{"ok": true, "cs_binary": "/Users/you/.local/bin/cs", "version": "cs 0.1.0"}
```

### `POST /session/start`

Request body is optional and reserved for future `galaxy` / `root`
fields; an empty body is accepted:

```json
{"galaxy": "cosmon", "root": ["delib-20260422-f6d6"]}
```

Responses:

- `200 OK` → `{"session_id": "session-…", "galaxy": "cosmon", "started_at": "…", "path": "…"}`
- `409 Conflict` → `{"error": "session already open"}` (exit code 2 from `cs`)

### `POST /session/note`

Request body:

```json
{"text": "Torvalds elected path a", "tag": "insight"}
```

`tag` is optional. Responses:

- `200 OK` → `{"ok": true, "ts": "2026-04-22T14:30:05Z"}`
- `409 Conflict` → `{"error": "no session open"}` (exit code 3)
- `400 Bad Request` → `{"error": "note text is empty"}`

### `POST /session/end`

Empty body. Responses:

- `200 OK` → `{"seal": "blake3:<hex>", "note_count": N, "session_id": "…", "ended_at": "…"}`
- `409 Conflict` → `{"error": "no session open"}`

### `GET /session/current`

Read-only view of the open carnet, parsed from the session file on
disk (no shell-out). Response:

```json
{
  "session_id": "session-2026-04-22T10-59-04Z",
  "notes": [
    {"ts": "10:59:35", "text": "first thought", "tag": null},
    {"ts": "11:02:14", "text": "follow-up",    "tag": "insight"}
  ]
}
```

When no session is open: `{"session_id": null, "notes": []}`.

### `GET /whispers?limit=50`

List the newest whispers under `.cosmon/whispers/inbox/` as deposited by
`cosmon-matrix-tick` (ADR-064). `limit` is clamped to `[1, 500]`
(default 50). Response:

```json
{
  "whispers": [
    {
      "id": "1776891587880-_H27kQ...",
      "room_id": "!room:matrix.org",
      "sender_nucleon_id": "you",
      "sender_mxid": "@you:matrix.org",
      "received_at": "2026-04-22T21:32:37Z",
      "body": "Salut 👋",
      "path": "/Users/.../inbox/_room_matrix.org/1776891587880-....md"
    }
  ]
}
```

When the inbox directory does not yet exist: `{"whispers": []}` with a
`200 OK`. The handler is read-only (no shell-out).

### `POST /whispers/{id}/archive`

Move `<inbox>/<room>/<id>.md` to `<archived>/<room>/<id>.md` (creating
the archived room directory on demand). Empty body. Responses:

- `200 OK` → `{"ok": true, "id": "<id>", "archived_path": "…"}`
- `404 Not Found` → `{"error": "whisper '<id>' not found under …"}`

### `POST /whispers/{id}/spark`

Promote a whisper into an `idea` molecule by shelling out to
`cs spark` (ADR-061). UI-facing only — not the in-loop whisper port of
ADR-038. Optional body:

```json
{"text": "override the spark text", "nucleon": "tenant_auditor@noogram.example"}
```

When both fields are omitted the text defaults to the whisper body and
the nucleon to its `sender_nucleon_id`. Responses:

- `200 OK` → `{"ok": true, "whisper_id": "<id>", "spark": {"id": "spark-…", …}}`
- `400 Bad Request` → `{"error": "whisper body is empty — refusing to spark an empty molecule"}`
- `404 Not Found` → `{"error": "whisper '<id>' not found under …"}`

### `GET /inbox?status=pending,running`

List molecules across every fleet on disk. The handler reads
`<state>/fleets/*/molecules/*/state.json` directly (no shell-out). The
default `status` filter is `pending,running`; pass `status=all` (or an
empty value) to return every molecule. Optional `limit` caps the list.

```json
{
  "molecules": [
    {
      "id": "task-20260422-db9f",
      "kind": "task",
      "status": "running",
      "topic": "cs-api v1 — ajouter 3 endpoints HTTP …",
      "tags": ["temp:hot"],
      "created_at": "2026-04-22T21:44:36Z",
      "updated_at": "2026-04-22T21:47:33Z",
      "formula": "task-work",
      "assigned_worker": "cs-api-v1-ajouter-3-db9f"
    }
  ]
}
```

The `kind` field is derived from the molecule id prefix
(`task-` → `task`, `delib-` → `deliberation`, `const-` →
`constellation`, `spark-` → `spark`, …).

### `GET /galaxies`

List every `.cosmon/`-bearing directory under `--galaxies-root`
(default `$HOME/galaxies`). Each entry reports pending/running counts
and the most recent `updated_at` seen across its fleets.

```json
{
  "galaxies_root": "/Users/you/galaxies",
  "galaxies": [
    {
      "name": "cosmon",
      "path": "/srv/cosmon/cosmon",
      "pending_count": 12,
      "running_count": 3,
      "last_activity": "2026-04-22T21:32:00Z"
    }
  ]
}
```

## Security (v0)

**Three invariants apply — read them before changing `--bind`.**

1. **Loopback by default.** The binary binds `127.0.0.1:4222` unless
   explicitly told otherwise. No other machine can reach `cs-api`
   in that mode.
2. **No auth.** v0 does not check a bearer token, API key, or origin
   header. If you bind anything other than loopback, you MUST put a
   network boundary in front of it. **The only supported non-loopback
   deployment for v0 is behind Tailscale** (`cs-api --bind
   100.x.x.x:4222` on a tailnet). Do **not** expose this daemon on a
   public IP, and do **not** configure router port-forwarding to it.
3. **CORS is permissive** (`Access-Control-Allow-Origin: *`). Harmless
   on loopback; on a tailnet it means any machine the daemon trusts
   (see #2) can also be hit from a browser. Plan accordingly.

### v1 plan

- Bearer token auth (`--token-file` or `$COSMON_API_TOKEN`).
- Origin pinning (`--allow-origin <URL>`), so CORS stops being `*`
  when the daemon is not bound to loopback.
- Tailscale auto-discovery via `neurion` (so iOS apps do not need
  manual IP entry).

## Running as a LaunchAgent

A template plist lives at `../../docs/guides/cs-api.launchd.plist`.
Copy it to `~/Library/LaunchAgents/dev.noogram.cs-api.plist`, edit the
binary path, and `launchctl bootstrap gui/$(id -u) …` it.

See [docs/guides/cs-api.md](../../docs/guides/cs-api.md) for the full
guide (LaunchAgent bootstrap, `curl` recipes, port-occupied
troubleshooting).

## Testing

```sh
cargo test -p cosmon-api
```

Each integration test spawns `cs-api` against a scratch
`$COSMON_STATE_DIR` tempdir, so nothing leaks into your real
`~/.cosmon/state/sessions/`.

## Scope guards

The v0 + v1 surface deliberately omits, per the molecule specs:

- No bearer token auth (lands in a follow-up — see [v1 plan](#v1-plan)).
- No WebSocket — pilots poll `/session/current` / `/whispers` / `/inbox`.
- No auto-install as a LaunchAgent; operator places the `.plist` by hand.
- No Tailscale discovery; operator configures the IP manually.
- `POST /whispers/{id}/spark` is a UI-facing promotion, **not** the
  in-loop whisper port of ADR-038. The daemon never pokes a live
  worker — it only shells out to `cs spark`, exactly what the operator
  would type.

Each of these moves to v1 once the v0 pattern is validated by the
pilot apps.
