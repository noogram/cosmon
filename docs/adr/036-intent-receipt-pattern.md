# ADR-036 — Intent + Receipt pattern for crash-safe transitions

**Status:** Accepted (draft — 2026-04-14)
**Scope:** `cs evolve`, `cs done`, and any future command that performs a
sequence of external effects with durable state implications.
**Parent:** deliberation `delib-20260414-8c82` (divergence D2, godel's
response).

## Context

Cosmon lives on the filesystem: `.cosmon/state/*.json` is the source of
truth, and external effects (git commits, tmux kills, worktree removal)
are interleaved with state writes. When a command is SIGKILL'd between
an external effect and its state write, a naive replay can double-apply
the effect — a second merge commit, a second step advance, an orphaned
branch delete.

godel's analysis of `cs evolve` and `cs done` (delib-8c82 synthesis,
§Divergence D2) identified two concrete replay hazards:

1. **`cs evolve` commit-without-state-bump.** If a future per-step git
   commit lands but the step-advance state write is interrupted, replay
   double-commits.
2. **`cs done` partial teardown.** Merge → kill-tmux → remove-worktree
   is a multi-effect sequence; partial completion leaves orphaned state.

Auto-resume (the D2 goal of `cs resume --auto`) is unsafe until both
flows are provably crash-idempotent.

## Decision

Adopt an **intent + receipt** protocol for every multi-effect
transition:

```
1. Intent   — write a durable marker to state.json (atomic rename).
2. Action   — perform the external effect(s).
3. Receipt  — write a durable record that the effect landed, and clear
              the intent marker.
```

All three writes use the existing atomic-rename discipline (see
`cosmon-filestore::lib::atomic_write`). Replay inspects the intent
marker and the receipt to determine which phase a crash interrupted and
re-applies only the missing steps.

### Applied to `cs evolve`

- **Intent.** `MoleculeData.pending_step: Option<PendingStep>` captures
  the advance's target step and start timestamp.
- **Action.** Step-artifact writes: `log.md`, `briefing.md`, and (future
  sibling `task-20260414-3a6d`) a per-step git commit whose SHA is
  stored in `PendingStep.commit_sha`.
- **Receipt.** `pending_step = None` — cleared under the fleet lock
  after artifacts land.

On replay of `cs evolve` with `pending_step.is_some()`, the caller can
reason:
- `target_step == current_step` → advance committed, artifacts may be
  missing, re-run them idempotently and clear the intent.
- `target_step != current_step` → stale intent from a prior run,
  clear unconditionally.

### Applied to `cs done`

`cs done` already implements the intent+receipt shape implicitly via
**probe-and-apply** — each teardown sub-effect is preceded by a
filesystem probe:

| Sub-effect      | Probe                                          |
|-----------------|------------------------------------------------|
| merge           | `git merge-base --is-ancestor <branch> HEAD`   |
| kill tmux       | `tmux has-session -t <session>`                |
| remove worktree | `std::path::Path::exists()`                    |
| delete branch   | `git show-ref --verify refs/heads/<branch>`    |
| purge fleet     | `fleet.workers.contains_key(&wid)`             |

A double-done is therefore a no-op on every sub-effect that was already
applied. This ADR formalises the invariant:

> **No `cs done` sub-effect may be applied without a prior probe that
> confirms its pre-condition.** Adding a new sub-effect requires
> documenting its probe here.

## Consequences

- **Positive.** Replay is safe by construction: `cs evolve` can be
  re-invoked after any crash; `cs done` can be re-invoked after
  partial teardown. Auto-resume (D2) is unlocked.
- **Positive.** Future per-step commits (`task-20260414-3a6d`) can
  store their SHA in `PendingStep.commit_sha`, so a crashed
  post-commit-pre-state-bump run can detect the dangling commit via
  `git log --grep` and reconcile rather than double-commit.
- **Cost.** One extra atomic state write per `cs evolve` (the receipt
  clear). Microsecond-scale; negligible.
- **Cost.** A new field on every `MoleculeData`. `#[serde(default,
  skip_serializing_if = "Option::is_none")]` keeps legacy state files
  and the steady-state `state.json` clean.

## Out of scope

- Idempotency keys for external APIs (GitHub, MCP). Deferred to a
  follow-up when those effects become first-class.
- Destructive-formula annotation (`destructive = true`). Added when
  a formula first needs it.

## References

- Deliberation `delib-20260414-8c82` — resilience panel synthesis.
- Sibling `task-20260414-3a6d` — per-step commits (benefits from the
  `commit_sha` receipt slot introduced here).
- Future `cs resume --auto` v2 — D2 auto-re-tackle path, safe once
  these guards land.
- [THESIS.md Part XX — Two-Axis Proof-of-Work](../../THESIS.md) and
  chronicle `2026-04-14-proof-of-work-two-axes.md`
  — this ADR realises the **process axis** of the two-axis doctrine.
  The intent+receipt markers make the process chain crash-safe; the
  per-step commit SHAs stored via `PendingStep.commit_sha` are the
  receipts that `cs verify --process` walks. The **epistemic axis** —
  `provenance.md` sidecar + `cs verify --claims` — is specified in
  ADR-041 (pending) and reuses the same intent+receipt discipline for
  its own sidecar writes.
