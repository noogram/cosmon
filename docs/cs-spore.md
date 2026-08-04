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

## The bundle's recipes must reach the mission registry

`cs spore run` reads the bundle's formulas **by path**, relative to the
manifest. What it germinates stores them **by id**. `cs tackle` later
resolves that id against the mission project's `.cosmon/formulas/` — the
registry of the galaxy the molecule now lives in, not the directory the
spore came from.

So a bundle whose recipes were never copied into that registry germinates
fine and then dispatches with none of its per-step `adapter` / `model`
pins: the id resolves to nothing and every node runs on the adapter
default. This was silent until task-20260725-eb3b, and cost a 23-node
run its entire documented model tiering while the recorded reason read
"no formula-step model pin" — a sentence about a recipe that pins
nothing, not one that was never found.

Two things now say it out loud:

- **at germination** — `cs spore run` warns, once per recipe, when a
  bundle formula that declares pins is absent from the mission registry,
  or is *shadowed* there by a same-named copy declaring different pins
  (dispatch would honour the registry's copy, not the bundle's). It names
  the path to install to.
- **at dispatch** — `cs tackle` warns on stderr and records the real
  cause in the `adapter_selected` / `model_selected` events, so an audit
  after the fact can tell a broken reference from a deliberate absence.

The remedy is a copy: put each `[spore.formulas.*]` file into
`.cosmon/formulas/` of the project you germinate into. Neither warning
refuses the run — a run without its pins is degraded, not invalid, and
you may be installing the recipes next.

`cs spore install` (below) is that copy, done correctly: it registers each
recipe under the name the recipe *declares*, which is the name `cs tackle`
resolves.

## The four verbs

| Verb | Role |
|------|------|
| `cs spore install <source>` | Fetch a bundle (git remote or local path) and **place** it into this project, registering its recipes in `.cosmon/formulas/`. |
| `cs spore validate <ref>` | Parse (N2) + expand (N3) as a **dry run**. Prints the ordered `cs nucleate ... --blocked-by ...` call list. Germinates nothing. |
| `cs spore run <ref>` | Parse + expand + **seal gate** (N4), then germinate the polymer into the live state store. |
| `cs spore export <ref>` | Emit a content-addressed bundle hash plus an ASTRA descriptive layer (D6) for sharing the spore. |

`<ref>` is a `spore.toml` file or a directory containing one.

## `cs spore install`: getting a bundle in the first place

The three verbs above all start from a bundle that is already on disk. Getting
it there was the step with no verb: clone or copy by hand, decide where it
lives, and — the part that actually bites — copy its recipes into
`.cosmon/formulas/`. That last step is the section above, *The bundle's recipes
must reach the mission registry*: skip it and the spore germinates fine and then
dispatches with every per-step `adapter`/`model` pin inert.

`cs spore install` is both steps.

```
cs spore install github:noogram/cosmon/spores/cosmon-dev
cs spore install https://github.com/noogram/cosmon/tree/main/spores/cosmon-dev
cs spore install ../shared/bundle --dest spores/shared
cs spore install github:o/r@v1 --expect-hash blake3:1a2b…    # verified fetch
cs spore install github:o/r --dry-run --json                 # plan only
```

### Why `install`, and not `add` or `import`

The verb's load-bearing effect is not the copy — it is the **registration** of
the bundle's recipes into the registry the dispatcher reads. That is what
"install" means.

- `add` is `cargo add`: it edits a dependency manifest. Cosmon has no dependency
  manifest for spores, so `add` would name a file that does not exist.
- `import` reads as the inverse of `export`, and it is not: `cs spore export`
  emits a hash and an RO-Crate *in place*, not a fetchable artifact. The inverse
  of export is checking the hash — which install does, under `--expect-hash`.

### `<SOURCE>`

| Spelling | Meaning |
|----------|---------|
| `./path/to/bundle` or `./bundle/spore.toml` | a local directory, copied |
| `github:owner/repo[/subdir][@ref]` | the shorthand |
| `https://github.com/owner/repo/tree/<ref>/<subdir>` | what the browser puts on the clipboard |
| `https://github.com/owner/repo/blob/<ref>/<subdir>/spore.toml` | same, pasted from the manifest's own page (the file name is dropped) |
| any other git remote (`https://`, `ssh://`, `git@host:path`, `file://`) | taken verbatim; pin its ref and path with `--git-ref` / `--subdir` |

`--git-ref` and `--subdir` always override whatever the URL encoded, which is
what makes a non-GitHub remote as addressable as a GitHub one without inventing
an ambiguous suffix grammar (`git@host:path` already contains an `@`).

### Where it lands, and what it registers

The bundle is copied to `<project>/spores/<spore-name>/` unless `--dest` says
otherwise. The project is the one whose `.cosmon/formulas/` the verb resolved —
not the process CWD — so the two halves of an install cannot end up in
different trees.

Each `[spore.formulas.*]` recipe is then written to
`.cosmon/formulas/<name>.formula.toml`, where `<name>` is the name the recipe
**declares** (`formula = "..."`), not the file name it had in the bundle. That
distinction is the whole point: a molecule stores its formula by id and
`cs tackle` resolves that id, so installing `recipe.formula.toml` under its file
name would put bytes in the registry and leave the pins just as unreachable as
never installing at all.

An install also writes `.spore-install.toml` into the destination, recording the
source, the resolved commit, and the bundle hash — a branch name alone does not
answer "a copy of what?" a week later. It is dot-prefixed and outside the
coverage set the bundle hash binds, so writing it never changes the id of the
bundle it describes.

### What it refuses, all before anything is written

- a bundle whose hash does not match `--expect-hash`;
- a bundle missing a file its own manifest declares (an incomplete bundle
  parses, then fails at germination — a worse place to learn it);
- a **symlink** anywhere in the fetched tree. It is refused, not followed and
  not skipped: the tree comes from a remote, and `spore.toml -> /etc/passwd` is
  the cheapest way to make a copy write bytes under a name the operator trusts;
- a relative path inside the bundle that would escape the destination (`..`,
  absolute, rooted);
- a non-empty destination, without `--force`;
- a registry that already holds a **different** recipe of the same name, without
  `--force` — overwriting changes what every molecule already germinated from
  that id will run.

A registry recipe that is byte-identical is a no-op, not a conflict, so
re-installing an unchanged bundle is idempotent and does not train an operator
to pass `--force` (which is how the one real conflict goes through unread).

`--no-formulas` places the bundle and leaves the registry alone; the report
still lists every recipe, marked `skipped`, because an operator who opted out
still needs to see which pins will not reach dispatch. `--dry-run` prints the
same report and writes nothing.

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
