<!-- SPDX-License-Identifier: AGPL-3.0-only -->

# CLI and UI parity audit

This audit records the operator-facing surface touched by issue #117. It is
scoped to the patrol dead-worker policy; it is not a full command inventory.

| Capability | CLI | Native UI | Reveal CLI |
|---|---|---|---|
| Configure the grace before a patrol dead-worker verdict | `cs patrol --dead-worker-grace-secs <seconds>`; default 120 | No control found in the native apps | No equivalent action found |
| Detect a codex launch menu before briefing delivery | `cs tackle --adapter codex` inspects the pane and records a blocking dialogue | No launch-menu status found in the native apps | No equivalent action found |
| Scan live panes for blocking dialogues | Every `cs patrol` run scans by default; `--auto-confirm-safe` remains opt-in | No dialogue scan control found in the native apps | No equivalent action found |

The dead-worker policy and dialogue scan are applied by the patrol command.
These native controls and Reveal CLI actions remain parity gaps under ADR-068.
