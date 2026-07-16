# Dynamic workflows or cosmon fleets?

Picture two kinds of work.

In the first, a foreman opens twenty temporary benches, gives each bench one
box to inspect, compares the answers, and clears the room before lunch. The
useful output is the final report. Nobody needs a permanent identity for bench
seventeen.

In the second, an expedition leaves for several days. Each team carries a
logbook, follows another team's map, may be interrupted, and must return its
samples on a separate manifest. Losing the leader's notebook must not erase
where every team got to.

A Claude Code **dynamic workflow** is the temporary workshop. A cosmon
**fleet** is the expedition register. Neither is a more powerful version of
the other. Choose the one whose failure boundary matches the work.

## The short decision

Ask one question:

> If this Claude Code session vanished now, would restarting the whole job as
> one unit be acceptable?

If yes, use a dynamic workflow. If no, put the durable boundaries in cosmon
molecules and connect them with a DAG.

| Need | Dynamic workflow | Cosmon fleet |
|---|---|---|
| Lifetime | One Claude Code session | Across sessions, crashes, and long missions |
| Recovery unit | The workflow run | Each molecule and formula step |
| Coordination | JavaScript phases, loops, and fan-out | Typed DAG between durable molecules |
| Operator view | `/workflows` for the current run | `cs peek` and `cs ensemble` for the fleet |
| Worker identity | Ephemeral subagent | Persistent molecule plus worker incarnation |
| Isolation | Fresh context; optional temporary worktree | Dedicated branch and worktree per worker |
| Delivery | Usually one aggregated result | Independent artifacts, gates, and merges |
| Size control | Prompt/config guidance; runtime caps | Admission and parallelism before dispatch |

## When a dynamic workflow is enough

Use a dynamic workflow when the orchestration is internal detail and the final
answer is the only durable object you care about. Typical shapes include:

- audit many files and return one ranked report;
- ask several researchers for independent views, cross-check them, and
  synthesize;
- apply the same bounded transformation across a large list;
- repeat a checker/fixer loop until it passes or stops making progress.

Claude Code keeps intermediate values in the workflow script and shows phases,
agents, token use, and progress in `/workflows`. Its subagents normally start
with fresh contexts and return results to the parent. This is exactly the right
trade when permanent per-agent identity would be bookkeeping without benefit.

The **Dynamic workflow size** setting is guidance to Claude, not a hard quota.
`small`, `medium`, and `large` aim for fewer than 5, 15, and 50 agents. A prompt
can request another size. Runtime caps still apply. See the
[official Claude Code workflow documentation](https://code.claude.com/docs/en/workflows).

## When a cosmon fleet is justified

Use cosmon when a subtask deserves to exist after its current executor is gone.
That is usually true when one or more of these apply:

- work must resume after a session crash or context loss;
- one result gates another through a typed dependency;
- different tasks need independent branches, reviews, or merges;
- the operator must see several live responsibilities in one durable view;
- evidence and lifecycle transitions matter as much as the final synthesis;
- workers use different adapters or models;
- the mission lasts long enough that “start the run again” is not recovery.

The lifecycle is explicit:

```text
cs nucleate → cs tackle → cs wait → cs done
```

Each tackled worker gets its own worktree and branch. The DAG carries ordering,
not content: an edge says only whether a predecessor is done. Files and git
history carry the actual output. Cosmon merges a predecessor before dispatching
its dependent, so the dependent starts from a checkout that already contains
the work it needs.

## Use both without fusing them

The common combination is simple: make one cosmon molecule the durable recovery
boundary, then let its Claude adapter use a dynamic workflow internally.

```text
cosmon molecule (durable lifecycle)
└── Claude dynamic workflow (temporary fan-out)
    ├── subagent
    ├── subagent
    └── verifier subagent
```

Cosmon owns the molecule state, branch, merge, and durable artifact. Claude Code
owns the temporary workflow phases and subagent execution. There is no need to
turn every subagent into a molecule unless the operator needs to recover,
schedule, merge, or audit it independently.

If that need appears, do not create “shadow molecules” after agents have already
started. A true one-to-one bridge must let cosmon admit each spawn before it
runs, assign its durable identity and worktree, and remain the only owner of its
lifecycle. That is an architectural change, not an adapter convenience.

## Rule of thumb

Use the smallest durable boundary that would make a failure boring.

- If rerunning one workshop is boring, use a dynamic workflow.
- If rerunning one team would be acceptable but rerunning the expedition would
  not, make each team a molecule in a cosmon fleet.
- If a dynamic workflow is only an implementation detail inside one team, keep
  it there and let `cs peek` show the durable boundary.

Durability has a cost. Pay it where recovery, isolation, or accountability
needs it—not for every temporary pair of hands.
