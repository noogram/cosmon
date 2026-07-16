# Formula catalog

> These commands use physics-inspired names (nucleate, evolve, decay, spore, …). New to the vocabulary? See [The physics vocabulary](../explanation/physics-vocabulary.md).

[Formulas: the only extension point](../explanation/formulas.md) says what a
formula *is*; the [Formula reference](./formulas.md) gives the schema you write
one against. This page answers the third question: **which formulas do you
already have?**

Two sets, and the difference matters:

- **Built-in formulas** are compiled into the `cs` binary. `cs init` writes them
  into `.cosmon/formulas/`, so `cs nucleate <name>` resolves on the very first
  invocation of a brand-new project.
- **Repository formulas** live in the cosmon source tree only. They are not in
  the binary, so `cs nucleate` will *not* find them until you copy the
  `.formula.toml` into your own `.cosmon/formulas/`.

The **Tier** column is the `[tier] level` field. Tier 0 is a leaf: it runs its
steps and completes, never creating child molecules. A higher tier may
*decompose* — its steps nucleate children, and the ordinal guard requires each
child's tier to sit strictly below its parent's.

## Built-in formulas

These nine arrive with every `cs init`.

| Formula | What it's for | Tier |
|---------|---------------|------|
| `task-work` | Execute one concrete engineering task: implement, then verify against the project's build/test/lint gates. The default for code. | 0 |
| `editorial-work` | The prose counterpart of `task-work`: draft, then verify. For deliverables that are a document rather than compiled code, where the exit criteria are editorial, not a compiler. | 0 |
| `deep-think` | Structured multi-perspective deliberation. Frames a question, runs a panel of expert personas in parallel, synthesizes convergences and divergences, then nucleates the follow-up work the panel identified. | 1 |
| `deep-think-inline` | The same panel, run inline by a single worker, producing a synthesis and a recommendation but never nucleating children. The leaf variant a Tier-1 controller can commission without violating the tier guard. | 0 |
| `idea-to-plan` | Take a raw idea through capture and feasibility assessment, then turn it into a small, finite set of actionable child molecules. | 1 |
| `mission-plan` | Compile a goal plus a fleet template into a DAG of task molecules assigned to fleet roles. Completes once the decomposition is done. | 1 |
| `mission-controller` | A mission planner that *persists*. It freezes after decomposing rather than completing, and downstream agents thaw it to feed results back and spawn new work across the mission's lifetime. | 1 |
| `temp-review` | Sweep the backlog: scan every pending molecule, triage it by age and temperature tag, and report the backlog's shape. See [Curate the backlog with temperature tags](../how-to/temperature-tags.md). | 0 |
| `verify-surface` | Render a visual surface and observe it from an independent molecule. Built in because the `surface_visual` gate refuses `cs complete` until a sibling `verify-surface` has landed green — a project without this formula could not satisfy the refusal. | 0 |

## Repository formulas

These ship in the cosmon repository under `.cosmon/formulas/`, but **not** in
the binary. Copy the file into your project's `.cosmon/formulas/` before
nucleating it. They are listed here because they are general-purpose; the
repository also carries formulas wired to cosmon's own maintenance workflow,
which are deliberately out of scope for this catalog.

| Formula | What it's for | Tier |
|---------|---------------|------|
| `producer-work` | Build a runner, pipeline, harness, or ingester — anything whose promised value is an output record. Adds a smoke-dispatch gate that executes the real production path and refuses to advance unless it leaves a non-empty output artifact. Compilation and unit tests cannot establish that a producer produced. | 0 |
| `bug-closure` | After a fix lands, walk the bug's whole semantic surface — help text, tests, docs, callers, invariants — and return a verdict: closed, or reopened naming the surfaces still uncovered. The companion ritual that stops a verb being repaired one half at a time. | 0 |
| `merge-conflict` | Resolve a merge conflict on a feature branch as a typed molecule, with a bounded retry count instead of unbounded escalation. | 0 |
| `visual-qa` | The gate for deliverables that are *seen* rather than compiled — decks, posters, rendered diagrams. Renders, rasterises, reads the pixels, runs an adversarial layout checklist, and fails closed. Neither `task-work` nor `editorial-work` ever looks at the rendered page. | 0 |
| `fleet-review` | Read the event log and molecule state over the last N days, compute vital signs (collapse rate, duration per step, backlog pressure), and emit a health report. Observation only: no suggestions, no config changes. | 0 |
| `retrospective` | Read the event log, ensemble snapshot, and patrol diagnostic; classify deviations into typed dysfunctions; and propose each fix as a `temp:warm` child for human triage. Proposes, never auto-fixes. | 1 |
| `map` | The fan-out pattern: apply a per-item formula to each element of a collection, nucleating N children in parallel. | 1 |
| `reduce` | The fan-in counterpart of `map`: consolidate N children's outputs into a single synthesis, ordered behind them on the DAG. | 0 |
| `while` | The iteration pattern: nucleate a body formula repeatedly until a condition holds or `max_iterations` is exhausted, recording each turn of the loop as a DAG edge. | 1 |
| `spark` | Capture a one-line intent as an untackled molecule on the backlog, with no worktree and no worker. The formula companion to the [`cs spark`](./lifecycle.md) verb. | 0 |

`map`, `reduce`, and `while` are worth reading together: they are cosmon's
demonstration that control flow needs no new machinery. Each is a plain TOML
formula over `cs nucleate --blocked-by`, not a new molecule kind and not a new
Rust type.

## Writing your own

None of these is privileged. A formula is a TOML file you drop into
`.cosmon/formulas/`, and `cs nucleate` resolves it by walk-up exactly as it
resolves the built-ins — see the [Formula reference](./formulas.md) for the
schema. Starting from the closest formula here and editing it is usually faster
than starting from the schema.

## See also

- [Formulas: the only extension point](../explanation/formulas.md): why the
  extension surface is formulas rather than commands.
- [Formula reference](./formulas.md): the full field table.
- [Your first molecule](../tutorials/first-molecule.md): a formula run end to end.
