# Molecule Metadata: Tags and Notes

Two lightweight, orthogonal metadata layers sit on top of every molecule:

- **Tags** — typed labels that classify, group, and filter.
- **Notes** — append-only audit-trail comments.

They share a design commitment: *metadata is cheap, semantics live in the
code that reads it*. Tags and notes never drive a state transition. They
are observable artifacts for humans and agents to coordinate over the
same molecule without inflating the core lifecycle.

---

## Tags

A **tag** is a short, typed label attached to a molecule. Tags follow a
`key:value` convention:

```
deferred:yes
priority:high
area:cli
bug
```

The key is kebab-case (`[a-z][a-z0-9-]*`) and mandatory; the value is
optional (printable ASCII, no whitespace, no `:`, 1–64 bytes). Full tag
length is capped at 128 bytes. Validation lives in
[`cosmon_core::tag::Tag`](../crates/cosmon-core/src/tag.rs).

### Conventions

Tags form a small shared vocabulary. Suggested namespaces:

| Key family | Purpose | Examples |
|------------|---------|----------|
| `priority:*` | Urgency hints | `priority:high`, `priority:low` |
| `deferred:*` | Snooze state | `deferred:yes`, `deferred:review` |
| `area:*` | Subsystem | `area:cli`, `area:runtime` |
| `type:*` | Nature | `type:regression`, `type:docs` |
| bare | Boolean flag | `blocked`, `wip` |

Keep the vocabulary stable — a tag used once is noise; a tag used 50
times is a view.

### CLI

```bash
# Attach tags at creation time (repeatable)
cs nucleate task-work --tag deferred:yes --tag area:cli

# Mutate tags on an existing molecule (idempotent)
cs tag task-20260411-abcd --add priority:high --remove deferred:yes

# Filter ensembles by tag (glob, any-match across --tag)
cs ensemble --tag 'deferred:*'
cs ensemble --tag 'priority:*' --tag 'area:cli'
```

The glob syntax supports `*` only. Patterns match against the full
`key[:value]` string.

### Storage

Tags live on `MoleculeData.tags` as a `BTreeSet<Tag>`, which gives
deduplicated, deterministically ordered serialization into `state.json`.
Legacy state files without the field deserialize to an empty set.

### Surface projection

`STATUS.md` includes a `Tags` column in the per-fleet molecule table.
Running `cs reconcile` re-derives the surface after any tag mutation.

---

## Notes

A **note** is a timestamped Markdown comment attached to a molecule.
Notes are **append-only**: once written, the file must never be edited
or deleted. The on-disk layout is an auditable trail any observer can
read without opening `state.json`.

### Storage layout

```
.cosmon/state/fleets/<fleet>/molecules/<id>/notes/
  001-human.md
  002-worker-onyx.md
  003-human.md
  ...
```

- File name: `NNN-author.md` where `NNN` is a zero-padded monotonic
  sequence number and `author` is either the worker id (when written
  with `--as-worker`) or the literal `human`.
- File body: YAML frontmatter + Markdown body.

```markdown
---
seq: 2
author: worker:onyx
timestamp: 2026-04-11T15:04:05+00:00
---
first observation on this molecule
```

### CLI

```bash
# Append a note as a human operator
cs note task-20260411-abcd "first observation"

# Compose in $EDITOR (opens a tempfile, writes on close)
cs note task-20260411-abcd --edit

# Worker-authored note (run inside a worktree by the agent)
cs note task-20260411-abcd --as-worker onyx "resumed after freeze"
```

### Observing notes

`cs observe <id>` renders the last N notes in the detail view (default
`--notes 3`; pass `--notes 0` to hide them entirely). The rendering is
collapsible by design — only the trailing window is shown so detail
mode stays readable even for long-running molecules.

### Append-only discipline

Workers and humans should treat notes as *events*, not a scratchpad. If
an observation needs correcting, add a new note that references the
wrong one by `seq`. Deleting or rewriting an existing note corrupts the
trail and is forbidden.

The sequence number is recomputed by scanning the directory on every
write, so a concurrent `cs note` from a second process cannot overwrite
an existing file — it simply picks the next index.

---

## Relationship to the core lifecycle

Neither tags nor notes participate in the typestate machine. They do
not gate `cs evolve`, they do not block `cs done`, and they never fire
events. That keeps the core small and the metadata layer composable:

- **Tags** are for classification and filtering (read by humans and
  ensemble queries).
- **Notes** are for audit and narrative (read by humans and by
  downstream agents reviewing context).

When in doubt, prefer a tag for structured signals and a note for prose.
