# Constellation — the fil-rouge molecule

A **constellation** is the cheapest way to make a hidden pattern across N
molecules survive compaction. It is a new kind of molecule (🌌) whose
single artifact, `constellation.md`, names the pattern and cites the
molecules that share it — and whose nucleation wires **typed `Refines`
edges** into the DAG so `cs deps` can see the fil-rouge.

## Why the constellation exists

Today, when an operator notices that molecules `delib-A`, `task-B`, and
`idea-C` are the same problem in disguise, the observation lives in the
chat and evaporates at the next compaction. Two weeks later, a fourth
molecule re-invents the fil-rouge — worse — because the agent has no
memory of the earlier convergence.

The loss is not a missing file. It is a missed convergence.

A constellation turns that convergence into an artifact on disk, visible
to:

- `cs deps <constellation_id>` — every citation appears under `downstream`.
- `cs deps <cited_mol>` — every constellation that cites it appears under
  `upstream` (via the symmetric `RefinedBy` back-edge).
- future verifier / overseer formulas — which can reason about the
  citation graph the same way they reason about `Blocks` / `DecayProduct`.

## Constellation vs. deliberation

The shorthand:

| | Constellation 🌌 | Deliberation 🧠 |
|---|---|---|
| When to use | You **already see** the pattern | You **want a panel to help you see** |
| Inputs | Citation list + pattern sentence | A framed question |
| Work | Operator (or light worker step) records the fil-rouge | Multi-persona panel dispatched in parallel |
| Output | `constellation.md` + `Refines` edges | `synthesis.md` + `outcomes.md` (+ decayed children) |
| Cost | Cheap — single step, no panel | Expensive — N subagents, synthesis pass |

A constellation is not a substitute for a deliberation, nor vice versa.
They operate at different levels of cognitive load.

## Nucleating a constellation

The canonical invocation:

```bash
cs nucleate constellation --kind constellation \
    --var pattern="three molecules circle the same missing primitive: \
cross-molecule cognitive connections have no first-class representation" \
    --var citations="delib-20260422-f6d6,task-20260421-cf57,idea-20260418-aaa1"
```

`cs nucleate` parses the comma-separated `citations` variable and, because
`--kind constellation` was set, auto-emits one `MoleculeLink::Refines`
edge per citation (plus a symmetric `MoleculeLink::RefinedBy` on each
cited molecule). Whitespace around the commas is tolerated.

Equivalent explicit form:

```bash
cs nucleate constellation --kind constellation \
    --var pattern="..." \
    --refines delib-20260422-f6d6 \
    --refines task-20260421-cf57 \
    --refines idea-20260418-aaa1
```

Explicit `--refines` flags and `--var citations` are merged and
deduplicated. Targets must exist — a dangling citation aborts nucleation.

Tackle as usual:

```bash
cs tackle <mol_id>
cs wait <mol_id> &
cs done <mol_id>
```

The worker's single step writes `constellation.md` with three sections:

1. **Pattern (one sentence)** — from `--var pattern`.
2. **Cited molecules** — bullet list with emoji, title, and pointer to
   each citation's primary artifact.
3. **Why this pattern** — narrative paragraph.

The `Refines` / `RefinedBy` edges are already on disk at the time the
worker runs — they were emitted during `cs nucleate`. The worker verifies
(as part of its exit criterion) that `cs deps` sees them.

## What a constellation is NOT

- **Not a decomposition.** A constellation does not decay, does not
  transform, does not spawn children. It is a leaf artifact.
- **Not a deliberation.** If you do not yet see the pattern, nucleate
  `deep-think` instead and let a panel surface it.
- **Not a progression edge.** `Refines` carries no blocking semantics:
  a cited molecule's status has no effect on the constellation, and
  vice versa.
- **Not auto-detected.** Cosmon does not cluster, embed, or infer
  fil-rouges. The operator (or a worker invoked by a spark-capture-style
  formula) names the pattern. IT FROM BIT: the system stores the
  decision; it does not guess it.
- **Not exclusive.** A single molecule may be cited by many
  constellations, and a constellation may cite any number of molecules.
  There is no cardinality limit.

## Reading constellations back

- `cs ensemble --kind constellation` — list every constellation in the
  fleet (the glyph 🌌 marks them in the default output).
- `cs deps <constellation_id>` — direct citations.
- `cs deps <constellation_id> --transitive` — walk the closure; a
  constellation that cites another constellation produces a chain.
- `cs observe <constellation_id>` — see the molecule's state and
  `typed_links` including the `Refines` edges.

## A tiny example

You notice that three molecules over the past week keep running into the
same friction: the lack of a first-class way to encode "these things
belong together". You nucleate:

```bash
cs nucleate constellation --kind constellation \
    --var pattern="three molecules re-invent the missing cognitive-connection primitive" \
    --var citations="task-20260414-aaaa,delib-20260418-bbbb,idea-20260421-cccc"
```

The system now remembers. Next time a fourth molecule appears that also
touches the same pattern, `cs deps task-20260414-aaaa` surfaces the
constellation as upstream — and a future worker briefed from that deps
view will know it is not the first to notice.

## See also

- `docs/adr/061-pilot-session-molecule-kind.md` — kindred vocabulary
  (Sparked-By, Refines, pilot-session).
- `.cosmon/formulas/constellation.formula.toml` — the formula itself.
- An internal chronicle — record a constellation sighting that
  illuminated a principle.
