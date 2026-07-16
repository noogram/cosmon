# Cosmon-app

**Sentir le réacteur depuis le canapé.**

Cosmon-app is the third native app of the local cluster (after Verdict and
Mur du Matin). It is a SwiftUI **universal iOS** target (iPhone + iPad)
that talks to **`cosmon-daemon`** over **HTTP-on-Tailscale** (port 8790).
The app is read-only in v1: Galaxies, Molecules, Fleets, drill-down on a
single molecule (briefing + log tail + tmux attach hint).

```
cosmon-daemon (Rust, axum, port 8790)
   ↑ HTTP/JSON over Tailscale (apps-transport-http)
CosmonApp.iOS  (SwiftUI, CosmonAppKit + AppsTransportHTTP)
```

## What it shows

- **Galaxies tab** — one row per `/srv/cosmon/<g>/` with a `.cosmon/state/`
  directory; running and pending counts.
- **Tap into a galaxy** → molecule list, status filter chips
  (running / pending / completed / collapsed / all), sorted by
  `updated_at` desc.
- **Tap into a molecule** → header (kind/status/step), briefing markdown,
  log tail (8 KiB), tmux attach hint when a worker is assigned.
- **Fleets tab** — worker count and (optional) attention budget per galaxy.

Polling: every 5 s while the app is foreground; paused on
`scenePhase == .background`. Pull-to-refresh triggers an immediate poll.
Push (SSE) is **v1.1**; write actions (tackle/done/collapse) are **v1.1**;
APNs notifications are **v1.2**.

## Install

### Run cosmon-daemon (one time)

The release binary is installed into `~/.local/bin/cosmon-daemon` by
`cargo build --release -p cosmon-daemon` + `cp` (see the workspace
verification section in the molecule briefing).

To run it manually:

```bash
~/.local/bin/cosmon-daemon
# logs: bind=192.0.2.10:8790 (auto-discovered Tailscale IPv4)
```

To run it as a LaunchAgent, copy the template and substitute `__HOME__`:

```bash
sed "s|__HOME__|$HOME|g" \
    /srv/cosmon/cosmon/scripts/launchd/cosmon-daemon.plist \
    > ~/Library/LaunchAgents/dev.noogram.cosmon.cosmon-daemon.plist
launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/dev.noogram.cosmon.cosmon-daemon.plist
```

### Build & install the iOS app

Prerequisites: Xcode 15+, `xcodegen` (`brew install xcodegen`), an Apple
developer team ID baked into `project.yml` (currently `69Y2Z265F9`).

```bash
cd /srv/cosmon/cosmon/apps/CosmonApp
xcodegen                              # regenerate CosmonApp.xcodeproj

# Simulator (iPhone 17 Pro):
xcodebuild -project CosmonApp.xcodeproj -scheme CosmonApp \
    -sdk iphonesimulator -configuration Debug \
    -derivedDataPath build/sim build
xcrun simctl install booted \
    build/sim/Build/Products/Debug-iphonesimulator/Cosmon.app
xcrun simctl launch booted dev.noogram.cosmon.app

# Device install (paired iPhone or iPad on the tailnet):
xcodebuild -project CosmonApp.xcodeproj -scheme CosmonApp \
    -configuration Release -destination 'generic/platform=iOS' \
    -archivePath build/CosmonApp.xcarchive archive
xcrun devicectl device install app \
    --device <DEVICE_UUID> \
    build/CosmonApp.xcarchive/Products/Applications/Cosmon.app
```

`xcrun devicectl list devices` lists paired iOS devices and their UUIDs.

### Mock mode (no Mac on tailnet)

The app reads `COSMON_USE_MOCK=1` from the process environment in DEBUG
builds. Set it under *Edit Scheme → Run → Arguments → Environment
Variables* to swap in `MockDaemonClient`. Production builds always hit
`LiveDaemonClient.fromInfoPlist()`.

## Debug

- **Daemon logs**: `tail -f ~/.cosmon/logs/cosmon-daemon.{out,err}`
  (when running as a LaunchAgent), or stderr for foreground runs.
- **Smoke check**: `curl http://$(tailscale ip --4 | head -1):8790/v1/health`
  should return `{"ok":true,"galaxies_count":31,…}`.
- **Daemon-offline banner** in the app means the URLSession could not
  reach the bound port. Check `ATS` in `Info.plist`
  (`NSAllowsLocalNetworking` is on), and that the device is on the same
  tailnet as the Mac.
- **Decode errors**: every timestamp on the wire is a Unix-seconds
  `Double`. The Swift decoder is `secondsSince1970`. If a future
  endpoint emits ISO-8601 strings the app will throw
  `protocolMismatch`.

## Extend

### Add a new endpoint

1. Implement the route in
   `/srv/cosmon/cosmon/crates/cosmon-daemon/src/handlers.rs`:
   add a handler, a response DTO with `Serialize` (timestamps as
   `f64`), and wire it into `build_router`.
2. Add an integration test under
   `/srv/cosmon/cosmon/crates/cosmon-daemon/tests/integration.rs`.
3. Mirror the wire shape in
   `Sources/CosmonAppKit/WireModels.swift` (`Decodable`, camelCase
   property names — the transport's `keyDecodingStrategy` does the
   snake_case conversion).
4. Add the verb to `DaemonClient` (and to `MockDaemonClient` for
   previews / tests).
5. Hook the verb into `ClusterStore` (or a new store), then render a
   SwiftUI screen under `App/Screens/`.
6. `cargo test -p cosmon-daemon && (cd apps/CosmonApp && swift test) &&
   xcodebuild -sdk iphonesimulator …` before committing.

### Add write actions (v1.1)

The daemon currently rejects writes implicitly (no POST routes). Pick a
verb (`tackle`, `done`, `collapse`), shell out to `cs <verb> <id>` from
the handler, and follow the same wire shape `{ok, …}` as
`cosmon-cockpit-http /api/spark`. Surface the verb in the molecule
detail's verdict-door card.

### Push (SSE) — v1.1

`apps-transport-http` will gain an SSE channel. Subscribe from
`ClusterStore.startPolling` and stop polling once a stream is open;
fall back to polling on disconnect.

## Wire reference (v1)

| Method | Path                                                   | Body                                                                 |
|-------:|--------------------------------------------------------|----------------------------------------------------------------------|
|  `GET` | `/v1/health`                                           | `{ok, service, version, galaxies_count, molecules_running}`          |
|  `GET` | `/v1/galaxies`                                         | `{galaxies: [{name, path, molecule_count, running_count, …}]}`       |
|  `GET` | `/v1/galaxies/{galaxy}/molecules?status=…`             | `{galaxy, molecules: [{id, status, kind, formula, updated_at, …}]}`  |
|  `GET` | `/v1/galaxies/{galaxy}/molecules/{id}`                 | `{galaxy, id, status, briefing, log_tail, tmux_attach_hint, …}`      |
|  `GET` | `/v1/galaxies/{galaxy}/molecules/{id}/log`             | `text/markdown` raw `log.md`                                         |
|  `GET` | `/v1/fleets`                                           | `{fleets: [{galaxy, worker_count, repo_count, attention_budget?}]}`  |

Errors come back as `{error, code, detail?}` with HTTP 400/404/409/422/500
per the `apps-transport-http` convention.
