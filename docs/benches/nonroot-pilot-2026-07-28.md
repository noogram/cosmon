# Non-root pilot replay, live — 2026-07-28

Two consecutive dispatches under one non-root uid, each spawning a **live
worker** that wrote and committed its own artefact, with the ownership-repair
path never entered by any process in the run.

Produced by [`scripts/container-nonroot-pilot-bench.sh`](../../scripts/container-nonroot-pilot-bench.sh).
Raw records: [`nonroot-pilot-2026-07-28/`](nonroot-pilot-2026-07-28/) —
`nonroot-pilot-record.json` is the machine-readable form of everything below.

Supports [ADR-165](../adr/165-resources-are-created-under-the-identity-that-consumes-them.md).

**Verdict: `NONROOT-PILOT-LIVE-CLEAN`.**

## The claim

**The nominal path invoked no ownership repair at all.**

That is not the same sentence as *the final owners are correct*. The second is
the neighbouring property and it is compatible with a repair having fired and
landed on the owner a path already had. Only the first is worth sending.

## Engine

```text
docker context : colima-cosmon-bench
server=29.2.1 os=Ubuntu 24.04.4 LTS arch=aarch64 kernel=6.8.0-100-generic
```

Resolved through [`scripts/lib/bench-engine.sh`](../../scripts/lib/bench-engine.sh),
which refuses INCONCLUSIVE rather than falling back to another context. This is
the tester's own bed: Colima, Lima-based, Ubuntu 24.04.4 LTS, aarch64
([engine fidelity capture](engine-fidelity-2026-07-27.md)).

## Provenance of the bytes

| | |
|---|---|
| source commit | `4c962ae898f5d7961f9645b3738d998fd923278d` |
| source tree at build | **clean** — the commit therefore describes the bytes |
| binary | `/usr/local/bin/cs`, `cs 0.3.0` |
| sha256(cs) **before** dispatch 1 | `625849bb897e551dc510067ea14c364490948af5183c5e2f058988a1cb793fe8` |
| sha256(cs) **after** dispatch 2 | `625849bb897e551dc510067ea14c364490948af5183c5e2f058988a1cb793fe8` |
| same bytes, both dispatches | **yes** |
| adapter | `2.1.220 (Claude Code)` |

The commit and the tree state are baked into the image at build time and read
by the harness from inside it, so the report cannot name a commit the binary
was not built from. The driver now also **refuses to build from a dirty tree**
— learned the hard way on this very molecule: a build launched from a clean
tree and then edited while `cargo` was still running inside the image copies
the edited files and still stamps the old commit as clean.

The harness also verifies the instrument is present in the binary under test.
Without that check an empty journal would be ambiguous between *no repair
fired* and *this binary never writes a journal*.

## Credential

Supplied by the operator, for this replay, with his explicit authorisation.

| | |
|---|---|
| route | read-only bind mount → `CLAUDE_CODE_OAUTH_TOKEN` in the **dispatcher's** environment |
| which route that is | (a) of the three the credential refusal itself names |
| what is recorded | that a token was supplied, and by which route |
| what is never recorded | the value, any prefix of it, or any hash of it |

The value is read into the environment in one gesture and never written to a
file this capture touches. It is passed to `docker run` as a mount, never as
`-e`, so it does not appear in the host's process arguments either. The config
dir stays **virgin at tackle time** and is only ever `stat`ed —
`.credentials.json` is never opened. Before anything is committed the driver
greps every artefact for the value and destroys the records rather than let a
capture leak the key it used; that gate passed with no occurrence.

## The invocation

```sh
docker exec -u 10001:10001 \
  -e HOME=/home/cosmon-worker \
  -e LC_ALL=C.UTF-8 \
  -w /home/cosmon-worker <container> /usr/local/bin/container-nonroot-pilot
```

Asserted inside, not assumed: `id -u` = 10001 (≠ 0), `HOME` set and writable by
that uid, `COSMON_WORKER_UID` **unset**. A partial invocation — `-u` without an
explicit `HOME` — measures the neighbouring property, because it leaves
`HOME=/root`, mode 0700, which is defect #2 of the four reintroduced by an
incomplete command.

The container name is derived per run. A fixed name is a shared resource
exactly like a shared file, and one destroyed a live mission this week.

## Method — how the absence of a repair was observed

**Route taken: instrumentation (a).** `cs` counts every **entry** into the
ownership-repair path, at two granularities and both *before any precondition
is examined*: once when `provision_and_decide_root_spawn` decides a demote is
on the table — so a run that traverses the machinery and finds nothing to do
still reads non-zero — and once per path handed to a chown. Each entry appends
one line to the file named by `COSMON_OWNERSHIP_TRANSFER_JOURNAL`, carrying the
pid that wrote it. **One journal file per dispatch** attributes each number to
one dispatch; the pid attributes it to one process. The worker's own `cs`
invocations inherit the variable, so the observation covers the worker and not
only the dispatcher.

**What would have been observed had a chown fired:** that dispatch's journal
would contain at least one `enter-repair-path to_uid=<uid>` line, followed by
one `chown tree|node uid=<uid> <path>` line per path touched. Each journal is
committed with a header stating this and its own event count, so a journal with
no events is a file that *says* no events rather than an absent file:

```text
# ownership-repair journal — dispatch 1: cs tackle and the worker it spawned
# every event line begins `pid=`; 0 event(s) recorded.
# an entry would read: pid=<pid> enter-repair-path to_uid=<uid>
# followed by:         pid=<pid> chown tree|node uid=<uid> <path>
# NO EVENTS. The repair path was not entered by any process here.
```

**Why not final-state ownership:** a chown onto the owner a path already had
leaves nothing a `stat` can see, so a final-state assertion cannot distinguish
*the repair never ran* from *the repair ran and changed nothing*. Ownership is
reported below as context, never as the evidence.

**Why not syscall tracing (b):** the engine's seccomp posture refuses `ptrace`
for root and unprivileged uids alike — the same posture that refuses `unshare`,
recorded in [engine-fidelity-2026-07-27.md](engine-fidelity-2026-07-27.md). A
tracing route would have been cause-not-isolated, so it was not attempted and
final state was not quietly substituted for it.

The instrument is pinned by two unit tests in
`crates/cosmon-transport/src/demote_provisioning.rs`:
`an_ownership_transfer_is_counted_even_when_it_changes_nothing` and
`entering_the_repair_path_counts_even_with_nothing_to_repair`.

### When the counter starts carrying information

**Now, and not before.** In the earlier credential-less run the counter read
zero because `demotion_configured: false` meant the repair path could not be
entered at all — zero *by construction*, not zero *by behaviour*. That number
proved nothing and is not reused here. Once a worker really spawns, does real
work, and commits through a linked worktree, the same zero is a statement about
what the code did. Every number below was measured again, per dispatch, from
the processes under test.

## Result

| | dispatch 1 | dispatch 2 |
|---|---|---|
| molecule | `task-20260727-e738` | `task-20260727-23bd` |
| worker spawned (live) | **yes** — tmux `create-file-named-artifact-1-e738` | **yes** — tmux `create-file-named-artifact-2-23bd` |
| commit **by the worker** | `94aeb95368230219787fac3c575ea16ebb96786d` | `2860b9d6e0e7d5c002e968ca639fb48f87b8ca80` |
| branch it is attributed to | `feat/task-20260727-e738` | `feat/task-20260727-23bd` |
| repair-path entries, dispatch | **0** | **0** |
| repair-path entries, commit-wait | **0** | **0** |

**Total repair-path entries across the whole run: 0.**

Nothing was touched between the two dispatches — no chown, no config edit, no
login, no reseed, no ownership exemption. A setup that passes once and fails the
second time is the trap this whole arc was about.

Liveness is asserted **positively**: `cs tackle`'s exit code proves a process
exited, never that a worker exists, so the tmux session `cs` named is probed
with `has-session` — the kernel answering rather than us inferring.

The commits are verified as commit **objects in the repository**
(`git -C <repo> cat-file -t <sha>` → `commit`) and each is checked reachable
from its own molecule's branch, so one commit cannot be counted twice or
credited to the wrong dispatch. The committed artefact is the worker's own
answer to *"what uid are you?"*:

```text
commit   2860b9d6e0e7d5c002e968ca639fb48f87b8ca80
cat-file -t -> commit
subject  feat: artifact 2
--- ARTIFACT-2.md as committed ---
10001
```

Also measured, as context rather than as evidence:

- every path under the galaxy is owned by uid 10001 **by creation**; no `chown`
  was run and there was nothing for one to do;
- git operates the repository with **no `safe.directory` exemption** for anyone
  — one uid created it and one uid operates it;
- each molecule's state dir under `.cosmon/state/` exists and is owned by
  10001, written by the worker's own `cs` from inside its worktree.

## What this reaches, and what it does not

It reaches the whole corridor: dispatch → live worker → work → commit through a
linked worktree's gitdir and the repository's common dir → molecule state
written out-of-worktree. That is the full set of resources the four hand-over
defects lived in, exercised by a real worker under one identity.

It does not reach `cs done`. Merging to trunk is deliberately outside this
replay, and the authorisation question it raises is opened as its own decision
(`task-20260727-7f01`), not answered here — see ADR-165, *The boundary we give
up*.

## Reproducing it

```sh
scripts/container-nonroot-pilot-bench.sh
```

Needs an operator-supplied token at `~/.cosmon/claude-oat.token` (override with
`COSMON_OAT_TOKEN_FILE`). Without one the harness still runs and reports
`REFUSED-AT-CREDENTIAL-GATE`, naming in its own record that the counter's zero
is then by construction rather than by behaviour.
