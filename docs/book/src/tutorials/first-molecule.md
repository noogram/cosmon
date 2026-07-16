# Your first molecule

In this tutorial you will create one piece of tracked work, hand it to an AI
worker, wait for it to finish, and close the loop: the full `nucleate → tackle →
wait → done` cycle that every piece of work in cosmon goes through. By the end
you will have watched a molecule go from nothing to a merged result.

**Before you start:** finish [Set up cosmon](./setup.md). You need `cs`, tmux, an
agent adapter, git, and a project where you have run `cs init`.

> The commands below use physics-inspired names. Each one is glossed the first
> time it appears; the full story is in
> [The physics vocabulary](../explanation/physics-vocabulary.md).

## The four verbs, in one picture

Cosmon runs every piece of work through the same four-step loop:

```
cs nucleate   →   cs tackle   →   cs wait   →   cs done
  create          start a         block until    merge the
  the work        worker on it    it finishes    result, clean up
```

You will run each verb once.

## Step 1: Nucleate a molecule

From your project root:

```sh
cs nucleate task-work --var topic="Add a --version flag to the CLI"
```

Two new words here:

- A **molecule** is cosmon's unit of tracked work: one running instance of a
  recipe, bound to a task. It has a state, a current step, and a durable trace on
  disk. Think of it as a single job with a memory.
- A **formula** is that recipe: `task-work` is a formula, a small TOML template
  of ordered steps ("implement", then "verify"). The molecule is one *run* of
  that formula, the way an object is one instance of a class.

**Nucleate** means *create the molecule from the formula*: pure creation,
nothing executes yet. The command prints the new molecule's id:

```
Nucleated task-20260711-a1b2 (task-work): pending
```

Copy that id; you will use it in the next three steps. (Your id will differ;
substitute it everywhere you see `task-20260711-a1b2` below.)

Confirm it exists and is `pending`:

```sh
cs observe task-20260711-a1b2
```

`cs observe` is a one-shot read of a single molecule's state. It reports
`pending`: the molecule exists on disk but no one is working on it yet.

## Step 2: Tackle it

```sh
cs tackle task-20260711-a1b2
```

**Tackle** is the verb that puts a molecule into motion. In one command cosmon:

1. creates a git worktree and a branch for this molecule,
2. opens a tmux session, and
3. launches your agent adapter inside it, with the molecule's briefing injected.

The agent is now a **worker**: a live process, in its own pane, driving this one
molecule. It reads the formula's steps and executes them, recording its progress
as it goes.

`cs tackle` returns immediately; it does **not** wait for the work to finish. The
worker runs in the background tmux session while your shell stays free.

## Step 3: Wait for it

You do not poll by hand. Ask cosmon to notify you:

```sh
cs wait task-20260711-a1b2
```

`cs wait` blocks until the molecule reaches a terminal state (completed or
collapsed), then returns. While it blocks, the worker is stepping through the
formula: implementing, then verifying, committing its work to the molecule's
branch at each step.

When `cs wait` returns, the molecule has finished its steps and marked itself
**completed**, but its work is still on its own branch, not yet in your `main`.

> **Tip.** In real use you background the wait (`cs wait <id> &`) so you can do
> other things while the worker runs, and get notified on completion. For this
> first run, a foreground wait is fine; it just sits there until the worker is
> done.

If you want to watch it work while you wait, open a second terminal and run
`cs peek`, the fleet portal shown in the next tutorial.

## Step 4: Done

```sh
cs done task-20260711-a1b2
```

**Done** closes the loop. It merges the worker's branch back into `main`, kills
the tmux session, and removes the worktree. After it returns, the work is in your
main branch and nothing is left running.

`cs done` is the *only* verb that merges and tears down; a worker can finish its
own steps, but it cannot merge itself. That is a human's call, which is why you
run `cs done`, not the worker.

Confirm the loop is closed:

```sh
cs status
```

The molecule now shows as completed, and the ensemble has no running workers.

## What just happened

You ran one molecule through its whole life:

| Verb | What it did | State after |
|------|-------------|-------------|
| `cs nucleate` | Created the molecule from the `task-work` formula | pending |
| `cs tackle` | Spawned a worker to drive it | running |
| `cs wait` | Blocked until the worker finished its steps | completed |
| `cs done` | Merged the branch and cleaned up | completed + merged |

The molecule's full trace (every step, every commit) is on disk in
`.cosmon/state/`, and survives long after the worker's tmux pane is gone. That
on-disk trace is the whole point: see
[Crash recovery](../explanation/crash-recovery.md) for why a worker dying never
loses your work.

## Next

- Run several molecules at once and watch them live:
  [Running a fleet of agents](./first-fleet.md).
- Wire molecules into a dependency graph: [Composing a DAG](./first-dag.md).
- See every verb grouped by role: the
  [Molecule lifecycle reference](../reference/lifecycle.md).
