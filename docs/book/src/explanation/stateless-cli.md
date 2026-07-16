# Why a stateless CLI (no daemon)

> These commands use physics-inspired names (nucleate, evolve, decay, …). New to
> the vocabulary? See [The physics vocabulary](./physics-vocabulary.md).

Most orchestration tools are a *server you run*. There is a scheduler process, a
database process, maybe a message broker, and your tasks live inside them. If
that process dies, or you did not start it, nothing works. Cosmon takes the
opposite bet: **there is no process in the loop.** The `cs` binary is a one-shot
tool, like `git`. You run it, it reads some files, changes them, and exits. When
it is not running, cosmon is just a directory of JSON files sitting on disk.

## What "stateless" actually means here

Every `cs` command is discrete: **read state, mutate, write, exit.** Nothing
lingers. There is:

- **No daemon**: no background process that has to be alive for the system to
  work.
- **No database server**: the local registry is embedded SQLite, a *library*
  linked into `cs`, not a server you start. (JSON files on disk remain the
  source of truth.)
- **No scheduler process**: cosmon does not own a clock. A human at a terminal,
  a cron job, or a shell loop drives it.

The source of truth is the filesystem. A molecule's authoritative state is a
`state.json` file; its history is an append-only `events.jsonl`; its
proof-of-work is a handful of tracked markdown files. You can read all of it with
`cat`, `jq`, and `git diff`. Nothing is hidden inside a running server's memory.

## Why this is the whole wedge

Temporal, Airflow, and Prefect orchestrate **functions**: deterministic code
that runs, returns, and is forgotten. Cosmon orchestrates **entities with
identity and state**: AI agents that crash, lose their context window, and need
to resume as *the same worker on the same task*. That difference is why the
stateless design is the point, not a limitation.

- **It survives crashes by construction.** If state lived in a running process's
  RAM, a crash would lose it. Because state is on disk after every command, a
  crash loses nothing; you re-run the next `cs` command and it picks up exactly
  where the files say you were. (See [Crash recovery](./crash-recovery.md).)
- **It needs no broker.** Molecules do not talk through mailboxes or queues.
  Ordering flows through typed links on disk; content flows through shared files.
  (See [Control plane vs data plane](./control-vs-data-plane.md).)
- **It composes with any scheduler.** Because `cs` is just a binary, you can
  drive it from cron, launchd, a Makefile, a CI job, or your own hands. Cosmon
  does not fight your infrastructure because it *has* no infrastructure to
  defend.
- **It is git-composable.** State on disk means state in git. A molecule's trace
  is a diffable, mergeable, revertable set of files.

For a team running three to ten AI agents on a single codebase, this is
radically simpler than any cluster-based alternative. There is nothing to deploy,
nothing to keep alive, nothing to page you at 3am when it falls over, because
there is no *it*, only files and a binary you invoke.

## The two layers

Cosmon is honest that a long-lived orchestrator is sometimes useful: walking a
large DAG of dependent work without a human tending each step. So the
architecture reserves room for one, as a strictly optional second layer:

1. **Transactional Core (today).** The stateless CLI. Every `cs` command you can
   run now. Files on disk are the truth. **Never a daemon.**
2. **Resident Runtime (optional, additive).** One long-lived process (`cs run`)
   that polls the on-disk state and dispatches ready work through the *same*
   commands a human would type. It is a **client** of the core, not a
   replacement. It owns no private state; kill it and restart it and it rebuilds
   everything from disk.

The inviolable rule is that Layer B never becomes the *only* path to anything.
Every capability is reachable from the plain CLI, human-driven. The runtime is
pure convenience layered on top of a system that works fully without it. That
discipline is what keeps the crash-recovery guarantee true: you can always
`cat` cosmon's state, because there is never a process that holds truth the
files do not.

See [Architecture: the two layers](./architecture.md) for how this maps onto the
crate structure, and [The three regimes](./regimes.md) for the clock-and-observer
model that formalizes when each layer is in charge.
