# mac-pilot

Native macOS menubar app that drives cosmon without opening a terminal.
**v1 scope**: five popover tabs —

| Tab         | Surface                                                                   |
|-------------|---------------------------------------------------------------------------|
| Session     | `cs session start` / `note` / `end` (the v0 carnet)                       |
| Whispers    | Reads `.cosmon/whispers/inbox/*/` — preview, transform to spark, archive |
| Inbox       | Shells `cs observe --json` — filter by `temp:hot` / `temp:warm`, tackle   |
| Galaxies    | Scans `/srv/cosmon/*/.cosmon/` — opens each in a terminal                  |
| Cluster     | HTTP `GET /ensemble` + `GET /peek` via local cs-api (Ensemble/Peek picker) |

The Cluster tab (`Cmd+5`) offers a segmented sub-picker between
**Ensemble** (workers + molecules grouped by status, per galaxy) and
**Peek** (monospace three-scale snapshot at city / building / skin).
Both panes poll a local `cs-api` (default `http://127.0.0.1:4222`;
override via the `CS_API_URL` environment variable). See
[`docs/guides/cluster-views.md`](../../docs/guides/cluster-views.md)
for the full endpoint reference.

The four legacy tabs still run shell-outs to `cs` (or direct filesystem
reads) — no daemon, no network required. Only Cluster calls cs-api.

| Key fact         | Value                                        |
|------------------|----------------------------------------------|
| Platform         | macOS 14 (Sonoma) or later                   |
| Framework        | SwiftUI + `MenuBarExtra(.window)` (macOS 13+)|
| Bundle ID        | `dev.noogram.cosmon.mac-pilot`                 |
| Category         | Productivity                                 |
| Sandbox          | **Off** in v0 — we shell out to `cs`         |
| External deps    | None (Foundation / SwiftUI / AppKit only)    |

## Build from the command line (headless)

From the repo root:

```bash
just install-mac-pilot
```

This runs `xcodebuild` in Release configuration, places the resulting
`mac-pilot.app` in `~/Applications/`, and relaunches it. The recipe uses
`-allowProvisioningUpdates` so Xcode's *Automatically manage signing* flow
refreshes the provisioning profile on its own.

**First-time setup** — before the recipe will produce a team-signed build,
open the project once in Xcode and pick your team under **Signing &
Capabilities**. The full walkthrough (identity inventory, common
troubleshooting, onboarding a new team member) lives in
[`docs/guides/mac-pilot-signing-setup.md`](../../docs/guides/mac-pilot-signing-setup.md).

**No Apple Developer team?** Fall back to the ad-hoc path:

```bash
scripts/mac-pilot-reinstall-adhoc.sh
```

This produces an unsigned (ad-hoc `-`) bundle that runs locally but cannot
be distributed.

### Raw xcodebuild invocation

If you need to call `xcodebuild` directly (for CI scripts or a custom
destination), the recipe expands to:

```bash
xcodebuild -project apps/mac-pilot/mac-pilot.xcodeproj \
  -scheme mac-pilot -configuration Release \
  -destination 'platform=macOS,arch=arm64' \
  -derivedDataPath /tmp/mac-pilot-build \
  -allowProvisioningUpdates \
  build
```

## Run from Xcode (development)

1. Open `apps/mac-pilot/mac-pilot.xcodeproj`.
2. Pick the **mac-pilot** scheme and the **My Mac** destination.
3. Click ▶ Run. The 🧭 icon appears in the menu bar. Click it to reveal the
   popover.

If `cs` is not on `PATH`, see *Troubleshooting* below.

## Install into `~/Applications/`

After a Release build, locate the produced `.app`:

```bash
xcodebuild -project apps/mac-pilot/mac-pilot.xcodeproj \
  -scheme mac-pilot -configuration Release -showBuildSettings \
  | awk '$1 == "BUILT_PRODUCTS_DIR" { print $3 }'
```

Then copy it:

```bash
cp -R "<BUILT_PRODUCTS_DIR>/mac-pilot.app" "$HOME/Applications/"
open -a "$HOME/Applications/mac-pilot.app"
```

v1 will produce a notarized DMG; for now this manual copy is enough.

## Manual test checklist (acceptance)

All seven points must be green before calling a build shippable:

1. Open `apps/mac-pilot/mac-pilot.xcodeproj` in Xcode, click Run →
   the 🧭 icon appears in the menu bar.
2. Click the icon → popover opens in under 200 ms.
3. Click **Start Session** → a new `session-*.md` file appears in
   `/srv/cosmon/cosmon/.cosmon/state/sessions/`. Status flips to
   *"Session ouverte depuis HH:MM"*.
4. Type `test menubar app` + Enter → `cs session note` runs, the note shows
   in the list, and the file on disk contains `## HH:MM:SS — ` followed by
   the note body.
5. Click **End Session** → the file is sealed with `seal: blake3:…`, the
   note list disappears, status goes back to *"Aucune session ouverte"*.
6. Click **Start Session** again → a fresh session file is created.
7. Headless build:

   ```bash
   xcodebuild -project apps/mac-pilot/mac-pilot.xcodeproj \
     -scheme mac-pilot -configuration Release build
   ```

   must return exit code 0.

## Keyboard shortcuts

| Shortcut   | Action                               |
|------------|--------------------------------------|
| ⌘1         | Session tab                          |
| ⌘2         | Whispers tab                         |
| ⌘3         | Inbox tab                            |
| ⌘4         | Galaxies tab                         |
| ⌘5         | Cluster tab (Ensemble / Peek picker) |
| Enter      | Submit the current note              |
| ⌘Enter     | Submit (backup — same effect)        |
| ⌘S         | Start / end the session (toggle)     |
| ⌘Q         | Quit mac-pilot                       |
| Esc        | Close the popover without sending    |

## Troubleshooting

### `cs` binary not found

The app resolves `cs` in this order:

1. `CS_BINARY_PATH` environment variable (scheme environment).
2. `$HOME/.local/bin/cs` (default `just install` target).
3. Whatever `which cs` returns (PATH lookup).

If the popover surfaces *"Binaire `cs` introuvable."*:

* Confirm `cs` is installed: `which cs`.
* If it lives outside the defaults, edit the Xcode scheme (Product → Scheme
  → Edit Scheme → Run → Arguments → Environment Variables), enable
  `CS_BINARY_PATH` and set it to the full absolute path, then relaunch.

### "Session already open" when clicking Start

A prior session is still open (possibly started from the terminal). Either
click **End Session** in the popover (it re-reads the state on next poll)
or run `cs session end` in the terminal.

### Polling shows stale notes

The popover refreshes every 3 seconds while open. If you edit the session
file externally, wait a tick — or close and reopen the popover to force a
refresh on `.task {}`.

## File layout

```
apps/mac-pilot/
├── README.md                ← you are here
├── mac-pilot.xcodeproj/     ← hand-authored pbxproj
│   ├── project.pbxproj
│   ├── project.xcworkspace/
│   └── xcshareddata/xcschemes/mac-pilot.xcscheme
└── mac-pilot/
    ├── App.swift            ← @main AppKit NSStatusBar entry point
    ├── PilotView.swift      ← top-level popover + tab router
    ├── WhispersView.swift   ← Matrix whispers — read, transform, archive
    ├── InboxView.swift      ← pending molecules — tackle, worktree, collapse
    ├── GalaxiesView.swift   ← peer `/srv/cosmon/*/` listing
    ├── CosmonBridge.swift   ← `Process` shell-out + filesystem readers
    ├── Models.swift         ← SessionState, Whisper, MoleculeSummary, Galaxy
    ├── Info.plist           ← LSUIElement=true, category, bundle id
    └── Assets.xcassets/     ← empty AppIcon / AccentColor placeholders
```

## Whispers tab (v1)

Reads `/srv/cosmon/cosmon/.cosmon/whispers/inbox/<room>/*.md` every 5 s while
the popover is visible. Each `.md` file carries a YAML frontmatter
(`sender_mxid`, `sender_nucleon_id`, `origin_server_ts`, `room_id`,
`received_at`) followed by the body. A row shows
`[nucleon_id] preview… • il y a 3m`; clicking surfaces the full body plus
raw frontmatter, with two actions:

* **Transformer en task** → shells out `cs spark "<body>"` (creates an
  `idea` molecule tagged `temp:hot`) and archives the whisper.
* **Marquer lu** → moves the `.md` file to
  `.cosmon/whispers/archived/<room>/<filename>`. The original file is not
  modified — only relocated.

An `(N)` badge on the tab counts un-archived whispers.

## Inbox tab (v1)

Shells out `cs observe --json` (rate-limited: one call per 10 s) to list
molecules currently `pending` / `queued` / `running`. The top filter row
toggles between *Tous / temp:hot / temp:warm* (applied as `--tag <glob>`).
Clicking a row fetches full detail (`cs observe <id> --json`) and exposes
three actions:

* **Tackle** → `cs tackle <id> --leaf` (single-worker spawn).
* **Worktree** → opens Finder on `.worktrees/<id>/` if present, otherwise
  on the molecule's state directory.
* **Collapse** → `cs collapse <id> --reason <reason>` behind a confirm.

A `(N)` badge on the tab counts `temp:hot` pending items.

## Galaxies tab (v1)

Enumerates every sibling under `/srv/cosmon/*/` containing a `.cosmon/`
subdirectory. Each row shows the galaxy name, pending-molecule count (read
from `.cosmon/state/fleets/default/molecules/*/state.json`), and last
modification time. Clicking the terminal icon shells out
`open -a Ghostty -n <path>` (fallback `Terminal.app`). The active galaxy
for the Session / Whispers / Inbox panes is still hard-coded to
`/srv/cosmon/cosmon/` — a runtime galaxy switch is a v2 concern.

## v2 (not in this PR)

* Multi-galaxy picker driving every pane (not just the peer list).
* Notarized DMG / sideload-friendly install.
* App Sandbox + `com.apple.security.files.user-selected.read-write`
  entitlements for galaxy-root selection.
* Native XPC channel to a future long-lived `cs` daemon (no shell-out).
* Transcript mode for Whispers (grouped by sender, infinite scroll).

See `docs/guides/mac-pilot.md` for the user-facing guide.
