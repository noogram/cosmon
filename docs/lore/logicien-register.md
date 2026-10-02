<!-- SPDX-License-Identifier: AGPL-3.0-only -->

# Logic register

## Issue #151 W2: worker acceptance refines the existing step guard

`response_artifact` adds no lifecycle state and no new transition. It supplies
a concrete precondition for the existing current-step advance: declared text
must be durably published, declared files must be fresh, or a legacy code step
must have a turn-scoped worktree change. The transition still records exactly
one current step through the existing completed-step set and never jumps to the
formula tail. Duplicate acceptance observes the completed step and is a no-op.

`docs/specs/CosmonRun.tla` therefore needs no new variable or action for W2.
The refinement obligations are executable at the Rust boundary: unsafe path
admission, empty text, stale/missing artifacts, unchanged legacy work, later
gate preservation, and duplicate finalization. If acceptance later gains a
separate persisted phase or attempt identity in the lifecycle model, the TLA+
specification must gain that state in the same change.

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
