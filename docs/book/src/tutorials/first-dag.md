# Composing a DAG

So far your molecules have been independent. Real work has *order*: clean the
data before you build features, build the API and the UI before you integration-
test them. In this tutorial you will wire molecules into a **DAG**, a directed
acyclic graph of dependencies, and let cosmon run them in the right order,
automatically. By the end you will have run a small pipeline end to end with a
single command.

**Before you start:** finish [Running a fleet of agents](./first-fleet.md).

> A **DAG** is just "work with arrows": each molecule points at the ones that
> must finish before it can start. "Acyclic" means no arrow ever loops back, so
> the graph always has a beginning. The running graph of linked molecules is
> called a **polymer** (or a *mission*): where one molecule is a single unit, a
> polymer is the whole wired chain.

## Step 1: Wire a linear chain

Molecules become a DAG when you connect them with `--blocked-by`. The rule reads
like English: if B is *blocked by* A, then A must finish before B can start.

Nucleate three tasks and chain them A → B → C:

```sh
cs nucleate task-work --var topic="Fetch raw data"                 # → A
cs nucleate task-work --var topic="Clean and validate" \
    --blocked-by <A>                                                # → B
cs nucleate task-work --var topic="Build features" \
    --blocked-by <B>                                                # → C
```

Each `--blocked-by` records the edge on *both* molecules at once: the new one
learns it is blocked, the referenced one learns it blocks. You never maintain two
sides by hand. `--blocks` is the mirror flag if you prefer to wire from the other
direction; both are repeatable, so `--blocked-by <A> --blocked-by <B>` fans two
dependencies into one molecule.

Cosmon validates the graph as you build it: a reference to a molecule that does
not exist aborts the nucleation, and so does any edge that would create a cycle.

## Step 2: Inspect the graph

Before running anything, look at what you wired:

```sh
cs deps <B>
```

```
⏳ Blocked by:
  <A>   [pending]

⛔ Blocks:
  <C>   [pending]
```

`cs deps` shows one molecule's direct neighbours. To see the whole connected
chain at once, walk it transitively:

```sh
cs deps <C> --transitive
```

Add `--json` to either for structured output you can pipe into other tools.

## Step 3: Run the whole DAG with `cs run`

You *could* tackle each molecule by hand in order, but that is exactly the
bookkeeping cosmon exists to remove. Hand the whole graph to the runtime with one
command, pointing it at the root:

```sh
cs run <A>
```

`cs run` is the resident runtime: it walks the DAG for you. Step by step it:

1. discovers the full graph by following the edges out from `<A>`,
2. finds the **ready frontier**: every molecule whose upstream is already done
   (at the start, just `<A>`),
3. advances the ready molecules,
4. when one finishes, unlocks whatever it was blocking, recomputing the frontier,
5. repeats until nothing is left, then exits.

So `<A>` runs first; only when it completes does `<B>` become ready; then `<C>`.
You wired the order once, declaratively, and the runtime enforces it.

`cs run` blocks your terminal until the whole graph drains. For anything
long-running, launch it in a detached tmux session so your shell stays free:

```sh
tmux new -d -s runtime cs run <A> --poll-interval 5
```

Useful options:

```sh
cs run <A> \
    --timeout 300 \       # give up after 300s (exit code 124); 0 = no limit
    --poll-interval 1     # seconds between scheduler ticks
```

## Step 4: When branches diverge, `cs done` protects you

`cs run` calls `cs done` for each molecule as it completes, merging its branch
before dispatching whatever depended on it, so each worker sees its
predecessor's committed output in its own worktree. This is why order matters for
*content*, not just timing: B's worker can read A's finished files because A was
merged first.

If two molecules that touch the same file ever merge in a way that conflicts,
`cs done` does **not** leave you with a broken tree. It aborts the merge cleanly
and prints exact recovery steps, for example:

```
Conflict detected in 1 file(s):
  src/config.rs

To resolve:
  cd .worktrees/<mol-id> && git merge main
  # fix conflicts
  git add . && git commit
  cd - && cs done <mol-id>
```

## Diamonds and parallelism

A chain is the simplest DAG. The same flags build a diamond, where two molecules
run in parallel and a third fans them back in:

```sh
cs nucleate task-work --var topic="Setup infrastructure"      # → infra
cs nucleate task-work --var topic="Build API"  --blocked-by <infra>   # → api
cs nucleate task-work --var topic="Build UI"   --blocked-by <infra>   # → ui
cs nucleate task-work --var topic="Integration tests" \
    --blocked-by <api> --blocked-by <ui>                              # → tests
```

Under `cs run`, `api` and `ui` become ready *together* the moment `infra`
completes, run in parallel, and `tests` waits for both. You describe the shape;
the runtime finds the parallelism.

## What just happened

You moved from "three independent jobs" to "a pipeline with a shape." The shape
lives in the edges (`--blocked-by`) and `cs run` turns that shape into the
correct execution order, including running independent branches at once. Nothing
about a single molecule changed; you only added arrows between them.

## Next

- Package a *whole* wired graph as a reusable, shareable template:
  [Germinate a polymer from a spore](../how-to/germinate-from-spore.md).
- Understand who holds the clock in each mode:
  [The three regimes](../explanation/regimes.md).
- Wire cosmon into your own scheduler instead of `cs run`:
  [Wire cosmon into an external scheduler](../how-to/external-scheduler.md).
