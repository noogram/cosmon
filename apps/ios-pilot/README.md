# ios-pilot

Universal iOS/iPadOS SwiftUI app for piloting cosmon from an iPhone or iPad.
Six tabs sit over `cs-api` (HTTP over Tailscale): **Session** (notes carnet),
**Whispers** (Matrix ingress), **Inbox** (pending / running molecules),
**Galaxies** (peer projects on the Mac), **Cluster** (Ensemble + Peek
across every local galaxy), **Réglages** (settings).

- **v0** (task-20260422-b031 / -16c1) — *Session* tab only, the dictaphone for
  `cs session`.
- **v1** (task-20260422-335b) — adds *Whispers*, *Inbox*, *Galaxies* tabs on
  top of the five `cs-api` endpoints shipped by `task-20260422-db9f`.
- **v1 cluster** (task-20260423-d3ae, this task) — adds the *Cluster* tab
  with a segmented sub-picker between **Ensemble** (workers + molecule
  status groups per galaxy, via `GET /ensemble`) and **Peek** (monospace
  three-scale snapshot at city / building / skin, via `GET /peek`).
  See [`docs/guides/cluster-views.md`](../../docs/guides/cluster-views.md)
  for the endpoint reference. No new shell-out on device — the Mac remains
  the oracle.

## Requirements

- macOS with Xcode 15+ (tested on Xcode 26.4).
- iOS Simulator runtime (any iOS 17+ runtime).
- `cs-api` running on the Mac (task-20260422-b031 for v0 session endpoints,
  task-20260422-db9f for v1 whispers/inbox/galaxies endpoints). For simulator
  testing, the mock client is used by default in `DEBUG` builds when the env
  var `COSMON_USE_MOCK=1` is set in the Xcode scheme. Without the flag, the
  app talks to the URL from **Réglages**.
- `xcodegen` (installed via `brew install xcodegen`).

## Build

The Xcode project is generated from `project.yml`:

```sh
cd apps/ios-pilot
xcodegen generate
```

Build for iOS Simulator:

```sh
xcodebuild \
  -target ios-pilot \
  -project ios-pilot.xcodeproj \
  -configuration Debug \
  -sdk iphonesimulator \
  build
```

The generated bundle lands in `build/Debug-iphonesimulator/ios-pilot.app`.

The `-scheme -destination` form also works on Xcode 26.4 with an installed
iOS 26 simulator runtime:

```sh
xcodebuild \
  -project ios-pilot.xcodeproj \
  -scheme ios-pilot \
  -destination 'platform=iOS Simulator,name=iPhone 17 Pro' \
  build
```

> If the simulator runtime is missing, fall back to the `-target -sdk` form
> above — it does not require a specific runtime version.

## Run in simulator

```sh
# Boot any iOS simulator
xcrun simctl boot 'iPhone 17 Pro'
open -a Simulator

# Install + launch
xcrun simctl install booted build/Debug-iphonesimulator/ios-pilot.app
xcrun simctl launch booted dev.noogram.cosmon.ios-pilot
```

To point at a real `cs-api`:

1. On the Mac: `cs-api serve --bind 0.0.0.0:4222` (see task-20260422-b031).
2. On simulator or device: open **Réglages**, paste the Tailscale URL
   (`tailscale ip -4` on the Mac → `http://<ip>:4222`), tap **Tester la
   connexion**.

## Run on device (sideload)

See `docs/guides/ios-pilot.md` — the cross-referenced workflow from
`task-20260422-16c1` (Blink Shell sideload) covers signing and trust
steps.

Quick path:

1. Open `ios-pilot.xcodeproj` in Xcode.
2. Select the target `ios-pilot`, switch to the Signing & Capabilities
   tab, pick your development team. The bundle identifier is
   `dev.noogram.cosmon.ios-pilot`; rename it if you hit a conflict in your
   Apple Developer account.
3. Plug in the iPhone/iPad over USB or enable Wi-Fi debugging.
4. Pick the device in Xcode's destination menu and tap **Run**.
5. On the device, the first launch asks for developer trust
   (Settings → General → VPN & Device Management).

## Manual test checklist (acceptance)

The v1 acceptance steps:

- [x] `xcodebuild -target ios-pilot -sdk iphonesimulator build` → exit 0.
- [x] `xcodebuild -scheme ios-pilot -destination '…iPhone 17 Pro' build` → exit 0.
- [x] Five tabs visible in the simulator: Session / Whispers / Inbox /
      Galaxies / Réglages.
- [x] With `COSMON_USE_MOCK=1`, **Whispers** lists two seeded whispers;
      tap → detail pane shows body + metadata + **Archiver** / **Transformer
      en spark** buttons.
- [x] With `COSMON_USE_MOCK=1`, **Inbox** lists three seeded molecules;
      tap → detail shows topic + tags + `cs tackle <id>` hint.
- [x] With `COSMON_USE_MOCK=1`, **Galaxies** lists three seeded galaxies
      with pending counts.
- [x] **Réglages** exposes polling interval 5/10/30s + off, "Inbox only
      temp:hot" toggle, log level picker.
- [x] iPad shows Whispers/Inbox in split-view (list on left, detail on
      right) via `NavigationSplitView`.
- [x] "Connecte cs-api dans Réglages" empty-state renders when the URL
      is empty.

## Architecture (v1)

| File | Role |
|------|------|
| `App.swift` | `@main` entry, injects `SessionStore` + `SettingsStore`. |
| `ContentView.swift` | `TabView` with five tabs; owns `PilotStores` so badges reflect live counts. |
| `SessionView.swift` | Composer, action row, recent notes, offline banner, haptics. |
| `WhispersView.swift` | `NavigationSplitView` of whispers (list + detail). Archive / spark buttons. |
| `InboxView.swift` | `NavigationSplitView` of molecules. Read-only v1 (shows `cs tackle <id>` to paste on the Mac). |
| `GalaxiesView.swift` | Read-only list of galaxies with pending/running counts. |
| `SettingsView.swift` | cs-api URL, polling 5/10/30s, hot-only toggle, log level, `/healthz` probe. |
| `Models.swift` | `SessionID`, `Note`, `SessionState`, `Seal`, `HealthzResponse`, `PendingNote`, `Whisper`, `MoleculeSummary`, `Galaxy`. |
| `CosmonAPI.swift` | `CosmonAPIProtocol`, live `CosmonAPI`, `MockCosmonAPI`, `CosmonAPIFactory`. |
| `SessionStore.swift` | Session polling + offline queue. |
| `SettingsStore.swift` | `apiURL`, `pollingEnabled`, `pollingInterval`, `onlyHot`, `logLevel`. |

## Scope guards (v1)

- **Read-only for Inbox & Galaxies** — v1 does not let iOS tackle, collapse,
  or switch active galaxy. The detail pane shows the exact `cs tackle <id>`
  command to paste on the MBP; iPad operators use split-view as a "prompt
  preview". Write actions (tackle, collapse) are a v2 concern once we have
  auth.
- **Spark/archive for Whispers** — these are the only mutations the iOS app
  performs. Both go through cs-api endpoints shipped in task-20260422-db9f.
  Mock mode simulates them in-memory.
- **No push notifications** — polling only, 5/10/30 s or off.
- **No application-level encryption** — Tailscale WireGuard is the transport.
- **No Bonjour / mDNS auto-discovery** — the operator pastes the Tailscale URL.
- **No §8k wheat-paste strict byte-identical test target** — text rendering of
  lists/details is plain SwiftUI `Text`, consistent with the mac-pilot text
  rendering when copied (same field order, same separators).
- **No UI tests target v1** — setup cost outweighs value; manual checklist
  above is the acceptance surface.

## Known limitations

- The Xcode 26.4 quirk previously documented (missing 26.4 simulator
  runtimes) remains — both the `-target -sdk` and the `-scheme -destination`
  builds ship as acceptance gates now.
- Mock mode activates only when `COSMON_USE_MOCK=1` is set in the Xcode
  scheme's Run → Environment. Without the flag the app dials the URL in
  Réglages and surfaces "cs-api injoignable" if the Mac is off.

## Pair guides

- `docs/guides/ios-pilot.md` — operator-facing daily workflow.
- `docs/guides/cs-api.md` — server-side reference for the endpoints
  consumed by this app.
- `apps/mac-pilot/README.md` — the sibling menubar app that reads the
  same whispers inbox directly from disk.
