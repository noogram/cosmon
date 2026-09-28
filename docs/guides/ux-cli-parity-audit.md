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

## Concurrent tackle claim (issue #119)

| Capability | CLI | Native UI | Other CLI views |
|---|---|---|---|
| Serialize a manual tackle and resident dispatch before model selection | `cs tackle` claims the molecule until the spawn verdict; a losing invocation exits non-zero and names the winning worker, adapter and selected model | No equivalent admission control audited | No equivalent admission control audited |

The loser receives a recorded model pin when one exists. If the winning
adapter chose its own default, the exact model is unrecorded and the error
says so.

## Blocked dependents (issue #118)

| Capability | CLI | Native UI | Other CLI views |
|---|---|---|---|
| Hold a dependent until its blocker completes | `cs tackle` refuses dispatch; `cs run --resident` and the frontier retain the pending dependent | No equivalent admission control audited | No equivalent admission control audited |
| Name dependents held by a collapsed or frozen blocker | Human `cs status` and `cs status <blocker>` name them; `cs peek` names the blocker on the dependent row and gives the recovery gesture | No equivalent detail audited | No equivalent detail audited |

After a freeze, finish the blocker, then `cs complete` and `cs done` it. After a collapse, collapse the
pending dependent and re-nucleate it with a new `--blocked-by` edge. The native
and other CLI surfaces in this table remain unverified for this capability.
