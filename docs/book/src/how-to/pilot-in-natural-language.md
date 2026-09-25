# Pilot cosmon in natural language

**Goal:** drive cosmon by *saying what you want* — "nucleate a task to fix the
flaky parser test, then tackle it and wait" — instead of typing `cs` commands by
hand. You do this by pointing an agentic coding CLI at cosmon's own help
surface, once, in a single line of config.

> This is not a new cosmon feature or a plugin. `cs` is a plain command-line
> tool with a self-describing help surface. Any agent that can read `cs help`
> and run a shell command can already pilot cosmon. What is missing is a
> pointer telling it that cosmon is there — a `CLAUDE.md`/`AGENTS.md` section
> for every CLI, and (for Claude Code specifically) a skill that carries the
> same text without needing `cs init` first. See
> [Claude Code: a skill, not just a pointer](#claude-code-a-skill-not-just-a-pointer).

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

````markdown
## Cosmon — Orchestration

Run `cs help` for the full command reference, `cs help guide` for the operator handbook,
and `man cs` for the manual page.

Source of truth: `.cosmon/state/` (JSON). Surfaces are projections — never edit directly.

The normal cycle for one unit of work:
```
cs nucleate task-work --var-file topic=<file>   # a long statement goes in a file, not a flag
cs tackle <id> --adapter claude                 # spawn a worker; omit --adapter for the project default
cs peek                                         # watch it work, or `cs wait <id>` to block
cs whisper <id> --file correction.md            # send a correction while the worker is still open
cs done <id>                                    # merge to the base branch + teardown (required)
```
Use `cs tackle`, not `cs run`, when you intend to read the result before merging: `cs run`
walks a whole DAG and calls `cs done` on completion itself, closing the review window.
````

That is the whole transport (`cs init --upgrade` writes and maintains it for you,
inside marker comments it can update without touching anything else you add to the
file). It carries no secrets and pins no versions: it names the tool, states that it
is on `PATH`, points at `cs help` for everything else, and spells out the one loop
that is easy to get wrong from `cs help` alone — a long statement belongs in a file
bound with `--var-file`, not typed into `--var`; a worker keeps running after `wait`
returns, so a correction goes through `cs whisper --file` before `cs done`; and
`cs tackle` (not `cs run`) is the verb that leaves the review window open.

Keep it minimal on purpose. A long transcription of cosmon's commands into your
context file is a second copy of the reference that will rot; the block above
delegates to the copy that ships with the binary. The same source text also ships
as a Claude Code skill — see [Which CLIs work](#which-clis-work) below.

## Step 3: Speak

With the pointer in place, you talk to your coding CLI normally:

> *"Nucleate a task to fix the flaky parser test, then tackle it with Claude and wait for it."*

and it resolves that into the cycle:

```sh
cs nucleate task-work --var topic="fix the flaky parser test"
cs tackle task-20260716-1a2b --adapter claude
cs wait   task-20260716-1a2b
```

A statement that runs to more than a sentence belongs in a file instead, so it
survives verbatim (front matter, quotes, accents, multiple pages) rather than
being squeezed through shell quoting:

```sh
cs nucleate task-work --var-file topic=enonce.md
```

**Reread, correct, accept** is the loop measured against a real worker: `cs
wait` returns as soon as the worker reaches `completed`, but its tmux pane,
worktree, and branch all survive that state, so you can still act on it.
Reread with `cs peek` or `git diff main...feat/<id>`; if something is off, send
a correction with `cs whisper <id> --file correction.md` — the fix lands as a
new commit on the same branch. Only `cs done <id>` merges it, so use `cs
tackle`, never `cs run`, whenever you intend to read a result before it
merges — `cs run` walks a whole DAG and calls `cs done` on completion itself,
which closes the review window before you get to look.

Other phrasings map the same way — *"what's running right now?"* becomes
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

## Claude Code: a skill, not just a pointer

Claude Code users have a second option that needs no `cs init` in the target
repository first: the same Orchestration text above also ships as a Claude Code
skill, `tools/cosmon-skill/SKILL.md` in this repository. One source text, two
renderings — the `## Cosmon — Orchestration` block `cs init --upgrade` writes
into `CLAUDE.md`/`AGENTS.md`, and this skill file — generated from the same
constant (`cosmon_filestore::project_upgrade::generate_cosmon_skill_md`), so a
test fails if the two ever say something different.

Install it once, at the user level, and it loads on demand in every
repository — including one that has never run `cs init`:

```sh
./tools/cosmon-skill/install.sh
```

That copies `SKILL.md` into `~/.claude/skills/cosmon/` (idempotent — safe to
re-run after an update). To scope it to a single project instead, copy or
symlink `SKILL.md` into that project's `.claude/skills/cosmon/` directory.
Either way, the skill costs nothing when unused — Claude Code only loads it
when you ask it to pilot cosmon — and it carries the same content as the
CLAUDE.md/AGENTS.md pointer, so a repository with `cs init` already run and a
repository without it read the same instructions.

## Related

- [Install cosmon](../getting-started/install.md) — putting `cs` on your `PATH`.
- [Set up cosmon (prerequisites)](../tutorials/setup.md) — the manual driving path.
- [CLI overview](../reference/overview.md) — the same surface `cs help` prints.
