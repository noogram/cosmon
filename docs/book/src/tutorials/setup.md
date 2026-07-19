# Set up cosmon (prerequisites)

This is the first tutorial. By the end you will have every prerequisite in
place, a project that cosmon can track, and everything the next tutorial,
[Your first molecule](./first-molecule.md), needs to actually run. If
you skip this page, the `nucleate → tackle → wait → done` cycle in that tutorial
will stall on a missing tool, so do it here, once.

> **Just want `cs` on your machine?** Installing the binary is its own page —
> [Install cosmon](../getting-started/install.md) — and a condensed run through
> the whole cycle is [Ten minutes to cosmon](../getting-started/ten-minutes.md).
> This tutorial covers what a *worker* needs around the binary (git, tmux, a
> model backend) and does not repeat the install routes.

> New to the physics-inspired names (nucleate, evolve, spore, …)? You do not need
> them yet. This page installs tools; the vocabulary is introduced word by word
> as you meet it, and explained in full in
> [The physics vocabulary](../explanation/physics-vocabulary.md).

## What you are about to install

Cosmon steers *agents*, AI coding sessions that run in their own terminal, and
it keeps their state in plain files inside your project. So the prerequisites
are the things an agent needs to live in, plus the files cosmon writes to:

| Prerequisite | Why cosmon needs it | Check it is there |
|--------------|---------------------|-------------------|
| **git** | Each worker runs on its own git branch, in its own worktree. Cosmon merges that branch back when the work is done. | `git --version` |
| **tmux** | A worker is a long-lived terminal session. Cosmon spawns each one inside a tmux pane so it survives your shell closing. | `tmux -V` |
| **A model backend** | The actual worker. By default the built-in `local` adapter drives a local OpenAI-compatible endpoint (e.g. Ollama); or pass `--adapter claude` to launch an external CLI in the tmux pane. | `ollama serve` (default) or `claude --version` |
| **The `cs` binary** | Cosmon itself: a single stateless command-line tool. | `cs --help` |
| **At least one formula** | A **formula** is the recipe a piece of work follows: a small TOML file of ordered steps. Cosmon ships canonical ones (like `task-work`) so you have one on day zero. | (created by `cs init`, below) |

If the first four checks all print a version, you already have the hard part.

## Step 1: Confirm git and tmux

```sh
git --version     # e.g. git version 2.44.0
tmux -V           # e.g. tmux 3.4
```

If either is missing, install it with your platform's package manager
(`brew install git tmux`, `apt install git tmux`, …) and re-run the checks.

## Step 2: Confirm a model backend

Cosmon does not do the coding itself; it *pilots* a model that does. With no
adapter configured the **default is the built-in `local` adapter**: cosmon drives
the agent loop itself against a local OpenAI-compatible endpoint — for example
[Ollama](https://ollama.com) on `localhost:11434`. Start that endpoint before the
first dispatch:

```sh
ollama serve        # or any OpenAI-compatible endpoint on localhost:11434
```

To pilot an external coding-agent CLI instead, pass `--adapter claude` (Claude
Code), `aider`, or `codex`; those need the tool installed and authenticated on
your `PATH`. The adapter only has to be reachable by name; cosmon spawns it for
you, you never call it directly. The full resolution chain (flag →
`$COSMON_DEFAULT_ADAPTER` → config → built-in `local`) is in the
[adapter explanation](../explanation/adapter.md).

## Step 3: Install the `cs` binary

If you have not installed it yet, do it now — the routes (install script,
Homebrew, from source), version pinning, and what the one-liner actually does
line by line all live on one page:

→ **[Install cosmon](../getting-started/install.md)**

The short version, on macOS or Linux:

```sh
curl -fsSL https://noogram.org/cosmon/install.sh | sh
```

Then confirm:

```sh
cs --version
cs --help
```

You should see the command groups (lifecycle, fleet, execution, …). The full
command surface is documented in the [CLI overview](../reference/overview.md).

## Step 4: Initialise a project

Pick any repository you want cosmon to track (a Rust crate, a research repo, a
plain folder of notes) and, from its root, run:

```sh
cd ~/path/to/your/project
cs init
```

`cs init` creates a `.cosmon/` directory. That directory is where cosmon keeps
**all** of its state: the molecules you will create, their event logs, and the
canonical formulas (including `task-work`). It walks up from wherever you run
`cs`, the way `git` finds `.git/`, so once `.cosmon/` exists every later command
finds it automatically.

`cs init` is safe to run twice: if `.cosmon/` already exists it does nothing.

Confirm the formulas landed:

```sh
ls .cosmon/formulas/
```

You should see `task-work.formula.toml` among others. That is the one formula
the next tutorial uses.

## Step 5: Confirm the project is live

```sh
cs status
```

`cs status` is cosmon's `git status`: a quick read of the project's tracked work.
On a freshly initialised project it reports an empty ensemble: no molecules yet.
That empty report is success: cosmon is installed, the project is registered, and
there is nothing running.

## You are ready

You now have the four tools and an initialised project. Nothing is running, and
that is the correct resting state; an initialised project holds files, not
processes.

Go to [Your first molecule](./first-molecule.md) to create and run one unit of
tracked work end to end.

> **You do not have to type `cs` by hand.** If you already work inside an
> agentic coding CLI (Claude Code, Codex, gemini-cli, opencode, aider, …), one
> line in its context file lets you drive the same cycle in plain English —
> *"nucleate a task to … , then tackle it and wait"*. See
> [Pilot cosmon in natural language](../how-to/pilot-in-natural-language.md).
> Learn the commands here first: it is what lets you tell whether the agent is
> doing the right thing.
