# Formula reference

> These commands use physics-inspired names (nucleate, evolve, decay, spore, …). New to the vocabulary? See [The physics vocabulary](../explanation/physics-vocabulary.md).

A **formula** is a TOML template that defines a workflow: the ordered
steps a molecule advances through, its kind prefix, and (for
decomposition formulas) the child molecules it nucleates. Formulas are
the *only* extension point; you extend cosmon by writing a formula, not
by adding a command.

> This page is a **hand-written stub** (ADR-B1′ open-Q4). The formula
> schema is stable enough to document by hand today; a future revision may
> generate it from the formula type via `schemars`. It is covered by the
> link check, not the generated golden diff.

## Where formulas live

Formulas are discovered from `.cosmon/formulas/*.formula.toml` in the
galaxy (walk-up from the worker's worktree, same as every `cs` command).
`cs nucleate <formula>` looks the name up there.

For the catalog of formulas cosmon ships — which ones `cs init` writes into
that directory for you, and which ones you copy in from the repository — see
the [Formula catalog](./formula-catalog.md).

## Anatomy of a formula

```toml
formula = "task-work"          # the name passed to `cs nucleate`
version = 1
description = """
Human-readable summary rendered into briefing.md.
"""
id_prefix = "task"             # molecule ids become task-YYYYMMDD-xxxx

# Optional. What these steps need of the worker that runs them.
requires_capabilities = ["shell", "vcs"]

[tier]
level = 0                       # 0 = leaf (no child nucleation)

[[steps]]
id = "implement"
title = "Implement the solution"
description = "What the worker does in this step."
acceptance = "The exit criterion the step must meet before advancing."
# Optional: declare the normal final response as this step's deliverable.
# The path is relative to the canonical molecule directory.
response_artifact = "result.md"

[[steps]]
id = "verify"
title = "Verify and validate"
description = "..."
acceptance = "cargo check + test + clippy + fmt all pass"
```

| Field | Role |
|-------|------|
| `formula` | The name `cs nucleate <name>` resolves. |
| `version` | Schema/version of this formula. |
| `description` | Rendered into the molecule's `briefing.md`. |
| `id_prefix` | Prefix of every molecule id nucleated from this formula. |
| `[tier] level` | `0` = leaf (no children); higher tiers may decompose. |
| `[[steps]]` | Ordered steps. Each `cs evolve` advances one step. |
| `steps.acceptance` | The exit criterion sealed into `briefing.md` per step. |
| `steps.response_artifact` | Optional relative destination for a normal, non-empty worker final response. |
| `requires_capabilities` | Optional. Worker faculties these steps need: `shell`, `vcs`, `cs-cli`. |

### Declared worker deliverables

An owned in-process worker advances only the formula step it was dispatched
for. Set `response_artifact` when that step's deliverable is the worker's final
text. Cosmon publishes the exact non-empty response atomically below the
canonical molecule directory before recording the transition; absolute paths,
`..`, and symlink escapes are refused. When the tenant single-result directory
is active, the same bytes are also published as `result.md`.

For file-producing steps, use `acceptance_artifacts` and structured
`verification`; only fresh artifacts declared by the current step count. A
legacy code step with neither declaration needs a turn-scoped worktree change.
A tool attempt, a read-only call, or an unchanged worktree is telemetry, not
deliverable evidence. One accepted response never skips a later worker, gate,
native, query, or checkpointed-model step.

### `requires_capabilities`

A formula whose steps *are* shell work — run the gate toolchain, execute a
producer script, resolve a merge conflict — cannot be satisfied by a
chat-only adapter, however carefully its prompts are worded. Declaring the
requirement makes `cs tackle` refuse the pairing up front (exit code `17`)
instead of spending a run on a mission that could never complete: no
worktree, no pane, no model call, and the molecule stays pending and
re-tacklable.

Today the split is exactly chat-loop versus coding agent — a local adapter
(`local` / `ollama` / `llama-cpp` / `llama`) has none of the three, every
other adapter has all three. The field is opt-in: a formula that omits it
dispatches everywhere it did before. An unrecognised token fails the
formula load rather than being ignored, because a silently-dropped
requirement is a formula claiming a gate it does not enforce.

`COSMON_SKIP_CAPABILITY_GATE=1` dispatches anyway, for an operator
deliberately experimenting on the local floor.

## Variables

`cs nucleate <formula> --var topic="…"` binds template variables. Each
variable is rendered into `prompt.md` (sealed at nucleation) and made
available to the step descriptions.

## Decomposition formulas

A formula whose steps nucleate child molecules (e.g. `deep-think` step 4,
`mission-controller` decompose) **must** tag each child `temp:warm`
immediately after nucleation: preventive backlog curation. See the
temperature-tag how-to and the composability principle in the project
CLAUDE.md.

## Related commands

- [`cs nucleate`](./lifecycle.md): create a molecule from a formula.
- [`cs evolve`](./lifecycle.md): advance a molecule one step.
- [`cs spore`](./execution.md): germinate a whole polymer from a
  shareable `spore.toml` template.
