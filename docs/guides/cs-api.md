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
| GET    | `/motion?window=15m`            | `{timestamp, window, galaxies_scanned, workers, running_molecules, recent_git_commits, recent_whispers, recent_sparks}` — see [motion-view.md](motion-view.md) |
| GET    | `/ensemble?scope=local`         | `{scope, galaxies: [{name, workers, molecule_groups, …}, …], totals}` |
| GET    | `/peek?scale=city\|building\|skin` | `{scale, focus, text}` — monospace snapshot (see [cluster-views](cluster-views.md)) |

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

## Security invariants

Read these before changing `--bind`.

**`cs-api` has no authentication, and some of its routes execute.**
`POST /molecules/{id}/tackle` spawns a worker — it runs agent code and
spends your credit. Every request that reaches the socket carries your
authority, because there is nothing else for it to carry. The bind
address is therefore not a preference: it is the only access-control
boundary the process has, and the binary enforces it rather than
documenting it.

1. **Loopback by default** (`127.0.0.1:4222`). Unreachable from other
   machines.
2. **`0.0.0.0` / `::` is refused**, always, with no flag to override
   it. It names every interface the host has now or acquires later, so
   the exposure cannot be determined — and where you cannot verify, do
   not claim. Name one concrete interface instead.
3. **A routable address requires an explicit gesture.** The daemon
   refuses to start on a non-loopback address unless you pass
   `--i-know-this-exposes-an-unauthenticated-api`. The flag is long on
   purpose; its `--help` text says what it opens.
4. **No CORS by default.** The Mac and iOS pilots are native clients
   and never send an `Origin`, so CORS buys them nothing; it only
   decides which *web pages* may drive this daemon. Name origins
   explicitly with `--allow-web-origin <ORIGIN>` (repeatable, exact
   match, no wildcard — `*` is refused by name).

The *only* supported non-loopback deployment is **behind Tailscale**:

```sh
cs-api --bind "$(tailscale ip -4):4222" \
       --i-know-this-exposes-an-unauthenticated-api
```

so only your tailnet peers can reach it. Check with `tailscale status`
that the tailnet holds only your own devices. Do not expose a public
IP; do not configure router port-forwarding to `cs-api`; and prefer
starting it when you pilot over leaving it in a LaunchAgent that opens
the port at every login.

### The gap that is deliberately still open

There is no authentication and this molecule did not add one. The
shape has been decided — `delib-20260727-f9ee`, five seats of five: a
**boot-minted seal** extending
[`admin_seal`](../../crates/cosmon-rpp-adapter/src/admin_seal.rs), a
secret minted at start, printed once, held only as a BLAKE3 digest,
where the absence of the credential *is* the closed state — and
explicitly **not** an ad-hoc token and **not** a
trust-whoever-reached-loopback posture. Cosmon's own client code
already refuses the latter: the OAuth redirect catcher in
[`cosmon-remote`](../../crates/cosmon-remote/src/oidc/loopback.rs)
binds loopback *and* demands a high-entropy `state` nonce, because any
open page can `fetch` a loopback socket. Until the seal lands, the bind
address is the whole boundary. See `docs/architectural-invariants.md`
§8z.

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
