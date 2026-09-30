<!-- SPDX-License-Identifier: AGPL-3.0-only -->

# Logic register

## Issue #73(b): model-evidence receipts are outside lifecycle state

The new assessment receipt is append-only advisory telemetry about the
observer's input. It qualifies the last reported model in attribution and in
read surfaces. It does not change molecule status, worker admission,
dependency readiness, dispatch ownership, harvest authority, or the
`ModelSelected` intention. The lifecycle transition relation in
`docs/specs/CosmonRun.tla` therefore does not gain a state variable or action
for this receipt.

The receipt's own safety conditions are executable journal properties: it is
scoped to one worker attempt; a failed or stale capture cannot certify healthy
coverage; concurrent publishers deduplicate under the observation lock; and
replay preserves a historical gap through latest-response recovery. These are
checked in the model-evidence assessment, ledger, and watcher integration
tests. A future change that lets quality affect dispatch or a lifecycle
transition must revisit the TLA+ model before that coupling ships.
