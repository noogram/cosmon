# Ten minutes to cosmon

This is the shortest path from an empty terminal to one piece of work that an AI
agent did, finished, and merged into your `main`. Every block below is meant to
be copied and run in order.

If you want the same journey with the *why* behind each verb, take the
[tutorials](../tutorials/setup.md) instead — this page is the ramp, they are the
lesson.

## 0. What you need

Three things beyond `cs` itself, because a cosmon worker is a real terminal
session doing real git work:

```sh
git --version     # each worker runs on its own branch, in its own worktree
tmux -V           # each worker lives in a tmux session
ollama serve      # a model backend on localhost:11434 (the default adapter)
```

Missing one? `brew install git tmux` / `apt install git tmux`, and see
[Set up cosmon](../tutorials/setup.md) for the backend options (a local
OpenAI-compatible endpoint by default, or `--adapter claude` / `aider` / `codex`
to pilot a coding-agent CLI you already use).

## 1. Install `cs`

```sh
curl -fsSL https://noogram.org/cosmon/install.sh | sh
cs --version
```

Homebrew and from-source routes, plus what that script does line by line, are on
[Install cosmon](./install.md).

## 2. Initialise a project

From the root of any repository you want cosmon to track:

```sh
cd ~/path/to/your/project
cs init
```

That creates `.cosmon/`, the directory holding **all** of cosmon's state: your
units of work, their event logs, and the canonical recipes. Later commands walk
up to find it, the way `git` finds `.git/`. Running it twice is safe.

## 3. Create a unit of work

```sh
cs nucleate task-work --var topic="Add a --version flag to the CLI"
```

A **formula** (`task-work`) is the recipe: a small TOML file of ordered steps. A
**molecule** is one run of that recipe — cosmon's unit of tracked work, with a
state, a current step, and a durable trace on disk.

**Nucleate** creates the molecule. Nothing executes yet. The command prints an
id:

```
Nucleated task-20260711-a1b2 (task-work): pending
```

Yours will differ; substitute it everywhere below.

## 4. Put it in motion

```sh
cs tackle task-20260711-a1b2
```

**Tackle** creates a git worktree and branch for this molecule, opens a tmux
session, and launches your agent inside it with the briefing injected. It returns
immediately — the worker runs in the background while your shell stays free.

## 5. Watch it work

```sh
cs peek
```

`cs peek` is the fleet portal: workers on the left, the selection followed on the
right. Press `p` to drop into the live pane of the selected worker, `q` to come
back up. Never `tmux attach` — it breaks the agent's rendering.

To block until the work is finished instead of watching it:

```sh
cs wait task-20260711-a1b2
```

## 6. Close the loop

```sh
cs done task-20260711-a1b2
```

**Done** merges the worker's branch into `main`, kills the tmux session, and
removes the worktree. It is the *only* verb that merges: a worker can finish its
own steps, but it cannot merge itself — that call stays yours.

Confirm:

```sh
cs status
```

The molecule shows as completed and no worker is running. That empty ensemble is
the correct resting state.

## The whole cycle, in one picture

```
cs nucleate   →   cs tackle   →   cs wait   →   cs done
  create          start a         block until    merge the
  the work        worker on it    it finishes    result, clean up
```

Everything the worker did is still on disk under `.cosmon/`, long after the tmux
pane is gone. That on-disk trace is the point: a worker dying never loses your
work.

## Where to go next

- **Understand what you just ran** — [Your first molecule](../tutorials/first-molecule.md)
  walks the same four verbs slowly, with the vocabulary explained.
- **Run several at once** — [Running a fleet of agents](../tutorials/first-fleet.md).
- **Chain work into a graph** — [Composing a DAG](../tutorials/first-dag.md).
- **Look up a command** — the [CLI reference](../reference/overview.md), generated
  from the tool itself.
- **Drive it in plain English** — [Pilot cosmon in natural language](../how-to/pilot-in-natural-language.md).
