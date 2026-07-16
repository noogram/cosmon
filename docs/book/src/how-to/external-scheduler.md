# Wire cosmon into an external scheduler

**Goal:** drive cosmon from *your* scheduler (cron, a systemd timer, a CI job, a
platform runtime) instead of leaving a `cs run` in the foreground. Cosmon is a
stateless CLI, so it composes with any scheduler the same way `git` does: you
call one-shot commands on a clock you own.

> Cosmon has two layers. The **Transactional Core** (`cs tackle`, `cs done`,
> `cs reconcile`, …) is stateless and git-like: one decision per invocation,
> files on disk are the truth. The **Resident Runtime** (`cs run`) is a
> long-lived *client* of that core; it owns no private truth and holds no lock.
> An external scheduler drives the core directly and never needs the runtime.
> See [Why a stateless CLI](../explanation/stateless-cli.md).

## Why this works: no daemon, no lock

Every `cs` command reads the on-disk state, changes it, writes it back, and
exits. There is no background process holding truth in RAM, so it is always safe
for an external scheduler to invoke `cs` on a timer. Two invocations never race
over a lock, because there is no lock: the state file is the single point of
coordination, and each command is one transaction against it.

## Pattern A: tick a DAG from cron

If you want cosmon to advance a dependency graph but do not want a long-running
`cs run`, drive it one tick at a time. Point your scheduler at a bounded run:

```sh
# crontab entry: every 5 minutes, advance the DAG rooted at <root>, then exit.
*/5 * * * *  cd /path/to/project && cs run <root> --timeout 60
```

`--timeout 60` bounds each invocation (exit code 124 on deadline), so the job
always returns and the scheduler owns the cadence. Because the runtime is a pure
client of the on-disk state, the next tick resumes exactly where the last one
left off; nothing is lost between fires.

## Pattern B: a hand-rolled tackle/done loop

For full control, script the Transactional Core verbs directly. This walks a
linear chain sequentially, one molecule per scheduler tick or all at once:

```sh
for mol in <A> <B> <C>; do
    cs tackle "$mol"     # spawn one worker on this node
    cs wait "$mol"       # block until it reaches a terminal state
    cs done "$mol"       # merge its branch, tear down, unblock the next
done
```

`cs tackle` puts a molecule into motion; `cs wait` blocks until it finishes;
`cs done` merges and cleans up. Your scheduler decides *when* to run the loop;
cosmon decides *what* each step means.

## Pattern C: the built-in patrol scheduler

Cosmon ships its own lightweight scheduler for recurring fleet maintenance,
configured by a `patrols.toml` file. Lint it before it ever fires: a zero-side-
effect pre-flight that doubles as a CI gate:

```sh
cs scheduler validate       # parse & validate patrols.toml, no dispatch
cs scheduler status         # last-known state of every patrol
```

`cs scheduler` is a **read-only** view onto the scheduler's state; adding or
editing a patrol is a `patrols.toml` edit, validated with the command above.

## Keeping projected surfaces fresh

Any batch of molecule changes can leave cosmon's projected surfaces
(`STATUS.md`, `ISSUES.md`, …) stale. Have your scheduler reconcile after a batch:

```sh
cs reconcile            # project current state onto all surfaces
cs reconcile --check    # dry-run; exit 1 if surfaces are stale (a CI gate)
```

`cs reconcile` is strictly idempotent (running it twice is the same as once) so
it is safe to call on every tick. `--check` makes it a CI guard that fails the
build when a surface has drifted.

## Reacting to events (notifications)

To push cosmon events to an external system, pipe its NDJSON event stream into a
hook script. Cosmon emits one JSON object per event; a hook reads them on stdin
and does whatever you need (post to chat, page on error). Filter to the kinds you
care about:

```sh
cs tail --follow --json | your-hook.sh
```

Event kinds include `worker_spawned`, `worker_terminated`,
`molecule_transitioned`, `step_completed`, and `error_occurred`. A hook can
forward only a subset (e.g. errors and terminations) and ignore the rest.

## What not to do

- **Do not run a per-project cosmon daemon.** It is prohibited by cosmon's
  architecture: the core is stateless on purpose. Your scheduler *is* the
  daemon; `cs` is the one-shot tool it calls.
- **Do not leave `cs run` in a foreground shell you depend on.** If you want it
  resident, detach it: `tmux new -d -s runtime cs run <root>`.

## See also

- [Why a stateless CLI](../explanation/stateless-cli.md): the design that makes
  external scheduling safe.
- [The three regimes](../explanation/regimes.md): who holds the clock.
- [Composing a DAG](../tutorials/first-dag.md): `cs run` in the foreground.
