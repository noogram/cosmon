# Recover a crashed agent

**Goal:** a worker died (the laptop rebooted, a tmux server was killed, a model
call timed out) and a molecule is now stranded. This guide gets it moving again
(or retires it) without losing any work.

> The key fact that makes recovery cheap: cosmon keeps its truth on disk in
> `.cosmon/state/`, not in the worker's memory. A crash preserves every molecule;
> it only *strands* the live worker that was driving it. Recovery is noticing the
> strand and re-attaching a fresh worker. See
> [Crash recovery](../explanation/crash-recovery.md) for the mechanism.

There is no single `recover` verb. Recovery is a short sequence of one-decision
commands: **scan, then decide one molecule at a time.**

## When you need this

- After a host reboot or an editor/IDE crash.
- After a `SIGKILL` or OOM of a tmux server.
- When `cs ensemble` shows molecules that say `running` but you cannot reach.
- As the first move in any "something is wrong, I don't know what" triage.

## Step 1: Scan for strands

```sh
cs patrol
```

`cs patrol` runs the fleet's health checks and surfaces molecules whose lifecycle
says `running` but whose worker process is gone. For a read-only look that
mutates nothing, use the anomaly catalog instead:

```sh
cs health
```

Both tell you the same thing: which molecules are stranded. `cs patrol` never
silently re-tackles anything; it detects and reports; you pick the fix.

## Step 2: Decide, one molecule at a time

For each stranded molecule, choose exactly one verb:

| Situation | Verb | What it does |
|-----------|------|--------------|
| The worker is alive but sitting idle mid-step | `cs resume <id>` | Re-propels the existing worker (a nudge to continue). |
| The worker is genuinely dead, but the work is worth continuing | `cs resurrect <id>` | Revives the molecule with a **fresh** worker: the re-tackle after a crash. |
| You want to park it and record why | `cs stuck <id> --reason "..."` | Freezes the molecule and notes the blocker. |
| It is not worth recovering | `cs collapse <id> --reason "..."` | Terminates it permanently, with a reason on the record. |

Example: a worker died on a molecule you still want:

```sh
cs resurrect task-20260711-a1b2
```

A fresh worker starts in a new tmux session, reads the molecule's on-disk trace,
and picks up from where the state file says it was.

## Step 3: Confirm it is moving

```sh
cs peek
```

Select the recovered molecule (`j`/`k`) and press `p` to see its new worker's
live pane. Or, for a one-shot check:

```sh
cs observe task-20260711-a1b2
```

If a frozen molecule's blocker later clears, bring it back with:

```sh
cs thaw task-20260711-a1b2
```

## Scope

Recovery is **project-local**: these verbs act on molecules in the current
project's `.cosmon/state/`, discovered by walking up from your working directory.
For another project, run the same verbs from that project's directory. `Pending`,
`Completed`, and `Collapsed` molecules are never "stranded"; only `running`
molecules with a dead worker are.

## See also

- [Crash recovery](../explanation/crash-recovery.md): why a crash loses nothing.
- [The three regimes](../explanation/regimes.md): who is entitled to move a
  molecule forward.
- [Molecule lifecycle reference](../reference/lifecycle.md): every recovery verb
  in full.
