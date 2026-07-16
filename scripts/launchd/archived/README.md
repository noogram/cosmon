# scripts/launchd/archived/

Retired LaunchAgent plists kept for rollback, not for installation.

Two migration lineages converge here:

- **Tick-based patrols** were superseded by `[[patrol]]` entries in
  `~/.config/cosmon/patrols.toml` (the unified `cosmon-scheduler`).
  See `docs/adr/050-unified-patrol-scheduler.md`.
- **Long-running daemons** were superseded by `[[daemon]]` entries in
  `~/.config/cosmon/daemons.toml` (the `cosmon-daemon-supervisor`).
  See the `cosmon-daemon-supervisor` crate and its internal rollout plan.

## Rollback — patrols

Ten-second reversal when a migrated patrol misbehaves:

```sh
# 1. Disable the TOML entry
$EDITOR ~/.config/cosmon/patrols.toml   # set `enabled = false`

# 2. Reinstall the archived plist
cp scripts/launchd/archived/<LABEL>.plist ~/Library/LaunchAgents/
launchctl load ~/Library/LaunchAgents/<LABEL>.plist
```

## Rollback — daemons

Same shape, different TOML:

```sh
# 1. Disable the TOML entry
$EDITOR ~/.config/cosmon/daemons.toml   # set `enabled = false`

# 2. Reinstall the archived plist
cp scripts/launchd/archived/<LABEL>.plist ~/Library/LaunchAgents/
launchctl load ~/Library/LaunchAgents/<LABEL>.plist
```

## Inventory

| Plist | Migrated | Replacement | Pilot molecule |
|-------|----------|-------------|----------------|
| `cosmon-chronicle-lint-weekly.plist` | 2026-04-18 | `chronicle-lint-weekly` patrol | `task-20260417-e5ec` |
| `com.you.notification-bot.plist` | 2026-04-19 | `notification-bot` daemon | `task-20260419-b31b` |
| `com.you.notification-bot.plist` | 2026-04-19 | `notification-bot` daemon | `task-20260419-5ad4` |
| `com.you.emacs-daemon.plist` | 2026-04-19 | `emacs-daemon` daemon | `task-20260419-5ad4` |
| `com.you.zotero-mcp.plist` | 2026-04-19 | `zotero-mcp` daemon | `task-20260419-5ad4` |
| `com.you.almanac.plist` | 2026-04-19 | `almanac` daemon | `task-20260419-5ad4` |
| `com.you.archive-service.plist` | 2026-04-19 | `archive-service` daemon | `task-20260419-5ad4` |
| `com.noogram.dashboard.plist` | 2026-04-19 | `noogram-dashboard` daemon | `task-20260419-5ad4` |
