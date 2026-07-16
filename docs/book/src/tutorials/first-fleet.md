# Running a fleet of agents

The whole reason cosmon exists is to run *many* agents at once without losing
track of who is doing what. In this tutorial you will start three independent
workers in parallel and watch them from a single live portal, `cs peek`. By the
end you will have three molecules running side by side and know how to read the
fleet at a glance.

**Before you start:** finish [Your first molecule](./first-molecule.md). You
should be comfortable with `nucleate → tackle → wait → done` for one molecule.

> The **ensemble** is the whole fleet: every molecule and worker seen at once.
> That is the word cosmon uses for "all your running work together."

## Step 1: Nucleate three independent molecules

These three tasks have nothing to do with each other, so they can all run at the
same time. Nucleate one after another:

```sh
cs nucleate task-work --var topic="Write the README quickstart"     # → mol A
cs nucleate task-work --var topic="Add unit tests for the parser"   # → mol B
cs nucleate task-work --var topic="Fix the changelog formatting"    # → mol C
```

Each prints its own id. Note all three (referred to below as A, B, C). Confirm
they are all pending:

```sh
cs status
```

You should see three pending molecules and no workers.

## Step 2: Tackle all three in parallel

Because the molecules are independent, you can put them all into motion at once.
Tackle each; the command returns immediately, so three quick calls launch three
workers:

```sh
cs tackle <A>
cs tackle <B>
cs tackle <C>
```

Each `cs tackle` gets its **own** worktree, its **own** tmux session, and its
**own** git branch, so the three workers never collide: they edit separate
checkouts of the repo and only meet again at merge time.

## Step 3: Watch the fleet with `cs peek`

Now the payoff. Instead of attaching to three terminals, open one portal:

```sh
cs peek
```

`cs peek` is cosmon's fleet observation command: a TUI (terminal UI) that shows
every worker in the ensemble on the left and a detail view on the right. It is
the *one* tool you reach for to watch a fleet; it is a plain window, not an action
on your molecules, so it can never disturb them.

Keys inside `cs peek`:

| Key | What it does |
|-----|--------------|
| `j` / `k` | Move the selection down / up the worker list |
| `p` | Show the selected worker's live tmux pane: what the agent is doing *right now* |
| `b` | Briefing: the plan the worker is following |
| `l` | Log: the worker's step-by-step history |
| `e` | Events: the raw event stream |
| `q` | Quit the portal |

Press `j`/`k` to move between your three workers and `p` to drop into each one's
live output. This is the "fractal descent" cosmon is built around: one portal,
one keystroke down to any single worker, back up with one keystroke.

> **Do not** `tmux attach` to a worker's session to check on it. That breaks the
> agent's rendering and confuses it. `cs peek` + `p` shows you the same pane
> read-only, which is always what you want.

## Step 4: Read the ensemble as a table

`cs peek` is the live portal; for a one-shot snapshot (handy in scripts) use
`cs ensemble`:

```sh
cs ensemble
```

```
NAME          ROLE            EFFECTIVE   LIVE            COST    MOLECULE
worker-A      Implementation  healthy     working:step 1  $0.42   <A>
worker-B      Implementation  healthy     working:step 2  $0.31   <B>
worker-C      Implementation  suspect     stale           $0.20   <C>
```

The `EFFECTIVE` / `LIVE` columns tell you the health of each worker at a glance:
`healthy` and `working` is good; `suspect` / `stale` means a worker has stopped
making progress (you will handle that case in
[Recover a crashed agent](../how-to/recover-crashed-agent.md)). Add `--json` for
machine-readable output.

## Step 5: Wait, then close each loop

Background a wait on each molecule so you are notified as they finish:

```sh
cs wait <A> &
cs wait <B> &
cs wait <C> &
```

The `&` sends each wait to the background, so your shell stays responsive and all
three notify you independently. As each molecule completes, close its loop with
`cs done`:

```sh
cs done <A>
cs done <B>
cs done <C>
```

Because the three workers touched different files, all three branches merge
cleanly into `main`. (When parallel workers *do* touch the same file, `cs done`
detects the conflict, aborts the merge cleanly, and prints exact recovery
commands; you will see that in [Composing a DAG](./first-dag.md).)

## What just happened

You ran a real fleet: three agents, three branches, one portal. The pattern
scales: ten workers read the same way as three, because `cs peek` and
`cs ensemble` always show the *whole* ensemble, never one session at a time.

## Next

- Make molecules depend on each other instead of running independently:
  [Composing a DAG](./first-dag.md).
- Go deeper on the monitoring toolkit:
  [Monitor the fleet with cs peek](../how-to/monitor-with-peek.md).
