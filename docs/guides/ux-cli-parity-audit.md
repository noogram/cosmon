<!-- SPDX-License-Identifier: AGPL-3.0-only -->

# CLI and UI parity audit

This audit records the operator-facing surface touched by issue #117. It is
scoped to the patrol dead-worker policy; it is not a full command inventory.

| Capability | CLI | Native UI | Reveal CLI |
|---|---|---|---|
| Configure the grace before a patrol dead-worker verdict | `cs patrol --dead-worker-grace-secs <seconds>`; default 120 | No control found in the native apps | No equivalent action found |

The policy is applied by the patrol command. The missing native control and
Reveal CLI action remain parity gaps under ADR-068; this change does not claim
that the native apps expose the new setting.
