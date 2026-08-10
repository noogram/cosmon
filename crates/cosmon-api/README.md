# cosmon-api

Local HTTP adapter (`cs-api`) for the `cs journal` CLI. Ships the tuyau
that native pilots (Mac menubar, iOS/iPad) use to open / annotate /
close an operator carnet without shelling out directly.

`cs-api` is **not** a cosmon runtime. It is a thin HTTP facade:
every request shells out to `cs journal …`, and the filesystem under
`$COSMON_STATE_DIR/journals/` (default `~/.cosmon/state/journals/`)
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
| `--bind <ADDR>` | `127.0.0.1:4222` | Socket to listen on. `0.0.0.0` / `::` is always refused; any other non-loopback address needs the flag below. |
| `--i-know-this-exposes-an-unauthenticated-api` | off | Consent to a bind other machines can reach. There is no auth: whoever routes there can spawn workers. See [Security](#security). |
| `--allow-web-origin <ORIGIN>` | none | Allow one browser origin (repeatable, exact match, no wildcard). Native pilots do not need this. |
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

## Security

**`cs-api` has no authentication, and some of its routes execute.**
`POST /molecules/{id}/tackle` spawns a worker: it runs agent code and
spends the operator's credit. `POST /whispers/{id}/spark`,
`POST /whisper/{mol_id}`, `POST /molecules/{id}/tag` and the
`POST /session/*` routes all write. Every request that reaches the
socket carries the operator's full authority, because there is nothing
else for it to carry.

The bind address is therefore the entire access-control boundary, and
`src/bind.rs` enforces it as a value the binary must construct before
it can listen:

1. **Loopback by default.** `127.0.0.1:4222`. No other machine can
   reach it.
2. **`0.0.0.0` / `::` is refused** — with or without the consent flag.
   It does not name a network; it names every interface the host has
   now or acquires later, so the exposure cannot be determined, and we
   fail closed rather than assume. Same refusal, same reason, as
   [`apps-transport-http`](../apps-transport-http/src/bind.rs).
3. **A concrete routable address requires
   `--i-know-this-exposes-an-unauthenticated-api`.** The only
   supported such deployment is a Tailscale address
   (`cs-api --bind "$(tailscale ip -4):4222" --i-know-…`). Never a
   public IP, never router port-forwarding.
4. **No CORS by default.** The Mac and iOS pilots are native clients
   that never send an `Origin` — CORS decides what *web pages* may do,
   and a wildcard on an unauthenticated executing surface lets any page
   the operator visits drive the daemon. `--allow-web-origin <ORIGIN>`
   (repeatable) names origins explicitly; they are matched exactly and
   echoed back, and `*` is refused by name.

Note what a warning would *not* have bought here: the operator who
typed the flag is not reading the log. The refusals are exits, not
lines in a file.

### The gap that is deliberately still open

Authentication. It is not missing by oversight and it is not
under-specified: `delib-20260727-f9ee` concluded, five seats of five,
that the right shape is a **boot-minted seal** extending
[`admin_seal`](../cosmon-rpp-adapter/src/admin_seal.rs) — a secret
minted at container/daemon start, printed once, held only as a BLAKE3
digest, compared in constant time, where `None` is simultaneously "no
credential" and "closed" so that "enabled but unprotected" cannot be
written down. It is explicitly **not** an ad-hoc token scheme and
**not** a "the caller reached loopback, so it must be the operator"
posture — cosmon's own client code already refuses that: the two
`is_loopback` call sites in the tree
([`cosmon-remote/src/oidc/loopback.rs`](../cosmon-remote/src/oidc/loopback.rs))
gate an OAuth redirect catcher that *also* demands a high-entropy
`state` nonce, precisely because any open web page can `fetch` a
loopback socket.

Two further items belong to the next molecule, not to this one:

- **`cosmon-rpp-adapter`'s unauthenticated `auth_claude` surface is
  credential-*writing*, not read-only.** Verified in this tree:
  `POST /v1/auth/claude/confirm` (`src/auth_claude/routes.rs`) takes
  only `State` and `Json` — no bearer is extracted anywhere on the path
  — and on success calls `write_credentials_file`, which writes
  `~/.claude/.credentials.json`. An unauthenticated caller who can
  reach that route can therefore plant an `(access_token,
  refresh_token)` pair, after which workers in that fleet run against
  an account they chose. Mitigating fact, also verified: the surface is
  inert unless the operator wired it — the handlers return
  `503 service_unavailable` when `AppState::auth_claude` is `None`.
  This is more severe than any read exposure and must be closed on its
  own merits.
- **Tailscale-address auto-discovery** via `neurion`, so the iOS app
  need not be handed an IP by hand.

See `docs/architectural-invariants.md` §8z for the invariant this
change ratifies.

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
`~/.cosmon/state/journals/`.

## Scope guards

The v0 + v1 surface deliberately omits, per the molecule specs:

- No bearer token auth — see
  [the gap that is deliberately still open](#the-gap-that-is-deliberately-still-open)
  for the decided shape (a boot-minted seal) and why the bind address
  carries the boundary until it lands.
- No WebSocket — pilots poll `/session/current` / `/whispers` / `/inbox`.
- No auto-install as a LaunchAgent; operator places the `.plist` by hand.
- No Tailscale discovery; operator configures the IP manually.
- `POST /whispers/{id}/spark` is a UI-facing promotion, **not** the
  in-loop whisper port of ADR-038. The daemon never pokes a live
  worker — it only shells out to `cs spark`, exactly what the operator
  would type.

Each of these moves to v1 once the v0 pattern is validated by the
pilot apps.
