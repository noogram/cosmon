# The Cosmon Runtime — `cs run`, `cs tackle`, and the two-layer model

**Status:** canonical (2026-04-14) — supersedes scattered notes across
`architectural-invariants.md`, `handbook.md`, and individual ADRs for the
purpose of documenting *how the runtime actually behaves today*. Cross-links
to the governing ADRs are listed at the bottom.

This document is the reference operators reach for when asking: "what does
`cs run` do, what does `cs tackle` do, how do they compose, and what are the
known failure modes?" It is written against the code that ships on `main`
today — not an aspirational runtime.

---

## 1. Two layers, one state store

Cosmon separates orchestration into two layers (see
[`docs/architectural-invariants.md`](architectural-invariants.md) and
[ADR-016](adr/016-autonomy-regimes-and-resident-runtime.md)):

1. **Transactional Core** — `cs tackle`, `cs evolve`, `cs complete`,
   `cs done`, `cs collapse`, … Stateless, one decision per invocation,
   git-like. Files on disk are the source of truth.
2. **Resident Runtime** — `cs run`. A long-lived client of the
   transactional core that polls the on-disk state, asks a pluggable
   [`Policy`] what to do next, and applies the result through the same
   `StateStore` the CLI uses. Shares the state store; owns no private
   truth.

The runtime is a **client**, not a replacement. It cannot mutate state
through any channel the CLI does not also expose. A human operator can run
`cs observe`, `cs freeze`, `cs tag` — even `cs collapse` — while the
runtime is running, and the runtime will see those changes on its next
tick. No locks, no daemon.

## 2. The three regimes, in practice

| Regime | Clock | Dispatcher | How you enter it |
|--------|-------|------------|------------------|
| **Inert** | Human | Human | `cs nucleate` — the molecule sits pending |
| **Propelled** | Human + fuel | Human via `cs tackle`; patrol watchdog can nudge | `cs tackle <id>` spawns one leaf worker on this node |
| **Autonomous** | Internal (runtime) | `cs run`'s [`DagPolicy`] | `cs run <root>` walks the DAG (1 or N nodes) |

The two verbs do not overlap (see §3).

## 3. `cs tackle` and `cs run` — two verbs, one routing decision

Since the verb-unification of delib-20260426-1bcd #2
(task-20260426-c33f), the dispatch decision is **the verb the operator
types**, not a flag on a polymorphic command:

- **`cs tackle <id>` — always one node.** Forks a single leaf worker
  (tmux pane + worktree, one Claude session) on `<id>`. Never inspects
  outgoing `Blocks` edges. Never auto-spawns a resident runtime. No
  fan-out banner.
- **`cs run <id>` — walks a DAG of N≥1 nodes.** Compiles a plan from
  `<id>`'s connected component, dispatches each ready node via
  `cs tackle`, calls `cs done` on completion. The 1-node case (leaf)
  is the same code path as the N-node case.

The historical bifurcation (auto-detect inside `cs tackle`, with
`--leaf` and `--force-runtime` escape hatches) was eliminated because
`--leaf` had become a reflex — when the "edge case" flag is the
operator's default, the auto-detection is inverted. Both flags are
deprecated no-ops kept during a one-month grace window:

- `cs tackle --leaf` — silent no-op (tackle is always leaf now).
- `cs tackle --force-runtime` — emits a deprecation warning, no-op.
- `cs run --force-runtime` — **still works**: it is the audited
  override for the ADR-048 backlog-sanity guard.

`COSMON_RUNTIME_ACTIVE` is preserved as a defensive marker on every
`cs tackle` spawn the runtime makes, so any pre-grace `cs tackle`
binary that still auto-detects will still recognise the nested-runtime
context and stay leaf.

## 4. When to use `cs run` directly (and when not to)

### Use `cs tackle` for one node, `cs run` for a DAG

For a single molecule, the right sequence is **nucleate → tackle →
wait → done**:

```
cs nucleate <formula> --var topic="..."
cs tackle <id>              # one worker, no DAG walk
cs wait <id> &              # non-blocking
# ...when wait returns:
cs done <id>
```

For a multi-molecule DAG, replace `cs tackle` with `cs run` (and let
the runtime call `cs done` for each node automatically):

```
cs nucleate <formula> --blocks <root> ...
cs run <root> --poll-interval 5     # walks 1 or N nodes
```

### Do use `cs run` directly when you need to

- **Custom `--poll-interval`** — the runtime ticks every 1s by
  default; `cs run --poll-interval 5` slows it for long-running DAGs
  (less store churn, less log spam).
- **Force runtime mode on a single molecule** — testing, debugging.
  `cs run <leaf-id>` treats it as a one-node DAG. Rare but legitimate.
- **Recover an abandoned DAG** — a runtime tmux session died mid-flight
  (OOM, laptop reboot). `cs run <root>` against the existing state
  resumes where the previous runtime left off; it is idempotent and
  picks up the current ready frontier.
- **Bounded timeouts** — `cs run <root> --timeout 3600` exits after
  an hour regardless of plan state. Useful in CI.

### Anti-patterns

- **Don't run `cs run` in your pilot's foreground shell.** It blocks
  your session until the whole DAG drains. Use `tmux new -d -s runtime
  cs run <root>` to detach.
- **Don't run `cs run` under the default tmux socket.** Workers and
  runtimes live on cosmon's dedicated socket (see `tmux_socket_name`
  in `cosmon-cli`). If you launch `cs run` from a shell with
  `TMUX=/default`, orphaned sessions pile up in `[default]` and
  `cs peek` won't see them. Always launch via cosmon-aware paths or
  via an explicit `-L cosmon` when going manual.
- **Don't reach for `--leaf` or `--force-runtime` on `cs tackle`** —
  both are deprecated no-ops. If you want a single worker without
  DAG walking, just type `cs tackle <id>`. If you want to walk a DAG,
  type `cs run <root>`.

## 5. Inside `cs run` — the event loop

The resident runtime ([`Runtime::run`](../crates/cosmon-runtime/src/lib.rs))
is a simple loop:

```
loop {
    if shutdown_signal { return SignalTripped }
    if deadline_hit    { return Deadline }

    snapshot = FleetSnapshot::load(store)
    for mol newly-completed-since-last-tick:
        executor.on_complete(mol)       // runs `cs done` — merge-before-dispatch

    for mol Running with native/gate tail:
        executor.drain_native_tail(mol) // keeps mixed formulas moving

    actions = policy.next_actions(snapshot)
    if policy.needs_recompile():
        policy.recompile(store)         // re-read edges from disk after splice
        actions = policy.next_actions(snapshot)

    if actions.is_empty():
        if any_running: sleep; continue
        return PolicyDrained

    for action in actions:
        apply(action)                   // Evolve / Complete / Collapse

    sleep(poll_interval)
}
```

Key properties:

- **Stateless restart.** Kill `cs run`, restart it — the loop rebuilds
  `FleetSnapshot` from disk and resumes. No in-memory invariants to
  lose.
- **Merge-before-dispatch.** `on_complete` fires *before* the next
  ready frontier is computed, so downstream workers see the
  predecessor's branch merged into their worktree base. This is why
  git lineage mirrors DAG lineage (ADR-022, DAG-aligned branching).
- **Critical-path ordering.** [`DagPolicy`] sorts the ready frontier
  by critical-path weight so the longest chain starts first. Ties
  break deterministically by id.
- **Idempotent policy.** `DagPolicy::next_actions` filters on
  `snapshot.status == Pending` before emitting `Evolve`, so a
  molecule already moved to `Running` is never re-dispatched.

## 6. Dynamic DAGs — the lateral DecayProduct drain

Molecules can decompose at runtime: `mission-controller`, `deep-think`
step 4, `idea-to-plan` all nucleate children mid-flight. These children
attach to their parent via `DecayProduct` typed links, not `Blocks` —
the parent already did the work of creating them and doesn't block on
their completion.

The runtime handles this in one place — **`DagPolicy::absorb_terminal`**.
When a tracked molecule reaches **any** terminal state — `Completed`,
`Frozen`, or `Collapsed` — splice both its `DecayProduct` and `Blocks`
targets into the edge list as `(parent, child)` edges, mark the parent
in the skip-set, and rebuild the plan.

`Collapsed` is deliberately handled **identically** to a clean
completion (task-20260706-4d1e): **`blocked-by` releases on *done*, not
on *verdict*.** A `reproduce` that concludes "refuted" (bug not
reproducible → collapse) still releases the `fix` that was `blocked-by`
it; the fix worker reads the "no repro" verdict from disk and decides.
The DAG edge carries one bit — done / not-done. This aligns `DagPolicy`
with `cosmon_state::frontier::compute_from_molecules`, which already
clears `Collapsed | Frozen` predecessors; before the alignment the two
readiness surfaces disagreed and, since dispatch is their intersection,
the fix silently never ran. (Supersedes the forward-`Blocks` half of
"option B", commit `dc66e2f`, 2026-04-14; the lateral `DecayProduct`
drain it introduced is preserved.)

The scenario harness pins the behavior:
[`tests/scenarios/collapsed-mission-orphans-children.toml`](../tests/scenarios/collapsed-mission-orphans-children.toml)
(post-fix) and the companion pre-fix red test in
`crates/cosmon-scenario/tests/scenarios.rs`.

After any splice, `DagPolicy` sets `needs_recompile = true` — the
runtime calls `recompile(store)` which re-walks the edge closure from
disk. This matters because the splice built from the parent's
`typed_links` only knows the `parent→child` edges; inter-child
`BlockedBy` edges (when one decay child blocks another) exist only on
disk. Without the recompile, N siblings would all appear ready at once
and launch N parallel workers.

## 7. The `COSMON_RUNTIME_ACTIVE` contract

Since the verb-unification (delib-20260426-1bcd #2 / task-20260426-c33f),
`cs tackle` is *always* leaf — it does not auto-detect DAG roots, so
the env var no longer guards an active routing decision in the current
binary. It is preserved as a defensive marker for two reasons:

- **Pre-grace `cs tackle` binaries.** Operators may still have an old
  binary on `$PATH` that auto-detects. With the var set, those binaries
  fall through to leaf-dispatch instead of nesting a runtime.
- **Audit & observability.** The env var lets downstream tooling
  recognise that a `cs tackle` invocation was triggered by a parent
  runtime rather than a human.

It is set to `"1"` in two places:

- `SubprocessExecutor::dispatch` — when spawning `cs tackle <id>` for
  a child molecule.
- `SubprocessExecutor::drain_native_tail` — when re-entering
  `cs tackle <id>` to drive a native-tail step.

Human operators should never set `COSMON_RUNTIME_ACTIVE` manually.
Workers inside a tmux pane launched by `cs tackle` inherit it
automatically.

## 8. Known limits (and their mitigation)

### Limit 1 — runtime scope is frozen at compile-plan time

`DagPolicy` bootstraps via `compile_plan(store, [root])` which walks
the transitive closure of `Blocks` / `BlockedBy` / `DecayProduct`
edges *from the molecules that exist on disk at that moment*. A
molecule nucleated dynamically by a worker — `mission-controller`'s
decompose step, `deep-think`'s step 4 panel, any formula that spawns
siblings — is visible to the runtime **only** if it is reachable from
the root via typed links that already exist.

In practice:

- A decay child linked to its parent via `DecayProduct` **is** reached
  on the parent's transition (the splice in `absorb_completion`
  handles this).
- A molecule nucleated by a worker *without* a typed link back to the
  root (e.g. a researcher spawning a side-task that uses `Refines`,
  not `Blocks`) is **invisible** to the runtime. It sits `Pending`
  on disk; the patrol propeller may pick it up, but the containing
  runtime will not.

**Mitigation (opt-in, introduced 2026-04-14):** see
[`RuntimeConfig::sweep_orphan_descendants_every`](../crates/cosmon-runtime/src/lib.rs).
When set, the runtime re-runs `compile_plan` on its current
`known_molecules` set every N ticks, absorbing any newly-reachable
pending descendants into the plan. Default is `None` (disabled) so
the change is zero-behavior-diff for existing pipelines. See
[ADR-038](adr/038-runtime-adaptive-scope.md) for rationale and the
discussion of the stronger "global pending sweep" variant that was
*not* adopted.

### Limit 2 — runtime exits when the root collapses

If the root molecule itself collapses, `DagPolicy` marks it absorbed
(collapse branch) and, if no `DecayProduct` children exist, returns an
empty action batch. The runtime observes no `Running` molecules, no
`Pending` in the ready frontier, and declares `PolicyDrained`. Any
*other* subgraph that was merely reachable from the root — a sibling
decomposition subtree, a lateral `Refines` graft — is orphaned.

This is the "collapse of the root exits `cs run`" pathology. It is
visible when a mission-controller root collapses partway through
decomposition: decay children run (see Limit 1's `absorb_terminal`
splice), but if no `DecayProduct` edges were recorded the runtime exits.

**Status:** not yet fixed. [ADR-038](adr/038-runtime-adaptive-scope.md)
proposes a `continue_after_root_collapse` config option with an
explicit event `RootCollapsedRunContinuedOnChildren`; the pilot's
arbitration on whether to implement now or defer is pending.

Workaround: re-run `cs run <any-surviving-child>` to pick up the
remaining subgraph. Since `cs run` is stateless, this is safe.

## 9. Reading the runtime's output

`cs run` and `cs watch` share the same renderer
([`crate::event_log`](../crates/cosmon-cli/src/event_log.rs)). Events
print as state diffs; a heartbeat line shows worker count, running
count, and session elapsed. `cs run`'s header label is `run`;
`cs watch`'s is `watch`.

JSON mode (`--json` via the global flag) suppresses all human output
and emits one summary object at termination:

```json
{
  "root": "task-20260414-abcd",
  "policy": "dag",
  "reason": "PolicyDrained",
  "ticks": 42,
  "actions_applied": 11,
  "molecules": [ { "id": "…", "status": "completed" }, … ]
}
```

This is the agent-first interface — operators who want a single
terminal status use `cs peek` or the human-readable summary.

## 10. Related ADRs and docs

- **[ADR-016 — Autonomy regimes and resident runtime](adr/016-autonomy-regimes-and-resident-runtime.md)** — the governing ADR for the two-layer model.
- **[ADR-022 — Native DAG scheduler](adr/022-native-dag-scheduler.md)** — `DagPolicy` design.
- **[ADR-026 — Dynamic fleet orchestration](adr/026-dynamic-fleet-orchestration.md)** — decay-child splicing.
- **[ADR-028 — Cosmon observability](adr/028-cosmon-observability.md)** — `cs peek`, event log, renderer.
- **[ADR-038 — Runtime adaptive scope](adr/038-runtime-adaptive-scope.md)** *(draft)* — addresses Limits 1 and 2.
- **[`docs/architectural-invariants.md`](architectural-invariants.md)** — non-negotiable rules for adding or modifying commands.
- **[`docs/DAG-GUIDE.md`](DAG-GUIDE.md)** — operator-facing guide to building DAGs.
- **[`docs/handbook.md`](handbook.md)** — conceptual operator handbook.

[`Policy`]: ../crates/cosmon-runtime/src/lib.rs
[`DagPolicy`]: ../crates/cosmon-runtime/src/dag_policy.rs
