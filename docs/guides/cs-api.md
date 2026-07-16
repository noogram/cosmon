# `cs-api` — local HTTP adapter for native pilots

`cs-api` is a tiny daemon that exposes `cs session start|note|end`
over HTTP. Native apps (Mac menubar, iOS/iPad, a tablet hooked on
your home WiFi) talk to it instead of shelling out to `cs` directly.
Every request is a shell-out to the real `cs`; the filesystem remains
the source of truth.

This guide is the operator's runbook: how to start it, run it as a
LaunchAgent, exercise every endpoint with `curl`, and recover when it
misbehaves.

## TL;DR

```sh
cargo install --path crates/cosmon-api
cs-api --bind 127.0.0.1:4222 &
curl -s http://localhost:4222/healthz | jq
```

Output:

```json
{"ok": true, "cs_binary": "/Users/you/.local/bin/cs", "version": "cs 0.1.0"}
```

## Starting a carnet from curl

```sh
# Open
curl -s -X POST http://localhost:4222/session/start | jq
# → {"session_id": "session-2026-04-22T14-30-05Z", "galaxy": null, "started_at": "…", "path": "…"}

# Annotate
curl -s -X POST http://localhost:4222/session/note \
  -H 'Content-Type: application/json' \
  -d '{"text": "first thought", "tag": "insight"}' | jq
# → {"ok": true, "ts": "…"}

# Inspect live
curl -s http://localhost:4222/session/current | jq

# Seal
curl -s -X POST http://localhost:4222/session/end | jq
# → {"seal": "blake3:<hex>", "note_count": 1, …}
```

## Endpoints at a glance

Session endpoints (v0):

| Method | Path               | Returns / error                                                  |
|--------|--------------------|------------------------------------------------------------------|
| GET    | `/healthz`         | `{ok, cs_binary, version}`                                       |
| POST   | `/session/start`   | `{session_id, galaxy, started_at, path}` or **409** already open |
| POST   | `/session/note`    | `{ok: true, ts}` or **409** no session open / **400** empty text |
| POST   | `/session/end`     | `{seal, note_count, session_id, ended_at}` or **409**            |
| GET    | `/session/current` | `{session_id, notes[]}` — `null` + `[]` when none open           |

Inbox / whispers / galaxies (v1):

| Method | Path                            | Returns / error                                                       |
|--------|----------------------------------|-----------------------------------------------------------------------|
| GET    | `/whispers?limit=50`            | `{whispers: [{id, room_id, sender_*, received_at, body, path}, …]}`   |
| POST   | `/whispers/{id}/archive`        | `{ok, id, archived_path}` or **404** not found                        |
| POST   | `/whispers/{id}/spark`          | `{ok, whisper_id, spark: {id, …}}` or **404** / **400** empty body    |
| GET    | `/inbox?status=pending,running` | `{molecules: [{id, kind, status, topic, tags, created_at, …}, …]}`    |
| GET    | `/galaxies`                     | `{galaxies_root, galaxies: [{name, path, pending_count, …}, …]}`      |
<<<<<<< HEAD
| GET    | `/motion?window=15m`            | `{timestamp, window, galaxies_scanned, workers, running_molecules, recent_git_commits, recent_whispers, recent_sparks}` — see [motion-view.md](motion-view.md) |
=======
| GET    | `/ensemble?scope=local`         | `{scope, galaxies: [{name, workers, molecule_groups, …}, …], totals}` |
| GET    | `/peek?scale=city\|building\|skin` | `{scale, focus, text}` — monospace snapshot (see [cluster-views](cluster-views.md)) |
>>>>>>> feat/task-20260423-d3ae

### Exercising the v1 endpoints

```sh
# All unprocessed matrix whispers, newest first
curl -s 'http://localhost:4222/whispers?limit=20' | jq

# Archive one (moves it from inbox/<room>/<id>.md to archived/<room>/<id>.md)
curl -s -X POST http://localhost:4222/whispers/1776891587880-_H27kQ.../archive | jq

# Promote a whisper into an idea molecule — text + nucleon default to the
# whisper's own body + sender_nucleon_id.
curl -s -X POST http://localhost:4222/whispers/1776891587880-_H27kQ.../spark \
  -H 'Content-Type: application/json' -d '{}' | jq

# Pending/running molecules across every fleet under $COSMON_STATE_DIR
curl -s http://localhost:4222/inbox | jq

# Every .cosmon/-bearing project under --galaxies-root
curl -s http://localhost:4222/galaxies | jq

# Cluster-wide state dump — workers + molecules grouped by status, per galaxy
curl -s 'http://localhost:4222/ensemble?scope=local' | jq
curl -s 'http://localhost:4222/ensemble?galaxies=cosmon,mailroom&statuses=running' | jq

# Monospaced three-scale snapshot — the Mac/iOS Peek pane renders this verbatim
curl -s 'http://localhost:4222/peek?scale=city'
curl -s 'http://localhost:4222/peek?scale=skin&focus=task-20260423-d3ae' | jq -r .text
```

The `/ensemble` and `/peek` endpoints are the HTTP surface behind the
Cluster tab in the Mac / iOS pilots. See
[cluster-views.md](cluster-views.md) for the full Motion / Ensemble /
Peek story and query-parameter reference.

### Scoping the scans

`cs-api` resolves file roots at request time:

- `/whispers` reads from `--whispers-inbox` if set, else
  `<cosmon-state parent>/whispers/inbox` (the `cosmon-matrix-tick`
  layout).
- `/inbox` reads `<cosmon-state>/fleets/*/molecules/*/state.json`.
- `/galaxies` lists top-level children of `--galaxies-root` (default
  `$HOME/galaxies`) that carry a `.cosmon/` directory.

When no flag is passed, the child `cs` binary inherits the server's
environment — so you can either run `cs-api` from the project root
(walk-up finds `.cosmon/`) or set `COSMON_STATE_DIR` in the
LaunchAgent's `EnvironmentVariables`.

## Security invariants (v0)

Read these before changing `--bind`.

1. **Loopback by default** (`127.0.0.1:4222`). Unreachable from other
   machines.
2. **No auth.** Bearer token lands in v1.
3. **Permissive CORS.** Any browser origin can call the API.

The *only* supported non-loopback deployment for v0 is **behind
Tailscale**. Run `cs-api --bind 100.x.x.x:4222` on a tailnet address,
so only authenticated tailnet peers can reach it. Do not expose a
public IP; do not configure router port-forwarding to `cs-api`.

## LaunchAgent (macOS)

Copy the template below to `~/Library/LaunchAgents/dev.noogram.cs-api.plist`,
edit `ProgramArguments[0]` to your absolute `cs-api` path, then:

```sh
launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/dev.noogram.cs-api.plist
launchctl kickstart -k gui/$(id -u)/dev.noogram.cs-api
```

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN"
  "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>dev.noogram.cs-api</string>
    <key>ProgramArguments</key>
    <array>
        <string>/Users/YOU/.cargo/bin/cs-api</string>
        <string>--bind</string>
        <string>127.0.0.1:4222</string>
    </array>
    <key>EnvironmentVariables</key>
    <dict>
        <key>PATH</key>
        <string>/Users/YOU/.cargo/bin:/Users/YOU/.local/bin:/usr/local/bin:/usr/bin:/bin</string>
    </dict>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
    <key>StandardOutPath</key>
    <string>/tmp/cs-api.out.log</string>
    <key>StandardErrorPath</key>
    <string>/tmp/cs-api.err.log</string>
</dict>
</plist>
```

To unload: `launchctl bootout gui/$(id -u)/dev.noogram.cs-api`.

## Troubleshooting

### Port already in use

```
Error: Address already in use (os error 48)
```

Find who owns it and kill or rebind:

```sh
lsof -iTCP:4222 -sTCP:LISTEN
cs-api --bind 127.0.0.1:4242
```

### `cs` not found

`cs-api` needs `cs` on `$PATH`, or an explicit `--cs-path`. Under a
LaunchAgent the environment is pristine — set `PATH` in
`EnvironmentVariables` (see the plist above) or pass the absolute path:

```sh
cs-api --cs-path /Users/you/.local/bin/cs
```

### Empty `version` on `/healthz`

Means `cs --version` printed something unexpected or nothing at all.
Confirm with `cs --version` from the same shell. If that fails, the
binary on `$PATH` is the problem, not `cs-api`.

## Scope guards (what v0 + v1 explicitly do not do)

- No bearer-token auth (lands in a follow-up molecule).
- No WebSocket — pilots poll `/session/current` / `/whispers` / `/inbox`.
- No auto-install as a LaunchAgent — operator places the plist.
- No Tailscale auto-discovery — operator configures the IP.
- `/whispers/{id}/spark` is a UI-facing promotion (exactly what the
  operator would type: `cs spark <body>`). It is **not** the in-loop
  whisper port of ADR-038 — no live worker gets poked, the handler
  just writes a new molecule to disk via the CLI.

Each of these is a real item on the roadmap, triggered by real pilot
feedback rather than speculation.
