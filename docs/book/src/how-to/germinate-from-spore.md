# Germinate a polymer from a spore

**Goal:** you have a mission shape (a whole DAG of molecules) that you want to
reuse or share, not re-wire by hand every time. A **spore** packages that shape
as one parameterizable template; germinating it stamps out the whole running
graph in one command.

> Two words up front. A **spore** is a shareable template of an *entire* wired
> DAG: recipes ([formulas](../explanation/formulas.md)) plus
> [fleet config](../explanation/fleets.md) and an optional proof, the way a
> formula is the template of a single molecule. A **polymer** is the running
> graph a spore germinates into: a mission of linked molecules. Where a formula
> *nucleates* one molecule, a spore *germinates* the whole set. It is the same
> generative relation, one scale up.

`cs spore` is a declarative front end over `cs nucleate`, not a new scheduler and
not a new molecule type. It replays the same nucleate-and-wire calls you would
otherwise type by hand.

## What a spore declares

A spore is one `spore.toml`. It bundles:

- `[spore]`: name, version, description.
- `[spore.params.*]`: the parameters callers fill in (typed: `string`, `int`,
  `bool`, `enum`, `list<string>`), each with a `required` flag and optional
  default.
- `[spore.formulas.*]`: named recipe aliases pointing at `*.formula.toml` files.
  A [formula](../explanation/formulas.md) is the recipe a single molecule follows.
- `[[spore.node]]`: each node: a `kind` (`fixed` / `fanout` / `emergent`), a
  formula alias, and per-node variables.
- `[[spore.edge]]`: the typed `blocked-by` edges wiring the DAG. The edge set
  must be acyclic.
- `[spore.seal]` (optional): a `.tla` module that proves a property of the plan.

The parser is fail-closed: it rejects an emergent node with no bounds, an edge
cycle, an unknown node kind, a parameter-type mismatch, and duplicate or dangling
node references.

## Step 1: Validate before you germinate (dry run)

Always check what a spore *would* create before it creates anything:

```sh
cs spore validate ./spore.toml --var subject="octopus cognition"
```

`cs spore validate` parses and expands the spore as a **dry run**: it prints the
ordered list of `cs nucleate … --blocked-by …` calls it would make, and
germinates nothing. Pass `--json` for one NDJSON object per expanded call. Fill
parameters with `--var key=value` (repeatable); a `list<string>` splits on commas
(`--var axes=a,b,c`).

```
spore: demo (v1) - 3 call(s)
seal: none
  • frame [fixed]      formula: work.formula.toml
  • analyse-0 [fanout] formula: work.formula.toml  blocked-by: frame
  • analyse-1 [fanout] formula: work.formula.toml  blocked-by: frame
```

## Step 2: Germinate it

When the dry run looks right, germinate the polymer into the live state store:

```sh
cs spore run ./spore.toml --var subject="octopus cognition"
```

This nucleates every node and wires every edge, in dependency order so each
`blocked-by` reference already exists when its dependent is created. Each
germinated molecule is tagged `temp:warm` automatically (backlog-curation
discipline; see
[Curate the backlog with temperature tags](./temperature-tags.md)).

```
seal: none
Germinated spore demo into 3 molecule(s):
  task-20260629-...  (work)
  task-20260629-...  (work)
  task-20260629-...  (work)
```

You now have a live polymer. Run it exactly like the DAG you wired by hand in
[Composing a DAG](../tutorials/first-dag.md): point `cs run` at the root, or
tackle nodes as they become ready.

You can pass a directory instead of a file (`cs spore run ./bundle/`), and
`--json` prints one NDJSON line per germinated molecule.

## The seal gate, stated honestly

`cs spore run` never claims a proof is verified when it is not:

- A spore with **no** seal germinates freely (`seal: none`).
- A **sealed** spore on a machine without the TLC verifier wired in **fails
  closed** by default and refuses to germinate.
- `--allow-unchecked-seal` opts into the risk; the status then reads
  `seal: present, NOT verified`, never `verified`.

## Step 3: Share a spore

Emit a content-addressed bundle for sharing:

```sh
cs spore export ./spore.toml            # prints a blake3: bundle id
cs spore export ./spore.toml --out dist/
```

The id is a stable hash over the manifest and every recipe and seal file it
references, in sorted order: the same content always yields the same id, so the
bundle *is* its own registry entry.

## See also

- [Composing a DAG](../tutorials/first-dag.md): wiring the same shape by hand.
- [Execution commands reference](../reference/execution.md): `cs spore`, `cs run`.
- [The physics vocabulary](../explanation/physics-vocabulary.md): spore, polymer,
  germinate, and how they relate to formula and molecule.
