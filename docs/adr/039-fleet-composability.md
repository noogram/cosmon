# ADR-039: Fleet Composability

## Status

Proposed (2026-04-14) — design ready for implementation.

Derived from deliberation `delib-20260414-778e` (9-persona panel: torvalds,
tolnay, einstein, godel, feynman, wheeler, jobs, hawking, shannon). See
`synthesis.md` in the deliberation molecule for the full convergence trail.

## Context

Cosmon's composability thesis operates at three levels today:

| Level | Mechanism | Status |
|-------|-----------|--------|
| Molecules → DAG | typed links (`Blocks`, `DecayProduct`, `Refines`, `Entangled`) | shipped |
| Formulas → chains | `nucleate` with inter-formula handoff | shipped |
| Universes → cross-galaxy edges | ADR-035 (content-addressed) | designed, not shipped |
| **Fleets → composition** | **one monolithic `fleet.toml` per `.cosmon/`** | **missing** |

Each galaxy has exactly ONE `fleet.toml`. Running a wiki-fleet (5 editorial
roles) + a dev-fleet (3 engineering roles) + a blob/observer fleet together
requires copy-pasting every agent block into one file. This is structurally
anti-composability: the one layer where cosmon mandates duplication.

Shannon's audit of a realistic workspace estimates **~75-85% of a typical
`fleet.toml` is boilerplate** with near-zero conditional entropy across
projects — the compression case is strong and quantified.

Wheeler's audit surfaces the deeper problem: fleets are the one primitive
without identity. They are addressable only by filesystem path. Molecules
have `MoleculeId`, formulas have versioned TOML, universes (ADR-035) get
content-addressed ids. Fleets are the unnamed primitive. Every retrofit of
identity onto an already-shipped `fleet.toml` format will be painful.

## Decision

**SHIP fleet composability v0 in ~1 month** with a minimal, load-time
flattening design. Runtime operates on a single effective fleet. Composition
is a pure function, not a runtime concept.

### 1. Fleet identity (wheeler)

Every `fleet.toml` declares:

```toml
[fleet]
schema_version = 1          # required integer
id             = "cosmon"   # required, [a-z0-9-]+
```

The canonical `FleetId` of a composed fleet is the content hash of its
resolved composition (tuple of child `FleetId`s in declared order, plus
merge metadata). No separate publish step, no registry, no `cs fleet
publish` command in v0 — content-addressing IS the registry. Required
`fleet.id` is the two-line minimum; it pays for itself the first time we
need to retroactively attribute an event to a fleet.

This directly extends ADR-035's content-identity principle (ADR-011) from
universes to fleets. Same hashing scheme. Same reference grammar. Distinct
object (fleets compose agents; galaxies compose molecule-spaces).

### 2. `[[fleet.include]]` — inline table grammar

```toml
[[fleet.include]]
source = "file:./fleets/wiki.toml"   # URI scheme required
as     = "wiki"                      # optional; prefix applied on collision
```

**Why inline table from day one (tolnay):** a plain-string `include =
["./fleets/wiki.toml"]` cannot grow to carry `as` or non-`file:` URI
schemes (ADR-035 `git+https:`, `cas:sha256-...`) without a wire-format
break. Inline tables absorb extensions additively. The operator ergonomic
cost is one extra line of TOML per include.

**URI schemes in v0:**

| Scheme | v0 parser | v0 resolver |
|--------|-----------|-------------|
| `file:` | accepts | resolves |
| `git+https:`, `git+ssh:` | accepts | errors: "not implemented in v0" |
| `cas:sha256-...` | accepts | errors: "gated on ADR-035 Retraction P3" |

This keeps the schema forward-compatible (tolnay) while respecting godel's
decidability argument (only `file:` is statically resolvable with cheap
cycle detection). Non-`file:` schemes land in a successor ADR.

### 3. Resolver: pure `compose` at load time (einstein)

```rust
/// Pure, total, deterministic. Runtime never sees composition as a concept.
pub fn compose(fleets: &[Fleet]) -> Result<Fleet, CompositionError>;
```

Composition is a load-time reduction. `cs` operations (`tackle`, `evolve`,
`done`, ...) read the flattened fleet. **Frame-equivalence theorem**
(einstein): for any worker operation `Op`,

```
Op(worker, compose([F1, F2, F3])) ≡ Op(worker, F_flat_equivalent)
```

The symmetry breaks only in the operator's frame (debugging, provenance
attribution). To preserve observability, every flattened entry carries:

```rust
origin_fleet_id: FleetId,          // which child fleet defined this
origin_line: Option<(path, usize)>  // best-effort source pointer
```

This is the gauge-metadata: invariant under flattening because it's just a
label, but recoverable when the operator needs it.

### 4. Cycle and diamond rules (godel)

**Cycle key** = `(canonical-fs-id, content-hash)`, computed post-fetch
pre-parse. Naive path-set is unsound (symlinks, relative/absolute, CAS
aliases — the last one is theoretical in v0 since `cas:` isn't resolved).

**Diamonds** deduplicate by key: a child included via two paths merges
exactly once. Merge operates over the *leaf set*, not pairwise — this is
the only associative definition.

**Self-reference** (fleet including itself via alias or symlink): rejected
by the cycle detector. Do not attempt fixpoint computation.

**Generated includes** (`[[fleet.include]]` produced by template expansion
or agent decision): **forbidden in v0**. Undecidable in general; out of
scope until a successor ADR defines the decidability envelope.

**Transitive includes** (child fleet with its own `[[fleet.include]]`):
**forbidden in v0**. Depth-1 only. Depth-N composition is a separate design
problem (namespace flattening rules, nested merge policy).

**Wildcard / glob includes:** forbidden in v0 (ordering becomes
filesystem-dependent, `as` inference becomes necessary).

### 5. Collision policy: hard-fail + opt-in `as` rename

Agent ids must be unique after flattening. Two child fleets declaring an
agent with the same `id` is a compose-time error:

```
error: duplicate agent id 'reviewer' in fleet 'blob-master'
  first defined in ./fleets/wiki.toml (line 8)
  redefined in   ./fleets/dev.toml  (line 4)
hint: add `as = "wiki"` to the wiki include (or `as = "dev"` to the dev
      include) to namespace it, or rename one of the agents.
```

When `as = "wiki"` is set on an include, every agent `id` from that child
is exposed as `wiki/<id>`. This is the operator's rename tool — explicit,
grep-able, stable across file moves.

**Rejected alternatives:**

- **Automatic namespacing from filename** (proposed by jobs): breaks every
  molecule reference to `wiki:editor` the day someone renames `wiki.toml`
  to `editorial.toml`. Violates the "we don't break userspace" rule.
- **Last-wins silent override**: makes fleet order load-bearing (einstein:
  "time-travel paradox waiting to happen"). Violates shannon's SNR
  argument (silently fabricates a config no author wrote).
- **Three-way merge policy (intersection/union/explicit)**: pairwise
  `explicit-resolution` is non-associative (godel's first Gödel
  sentence of the resolver), and the policy matrix is a support surface
  cosmon does not need. Hard-fail + explicit rename subsumes every real
  use case with one rule.

### 6. Fields reserved to the master

The following fields are valid ONLY in the master `fleet.toml` (the one
with `[[fleet.include]]` blocks); they are rejected at load in children:

- `[runtime]`, `[hooks.post_merge]`, `[surfaces]` — execution envelope is
  one per composite.
- `fleet.socket` / tmux socket name — see §7.
- `fleet.constitution_root` — one constitution anchor per running fleet.

Children MAY declare `fleet.id`; the master uses the include's `as` to
rename if needed.

### 7. Tmux socket: ONE per composite (einstein)

The fleet-scoped tmux socket (commit 11dfe6f) isolates process graphs that
share one state store. A composed fleet has ONE state store
(`.cosmon/state/` under the master) → ONE socket.

N sockets (one per child) would partition one state store into three
process-visibility regions, breaking the data-plane invariant. Child
identity is carried by `origin_fleet_id` metadata on each agent, not by
the socket.

### 8. Constitution hash-pinning at nucleation (hawking)

**The drift bomb to disarm.** If the master includes `wiki.toml@v1` and an
operator edits `wiki.toml` in place to v2, existing running `wiki:editor`
molecules already hold v1 constitutions in their checkpoint state. Without
a pin, `cs reconcile` or the next worker tick silently re-materializes v2
into a running molecule — a **stealth constitutional amendment**. This is
the bug that sank every ambitious orchestrator before cosmon. We do not
ship fleet composability without this guard.

**Fix:** at `cs nucleate`, the effective constitution of each agent is
hashed and the hash is recorded on the molecule's frontmatter. On every
subsequent operation against that molecule, the loader resolves the
constitution by hash (from a content-addressed cache), NOT by path.

- The manifest (`fleet.toml`) is the spec for *new* molecules only.
- Existing molecules carry their constitution as a committed artifact
  (hash reference).
- Editing `wiki.toml` affects future `cs nucleate` invocations; it
  cannot retroactively change running molecules.

This also provides the audit trail Shannon's explicit-channel argument
requires: every molecule's constitution is reproducible from its hash.

### 9. CLI surface: `cs fleet resolve` (feynman)

One new command:

```
cs fleet resolve [--json]
```

Prints the flattened effective fleet (merged agents, merged channels,
merged grades) with `origin_fleet_id` on each entry. `--json` is the
script-friendly form.

**Deferred to v1:** `cs fleet split`, `cs fleet merge`, `cs fleet publish`,
`cs link`. None of these are required to solve the copy-paste pain; all
can be added without schema change.

**`cs link` explicitly deferred.** The panel converged on deferring lazy
DAG attachment out of v0 (torvalds, jobs, shannon). The use cases (late
skeptic, late observer) are better served by intent-carrying formulas
(`cs nucleate skeptic --about <mol_id>`) than by a raw edge-creation
primitive that would need transitive cycle checks, merge-before-dispatch
reconciliation, and handling of drained/frozen targets (hawking's (d)
and (e)). Revisit only with a named use case no existing surface serves.

### 10. `cs ensemble` / `cs peek` surface

- `cs ensemble` shows a flat agent list with an `origin` column
  (empty for monolithic fleets). No child-fleet sub-tree.
- `cs peek` renders a single graph per composite. Fleets are not a
  visual grouping primitive in v0.
- `cs deploy` accepts the master file; children are not separately
  deployable (they are not standalone runnable fleets once included).

### 11. Backward compatibility

A monolithic `fleet.toml` without any `[[fleet.include]]` block continues
to parse unchanged. Existing galaxies require NO migration. `fleet.id`
becomes required — supply a default derived from the `.cosmon/` directory
name if absent in legacy files, and warn (migration hint).

No migration tooling is shipped in v0. Operators decomposing a large
monolithic fleet into sub-fleets do so by hand: extract `[[agents]]`
blocks into `fleets/*.toml`, add `[[fleet.include]]` entries to the
master. `cs fleet resolve --json` + diff confirms equivalence.

### 12. Interaction with existing ADRs

| ADR | Interaction |
|-----|-------------|
| ADR-011 (Content-Identity) | Fleet `id` joins molecule/universe under the same principle. |
| ADR-016 (Regimes) | Regime is per-molecule, not per-fleet. Composed fleets do NOT introduce a "composite regime" — einstein's kill of that concept. |
| ADR-030 (Selective gitignore) | `fleets/*.toml` are tracked; `.cosmon/state/` remains gitignored. |
| ADR-035 (Cross-galaxy) | Same identity machinery (content hash), distinct compose semantics (shared state store vs federated). Within-galaxy composition is a special case with shared state; cross-galaxy is federation via projection. |
| Fleet-scoped tmux socket (commit 11dfe6f) | One socket per composite, not per child (§7). |

## Acceptance criteria

Operator-defined success: "a test galaxy composing 3 fleets in 1 minute."

Encoded as an integration test (scenario 1, feynman):

```bash
time (
  mkdir -p fleets
  cat > fleets/wiki.toml  <<<'[fleet]\nid = "wiki"\n[[agents]]\nid = "editor"\n...'
  cat > fleets/dev.toml   <<<'[fleet]\nid = "dev"\n[[agents]]\nid = "coder"\n...'
  cat > fleets/blob.toml  <<<'[fleet]\nid = "blob"\n[[agents]]\nid = "patrol"\n...'
  cat > fleet.toml <<'EOF'
[fleet]
schema_version = 1
id = "test-master"

[[fleet.include]]
source = "file:./fleets/wiki.toml"

[[fleet.include]]
source = "file:./fleets/dev.toml"

[[fleet.include]]
source = "file:./fleets/blob.toml"
EOF
  cs fleet resolve --json | jq '.agents | length'
)
# expected: output 4, wall-clock < 60s
```

Land in `crates/cosmon-cli/tests/fleet_compose_v0.rs` before declaring v0
shipped.

## Explicit non-goals (v0)

- `cs link` / lazy DAG attachment.
- Non-`file:` URI schemes in the resolver.
- Transitive includes (depth > 1).
- Wildcard / glob includes.
- Generated / template-expanded includes.
- User-facing merge policy variants.
- `cs fleet split` / `cs fleet merge` migration tooling.
- Fleet registry / `cs fleet publish`.

Each is tracked as a potential successor ADR. None are required to solve
the copy-paste pain.

## Consequences

### Benefits

- Removes the ~75% boilerplate in cross-project fleet files (shannon).
- Makes fleets first-class named objects (wheeler), unblocking future
  sharing/versioning without a retrofit.
- Preserves every cosmon invariant: stateless core, DAG-only control
  plane, fleet-scoped socket, CLI-first workers, merge-before-dispatch
  (einstein's invariance table).
- One-month ship horizon (torvalds): loader change + collision check +
  hash-pin wiring. No new runtime concepts.

### Costs and risks

- `[[fleet.include]]` inline-table syntax is slightly more verbose than
  a string list. One extra line per include. The semver win justifies it
  (tolnay).
- Constitution hash-pinning adds a column to molecule frontmatter and a
  content-addressed constitution cache. Small persistent state addition;
  worth it to disarm the drift bomb (hawking).
- Deferring `cs link` may frustrate a future use case that doesn't fit
  the `cs nucleate <formula> --about <mol>` pattern. Acceptable: we
  revisit with evidence, not imagination.

### Regression guard

If a later change makes `compose` non-pure, non-total, or order-dependent,
the frame-equivalence theorem (§3) breaks silently. Add a property test
in `cosmon-core`: `compose([F1,F2,F3]) == compose([F1,F2,F3])` for any
permutation of input order, up to `origin_fleet_id` metadata, for
well-formed inputs.

## References

- Deliberation: `delib-20260414-778e` synthesis.md
- Panel personas: torvalds, tolnay, einstein, godel, feynman, wheeler,
  jobs, hawking, shannon
- Related ADRs: 011, 016, 030, 035
- Commit for fleet-scoped tmux socket: 11dfe6f
