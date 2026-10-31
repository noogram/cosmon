# Transfer a bot reader between machines

The supported guarantee is at most one admitted managed reader during a
transfer between enrolled hosts under operator control. The source is fenced
before the destination becomes active. Completion additionally requires a
fresh successful destination capture poll and an observed transferred source.
A scheduled reader can be idle between invocations.

This assumes upgraded scripts on both hosts, one canonical host-local state
root per bot, durable state without rollback or cloning, and participation by
every consumer of the credential. An unmanaged credential holder is outside
this guarantee. Absence of HTTP/API 409 never proves that only one reader exists.
A webhook excludes polling; the operator must establish that polling is the
intended configuration before enrollment. These scripts do not discover or
change webhook configuration and do not perform a remote identity lookup:
they compare the bot ID prefix in the independently provisioned credentials.

## Before enrollment

The operator first disables every legacy reader entry point on both machines
(including old scheduler, cron, launchd and manual invocations) and establishes
that any admitted request has completed. A legacy executable does not implement
the journal guard. Merely installing the new scripts or placing a stop file
cannot fence an old executable already running. Do not use a diagnostic
`getUpdates` request: it can acknowledge messages outside durable capture.

Install the matching supported scripts and Python 3 on both machines. Use
explicit SSH aliases with authenticated access, verified host keys and
noninteractive authentication. Provision the destination credential out of band,
by the operator, in its protected credential file; never copy it into a manifest,
a command argument, or a transfer log. The production credential path is
`~/.showroom/bot.toml` (`bot_token`); the supported state root is `~/.cosmon`.
Do not choose another root for a second reader using the same bot.

On the source, inspect the legacy `telegram-offset`. It must be a nonnegative
integer representing durably handled updates. Enrollment creates zero only
when that legacy checkpoint is absent: establish whether zero is appropriate
before enrolling, rather than treating missing history as permission to replay.
Then run on the source, once:

```sh
python3 scripts/bot-reader-transfer.py enroll --host-id host-a
python3 scripts/bot-reader-transfer.py status
```

Drain or archive captured inbox work through the existing consumer before
export. Export refuses a nonempty `telegram-inbox`; it migrates the checkpoint,
not downstream work or consumer acknowledgements. Do not delete inbox messages
to make the check pass. A final admitted poll may add messages while fencing;
if export refuses, process those messages while the source stays fenced.

## Stop, export, import, activate, verify

Choose stable host IDs (`host-a`, `host-b` below), distinct from SSH aliases.
From a private operator working directory, define these helpers with the
installed absolute script paths and configured aliases for your machines:

```sh
on_a() { ssh -o BatchMode=yes -o StrictHostKeyChecking=yes -T reader-a python3 /opt/cosmon/scripts/bot-reader-transfer.py "$@"; }
on_b() { ssh -o BatchMode=yes -o StrictHostKeyChecking=yes -T reader-b python3 /opt/cosmon/scripts/bot-reader-transfer.py "$@"; }
listen_b() { ssh -o BatchMode=yes -o StrictHostKeyChecking=yes -T reader-b bash /opt/cosmon/scripts/telegram-listen.sh; }
```

Run the following sequence with the destination's scheduled invocations paused
until verification finishes. `fence` places the stop first, then waits under
the lock for the already admitted poll. A free lock alone does not establish
that a crashed or timed-out server request ended. Store manifest and receipt
files privately (`umask 077`); the receipt is transfer authority. Every failed
command stops the sequence. Do not activate using an uncommitted manifest.

<!-- rehearsal: transfer -->
```sh
set -euo pipefail
umask 077
on_b prepare --host-id host-b
on_a fence
on_a export --destination host-b > offered.json
python3 -c 'import json; print(json.dumps(json.load(open("offered.json"))["manifest"]))' > manifest.json
on_b import < manifest.json
on_a commit < manifest.json > committed.json
python3 -c 'import json; print(json.dumps(json.load(open("committed.json"))["receipt"]))' > receipt.json
on_b activate < receipt.json
listen_b > poll.json
on_a status > source.json
on_b status > destination.json
python3 - <<'VERIFY'
import json
from pathlib import Path
source, destination, poll, manifest = [json.loads(Path(name).read_text()) for name in
    ("source.json", "destination.json", "poll.json", "manifest.json")]
assert source["phase"] == "transferred" and source["lock"] == "free"
assert not source["in_flight"]
assert destination["phase"] == "active" and not destination["stop"]
assert not destination["conflict_stop"] and not destination["in_flight"]
assert destination["epoch"] == source["epoch"] == manifest["epoch"]
assert source["host_id"] == manifest["source"]
assert destination["host_id"] == manifest["destination"]
assert source["transfer"]["transfer_id"] == destination["transfer"]["transfer_id"] == manifest["transfer_id"]
assert destination["checkpoint"] >= manifest["checkpoint"]
assert poll["outcome"] in ("empty", "filtered", "captured")
print("Verified transferred source and one fresh destination capture poll")
VERIFY
```
<!-- rehearsal: end -->

Keep these observations as the completion receipt, then allow the destination's
existing schedule to continue. They establish the enrolled-host state and one
fresh poll within this window; they cannot certify absence of an unmanaged
reader. `status` is a local observation, with no polling or repair. There is no
`verify` verb: verification above uses status plus the normal capture path.

For routine transfers, the coordinator performs prepare/export/import/commit/
activate in that order, passing manifests on stdin over batch-mode SSH:

```sh
python3 scripts/bot-reader-transfer.py transfer \
  --source ssh:reader-a --destination ssh:reader-b \
  --destination-host-id host-b --remote-script /opt/cosmon/scripts/bot-reader-transfer.py
```

Its `transferred` result records activation, not fresh poll verification.
Still perform `listen_b` and the status checks above. Never log the receipt
as a public diagnostic or place it in a command argument.

## Failure and recovery

| State or outcome | Operator action |
| --- | --- |
| `not_enrolled` | Establish the legacy shutdown and checkpoint history before initial enrollment. |
| `active` | Only this phase can poll, subject to the stop file and conflict latch. |
| `fenced` | Source is stopped. Drain inbox and retry export, or `cancel` without a transfer ID to withdraw a bare fence. |
| `offered` | Source offered one destination. Retry the same tuple, or `cancel --transfer-id ID` before commit. |
| `standby` | Destination is prepared and inactive; import the source's manifest. |
| `staged` | Destination has an offer but cannot poll. Obtain the exact source commit receipt and retry activation. |
| `transferred` | Source has committed and stays fenced. Never cancel or manually restart it. Reverse transfer is required. |
| `stopped` | A local stop file remains authoritative. Remove only your own deliberate stop after resolving its purpose; transfer preserves preexisting stops. |
| `transfer_pending` | Ownership is inactive. Consult the journal phase and resume the matching transition. |
| `api_conflict` | HTTP/API 409 latched a durable conflict stop. Follow explicit reconciliation below. |
| `indeterminate_in_flight` | Request completion is unknown. Refuse transfer; a free lock or elapsed timeout is insufficient. No automated reconciliation of this state is supported. |
| `inbox_not_empty` | Consume/archive the source's durable messages before retrying export. |
| `invalid_state` / invalid checkpoint | Stop and investigate against trusted durable records. Do not delete the journal, lock, or checkpoint to obtain admission. |
| `credential_failure` | Operator repairs credential provisioning out of band, then retries the same transaction. |
| `transport_failure` / `network_failure` | Establish reachability and request state. No time-based promotion or speculative source restart. |
| `stale_epoch`, `receipt_mismatch`, `offer_mismatch` | Reject replay or mismatched authority. Inspect both hosts; use the current transaction, never rewrite epochs. |

The source must be reachable to establish its fence and commit. An outage
before fencing can leave the source active; the destination remains inactive.
An outage after fencing leaves zero readers until recovery. Retry the same
coordinator invocation to resume idempotently. If a commit reply is lost, the
source may already be `transferred`: query it and retrieve the same receipt.
If an activation reply is lost, the destination may already be active: query
it, then retry the same activation. Never restart the source speculatively.

A crash or timeout can leave `telegram-listen.inflight`. Export refuses it.
There is no supported force-clear command and the 409 reconciliation verb does
not clear it. Establish request completion through a separately validated
operator recovery procedure before any transfer; do not treat marker deletion
as evidence. Preserve permanent lock inodes and the protected ownership journal.

### Reconcile a 409 on its host

A 409 records `conflict_stop: true` atomically in that host's ownership journal
under the same lock used for polling and transfer. Later invocations reread
it before network activity. Neither time, removing `telegram-listen.off`, nor
a subsequently available HTTP 200 clears it.

The operator establishes that the competing consumer has stopped, then runs
locally on the affected active host:

<!-- rehearsal: reconcile -->
```sh
on_a status
on_a reconcile --competing-consumer-gone
```
<!-- rehearsal: end -->

This acknowledges the operator's finding; it does not discover or stop another
consumer. It makes no network request, preserves ownership, checkpoints,
in-flight markers and unrelated stop controls, and refuses inactive phases
including `fenced` and `transferred`. The next scheduled or manual listener
invocation admits at most one ordinary capture poll. A fresh 409 latches again.
Cancelling a fence or activating a later transfer never clears a host's latch.

### Reverse transfer and replay

After a committed transfer, returning ownership is a new transfer from B to A,
with a higher epoch. Drain B's inbox, then use `--source ssh:reader-b
--destination ssh:reader-a --destination-host-id host-a` with the coordinator.
Verify A with its normal listener and inspect B's transferred state. Old
manifests and receipts cannot reactivate a host that has transferred away;
activation needs the matching staged offer and receipt. A manifest alone is
never authority.

## Diagnostics and limits

Keep diagnostics bounded: outcome, phase, host ID, epoch, transfer ID and
checkpoint. Do not publish credentials, message bodies, names, raw API replies,
credential-bearing URLs, private host paths, manifests or receipts. Status omits
the receipt secret; transfer output carries it for private operator use.

The procedure does not migrate inbox/consumer state, guarantee exactly-once
downstream effects, or recover messages beyond the service's retention window.
Record elapsed outage and any known gap. Fake loopback tests demonstrate
ordering, capture and refusal paths with synthetic credentials; they do not
qualify a live transfer or an installed scheduler. There are no UI or
`cosmon-remote` equivalents for this scripted operator surface.
