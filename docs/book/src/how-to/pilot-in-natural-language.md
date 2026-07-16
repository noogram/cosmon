# Pilot cosmon in natural language

**Goal:** drive cosmon by *saying what you want* — "nucleate a task to fix the
flaky parser test, then tackle it and wait" — instead of typing `cs` commands by
hand. You do this by pointing an agentic coding CLI at cosmon's own help
surface, once, in a single line of config.

> This is not a new cosmon feature or a plugin. `cs` is a plain command-line
> tool with a self-describing help surface. Any agent that can read `cs help`
> and run a shell command can already pilot cosmon. The only thing missing is
> a pointer telling it that cosmon is there.

## The idea in one picture

An agentic CLI (Claude Code, Codex, gemini-cli, opencode, aider, …) reads a
context file in your repository when it starts. If that file says *"to operate
cosmon, run `cs help`"*, the agent discovers the whole command surface on its
own, and your English turns into the right `cs` invocations.

```text
you (English)  →  agentic CLI  →  reads `cs help`  →  runs `cs nucleate …`
```

Three steps: install `cs`, add the pointer, speak.

## Step 1: Install `cs`

```sh
curl -fsSL https://noogram.org/cosmon/install.sh | sh
```

This installs a single binary into `~/.local/bin` (falling back to
`/usr/local/bin`), verifying its checksum against the release `SHA256SUMS`. If
that directory is not on your `PATH`, the installer says so and prints the line
to add. Confirm:

```sh
cs --version
cs help          # the surface the agent will read
```

`cs help` is the load-bearing part. It prints every command grouped by theme
(molecule lifecycle, fleet management, execution, …) with a one-line
description each, and every subcommand takes `--help`. That is enough for an
agent to work out the vocabulary without any further documentation.

> **`man cs` is a contributor extra, not part of this path.** The published
> installer ships the `cs` binary and nothing else, so do not expect a man page
> on a fresh machine. Contributors who build from the repository get one via
> `just install`. Point your agent at `cs help`, which is always present.

## Step 2: Add the pointer to your agent's context file

Each CLI reads its own file at startup — commonly `AGENTS.md` or `CLAUDE.md` at
the repository root; check your tool's documentation for the exact name. Add a
short section like this:

```markdown
## Orchestration

This project is orchestrated with cosmon. The `cs` binary is on `PATH`.

To operate it, discover the command surface first: run `cs help` for the
grouped command list, and `cs <command> --help` for any single command.
Do not guess flags — read the help output.

The normal cycle for one unit of work is:
nucleate → tackle → wait → done.
```

That is the whole transport. It carries no secrets and pins no versions: it
names the tool, states that it is on `PATH`, and tells the agent to read the
help rather than invent flags. Because the pointer defers to `cs help` instead
of restating commands, it cannot drift out of date when cosmon's surface
changes.

Keep it minimal on purpose. A long transcription of cosmon's commands into your
context file is a second copy of the reference that will rot; the two lines
above delegate to the copy that ships with the binary.

## Step 3: Speak

With the pointer in place, you talk to your coding CLI normally:

> *"Nucleate a task to fix the flaky parser test, then tackle it and wait for it."*

and it resolves that into the cycle:

```sh
cs nucleate task-work --kind task --var topic="fix the flaky parser test"
cs tackle task-20260716-1a2b
cs wait   task-20260716-1a2b
cs done   task-20260716-1a2b
```

You stay in the loop: the agent proposes the commands, you watch the molecule
run. Other phrasings map the same way — *"what's running right now?"* becomes
`cs status` or `cs ensemble`, *"show me what that worker is doing"* becomes
`cs peek`.

If the agent guesses a flag that does not exist, that is the signal your pointer
is being skipped — make sure the context file is at the repository root and that
your CLI actually loads it.

## Which CLIs work

Any coding agent that can read a context file and run shell commands. That
includes Claude Code, Codex, gemini-cli, opencode, and aider, among others.
Cosmon does not integrate with them individually and does not detect which one
you are using — it exposes `cs help` and they read it. Support is therefore a
property of the CLI (does it read a context file? can it run a shell command?),
not something cosmon maintains per tool.

> This is a different axis from `--adapter`. Here an agentic CLI drives cosmon
> *from the outside*, translating your English into `cs` commands. The
> [adapter](../explanation/adapter.md) is the reverse: cosmon spawning a model
> *underneath* to do the work of a molecule. You can use either alone, or both.

## Related

- [Set up cosmon (prerequisites)](../tutorials/setup.md) — the manual install path.
- [CLI overview](../reference/overview.md) — the same surface `cs help` prints.
