# Crash recovery: state on disk, not in RAM

> These commands use physics-inspired names (nucleate, evolve, reconcile, …). New
> to the vocabulary? See [The physics vocabulary](./physics-vocabulary.md).

This is the headline wedge. AI agents crash. They run out of context window, the
laptop reboots, a tmux session dies, a model call times out. In a system that
kept its truth inside a running process, a crash would lose that truth. Cosmon is
built so that **a crash loses nothing**, and the reason is one sentence: the
authoritative state is always on disk, never only in RAM.

## The mechanism, in one loop

Every `cs` command is a discrete transaction: **read the files, change them,
write them back, exit.** After the command returns, the full truth of the system
is sitting on disk in `.cosmon/state/`: a molecule's `state.json`, its
append-only `events.jsonl`, its tracked markdown trace. There is no in-memory
invariant that the files do not also record.

So recovery is not a special mode you switch into. It is just: **run the next
command.** The next `cs evolve`, the next `cs wait`, the next `cs run` re-reads
the files and continues from exactly where they say you were. A restarted
resident runtime is indistinguishable from a fresh one; it rebuilds its whole
picture from the same JSON the CLI reads. There is no crash-recovery *procedure*
because statelessness makes recovery the default.

## Desired vs Observed vs Effective: telling wish from reality

A crash creates a gap between what you *wanted* and what is *actually true*. You
asked for a worker to be running; the process died; the tmux pane may or may not
still linger as a zombie. Cosmon resolves this gap by never storing a single
muddy "status." It computes health fresh, from three separate axes:

- **Desired** is what *you* asked for, and the only thing persisted: `Running`,
  `Paused`, or `Stopped`. Stored in `fleet.json`.
- **Observed** is what *reality* says right now, computed fresh and never stored:
  is the transport (tmux) alive, dead, or unknown? is the session idle, working,
  or blocked? is the agent's own cognitive trace fresh or stale?
- **Effective** is the honest verdict, `reconcile(desired, observed)`: `Healthy`,
  `Diverged`, `Suspect`, `Blocked`, `Paused`, `Stopped`, or `Error`.

Because *desired* is the only thing written down, and *observed* is always
recomputed, a crash cannot leave a lie on disk. If you asked for `Running` and
the process is `Dead`, reconcile reports `Diverged`, and it says so every time,
because it re-measures reality rather than trusting a stored flag.

| What happened | Desired | Transport | Effective | What cosmon does |
|---------------|---------|-----------|-----------|------------------|
| Agent working normally | Running | Alive, working | **Healthy** | Nothing, reality matches intent |
| Agent crashed | Running | Dead | **Diverged** | Respawn (until the restart limit) |
| Zombie: killed but tmux lingers | Stopped | Alive, idle | **Diverged** | Kill the stray session |
| Stuck on a permission prompt | Running | Alive, blocked | **Blocked** | Surface it; a human is needed |
| Restart limit hit | Running | Dead | **Error** | Circuit-break; stop retrying |

That last row matters: recovery is bounded. A worker that keeps dying is not
respawned forever; after a set number of failures the circuit breaks and the
molecule is flagged `Error` for a human, rather than looping on a broken task.

## Reconcile is a pure projection

Because all the authoritative content lives on disk, rebuilding every *derived*
view is a deterministic function of the files. `cs reconcile` takes the state and
re-projects it onto the surfaces humans and other tools read (status files,
issue lists, dashboards) and it is **idempotent by construction**: run it once
or run it ten times, you get the same result. This is enforced by tests. A
projection that could drift on a second run would be a bug, because there is
exactly one source of truth and reconcile only ever *reads* it.

## Why this is the wedge, not a feature

Other orchestrators can restart a failed *function*: re-run the code and hope it
was idempotent. Cosmon restarts a failed *entity*: the same worker, on the same
molecule, resuming the same task with its predecessors' work already merged into
its worktree. That is only possible because identity and state were never trapped
in a process that could die. They were on disk the whole time, waiting for the
next command to pick them up.

See [Why a stateless CLI](./stateless-cli.md) for the design bet this rests on,
and [Control plane vs data plane](./control-vs-data-plane.md) for why no message
was ever in flight to lose.
