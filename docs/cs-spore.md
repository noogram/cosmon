# `cs spore`: germinate a whole polymer from a shareable template

`cs spore` germinates an entire polymer (a DAG of molecules) from one
shareable `spore.toml` template, the way `cs nucleate` germinates a single
molecule. It is a declarative front end over the existing `cs nucleate`
verb: not a new scheduler, not a new molecule type.

Implementation:
[`crates/cosmon-cli/src/cmd/spore.rs`](../crates/cosmon-cli/src/cmd/spore.rs)
(shell) over the pure core in
[`crates/cosmon-core/src/spore/`](../crates/cosmon-core/src/spore/)
(`mod.rs` parser, `expand.rs` expansion, `seal.rs` seal types).
Governing decision: [ADR-140](adr/140-spore-format-expand-deterministic-cache-astra.md).
Design decomposition:
[docs/design/spore-impl-dag-manifest.md](design/spore-impl-dag-manifest.md).

## What a spore is

A **spore** is a parameterizable mission plan. Its `spore.toml` declares:

```
Spore = Fleet (crew) + [Formula] (per-node recipes) + ParamSchema
      + DAG-of-typed-edges + an optional .tla seal
```

- `[spore]`: name, version, description.
- `[spore.params.*]`: the `ParamSchema`. Each param has a `type`
  (`string`, `int`, `bool`, `enum`, `list<string>`), a `required` flag,
  and an optional `default`.
- `[spore.formulas.*]`: named recipe aliases, each pointing at a
  `*.formula.toml` path relative to the manifest.
- `[[spore.node]]`: a node, with an explicit `kind`
  (`fixed` / `fanout` / `emergent`), a `formula` alias, and per-node
  `vars`. A `fanout` node carries `for_each`; an `emergent` node MUST
  carry a `[spore.node.bounds]` ceiling.
- `[[spore.edge]]`: a typed `blocked-by` edge (`from`, `to`, `type`).
  The edge set must be acyclic.
- `[spore.seal]`: optional, points at a `.tla` module (and `.cfg`) that
  proves a property of the plan.
- `[spore.astra]`: optional RO-Crate / ASTRA emission config.

The parser is fail-closed (ADR-140). It rejects: an emergent node without
bounds, an edge cycle, an unknown node kind, a param-type mismatch, plus
structural checks (duplicate node ids, dangling edges, unknown formula or
edge aliases).

## The three verbs

| Verb | Role |
|------|------|
| `cs spore validate <ref>` | Parse (N2) + expand (N3) as a **dry run**. Prints the ordered `cs nucleate ... --blocked-by ...` call list. Germinates nothing. |
| `cs spore run <ref>` | Parse + expand + **seal gate** (N4), then germinate the polymer into the live state store. |
| `cs spore export <ref>` | Emit a content-addressed bundle hash plus an ASTRA descriptive layer (D6) for sharing the spore. |

`<ref>` is a `spore.toml` file or a directory containing one.

## Usage

```
cs spore validate ./spore.toml --var subject="octopus cognition"
cs spore validate ./spore.toml --json                # NDJSON expansion

cs spore run ./spore.toml --var subject="..." --var axes=a,b,c
cs spore run ./bundle/ --fleet default               # directory ref
cs spore run ./spore.toml --allow-unchecked-seal     # sealed, no TLC
cs spore run ./spore.toml --json                     # one NDJSON line/molecule

cs spore export ./spore.toml                         # bundle hash to stdout
cs spore export ./spore.toml --out dist/             # ASTRA into dist/
```

### `--var key=value`

Repeatable. Each value is coerced into the param's declared `ParamSchema`
type before expansion: `int` and `bool` parse from the string, a
`list<string>` splits on commas (`axes=a,b,c`), `string` and `enum` stay
raw and are checked by `expand`. An undeclared key is rejected by the
expansion (a single source of truth for schema membership).

### `--json`

`validate` and `run` honor `--json` for the agent-first invariant.
`validate --json` prints one NDJSON object per expanded call (`alias`,
`formula`, `kind`, `blocked_by`, `vars`, `for_each`, `bounds`).
`run --json` prints one NDJSON object per germinated molecule (`alias`,
`id`, `formula`, `blocked_by`, `status`); the seal status note is written
to **stderr** so stdout stays clean NDJSON.

### `cs spore run` side effects

Each germinated molecule is tagged `temp:warm`
(decomposition-auto-tag discipline) and wired to its `blocked-by`
predecessors. The expansion is ordered so every `blocked_by` alias is
already germinated when its dependent is reached, so the alias-to-id
wiring always resolves on disk.

## The seal gate (ADR-140 D4), stated honestly

`cs spore run` never claims a seal is verified when it is not.

- A spore with **no** `[spore.seal]` germinates freely. Status: `seal: none`.
- A **sealed** spore cannot be proven on a machine without the TLC
  verifier wired in, so `cs spore run` **fails closed** by default and
  refuses to germinate.
- Pass `--allow-unchecked-seal` to opt into the risk. The status line then
  reads `seal: present, NOT verified`, never `verified`.

`cs spore validate` reports the seal label read-only and never refuses;
the gate is a `run`-time concern.

The bundle hash from `cs spore export` is content-addressed: a stable
`blake3:` id over the manifest and every recipe and seal file it
references, in sorted order. The same bundle content always yields the
same id (content-addressing is the registry, ADR-039). The ASTRA layer
attaches the seal verdict honestly, marked present/absent and never
claimed verified.

## Example

```
$ cs spore validate ./spore.toml --var subject="octopus cognition"
spore: demo (v1) - 3 call(s)
seal: none
  • frame [fixed]
      formula: work.formula.toml
      var subject = octopus cognition
  • analyse-0 [fanout]
      formula: work.formula.toml
      blocked-by: frame
      var axis = a
  • analyse-1 [fanout]
      formula: work.formula.toml
      blocked-by: frame
      var axis = b

$ cs spore run ./spore.toml --var subject="octopus cognition"
seal: none
Germinated spore demo into 3 molecule(s):
  task-20260629-... (work)
  task-20260629-... (work)
  task-20260629-... (work)
```

## See also

- `cs help spore`, `cs spore --help`, and `cs spore <verb> --help` for
  the live CLI reference (single source of truth: the clap tree).
- `man cs` (the `SPORE` section of the `DESCRIPTION`).
- `cs nucleate`: germinate a single molecule (the primitive a spore
  replays).
- `cs run`: walk a DAG of molecules that already exist (a spore creates
  them first).
- [ADR-140](adr/140-spore-format-expand-deterministic-cache-astra.md): format and contract.
- [docs/design/spore-impl-dag-manifest.md](design/spore-impl-dag-manifest.md):
  the implementation decomposition (N1 through N8).
