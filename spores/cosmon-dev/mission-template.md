# cosmon-dev — mission briefing (parameterized)

> This is the refinable "what to produce" recipe every germinated `cosmon-dev`
> mission carries. The spore interpolates its params into each node's `topic`
> at germination; a recipient never edits this file, only the `spore.toml`
> `[spore.params.*]` table. Register: plain, outgoing-surface (no em dashes).

## The one sentence

`cosmon-dev` turns **one issue reported by an external tester** into a
**deterministic red reproduction** (a gate, not an anecdote), a **smallest fix**,
a **double clean-room review by two different provider families rendered as two
immutable verdicts**, an **unconditional root/non-root x amd64/arm64 rehearsal of
the packaged bytes**, an **author-independent walk of the published install route
by a brief that was never told what is broken**, and a **release that must declare,
non-empty, the arms it did not run** — with **no agent pushing to any remote** (the
human gate is the only door to the world).

## Parameters

| param | type | required | meaning |
|-------|------|----------|---------|
| `issue` | string | yes | The reported defect: id, title, and the tester's verbatim symptom. The trusted evidence. |
| `affected_ref` | string | yes | The git ref the red reproduction MUST fail on, for the right reason (e.g. `v0.2.2`). |
| `upstream_version` | string | yes | The released version the tester actually ran (what the world saw fail). |
| `risk` | enum `{normal, security}` | no (`normal`) | The review jury floor, and nothing else. `security` adds a third distinct provider family (the `openai` adapter pinned to `api.mistral.ai`). The rehearsal matrix no longer reads this param: it is unconditional. Entry is mechanical, derived from the paths the diff touches, never from a value a pilot types. |
| `max_rounds` | int | no (`2`) | Runaway backstop only — the control flow is the two immutable verdicts (INITIAL, CONFIRMATION) and this number governs no decision in it. It bounds COST, never INFORMATION. Exceeding it is a wiring failure (`verdict-plumbing`), never a quality signal. |

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
- `NOT-RUN` blocks exactly like a `FAIL` blocks. No exit-0-silent. A rehearsal cell
  that did not run is `BLOCKED`, never an absent row: an absent row is how a whole
  arm goes missing without anybody deciding to drop it.
- Every **review seat** writes `surface_enumerated.json` `{arms[], paths[],
  commands[]}` **before** it reviews, and a seat that renders a verdict without one
  is void on the plumbing rule. This makes "I reviewed X" a claim with a domain
  instead of testimony, which is the only form in which a coverage gap is visible.
  `release` compares the declared surfaces against the enumerated semantic surface
  and blocks on a gap it cannot explain.
- The `release` manifest carries a non-empty `arms_not_run[]`. It is not a verdict,
  it is a declared blind spot, and a manifest claiming there is no unrun arm is
  refused.
- The edges of this spore **order** the molecules; they do not **prove** a review
  passed, that two seats had different identities, or that a branch rule held.
  Identities, credentials, and branch-protection stay external human controls.
  Each gate re-reads and re-validates its own upstream verdict; `release`
  validates the WHOLE manifest, it never infers success from completion.

See `docs/architectural-invariants.md` §8b: every gate here makes a bypass
*visible and attributable*, not impossible.
