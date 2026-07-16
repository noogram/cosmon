# ADR-024: Worker Output Yield Protocol

## Status
Accepted (2026-04-10)

## Context

Three stability incidents in quick succession (April 2026) revealed a structural
gap in the molecule lifecycle: **the system tracks molecule *status* but not
molecule *outputs***. The teardown path (`cs done`) performed irreversible
operations — worktree removal, branch deletion — without verifying that the
worker had actually persisted its work.

A five-persona deliberation (`delib-20260410-861f`) converged on the diagnosis
from independent angles:

- **Feynman**: One bug — "instructions without enforcement." The bootstrap
  prompt tells the worker to commit, but no programmatic guard verifies
  compliance. The prompt is a cargo cult: the form of safety without the
  substance.
- **Torvalds**: One line — `done.rs:193` converted a fatal worktree-removal
  failure into a non-fatal warning, defeating git's own safety net.
- **Architect**: One missing concept — **yield**. The lifecycle state machine
  knows a molecule is `Running` or `Completed` but has no notion of "did
  the worker produce durable output?"
- **Tolnay**: Five contract violations in `cs done`, including missing
  dirty-worktree precondition, branch deletion after worktree-removal
  failure, and silent data destruction under `--no-merge`.

### The three incidents

1. **Untracked artifact loss**: Worker wrote `synthesis.md` to the worktree
   root instead of `$COSMON_MOL_DIR`. `cs done` removed the worktree; the
   file vanished. `git worktree remove` (without `--force`) only refuses on
   modified *tracked* files — untracked files are destroyed silently.

2. **Uncommitted work loss**: Worker failed to commit before `cs done` was
   called. The worktree was removed with uncommitted changes.

3. **Merge conflict at integration**: Two parallel workers modified
   overlapping files. The conflict surfaced only at `cs done` merge time —
   the latest possible moment and the worst time to discover it.

### The missing invariant

The architectural invariants (checklist item 5) require symmetric undo:
"If it creates state, is there a reverse command?" `cs tackle` creates two
output zones — a git worktree (for code changes) and a molecule_dir (for
artifacts). Neither `cs complete` nor `cs done` verified that outputs
migrated from the ephemeral zone (worktree) to the durable zone (committed
git + molecule_dir). The symmetry was broken.

## Decision

### 1. The Yield concept

**Yield** is the tangible output a worker produces that must survive teardown.
In the physics vocabulary: a molecule's yield is the energy it converts into
lasting structure.

| Category | Location | Persistence mechanism | Failure mode |
|----------|----------|----------------------|--------------|
| Code changes | worktree (git branch) | `git commit` + merge | Incidents 1, 2 |
| Molecule artifacts | `molecule_dir` | Direct file write | Incident 1 |

**Yield is NOT a core type.** It is a Propelled-regime concern enforced at
the command boundary. The yield gate belongs in the CLI layer (`cosmon-cli`),
not in `cosmon-core`. The domain model does not grow a new type — the
enforcement is operational, not ontological.

### 2. `COSMON_MOL_DIR` environment injection

`cs tackle` injects a `COSMON_MOL_DIR` environment variable into the worker's
tmux session, pointing to the absolute path of the molecule's state directory.

This eliminates the root cause of Incident 1: the worker no longer has to
guess where artifacts belong. The canonical output directory is unambiguous.

**Implementation** (already shipped in `tackle.rs`):

```rust
let mol_state_dir = store.molecule_dir(&mol_id);
// ... later in spawn:
let claude_cmd = format!("COSMON_MOL_DIR={mol_dir_str} {claude_bin} ...");
```

The variable is available to any process in the tmux session. The bootstrap
prompt references it, but the env var is the programmatic contract — prompts
are advisory, environment variables are mechanical.

### 3. Yield gate in `cs done`

`cs done` enforces a **yield gate** between the terminal-state verification
and the merge step. The gate runs four checks:

| Check | Method | Blocks teardown? |
|-------|--------|-----------------|
| Dirty worktree | `git -C {worktree} status --porcelain` | **Yes** (unless `--force`) |
| Misplaced artifacts | Scan worktree root for artifact patterns | **Yes** (unless `--force`) |
| Empty branch | `git rev-list main..feat/{id} --count` | Warning only |
| Formula-declared outputs | Check `molecule_dir` for expected files | Warning only |

#### Dirty worktree check

The primary guard. Uses `git status --porcelain` rather than relying on
`git worktree remove`'s own check, because the latter only catches modified
*tracked* files — untracked files are destroyed silently. Both modified
tracked and untracked files are caught by the porcelain check.

**Implementation** (already shipped in `done.rs`):

```rust
fn worktree_is_dirty(worktree_path: &Path) -> Result<Vec<String>, anyhow::Error> {
    let out = Command::new("git")
        .args(["-C", &worktree_path.to_string_lossy(), "status", "--porcelain"])
        .output()?;
    // ... parse non-empty lines as dirty files
}
```

If the worktree is dirty and `--force` is not set, `cs done` refuses with
an explicit error listing the dirty files and recovery instructions.

#### Merge-gated branch deletion

Branch deletion is gated on merge success. If the merge was skipped
(`--no-merge`) or failed, and the branch is not an ancestor of HEAD, the
branch is preserved. This prevents the data-loss vector identified by Tolnay:
`--no-merge` + branch deletion = silent destruction of the only copy.

**Implementation** (already shipped in `done.rs`):

```
--no-merge implies --no-branch-delete (unless --force)
```

### 4. Conflict preview at `cs complete` (Phase 1)

When `cs complete` is run from inside a worktree, it performs an advisory
file-overlap check:

1. Compute the file-change set: `git diff --name-only main..HEAD`
2. For each other running molecule with a worktree, compute its change set
3. Emit a warning if any files overlap

This is **prediction, not prevention**. It does not block the transition.
It gives the human (who will later run `cs done`) information to sequence
merge operations or resolve conflicts proactively.

This aligns with the panel consensus: do NOT build a file-lock system.
File-level locks are wrong for long-running workers that touch files
unpredictably (Torvalds, Architect, Feynman all reject this). Instead,
improve conflict *awareness* and *recovery*.

### 5. Regime compatibility

The yield protocol respects the three autonomy regimes defined in
[ADR-016](016-autonomy-regimes-and-resident-runtime.md):

| Guardrail | Inert | Propelled | Autonomous |
|-----------|-------|-----------|------------|
| Dirty worktree check | N/A (no worktree) | `cs done` gate | Runtime checks before merge |
| Misplaced artifact scan | N/A | `cs done` gate | Runtime scans execution context |
| `COSMON_MOL_DIR` env | N/A | Set by `cs tackle` | Set by runtime dispatch |
| Conflict preview | N/A | `cs complete` warning | DAG policy dynamic edge |

- **Inert** molecules have no worktree and no worker — the yield protocol
  does not apply. They transition manually via `cs evolve` / `cs complete`.

- **Propelled** molecules are the primary target. `cs tackle` creates the
  worktree and injects `COSMON_MOL_DIR`. `cs done` runs the yield gate.
  `cs complete` (from worktree) runs the conflict preview.

- **Autonomous** molecules (future, ADR-016 Phase 3+) will be managed by
  the resident runtime. The `DagPolicy` inherits the same yield-check
  logic before merging. The `DynamicDagPolicy` can additionally insert
  `Blocks` edges based on file-change manifests, turning advisory conflict
  warnings into scheduling constraints.

### 6. Coherence checklist

1. **Stateless?** Yes. Each check reads git state, produces a report, exits.
2. **Idempotent?** Yes. Running `cs done` twice with the same state produces
   the same result.
3. **Regime-aware?** Yes. Checks only apply when a worktree exists (Propelled).
4. **Single perimeter?** Yes. The yield gate is internal to `cs done`. The
   conflict preview is internal to `cs complete`. No new commands.
5. **Symmetric undo?** N/A — these are checks, not state creation.
6. **Runtime-compatible?** Yes. The resident runtime implements the same
   checks programmatically.
7. **Worker/human boundary?** Respected. `cs done` is human-callable.
   Workers cannot bypass the yield gate.

### 7. The `--force` escape hatch

All blocking checks respect `--force`. The flag already existed and its
semantics are extended naturally: "I know what I'm doing, skip the safety
gates." No new flag semantics are introduced.

## Implementation Sequence

### Phase 0 — Yield Gate (shipped)

Prevents incidents 1 and 2. Already implemented:

- `COSMON_MOL_DIR` env injection in `cs tackle` (`tackle.rs`)
- `worktree_is_dirty()` check before worktree removal (`done.rs`)
- Merge-gated branch deletion (`done.rs`)
- `--no-merge` implies `--no-branch-delete` unless `--force` (`done.rs`)

### Phase 1 — Conflict Preview (planned)

Addresses incident 3:

- File-overlap detection in `cs complete` from worktree context
- `cs observe --overlap <mol>` for human inspection of contention
- Improved conflict error messages with exact recovery commands

### Phase 2 — Autonomous Regime Integration (future)

Extends the yield protocol to the resident runtime (ADR-016):

- `DagPolicy` inherits yield-check logic before merge
- `DynamicDagPolicy` uses file-change manifests for contention edges
- Post-merge verification (`merge-base --is-ancestor`)

## Consequences

### Positive

- **No more silent data loss**: The yield gate catches uncommitted changes
  and untracked files before the irreversible worktree removal.
- **No more branch orphaning**: `--no-merge` preserves the branch, preventing
  the Tolnay vector.
- **Worker clarity**: `COSMON_MOL_DIR` eliminates the worktree/molecule_dir
  confusion that caused Incident 1.
- **Audit trail**: Dirty-worktree refusals are logged with exact file lists,
  giving the human precise recovery information.

### Negative

- **`--force` fatigue**: If workers frequently leave benign untracked files
  (editor swap files, build artifacts), the dirty check may trigger often.
  Mitigation: formula-level `.gitignore` patterns.
- **Phase 1 cost**: Conflict preview requires scanning all running worktrees
  on every `cs complete`. For large fleets this could be slow. Mitigation:
  cache change manifests in molecule state.

### Neutral

- No new core types. The domain model is unchanged.
- No new commands. The yield protocol is embedded in existing commands.
- No API surface changes. `--force` semantics are extended, not changed.

## References

- [ADR-016: Autonomy Regimes and the Resident Runtime](016-autonomy-regimes-and-resident-runtime.md)
- [ADR-021: Principal Separation — Caller vs Worker](021-principal-separation-caller-vs-worker.md)
- Deliberation: `delib-20260410-861f` (stability retrospective, 4-persona panel)
- Feynman: "The cargo cult principle — every invariant enforced only by prompt
  text is a latent data-loss bug"
