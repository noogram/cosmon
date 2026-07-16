# ADR-051: Worker Worktree Harness-Ignore Primitive

**Status:** Accepted
**Date:** 2026-04-18
**Parent task:** `task-20260417-3a10`
**Related:**
[ADR-031](031-cs-init-template.md) (`cs init` template — parent `.gitignore`
seeding),
[ADR-049](049-cosmon-ward-feedback-flow.md) (cosmon-ward feedback flow — this
ADR is the second binding instance of that rule)

## Context

On 2026-04-17 evening, while the operator ran *Opération Executor* on the
`showroom` galaxy, two cosmon workers tackled from showroom committed
a Claude Code harness artifact — `.claude/scheduled_tasks.lock` — onto
their feat branches:

| Commit | Molecule | Branch | Delta |
|---|---|---|---|
| `185a97c` | `task-20260417-6b74` step 2/2 | `feat/task-20260417-6b74` | `+1` line in `.claude/scheduled_tasks.lock` |
| `f001a0a` | `idea-20260417-fb89` step 1/3 | `feat/idea-20260417-fb89` | `-1` line in `.claude/scheduled_tasks.lock` |

The file is a per-Claude-session lockfile (`{sessionId, pid, acquiredAt}`
JSON) written by the Claude Code harness itself — it has no meaning
outside the harness, is regenerated on every session, and must never
appear in git history. In the two incidents above the file self-repaired
(a later worker's commit deleted the stale entry, and the merge path
dropped it), so the damage was cosmetic. **The fact that it repaired
itself is not the lesson.** The lesson is that *the harness leak is a
primitive-absent signal*: `cs tackle` creates a worktree with no
machinery to keep harness-level artifacts out of the commit graph,
and the only defense today is a per-galaxy local `.gitignore` entry.

### What actually happens inside a worktree

`cs tackle` creates the worktree via `git worktree add <path> <branch>`
(see `crates/cosmon-cli/src/cmd/tackle.rs:create_worktree`, line 1550).
The worktree inherits whatever `.gitignore` is tracked on its start-point
branch — which for most galaxies is `main`, and which for most galaxies
does **not** list `.claude/`. The Claude Code harness writes
`.claude/scheduled_tasks.lock` (and sometimes `.claude/settings.local.json`)
inside the worktree while the worker is running. The worker's auto-commit
paths — both the `evolve` step auto-commit and the `task-work` formula's
`git add -A && git commit` — stage everything that is not ignored, so the
lockfile lands on the feat branch.

The leak is structural: it is the combination of (a) the harness writing
ephemeral state inside the worktree, (b) `git add -A` being the simplest
honest default for a worker that has no per-commit file manifest, and
(c) `cs tackle` producing a worktree with no harness-aware ignore layer.
Every galaxy that runs Claude Code inside a cosmon-tackled worktree is
exposed to the same leak, independent of the galaxy's own conventions.

### Showroom's local workaround (already applied)

Showroom appended the following entries to its `.gitignore` (committed
on `main`):

```
# Claude Code harness artefacts — should never be tracked in git
# (local defense; cosmon-ward molecule task-20260417-3a10 proposes the
# default worktree .gitignore primitive to cover this upstream)
.claude/scheduled_tasks.lock
.claude/settings.local.json
```

This fixes showroom but requires every galaxy to repeat the exercise
and to keep the list in sync with the Claude Code harness as new
ephemeral files appear. That is the silent-erosion anti-pattern ADR-049
names explicitly: every galaxy papers over cosmon's missing primitive,
the signal never reaches cosmon, and the list of harness files drifts.

## Decision

### 1. The primitive — worktree-local harness exclude

`cs tackle`, when it creates a worktree, **MUST** seed a worktree-local
exclude file that lists known harness-level artifacts. The natural seat
for this is git's **per-worktree** `info/exclude`:

```
<git_common_dir>/worktrees/<wt_name>/info/exclude
```

Per-worktree `info/exclude` has three properties that make it the right
mechanism:

1. **It does not touch the tracked `.gitignore`.** The parent branch is
   unchanged, and merging the worker's branch back to `main` carries no
   gitignore churn.
2. **It is scoped to the worktree.** When `cs done` removes the worktree,
   the exclude file is removed with it (git cleans up
   `<git_common_dir>/worktrees/<wt_name>/` on `git worktree remove`).
3. **It is honoured by `git add -A`.** The worker's default staging
   command, and every auto-commit path in `cs evolve` and the formulas,
   respects it identically to a tracked `.gitignore`.

Concretely, `create_worktree` (tackle.rs:1550) gains a post-add hook that
writes the excluded entries to `info/exclude` once the worktree exists.
The write is idempotent (append-if-absent) so re-running `cs tackle`
against the same worktree is a no-op.

### 2. The canonical harness-artifact list (v0)

The following entries ship as the default cosmon harness-ignore list.
The list lives in `crates/cosmon-cli/src/cmd/tackle.rs` as a
`HARNESS_IGNORE_ENTRIES` constant and is additive-only across cosmon
releases (entries are never removed without a successor ADR, so galaxies
that rely on the mechanism never regress).

```
# Cosmon worktree harness exclude (ADR-051)
# Per-worktree ephemeral artefacts written by the Claude Code harness
# or similar coding-agent harnesses. Never belong in git history.

# Claude Code — session lockfiles and local settings
.claude/scheduled_tasks.lock
.claude/settings.local.json
.claude/.cache/
.claude/logs/

# Claude Code — hook temp dirs
.claude/hooks/tmp/
.claude/hooks/*.tmp

# Generic agent scratch directories
.agent-cache/
.agent-tmp/
```

The list is deliberately **narrow** — it covers files that are (a)
demonstrably ephemeral, (b) harness-internal (no project-level meaning),
and (c) observed to leak in practice (the `.claude/scheduled_tasks.lock`
incident) or expected to leak by symmetry (`.claude/settings.local.json`
is the pilot's local override file). It is **not** a general-purpose
`.gitignore` for Rust or Python projects; project conventions remain the
galaxy's responsibility.

### 3. The refuse-list is rejected

An alternative was considered: `cs tackle` installs a pre-commit hook
that refuses to commit files matching a harness-artifact allow-list.
This is rejected for three reasons:

- **Double-enforcement surface.** A worker that hits a pre-commit refusal
  mid-evolve is blocked from advancing, and the recovery path is unclear
  (amend? skip? collapse?). `git add -A` silently skipping ignored files
  is a well-understood path with no new failure modes.
- **Hook-install scope creep.** `cs tackle` already avoids installing
  hooks into worktrees (hooks are a galaxy-level concern, configured
  via `.cosmon/config.toml hooks.*`). Adding a harness-refuse hook would
  blur that boundary.
- **Composition with existing hooks.** Galaxies that already run
  pre-commit, husky, lefthook, or similar would see cosmon's refuse hook
  collide with their own. The exclude-file mechanism has no such
  collision — git applies excludes before hooks run.

### 4. Scope — what this ADR does and does not change

**Does change:**

- `create_worktree` in `crates/cosmon-cli/src/cmd/tackle.rs` seeds the
  per-worktree `info/exclude` file after `git worktree add` succeeds.
- `resurrect.rs:159` (which also calls `create_worktree`) inherits the
  seeding for free.
- A new module-level constant `HARNESS_IGNORE_ENTRIES` is added, versioned
  at `v0` for future negotiation.
- A unit test in `crates/cosmon-cli/tests/` verifies the exclude file is
  written and matches the constant byte-for-byte.
- An integration test verifies `.claude/scheduled_tasks.lock` created
  inside the worktree is not staged by `git add -A`.

**Does not change:**

- The parent repo's `.gitignore` (no mass mutation of all galaxies'
  ignore files).
- `cs init` behaviour (`.cosmon/.gitignore` and `/.gitignore` seeding
  stay as they are — they are orthogonal: `cs init` handles cosmon's
  own state, this ADR handles the harness's state).
- The MCP path (workers use the CLI exclusively per the CLI-first
  invariant; the MCP spawner does not create worktrees).
- Existing worktrees (seeding is new-worktree-only; operators who want
  retroactive coverage can run `cs tackle --force` or manually append
  to `info/exclude`).

### 5. Extensibility — per-galaxy override

Galaxies that want to add harness artifacts specific to their tooling
(a custom coding-agent, a project-local harness) can configure an
additive list in `.cosmon/config.toml`:

```toml
[worktree.harness_ignore]
extra = [
  ".my-agent/cache/",
  ".my-agent/session.lock",
]
```

The galaxy's `extra` list is appended to `HARNESS_IGNORE_ENTRIES` when
the exclude file is seeded. This preserves the cosmon baseline while
letting galaxies extend without patching cosmon. The mechanism is
opt-in — the `extra` key is optional and the default is the empty list.

## Consequences

**Positive.**

- **Closes the observed leak class.** The `.claude/scheduled_tasks.lock`
  incident (and future harness-leak siblings) can no longer land on
  feat branches from a cosmon-tackled worktree, in any galaxy, without
  someone consciously editing `info/exclude` to remove the entry.
- **Single source of truth.** Every galaxy gets the same baseline
  without each maintaining its own `.gitignore` addendum. Cosmon owns
  the cross-galaxy harness-ignore vocabulary.
- **Clean merge surface.** Because the exclude lives in
  `info/exclude` (worktree-local) and not in a tracked file, the feat
  branch carries no gitignore noise back to `main`. Merge diffs stay
  semantic.
- **Cosmon-ward closure.** Showroom's local `.gitignore` workaround
  can be removed once this ADR ships (showroom cites back to this
  ADR and reverts its local entries). The reactor has learned.

**Negative.**

- **Hidden-state gotcha for power users.** An operator who inspects
  a worktree and wonders *why* `.claude/scheduled_tasks.lock` is not
  tracked must know to check `<git_common_dir>/worktrees/<wt_name>/info/exclude`
  rather than `.gitignore`. Mitigated by: the `cs tackle` summary
  prints the path to the exclude file on creation, and `cs peek` gains
  a `g`-tab field listing active excludes (follow-up task).
- **List drift risk.** The canonical list will age as the Claude Code
  harness evolves. Mitigated by: the additive-only discipline (§2),
  a `HARNESS_IGNORE_VERSION` bump rule (version in the exclude file
  header comment so `cs init --upgrade` can migrate), and an annual
  review of the list against `ls .claude/` on a fresh harness.
- **Not a full refuse mechanism.** A malicious or buggy worker that
  explicitly runs `git add -f .claude/scheduled_tasks.lock` would
  still bypass the exclude. This ADR is not a security boundary; it
  is a default-cleanliness primitive. Cosmon does not today enforce
  a commit allow-list, and adding one would be a separate ADR.

**Neutral.**

- No public CLI change. `cs tackle` gains an internal behaviour but no
  new flags. `--force` semantics (idempotent re-seed) are implicit.
- No state-store schema change.
- No breaking change to existing galaxies — worktrees created before
  this ADR lands are unchanged; only new `cs tackle` invocations seed
  the exclude.

## References

- **Inaugural incidents:** commits `185a97c`
  (`feat/task-20260417-6b74`, step 2/2) and `f001a0a`
  (`feat/idea-20260417-fb89`, step 1/3) in
  `~/dev/projects/showroom`, both adding or removing
  `.claude/scheduled_tasks.lock` in feat-branch commits.
- **Local workaround (to be removed after this ADR lands):**
  `~/dev/projects/showroom/.gitignore` §*Claude Code harness artefacts*.
- **Implementation seat:**
  `crates/cosmon-cli/src/cmd/tackle.rs:create_worktree` (line 1550)
  and `crates/cosmon-cli/src/cmd/resurrect.rs:159`.
- **Adjacent gitignore machinery:**
  `crates/cosmon-cli/src/cmd/init.rs` §`COSMON_GITIGNORE_CONTENT`,
  `GITIGNORE_ENTRIES`, `ensure_project_gitignore`. This ADR does not
  modify any of them — it adds a third, orthogonal mechanism at the
  worktree layer.
- **Cosmon-ward feedback flow:** [ADR-049](049-cosmon-ward-feedback-flow.md)
  — this ADR is the second binding instance of the rule. Showroom
  surfaced a cosmon pathology (harness leak through `cs tackle`
  worktrees) and cosmon responds with a primitive rather than every
  galaxy silent-patching locally.
- **Git docs:** `git-worktree(1)` §*DETAILS* on per-worktree
  administrative area and `gitignore(5)` §*PATTERN FORMAT* on
  `info/exclude` semantics.
