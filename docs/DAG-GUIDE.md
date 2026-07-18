# DAG Guide — Building and Executing Dependency Graphs with Cosmon

This guide walks you through creating, inspecting, executing, and troubleshooting
molecule DAGs. By the end you will have run your first multi-molecule pipeline.

> **Bridge.** The DAG you wire here by hand with `--blocked-by` is exactly what
> a [`spore`](vocabulary.md#spore)
> ([ADR-139](adr/139-spore-shareable-polymer-template.md)) packages as a
> shareable, parameterizable template: a formula is the template of one node, a
> spore is the template of the whole wired graph, and `germinate` is its
> `nucleate`. The static-wiring manifest is the only genuinely new piece;
> everything else here (edges, validation, the ready frontier) is reused.

**Prerequisites:** `cs` installed, a `.cosmon/` project initialized (`cs init`).

**Reference ADRs:**
[ADR-016 — Autonomy Regimes](adr/016-autonomy-regimes-and-resident-runtime.md) |
[ADR-022 — Native DAG Scheduler](adr/022-native-dag-scheduler.md) |
[Architectural Invariants](architectural-invariants.md)

---

## 1. Creating a DAG

Molecules become a DAG when you wire them with `--blocks` and `--blocked-by`.
The edge convention follows the "must complete before" relation:
if A `--blocks` B, then A must finish before B can start.

### Linear chain (A → B → C)

```sh
# Create three tasks. Wire them as a chain: A before B, B before C.
cs nucleate task-work "Fetch raw data"          # → task-20260410-aaaa
cs nucleate task-work "Clean and validate" \
    --blocked-by task-20260410-aaaa              # → task-20260410-bbbb
cs nucleate task-work "Build features" \
    --blocked-by task-20260410-bbbb              # → task-20260410-cccc
```

Each `--blocked-by` creates a symmetric pair of links:
the new molecule gets `BlockedBy { source }` and the referenced molecule
gets `Blocks { target }`. You never need to maintain both sides manually.

### Diamond DAG (A → B, A → C, B → D, C → D)

```sh
cs nucleate task-work "Setup infrastructure"     # → infra
cs nucleate task-work "Build API" \
    --blocked-by infra                            # → api
cs nucleate task-work "Build UI" \
    --blocked-by infra                            # → ui
cs nucleate task-work "Integration tests" \
    --blocked-by api --blocked-by ui              # → tests
```

The `tests` molecule has two upstream dependencies. It will not become ready
until both `api` and `ui` are completed.

### Wiring from the other direction

`--blocks` is the mirror of `--blocked-by`. Use whichever reads more naturally:

```sh
# "infra blocks api" is equivalent to "api is blocked-by infra"
cs nucleate task-work "Setup infrastructure" --blocks api
```

Both flags are repeatable — pass multiple IDs to fan in or fan out.

### Validation

Cosmon validates all edges at nucleation time:

- **Dangling references** — every ID in `--blocks` / `--blocked-by` must already
  exist. A missing target aborts nucleation with a descriptive error.
- **Cycles** — the graph is validated as acyclic before persistence. A cycle
  aborts with the offending node identified.

---

## 2. Inspecting the Graph

### Direct dependencies

```sh
cs deps task-20260410-bbbb
```

Output:

```
⏳ Blocked by:
  task-20260410-aaaa  [completed]

⛔ Blocks:
  task-20260410-cccc  [pending]
```

### Transitive closure

```sh
cs deps task-20260410-cccc --transitive
```

BFS-walks both directions from the given molecule, showing the full connected
component with status annotations. Dangling references show `[MISSING]`.

### Machine-readable

```sh
cs deps task-20260410-cccc --transitive --json
```

Returns structured JSON with `blocked_by` and `blocks` arrays, suitable for
piping into `jq` or feeding to other tools.

---

## 3. Executing with `cs run` (Autonomous Regime)

`cs run` launches the resident runtime with a `DagPolicy` that automatically
advances molecules through the graph.

```sh
cs run task-20260410-aaaa
```

**What happens step by step:**

1. **Compile plan.** BFS from the root molecule, walking all `Blocks` /
   `BlockedBy` links to discover the full connected component. Edges are
   materialized and validated for acyclicity. The result is a `Plan` — a
   ready-frontier tracker.

2. **Compute ready frontier.** All molecules with no unsatisfied upstream
   dependencies enter the ready set. In a linear chain, only the first
   molecule is ready. In a diamond, the root is ready.

3. **Evolve ready molecules.** The `DagPolicy` emits `Evolve` actions for
   each ready molecule. The runtime transitions them to `Completed`.

4. **Unlock dependents.** When a molecule completes, `Plan::mark_done`
   recomputes the frontier — any molecule whose upstream is now fully
   satisfied becomes ready.

5. **Decay-aware re-planning.** If a completed molecule has `DecayProduct`
   links (molecules spawned by decay), the policy splices them into the
   live plan via `insert_subgraph`. The plan rebuilds without restarting
   the runtime.

6. **Drain.** When the ready set and running set are both empty, the
   policy returns no actions. The runtime exits with `PolicyDrained`.

### Options

```sh
cs run task-20260410-aaaa \
    --policy dag \           # default; "noop" for testing
    --timeout 300 \          # seconds; 0 = no limit; exit code 124 on deadline
    --poll-interval 1        # seconds between ticks
```

### Critical path

The `DagPolicy` computes the critical path (longest chain by edge count)
each tick and prioritizes those molecules when multiple are ready. This
minimizes wall-clock time for the overall DAG.

### Ctrl-C

The runtime wires Ctrl-C to a `ShutdownSignal`. On interrupt, the current
tick completes and the runtime exits with `ShutdownReason::SignalTripped`.
No molecule state is corrupted — each action is applied transactionally.

---

## 4. Manual Convoy (Propelled Regime)

When `cs run` is not appropriate — for example, when molecules require
human-supervised workers with full Claude Code sessions — use the
tackle → wait → done pattern.

### Single molecule

```sh
cs tackle task-20260410-aaaa        # creates worktree + tmux + worker
# ... worker executes formula steps autonomously ...
cs done task-20260410-aaaa          # merges branch, tears down worktree
```

### Shell one-liner for a linear chain

```sh
for mol in task-20260410-aaaa task-20260410-bbbb task-20260410-cccc; do
    cs tackle "$mol"
    cs wait "$mol"                  # blocks until molecule reaches terminal state
    cs done "$mol"
done
```

This executes the chain sequentially — each molecule completes before the
next one is tackled. The worker in each tmux session runs its formula steps
and calls `cs complete` when done; `cs wait` blocks until that happens.

---

## 5. Parallel Execution

Independent molecules (no edge between them) can be tackled simultaneously.
In the diamond DAG, `api` and `ui` are independent after `infra` completes.

### Tackle in parallel

```sh
# After infra is done:
cs tackle api &
cs tackle ui &
wait                                # both workers run concurrently
```

Each gets its own worktree (`.worktrees/api`, `.worktrees/ui`) and its own
tmux session (`cosmon-api`, `cosmon-ui`). They work on separate git branches
(`feat/api`, `feat/ui`) so there are no conflicts during execution.

### Merging divergent branches with `cs done`

When multiple workers run in parallel, their branches diverge from `main`.
The default merge strategy is `--no-ff` (merge commit), which handles
divergence cleanly:

```sh
cs done api                         # merges feat/api into main (merge commit)
cs done ui                          # merges feat/ui into main (merge commit)
```

If the branches touch different files, both merges succeed. If they conflict,
`cs done` aborts the merge cleanly (`git merge --abort`), prints the
conflicting files, and gives you exact recovery commands:

```
Conflict detected in 2 file(s):
  src/config.rs
  src/lib.rs

To resolve:
  cd .worktrees/ui && git merge main
  # fix conflicts
  git add . && git commit
  cd - && cs done ui
```

### Merge strategies

| Strategy | Flag | When to use |
|----------|------|-------------|
| `merge` (default) | `--strategy merge` | Parallel workers — always works |
| `ff-only` | `--strategy ff-only` | Sequential chains without native attribution — cleaner history |

Native attribution requires the default `merge` strategy because its
`Co-Authored-By` provenance rides on the cosmon-owned merge commit. `cs done`
refuses `ff-only` when attribution trailers are configured instead of silently
fast-forwarding unstamped worker commits. The carrier is exactly one line:
`Co-Authored-By: Noogram (<adapter>) <noreply@noogram.org>` when an adapter
witness exists, or `Co-Authored-By: Noogram <noreply@noogram.org>` otherwise.
The adapter is metadata in the display name; cosmon never synthesizes a
model-specific email identity.

---

## 6. Dynamic DAG — Decay Products

Molecules can spawn children during execution via the decay mechanism.
When a running molecule decays, it produces `DecayProduct` molecules linked
back to the parent.

Under `cs run`, the `DagPolicy` detects new `DecayProduct` links on each
tick and splices them into the live plan:

1. The completed parent molecule has `DecayProduct { id: child }` links.
2. `absorb_completion` detects these and creates new edges `(parent, child)`.
3. `insert_subgraph` merges the new edges into the existing plan, validating
   acyclicity on the union.
4. The plan rebuilds with the new nodes in the frontier.

This means a DAG can grow at runtime — a molecule can discover work that
needs to happen and nucleate sub-molecules, which get scheduled automatically
without restarting the runtime.

The `insert_subgraph` primitive is pure and idempotent: re-inserting the
same edges is a no-op; a cycle in the union is rejected with an error.

---

## 7. Monitoring

### `cs watch` — Live terminal view

```sh
cs watch
```

A polling loop with three cadences:

| Tier | Cadence | What it does |
|------|---------|-------------|
| Heartbeat | ~250 ms | Spinner, elapsed time, molecule counts |
| State poll | 1 s (configurable) | Diff state, emit event lines for transitions |
| Propel nudge | 60 s (configurable) | Nudge stale workers via tmux transport |

Options:

```sh
cs watch \
    --stale-after 300 \     # seconds before a worker is considered stale
    --poll-ms 1000 \        # state polling interval
    --propel-every 60 \     # propulsion nudge interval
    --once \                # single pass then exit (for scripts)
    --no-tmux               # read-only mode, no propulsion
```

`cs watch` is a poller, not a daemon — it stays within the Transactional
Core layer. It coexists with `cs run` (which owns the Autonomous regime).

### `cs ensemble` — Fleet status snapshot

```sh
cs ensemble
```

Prints a table of all active workers:

```
NAME             ROLE            DESIRED   EFFECTIVE   LIVE              INPUT   OUTPUT   COST    MOLECULE
worker-aaaa      Implementation  running   healthy     working:step 2    1.2M    45K      $4.12   task-aaaa
worker-bbbb      Implementation  running   suspect     stale             890K    32K      $2.87   task-bbbb
```

Plus a molecule summary footer counting pending / running / completed / collapsed.

```sh
cs ensemble --json          # machine-readable output
```

### Horizon cockpit — Browser view

The `cosmon-cockpit-http` binary serves a live dashboard at `http://127.0.0.1:7878`:

- Molecule status overview with real-time updates
- `/api/spark` endpoint for idea nucleation from the browser
- Event-log tail panel
- Fleet glyph headers

Start it with:

```sh
cargo run --bin cosmon-cockpit-http
```

---

## 8. Troubleshooting

### Worker stalls (no progress for > 5 minutes)

**Diagnosis:**

```sh
cs ensemble                     # check EFFECTIVE column for "suspect" or "stale"
cs watch --once                 # single-pass status check
```

**Fix — propulsion nudge:**

```sh
cs patrol --propel              # nudges all stale workers via tmux
```

This sends a propulsion message to the worker's tmux session:

> *"PROPULSION — you appear idle mid-molecule. Re-read your current step
> and continue execution immediately."*

You can also run continuous propulsion:

```sh
cs watch --propel-every 60      # auto-nudge every 60 seconds
```

### `cs done` reports a merge conflict

The merge is automatically aborted (clean state). Follow the printed
recovery commands:

```sh
cd .worktrees/<mol-id>
git merge main                  # replay the merge
# fix conflicts in your editor
git add .
git commit
cd -
cs done <mol-id>                # retry teardown
```

### Molecule is stuck (cannot evolve)

```sh
cs observe <mol-id>             # inspect current state, step, links
cs deps <mol-id>                # check if upstream dependencies are blocking
```

If an upstream molecule is the blocker, complete or collapse it first.

If the molecule itself is broken:

```sh
cs stuck <mol-id> --reason "blocked on external dependency"   # freeze it
# or
cs collapse <mol-id> --reason "unrecoverable error"           # terminal failure
```

### `cs run` exits immediately with `PolicyDrained`

The root molecule (or its entire connected component) is already completed.
Check with `cs observe <mol-id>` and ensure at least one molecule is `Pending`.

### `cs run` exits with code 124

The `--timeout` deadline was reached. Increase it or set `--timeout 0` for
no limit:

```sh
cs run <mol-id> --timeout 0
```

### Cycle detected during nucleation

```
error: dependency cycle in compiled DAG at molecule task-20260410-xxxx
```

You are creating a circular dependency. Review the graph with
`cs deps <mol-id> --transitive` and break the cycle by removing or
redirecting one edge.

### Worktree has dirty files on `cs done`

`cs done` refuses to remove a worktree with uncommitted changes. Either:

```sh
cd .worktrees/<mol-id>
git add . && git commit -m "final changes"
cd -
cs done <mol-id>
```

Or force teardown (loses uncommitted work):

```sh
cs done <mol-id> --force
```

### Dry-run teardown plan

Before running `cs done`, preview what will happen:

```sh
cs done <mol-id> --dry-run
```

Returns a structured plan showing: molecule status, whether a merge is
needed, dirty files, tmux session state, branch status, and planned actions.
