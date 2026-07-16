# The three regimes: Inert / Propelled / Autonomous

> These commands use physics-inspired names (nucleate, tackle, evolve, …). New to
> the vocabulary? See [The physics vocabulary](./physics-vocabulary.md).

A molecule is a piece of tracked work. At any moment it sits in exactly one of
three **regimes**, and the regime is not really about the molecule itself. It is
about a simpler question: **who holds the clock?** Who has the right to move this
work forward, right now?

Asking "is the system alive?" leads nowhere; you end up inventing words like
*semi-alive* that fall apart on the first edge case. The sharp question is: **who
is entitled to perform the next step?** The three regimes are the three answers.

| Regime | Who holds the clock | Who performs the next step | When it applies |
|--------|---------------------|----------------------------|-----------------|
| **Inert** | Nobody, external only | A human, next time they type a `cs` command | Pending molecules; finished molecules |
| **Propelled** | A human, plus fuel | The worker running in a tmux pane | A tackled molecule actively running |
| **Autonomous** | An internal runtime | A policy inside `cs run` | A DAG being walked by the runtime |

## Inert: the parked car

An **inert** molecule has state but no motion. It sits on disk and does
absolutely nothing until something outside it reaches in and pushes.

Two very different molecules are both inert. A freshly `nucleate`d molecule that
nobody has started yet is inert; it is waiting to begin. And a `completed`
molecule is *also* inert; it is finished, but its shell stays on disk so you can
still read its trace. One is inert before its life, the other after. Neither has
a clock of its own. The only way an inert molecule ever changes is when a human
runs a command against it.

*A parked car. It has everything it needs to move, but it will sit in the
driveway forever until someone gets in and turns the key.*

## Propelled: the car with a driver and a tank of fuel

You `cs tackle` a molecule and it enters the **propelled** regime. Now it has
momentum. A worker (an AI agent in its own tmux pane and git worktree) is
driving it forward, step by step, along the formula's predetermined path. It
keeps going until the fuel runs out (all steps complete) or the motion dies (it
stalls on a blocker).

The clock here is *external plus fuel*: a human lit the fuse with `cs tackle`,
and now the worker carries the work along a fixed trajectory. If the worker
stalls, a watchdog (`cs patrol --propel`) can give it a nudge from outside, the
way you would tap a stuck toy back into motion. But the trajectory was fixed the
moment the molecule was nucleated; the steps do not change mid-drive.

This is the regime you spend almost all your time in today. The full loop is
**nucleate → tackle → wait → done**: create the molecule, start a worker on it,
wait in the background for it to finish, then tear it down and merge its work.

## Autonomous: the self-driving fleet (the north star)

In the **autonomous** regime the clock moves *inside* cosmon. A long-lived
process (`cs run`) walks a whole DAG of dependent molecules, and a **policy**
decides what to start next: a DAG scheduler, a decay-aware re-planner, or an
external planner speaking over MCP. No human types each `cs tackle`; the runtime
does it, and calls `cs done` on each node as it finishes.

Two honest notes about this regime:

- **The runtime is a client, not a new brain.** It cannot do anything a human
  could not do at the CLI. It owns no private truth; it reads the same JSON
  files, emits the same `cs evolve` / `cs done` calls. Kill it and restart it,
  and it rebuilds everything from disk. That is why it never threatens the
  crash-recovery guarantee.
- **Full autonomy is still the north star, not the whole sky.** `cs run` walks
  DAGs today; the deeper self-directed regime (long unattended missions with a
  pluggable planner) is on the roadmap (ADR-016), not something that ships
  finished. When you read "autonomous," read it as the direction cosmon is built
  to grow into, with the guardrails already in place.

## Bounded autonomy: free inside a fence, stopped at the gates

The autonomous regime is exactly where the fear of *runaway* lives: if I hand
over the clock, what stops it doing something I would never have allowed? The
answer is that autonomy in cosmon is always **bounded**: free movement inside a
real fence, with a leash you never let go of.

Think of a dog in a fenced yard. Inside the fence it runs free: it tidies up
finished work, picks up the next obviously-ready job, jots a note about something
it noticed. All small, safe, reversible chores. But at the edge of the yard there
are gates it will **never** push open alone; it sits and barks for you instead:

- **A new direction**: a whole new area of work outside the job you posed.
- **A real fork**: a design choice with genuine trade-offs.
- **Anything it cannot take back**: pushing, deleting, publishing, changing DNS.
- **Rewiring the house**: installing a service, editing the rules it itself
  obeys, touching shared config.

And you always hold the leash. Any one of three tugs freezes it where it stands:
you **speak** (any message pauses it), you **drop a stone in the yard**
(`touch ~/.cosmon/autopilot.off`, which it checks at every decision point), or it
**trips three times on the same rock** (three failures in a row and it stops
itself). Autonomy is never *"the robot takes over"* and never *"you watch it like
a hawk"*; it is **calibrated trust**: a fence you can see, gates it always knocks
on, and a leash you always hold.

## Why the regimes matter

Every cosmon command operates on one or more regimes, and mixing them up is where
design goes wrong. `cs nucleate` only ever produces an inert molecule; nothing
runs. `cs tackle` is the *sole* doorway from inert into propelled, and it is
human-only. `cs done` is the doorway back out. The autonomous regime, when it is
active, uses the very same doorways; it just walks through them on its own
schedule.

So the regime is really a statement about delegation: `cs tackle` delegates the
next observation to a worker; `cs patrol --propel` delegates it to an external
scheduler; `cs run` delegates it to the runtime's policy. Name the regime and you
know who holds the clock, and once you know who holds the clock, every command's
behaviour follows.
