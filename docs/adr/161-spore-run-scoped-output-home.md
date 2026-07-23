# ADR-161: The run-scoped output home for germinated spore nodes

**Status:** Accepted (2026-07-23). Ships implementation code (a pure
path primitive + the germination-shell wiring) plus the `cosmon-dev` spore
artifact wiring. Extends
[ADR-140](140-spore-format-expand-deterministic-cache-astra.md) (germination)
and realizes its seal property `NoResourceCollision` concretely.
**Date:** 2026-07-23.
**Decider:** Noogram (operator canonisation).
**Entry artefact.** The `cosmon-dev` dogfooding on Jesse's issues #20/#21
(knowledge finding **F9**, 2026-07-23). This is a cosmon-ward surfacing of a
**missing primitive**, not a silent application-side work-around.

---

## Context — the pathology (F9)

A spore is a reusable **moule** (template); germinating it produces
**instances**. When the `cosmon-dev` spore was germinated on #20/#21, its
pipeline workers had **no defined place to write** their per-gate deliverables
(G0 intake, G1 contract, G4 implement, G5 green, G9 rehearsal, G10 release…),
so each worker **invented** a path. The invented paths landed in three wrong
homes:

1. **Inside the spore definition dir** —
   `spores/cosmon-dev/{intakes,contracts,implementations,greens,…}/issue-*-g*/`.
   Writing an instance back into the reusable moule pollutes the public repo and
   is guaranteed to collide on the next germination.
2. **Issue-specific instances mixed with the definition** —
   `spores/cosmon-dev/repro/contract-2{0A,0B,1}*.md`,
   `clean-room/Dockerfile.repro20-facetA`.
3. **The repo root** — `reproduction.md` dumped at top level.

The clean-up was manual (`git rm --cached` + a hand-authored `.gitignore`). Real
machine state (`events.jsonl`, `fleet.json`, `frontier.json`) was already
correctly under `.cosmon/state/` (gitignored); the gap was specifically the
human-readable **gate records** produced by germinated workers. A worker could
not do the right thing because **there was no right place defined and handed to
it.**

Why the improvisation happened: `task-work`'s result contract points a worker at
`$COSMON_ARTIFACT_DIR` when set (the RPP path) and otherwise "the worktree". The
worktree is destroyed at `cs done`, so a worker seeking a *durable* home, unaware
of one, reaches for the nearest tracked directory it can see — the spore it is
running from.

## Decision

Germination computes a **run-scoped, gitignored, germination-id-namespaced**
output home under the state store and **hands it to every node**:

```text
<state_root>/spore-runs/<germination-id>/         → ${run_dir}    (shared root)
<state_root>/spore-runs/<germination-id>/<alias>/ → ${output_dir} (per node)
```

- **Where:** under `.cosmon/state/`, so the existing `.gitignore` rule keeps it
  out of the tracked tree by construction — no per-spore band-aid `.gitignore`
  is required (the one added during F9 clean-up is now redundant
  defense-in-depth).
- **Run-scoped + namespaced:** a per-run germination id (`germ-<date>-<hex>`,
  the same shape as a `MoleculeId`) means two germinations of the *same* params
  never alias. This is the seal's `NoResourceCollision` made concrete across
  runs, and per node across the `<alias>` segment.
- **Shared:** all nodes of one polymer share `${run_dir}`, so a downstream gate
  resolves an upstream gate's output through `${run_dir}/<gate>/` (the spore's
  topics genuinely need this: G3 reads `reproduce/`, G5 re-runs the frozen red
  from `reproduce/`, G10 reads every upstream `verdict.json`).
- **Handed, not guessed:** the two paths are interpolated into each node's brief
  and recorded as the `output_dir` / `run_dir` molecule variables. A worker
  writes where it is **told**.

### The invariant this enforces

> A germinated worker MUST NOT write gate records into the spore definition tree
> or the repo root.

The pure detector `forbidden_gate_output(path, spore_dir, repo_root)` decides
this; it flags a path inside the spore definition tree (`InsideSporeDefinition`)
or dumped directly at the repo root (`RepoRoot`), and passes anything under the
run home.

## How it is wired (realize, do not reinvent)

- **Pure core** (`cosmon_core::spore::output`, I/O-free per ADR-082): composes
  the paths (`run_dir`, `node_output_dir`), performs the token→path injection
  (`inject_run_outputs`), and hosts the anti-pattern detector
  (`forbidden_gate_output`). `expand` stays pure and untouched — it cannot mint
  a run id (no clock, no randomness), so the two reserved tokens `${run_dir}` /
  `${output_dir}` are left **verbatim** by `expand` (like `${nodes.x.findings}`)
  and resolved in the later shell pass.
- **Germination shell** (`cs spore run`): mints the germination id, composes and
  `mkdir -p`s the run home, calls `inject_run_outputs`, and prints
  `run home: …` to stderr.
- **Spore artifacts** (`cosmon-dev`): `mission-template.md`, every node `topic`,
  and the two new formulas reference `${output_dir}` for a node's own records and
  `${run_dir}/<gate>/` for cross-node reads.

## Alternatives considered and rejected

- **Per-molecule `molecule_dir` as the sole home.** It already exists, is
  gitignored, and is guarded by `cs evolve`'s artifact-presence check. But it is
  **not shared**: a downstream node cannot resolve `reproduce/` without the
  upstream molecule's id, and the spore's topology depends on exactly those
  cross-node reads. Rejected as the *sole* home; it remains each molecule's own
  durable dir, and the run home is layered alongside it for the shared surface.
- **A `runs/` dir under the spore, gitignored by convention.** Keeps outputs
  physically beside the definition — the very adjacency F9 showed is fragile (a
  missing ignore line re-pollutes the public repo, and `cs spore export` bundles
  the spore tree). Rejected: the state store is the right owner of run state.
- **Deriving the germination id deterministically from the bundle+params hash.**
  Would make two runs of identical params **collide** — the opposite of the
  namespacing F9 demands. Rejected: the id must carry per-run entropy, which is
  why it is minted in the shell, not the pure core.
- **Promoting selected records to `knowledge/` as documentation.** Orthogonal
  and complementary: a human may still curate a run's findings into `knowledge/`
  (F9 itself lives there). That is a downstream editorial act, not the machine's
  default write target.

## Consequences

- Germinated workers have one defined, durable, shared, collision-free home; the
  spore definition tree and repo root stay clean without hand-cleanup.
- The reserved tokens `${output_dir}` / `${run_dir}` join the runtime-reference
  family `expand` passes through verbatim; documented in the `output` module.
- **Follow-up (named, not silently dropped):** a *refusal* guard in `cs evolve`
  that calls `forbidden_gate_output` over a spore-run molecule's freshly-written
  files and blocks the advance (the same family as the existing
  artifact-presence guard). This ADR ships the **detector** and the **handed
  home** — which already removes the *cause* of the improvisation — and leaves
  the active refusal as a bounded next molecule, since threading the
  spore-definition-dir and repo-root into the `evolve` seam is a separate change.
