# cosmon-dev — mission briefing (parameterized)

> This is the refinable "what to produce" recipe every germinated `cosmon-dev`
> mission carries. The spore interpolates its params into each node's `topic`
> at germination; a recipient never edits this file, only the `spore.toml`
> `[spore.params.*]` table. Register: plain, outgoing-surface (no em dashes).

## The one sentence

`cosmon-dev` turns **one issue reported by an external tester** into a
**deterministic red reproduction** (a gate, not an anecdote), a **smallest fix**,
a **double clean-room review by two different provider families iterated until
CLEAN**, and a **release** — with **no agent pushing to any remote** (the human
gate is the only door to the world).

## Parameters

| param | type | required | meaning |
|-------|------|----------|---------|
| `issue` | string | yes | The reported defect: id, title, and the tester's verbatim symptom. The trusted evidence. |
| `affected_ref` | string | yes | The git ref the red reproduction MUST fail on, for the right reason (e.g. `v0.2.2`). |
| `upstream_version` | string | yes | The released version the tester actually ran (what the world saw fail). |
| `risk` | enum `{normal, release, security}` | no (`normal`) | Drives the review jury floor and the rehearsal matrix. `release`/`security` widen both. |
| `review_scale` | enum `{mission, submission}` | no (`mission`) | `mission`: one convergence loop wraps the whole fix. `submission`: each gate/sub-mission carries its own nested loop. |
| `max_rounds` | int | no (`5`) | The hard cap on convergence rounds. Exhaustion is `blocked` + human escalation, NEVER a silent pass. |

## The invariant no worker may break

**When an external reproduction contradicts an internal proof, the reproduction
wins and the proof becomes the bug.** (codex-sol, blueprint §9.) A green test
suite is not a witness that the tester's world is fixed; the red-that-flips is.

## Where gate records go (the run-scoped output home, ADR-161)

Germination hands every node a durable place to write, so no worker has to
invent one. Two variables are interpolated into each node's brief:

| variable | value | use |
|----------|-------|-----|
| `${output_dir}` | `<state>/spore-runs/<germination-id>/<gate>/` | this node's OWN gate records (`verdict.json`, `intake.md`, …) |
| `${run_dir}` | `<state>/spore-runs/<germination-id>/` | the SHARED root, for cross-node reads (`${run_dir}/reproduce/`) |

The home lives under `.cosmon/state/` (gitignored) and is namespaced by a
per-run germination id, so it is durable across `cs done` teardown, shared so a
downstream gate can read an upstream gate's output, and collision-free across
runs. **A germinated worker MUST NOT write gate records into the spore
definition tree (`spores/cosmon-dev/…`) or the repo root** — those are the
reusable moule and the public surface; writing an instance there pollutes both
and collides on the next germination (dogfooding finding F9). Always write to
`${output_dir}`; reference a sibling gate through `${run_dir}/<gate>/`.

## The gate contract (every node obeys this)

- Every gate writes a machine-readable `verdict.json` to **`${output_dir}`**:
  `{ "verdict": "PASS"|"BLOCKED"|"CLEAN"|"FINDINGS", "count": <int>,
  "findings": [ { "loc", "quote", "fix", "severity" } ] }`.
- A gate is **fail-closed**: an absent or malformed `verdict.json` is `BLOCKED`,
  never `PASS`. A gate that cannot fail is not a gate (codex-sol #28).
- `NOT-RUN` blocks exactly like a `FAIL` blocks. No exit-0-silent.
- The edges of this spore **order** the molecules; they do not **prove** a review
  passed, that two seats had different identities, or that a branch rule held.
  Identities, credentials, and branch-protection stay external human controls.
  Each gate re-reads and re-validates its own upstream verdict; `release`
  validates the WHOLE manifest, it never infers success from completion.

See `docs/architectural-invariants.md` §8b: every gate here makes a bypass
*visible and attributable*, not impossible.
