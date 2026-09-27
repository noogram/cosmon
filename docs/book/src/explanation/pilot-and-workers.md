# Who does what: you, the pilot, and the workers

A cosmon mission has three actors, not one. Confusing them is the most common
source of a first-time user's unease — not "does the tool work", but "what is
it doing right now, without me, and is that okay?"

- **You** — the human. You state the mission, and every decision this page
  calls "yours" stays yours regardless of how capable the other two get.
- **The pilot** — the agentic coding CLI session you are talking to (Claude
  Code, Codex, or any tool that reads `cs help`). It reads your English,
  drives the `cs` command line, and reports back.
- **The workers** — the processes `cs tackle` (or `cs run`) spawns, one per
  molecule, each in its own git worktree. They do the actual work: writing
  code, running tests, producing a report.

Cosmon's own separation of *Transport* (the framework: routes, spawns,
persists) and *Cognition* (the agent: thinks) runs through this triangle too.
The pilot and the workers both think; cosmon itself only ever routes between
them and writes their state to disk.

## What the pilot does on its own

Once you have stated a mission, the pilot can carry the mechanical parts of
the cycle without checking back on each step:

- **Nucleate** — turn your mission statement into a molecule (`cs nucleate`).
- **Tackle** — spawn a worker on it (`cs tackle`), or walk a whole DAG
  (`cs run`).
- **Watch** — poll status without you (`cs peek`, `cs wait`), instead of
  leaving you to babysit a terminal.
- **Relay corrections** — read your feedback and forward it to a live worker
  (`cs whisper`), without treating a whisper as a demand for a fresh mission.
- **Harvest** — merge a finished molecule to the base branch and tear down its
  worktree (`cs done`), once the result is worth keeping.

This is the ordinary cycle, and it is safe to let the pilot run all of it
end-to-end for routine, reversible steps — that is the entire point of
delegating. What changes below is not *whether* the pilot may act, but which
specific decisions it must not make for you.

## What only you decide

Three kinds of decision stay with the human regardless of how well the pilot
and its workers are performing:

- **The mission statement.** What "done" means for this unit of work is a
  fact about your intent, not something derivable from the code. The pilot
  can help you phrase it; it should not invent scope you never asked for.
- **Protected inputs.** Reference data, golden fixtures, anything a worker
  must validate *against* rather than overwrite, is declared with
  `cs nucleate --protect <path>`. A worker that quietly rewrites its own
  reference to match its own output turns validation circular — declaring
  the path up front is what makes that mistake fail loudly instead of
  merging silently.
- **Accepting or dropping a result, and anything irreversible or
  outward-facing.** Merging a molecule's work (`cs done`) or discarding it
  (`cs collapse`) is a judgment about the result, not a mechanical next step.
  The same holds for anything that leaves the local checkout — a push, a
  publish, a message sent, a payment, a deletion of shared state. Cosmon's
  own operating discipline treats these as needing your authorization each
  time, not once; a pilot that already pushed for you yesterday still asks
  before it pushes today.

## When you are expected to look

Three moments are where your attention actually changes the outcome — not
because cosmon requires a click, but because each is the last point where
correction is cheap:

1. **Before a dispatch.** Once `cs tackle` spawns a worker, it is off running
   in its own worktree. Read the mission statement it was handed before that
   happens; a wrong statement wastes a whole worker cycle before you see it
   again.
2. **When a worker finishes.** Its result sits on `feat/<id>` in
   `.worktrees/<id>`, unmerged, specifically so you (or the pilot, on your
   behalf) can read it before it becomes part of the base branch.
3. **Before `cs done`.** This is the merge gate. After it, the worker's
   branch and worktree are gone; whatever was wrong in the result now has to
   be fixed forward instead of simply not merged.

## How to stop safely

Stopping a mission partway through is not a failure state — it is a normal
outcome the lifecycle already accounts for. The exact mechanism (`cs done` to
keep partial work, `cs collapse` to drop it, `cs status` to list what is
still waiting on one of the two) is spelled out once, in the pointer block
every project's `CLAUDE.md`/`AGENTS.md` carries — see [Pilot cosmon in
natural language](../how-to/pilot-in-natural-language.md) for that exact
text, so it is not repeated here and cannot drift from what the pilot
actually reads at runtime.

## Telling cosmon's contribution from a good brief

A well-written mission statement and a permissive sandbox make almost any
agent look more capable, with or without cosmon. What cosmon specifically
adds is everything in the first section above: a typed lifecycle that
survives a crash, a worktree per unit of work so a worker cannot collide with
another, an on-disk record of every step, and the protected-inputs and
merge-gate mechanics in the second section. If a result would have been just
as good from handing the same brief and the same permissions to a bare agent
in a scratch directory, the improvement came from the brief, not from cosmon.
The differential test is whether the *lifecycle* mattered: did crash recovery
save a run, did a protected path catch a real mistake, did the merge gate
stop a bad result from landing? If none of that fired, cosmon added
orchestration overhead to a task that did not need it — which is a legitimate
finding, not a failure of the tool.
