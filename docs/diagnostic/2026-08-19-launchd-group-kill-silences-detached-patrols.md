# Diagnostic — launchd group-kills every detached patrol, and the log says it fired

**Date:** 2026-08-19 (fix landed 2026-09-18) · **Molecule:**
`task-20260918-2efa` · **Surface:** `com.cosmon.scheduler` LaunchAgent,
`cosmon-scheduler` dispatcher.

## Verdict: the plist lacked `AbandonProcessGroup`

The installed `com.cosmon.scheduler` LaunchAgent did not declare
`AbandonProcessGroup`. launchd's default for a job without that key is to
SIGKILL the job's **entire process group** the moment the job's main process
exits. `cosmon-scheduler tick` is a one-shot: it reads `patrols.toml`,
dispatches what is due, and returns in milliseconds.

Every patrol dispatched in `dispatch = "detached"` mode was therefore killed
by launchd — not by the scheduler, not by a crash — a few milliseconds after
being spawned, before it had produced anything. Neither `nohup` nor
`trap '' HUP` protects against this: the signal is addressed to the process
group, not to the process, so nothing the child does to itself is relevant.

The scheduler's own reasoning was correct and insufficient. Its dispatcher
deliberately does not `wait()`, on the sound Unix ground that dropping a
`Child` neither signals nor reaps it, so the child is reparented to `init`
when the parent goes. That is true. It is also beside the point: reparenting
answers *who is my parent*, and the group-kill answers *who dies with this
job*. The module documented the first as if it settled the second
(`crates/cosmon-scheduler/src/dispatch.rs`, now corrected).

## Measurements

One patrol on the 60-second clock, over a 48-hour window ending 2026-08-19:

| Signal | Count |
|---|---|
| Fires recorded by the scheduler | 7276 |
| Starts reaching the patrol's own log | 114 |
| Complete executions | a handful |

The surviving runs are the ones where the tick itself happened to last long
enough to cover the child's work — i.e. the survivors are an artefact of
timing, not of any code path intending them.

Consequence: the index that patrol feeds sat frozen for roughly 18 hours with
nothing in any log saying so. Two further patrols, silent for about two
months, are presumed to share the cause.

## Why this mode is the expensive one

The scheduler logged `FIRE <patrol> (pid=… detached)` on all 7276 occasions.
Every one of those lines was true: dispatch succeeded, the pid was real. The
lie was in what a reader takes the line to mean.

So the instrument reported health while producing nothing, and the only
evidence of failure was an absence — no output, in a log nobody reads when
there is no alarm. For a tool whose whole job is to direct attention, an
absence that looks exactly like success is the worst available failure mode:
the louder failure would have been the cheaper one. The measured cost of the
quiet version was 18 hours on one patrol and about two months on two others.

## What changed

1. `scripts/launchd/com.cosmon.scheduler.plist` declares
   `AbandonProcessGroup = true`, with the reasoning in the template so a
   later editor cannot read it as ceremony.
2. `scripts/install-scheduler.sh` verifies the **rendered** bytes carry the
   key and refuses to install otherwise, and `status` now reports when an
   already-installed plist predates the fix (repair: `reload`).
3. `crates/cosmon-scheduler/src/dispatch.rs` no longer claims that not
   waiting is sufficient for a detached patrol to survive.
4. `scripts/check-abandon-process-group.sh` enumerates installed one-shot
   LaunchAgents missing the key, as candidates for review.

## The rest of the parc

The shape — one-shot, key absent — is decidable from the plist, but whether a
job dispatches detached work is only decidable from the program it runs. So
each agent shipped by this repository was read, not just scanned:

| Agent | Shape | Verdict |
|---|---|---|
| `com.cosmon.scheduler` | one-shot, dispatches detached | **fixed** |
| `com.cosmon.daemon-supervisor` | `KeepAlive`, never exits on purpose | key deliberately absent — see below |
| `dev.noogram.cosmon.{letter-monday,session-route,session-to-spark,whisper-to-spark}` | one-shot, all work synchronous | no key needed |

The one-shot agents installed outside this repository on the machine where
the defect was observed were reviewed the same way and all do their work
synchronously; `scripts/check-abandon-process-group.sh` is what reproduces
that sweep on any machine, so the next such agent is caught by its shape
rather than by an 18-hour gap.

For `com.cosmon.daemon-supervisor` the absence is correct, not an oversight.
The key matters for a one-shot dispatching work meant to outlive it. The
supervisor is the opposite shape: it never exits on purpose and tracks its
children by pid in its own memory, so a child that outlived it would be an
orphan nobody supervises, duplicated the moment launchd brings the supervisor
back. Group-kill is the right teardown there.
