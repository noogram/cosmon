#!/usr/bin/env bash
#
# <bitbar.title>cosmon-pulse</bitbar.title>
# <bitbar.version>v0.1.0</bitbar.version>
# <bitbar.author>Noogram</bitbar.author>
# <bitbar.author.github>you</bitbar.author.github>
# <bitbar.desc>Pastille cosmon-pulse — ambient runtime-vitality indicator for the cosmon agent fleet. Colored dot (🟢/🟡/🔴) + RPM headline in the menu bar; dropdown shows the six voyants + fuel. This script is a thin shim: SwiftBar calls it every 10 s, which execs the installed cs binary. All logic lives in crates/cosmon-cli/src/cmd/pulse.rs.</bitbar.desc>
# <bitbar.dependencies>cs (cosmon CLI, installed at ~/.local/bin/cs)</bitbar.dependencies>
# <bitbar.abouturl>file:///srv/cosmon/cosmon/menubar/cosmon-pulse.10s.sh</bitbar.abouturl>
#
# <swiftbar.hideAbout>false</swiftbar.hideAbout>
# <swiftbar.hideRunInTerminal>true</swiftbar.hideRunInTerminal>
# <swiftbar.hideLastUpdated>true</swiftbar.hideLastUpdated>
# <swiftbar.hideDisablePlugin>false</swiftbar.hideDisablePlugin>
#
# Refresh interval is encoded in the filename: .10s. = 10 seconds.
# Mirror of /srv/cosmon/airflow/menubar/respire.5s.sh — same shim pattern.
# Install: symlink or copy into ~/Library/Application Support/SwiftBar/
#   ln -s /srv/cosmon/cosmon/menubar/cosmon-pulse.10s.sh \
#         ~/Library/Application\ Support/SwiftBar/cosmon-pulse.10s.sh

BIN=/Users/you/.local/bin/cs
[ -x "$BIN" ] || { echo "?"; echo "---"; echo "cs absent — cargo install or just install"; exit 0; }

exec "$BIN" pulse --swiftbar
