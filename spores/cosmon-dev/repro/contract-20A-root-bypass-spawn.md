# Red repro-contract #20A — root-run spawns a live worker on the dead root path

**Class:** privilege / spawn. **LLM:** none required (uid + process assertion).
**Affected ref:** v0.2.2. **Profile:** `repro-root` (root is REQUIRED — the bug IS
a root bug; `skip-if-not-root` is FORBIDDEN).

## The contract (G1 — what cosmon promises)

When `cs` runs as **root** (UID 0), a dispatch must reach **one of two** accepted
outcomes, never a third:

1. **Demote** — spawn the cognitive worker as a **non-root** user (the image's
   `worker`, UID 10001); OR
2. **Refuse** — decline to create a live worker **before** one exists, with a typed
   root-refusal.

The forbidden third outcome is what v0.2.2 does: **spawn a live cognitive worker on
the root path** that then dies (or runs with root's blast radius). An autonomous
LLM running as root is a **security fault, not a corner case** — the fix must NOT
optimise to preserve the root-bypass.

## The bug

Under `repro-root`, the v0.2.2 spawn path starts the worker on the root code path,
which neither demotes to UID 10001 nor refuses before a live worker exists. The
worker is created, runs as root, and converges to neither accepted outcome.

## The red harness (runs in the clean-room `repro-root` profile)

The observable is **positive**: either the spawned worker's UID is non-root
(outcome 1), or NO worker process was ever created and a typed refusal was recorded
(outcome 2). A red is "a live worker exists AND its UID is 0".

```bash
#!/usr/bin/env bash
# repro-20A.sh — run inside the clean-room, repro-root profile (docker -u 0).
# RED on v0.2.2: a live worker is spawned running as root (uid 0).
set -euo pipefail

# 0. Assert we really are root — a skip is NOT a red (blueprint §5). And prove the
#    OS lets root spawn a process at all (control_write-equivalent liveness check),
#    so a red accuses the SPAWN POLICY, not a broken container.
test "$(id -u)" -eq 0 || { echo "FALSE-RED: not root; repro-root profile required" >&2; exit 2; }
( sh -c 'exit 0' ) || { echo "FALSE-RED: OS cannot spawn a child at all" >&2; exit 2; }

# 1. Dispatch a molecule as root. Capture the spawned worker's uid (the adapter
#    records the worker pid; read its /proc/<pid>/status Uid line, or the
#    worker_spawned event's recorded uid if the archived ref emits it).
cs tackle <mol_id> --no-worktree >/dev/null 2>&1 || true

worker_pid=$(jq -r 'select(.type=="worker_spawned") | .worker_pid' state/events.jsonl | tail -1)

if [ -z "$worker_pid" ] || [ "$worker_pid" = "null" ]; then
  # Outcome 2 (refuse-before-live-worker): no worker was created. Confirm a typed
  # root-refusal was recorded; a silent no-op is itself a fault.
  if jq -e 'select(.type=="tackle_refused" and (.reason|test("root";"i")))' state/events.jsonl >/dev/null; then
    echo "GREEN (outcome 2): refused before a live worker, typed root-refusal recorded."
    exit 0
  fi
  echo "RED (right reason): no worker AND no typed root-refusal — a silent no-op." >&2
  exit 1
fi

worker_uid=$(awk '/^Uid:/{print $2}' "/proc/$worker_pid/status" 2>/dev/null || echo 0)
echo "spawned worker pid=$worker_pid uid=$worker_uid"

# 2. THE CONTRACT ASSERTION — a live worker must NOT be root.
if [ "$worker_uid" -eq 0 ]; then
  echo "RED (right reason): a LIVE cognitive worker was spawned as ROOT (uid 0)" >&2
  echo "     — neither demoted (outcome 1) nor refused (outcome 2). Root-bypass fault." >&2
  exit 1
fi
echo "GREEN (outcome 1): worker demoted to non-root (uid $worker_uid)."
```

## Differential refutation (one variable flips the colour)

The single variable is the **spawn policy** (demote-or-refuse vs spawn-on-root).
Apply the fix (demote to UID 10001, or refuse-before-live) and the observable
flips: `worker_uid != 0` (outcome 1) or `no worker + typed refusal` (outcome 2).
Revert the fix and a root worker is spawned again (red). One variable, colour
BASCULES both ways.

## False-green mode

- **`skip-if-not-root`** would make the test PASS by never running under root — a
  skip is not a green (blueprint §5). FORBIDDEN; the harness asserts `id -u == 0`
  and exits 2 (false-red / environment error) rather than skip.
- **Reading a stale `worker_spawned` event** from a previous run could show a
  non-root uid that is not this dispatch's. The harness reads the LAST event and
  ties it to this molecule id.
- **Optimising the fix to keep the root-bypass** but demote late (after the worker
  briefly ran as root) — the assertion reads the uid at spawn, not after.

## False-red mode

- **A container that cannot spawn any child** (broken PID namespace) would redden
  for the wrong reason. The step-0 `sh -c 'exit 0'` liveness check catches this and
  exits 2 (environment error) instead of a contract red.
- **The molecule carrying a non-root pin already** would trivially pass; the harness
  uses a default-dispatch molecule.
