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

[tier]
level = 0                       # 0 = leaf (no child nucleation)

[[steps]]
id = "implement"
title = "Implement the solution"
description = "What the worker does in this step."
acceptance = "The exit criterion the step must meet before advancing."

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
