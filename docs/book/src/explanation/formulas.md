# Formulas: the only extension point

A **formula** is the recipe a molecule follows. It is a TOML template of ordered
steps, and it is the *one* way you extend cosmon. This page says what a formula
is and why it is the only extension point; for the full field table, it hands you
off to the [Formula reference](../reference/formulas.md).

## Template and instance

A formula is a template; a molecule is one running instance of it. The formula is
immutable text on disk: the same `task-work.formula.toml` describes every task
molecule ever nucleated from it. The molecule is the thing that lives and dies: it
has a state, a current step, and a durable trace of what happened. Writing the
formula does not run anything; `cs nucleate <formula>` stamps out a molecule that
does.

The relation is exactly the one the glossary draws for the whole vocabulary; see
the template/instance table in [The physics vocabulary](./physics-vocabulary.md).
`formula ─nucleate→ molecule` is the single-unit case; a spore germinating a
polymer is the same relation one scale up.

## Why formulas are the only extension point

You extend cosmon by writing a formula, not by adding a command, a daemon, a
plugin interface, or a new state store. Everything cosmon tracks is a molecule,
and every workflow is a formula over molecules. A bug report, a design decision, a
multi-perspective deliberation, a backlog sweep: each is a molecule running some
formula. This is the composability principle: one concept, and the extension
surface is the formulas you write on top of it.

The discipline that follows: before reaching for new machinery, ask whether the
thing can be a formula over existing molecules. It almost always can. A formula
whose steps exist only to satisfy the system (a single trivial step wrapping one
command) is the signal that the abstraction is being over-applied, not that
cosmon needs a new primitive.

## Anatomy at a glance

A formula names itself, declares an id prefix for the molecules it produces, and
lists ordered steps with exit criteria:

```toml
formula = "task-work"
id_prefix = "task"

[tier]
level = 0            # 0 = leaf; higher tiers may nucleate children

[[steps]]
id = "implement"
title = "Implement the solution"
acceptance = "Implementation complete, compiles clean"

[[steps]]
id = "verify"
title = "Verify and validate"
acceptance = "cargo check + test + clippy + fmt all pass"
```

Each `cs evolve` advances the molecule one step and seals that step's acceptance
criterion into its briefing. The full field table (`version`, `[tier] level`,
variables, per-step fields) lives in the
[Formula reference](../reference/formulas.md).

## Decomposition formulas

A formula whose steps *nucleate child molecules* (a plan splitting into tasks, a
deliberation fanning out into follow-ups) is a decomposition formula. Its higher
`[tier] level` says it may produce children rather than just advance itself. Such
a formula must tag each child as it creates it, so no spawned molecule sits
untagged on the backlog; see
[Curate the backlog with temperature tags](../how-to/temperature-tags.md).

## See also

- [Formula catalog](../reference/formula-catalog.md): the formulas cosmon ships,
  and what each one is for.
- [Formula reference](../reference/formulas.md): the full schema and field table.
- [Your first molecule](../tutorials/first-molecule.md): a formula run end to end.
- [The physics vocabulary](./physics-vocabulary.md): formula, molecule, and the
  template/instance table.
