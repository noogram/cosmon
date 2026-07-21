# Where deliberation artifacts must be written

The `deep-think` family of formulas produces artifacts — `frame.md`,
`responses/`, `synthesis.md`, `outcomes.md`. **These MUST be written to the
molecule state directory, never to the git worktree.**

## The rule

Resolve the destination with `cs --json observe {mol_id}` and read the
`molecule_dir` field. Typically:

```
.cosmon/state/fleets/default/molecules/{mol_id}/
```

Write every artifact there.

## Why — the failure this prevents

Files written to the worktree (`.worktrees/{mol_id}/`) are **destroyed** when
`cs done` tears the session down. An early deep-think deliberation wrote its
entire `synthesis.md` to the worktree root; at teardown the worktree — and the
synthesis with it — was removed, and the panel's work was lost.

That near-loss is why the `synthesize` step now carries a **hard gate**:
`cs evolve` refuses to advance off the step until `synthesis.md` exists in the
canonical `molecule_dir`, and a companion lever auto-repatriates an artifact a
worker accidentally wrote to the worktree root before the gate fires. The
guidance lives here in docs rather than inside every user's formula template so
the template stays a clean contract.
