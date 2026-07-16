# Cargo Cult Sweep: Prompt-Only Invariants Audit

> *"Instructions to an autonomous agent are suggestions; programmatic guards are
> guarantees."* — delib-20260410-861f (Feynman principle)

This audit identifies every invariant in cosmon that is enforced **only** by
prompt text (CLAUDE.md, MCP INSTRUCTIONS, bootstrap prompts, formula
descriptions, ADR prose) and **not** by programmatic guards (type system,
runtime validation, tests, CI checks).

**Methodology**: Cross-referenced all invariant-bearing documents (CLAUDE.md,
THESIS.md, architectural-invariants.md, MCP INSTRUCTIONS in `tools.rs`,
bootstrap prompt in `tackle.rs`, ADRs, formula files) against the actual
enforcement mechanisms in the codebase (typestate, `evolve()` guards, `done.rs`
yield gate, CI workflow, gate.rs checks, id.rs validation).

---

## Severity Legend

| Severity | Meaning |
|----------|---------|
| **P0** | System-breaking if violated — data loss, orphaned state, security boundary bypass |
| **P1** | Architectural erosion — degrades the system's structural guarantees over time |
| **P2** | Quality/discipline gap — annoying but recoverable |

---

## P0 — Critical: Prompt-Only Invariants That Are System-Breaking

### 1. Principal Separation: Callers Cannot Evolve

**Invariant**: Only fleet-registered workers should be able to call
`cosmon_evolve`. Callers (non-workers) must be rejected.

**Where stated**: ADR-021 (`docs/adr/021-principal-separation-caller-vs-worker.md`),
MCP INSTRUCTIONS (Tier 1 anti-pattern #1).

**Current enforcement**: **NONE**. The `cosmon_evolve` MCP tool
(`crates/cosmon-mcp/src/tools.rs:638`) validates molecule status and formula,
but does **not** check `COSMON_WORKER_ID` or verify the caller is a
fleet-registered worker. Any MCP client can evolve any molecule.

**Risk**: A caller agent can execute formula steps inline after nucleating
(the "agent-attacks-molecule-itself" anti-pattern), bypassing worktree
isolation, creating conflicts with the real worker, and producing work in the
wrong process.

**Proposed enforcement**:
```rust
// In cosmon_evolve(), before any state mutation:
let worker_id = std::env::var("COSMON_WORKER_ID")
    .map_err(|_| McpError::invalid_params(
        "cosmon_evolve requires COSMON_WORKER_ID (worker-only tool)", None
    ))?;
let wid = WorkerId::new(&worker_id)
    .map_err(|e| McpError::invalid_params(format!("invalid worker: {e}"), None))?;
let fleet = store.load_fleet()
    .map_err(|e| McpError::internal_error(format!("fleet: {e}"), None))?;
if !fleet.workers.contains_key(&wid) {
    return Err(McpError::invalid_params(
        format!("worker {wid} not registered in fleet"), None
    ));
}
```

---

### 2. Agent-Attacks-Molecule-Itself Anti-Pattern

**Invariant**: After calling `cosmon_nucleate`, the caller must NOT execute
formula steps inline. They must hand off via `cs tackle <id>`.

**Where stated**: MCP INSTRUCTIONS (Tier 1 anti-pattern #1, "The One Rule"),
bootstrap prompt.

**Current enforcement**: **Partial**. The `cosmon_nucleate` response hides raw
step bodies and returns `next_action: "cs tackle <id>"`. But nothing prevents
the caller from immediately calling `cosmon_evolve` on the freshly nucleated
molecule (see #1 above).

**Risk**: Work done in the wrong process (no worktree isolation), lost on
teardown, conflicts with future real worker.

**Proposed enforcement**: Gate `cosmon_evolve` behind worker credential (#1
above). Additionally, molecules in `Pending` status should reject evolve — this
is already partially enforced by `evolve()` requiring `Running` or `Queued`,
but an unassigned molecule that gets assigned inline could bypass this.

---

### 3. Workers Cannot Self-Destroy (cs done is Human-Only)

**Invariant**: Workers must not call `cs done` — they cannot tear down their own
worktree, merge their branch, or kill their tmux session.

**Where stated**: CLAUDE.md (command perimeters), architectural-invariants.md,
bootstrap prompt ("DO NOT" violations list omits `cs done` from worker protocol).

**Current enforcement**: **NONE**. `cs done` (`crates/cosmon-cli/src/cmd/done.rs`)
checks molecule terminal status and worktree state, but does not check whether
the caller is a worker or human. A worker could `cs done` its own molecule,
causing self-destruction (removing its own worktree while running in it).

**Risk**: Worker self-destruction — undefined behavior when the running process's
filesystem is removed.

**Proposed enforcement**:
```rust
// In done::run(), early guard:
if std::env::var("COSMON_WORKER_ID").is_ok() {
    let wid_str = std::env::var("COSMON_WORKER_ID").unwrap();
    let session_name = format!("cosmon-{}", args.molecule);
    if wid_str == session_name {
        anyhow::bail!(
            "worker {wid_str} cannot tear down its own molecule — \
             use `cs complete` then let a human run `cs done`"
        );
    }
}
```

---

### 4. Surfaces Are Read-Only Projections

**Invariant**: STATUS.md, ISSUES.md, IDEAS.md, DELIBERATIONS.md,
`docs/adr/INDEX.md` are auto-generated by `cs reconcile`. Manual edits are
overwritten.

**Where stated**: MCP INSTRUCTIONS (Tier 1 anti-pattern #2), CLAUDE.md,
surface-sync-protocol.md, every tool description mentioning "SURFACE SYNC".

**Current enforcement**: **NONE at write time**. The `cs reconcile --check`
command detects staleness (dry-run) but runs only in CI and only when
explicitly invoked. Nothing prevents a file write to STATUS.md — the next
reconcile silently overwrites it.

**Risk**: Agent edits STATUS.md thinking it's the source of truth; edits are
silently lost on next reconcile; agent's mental model diverges from actual state.

**Proposed enforcement**:
- **Git pre-commit hook**: reject staged changes to generated surface files
  unless a `COSMON_RECONCILE=1` env var is set (which `cs reconcile` would set).
- **CI gate** (already exists via `cs reconcile --check`): ensure this is
  actually wired into `.github/workflows/ci.yml`.

---

### 5. Artifact Paths: Molecule State Dir, Not Worktree

**Invariant**: Deep-think deliberation artifacts (frame.md, responses/,
synthesis.md, outcomes.md) MUST be written to `COSMON_MOL_DIR` (molecule state
directory), NOT the worktree. Files in `.worktrees/{mol_id}/` are destroyed by
`cs done`.

**Where stated**: deep-think.formula.toml (bold, all-caps), bootstrap prompt
(COSMON_MOL_DIR injected as env var).

**Current enforcement**: **NONE**. The `COSMON_MOL_DIR` env var is injected by
`cs tackle` into the tmux session, but nothing validates at `cs done` teardown
or `cs evolve` time that artifacts are actually in the right directory.

**Risk**: Deliberation artifacts written to worktree are silently destroyed on
`cs done`. This is a data-loss scenario.

**Proposed enforcement**:
- In `cs done`, scan worktree for artifact patterns (`frame.md`, `synthesis.md`,
  `outcomes.md`, `responses/`) and **warn or block** if found (similar to
  existing dirty-worktree check). Already partially addressed by the yield gate
  concept in ADR-024 — the "misplaced artifacts" check. Verify it's implemented.

---

## P1 — Architectural Erosion: Prompt-Only Invariants

### 6. No Daemon in Transactional Core (Layer A)

**Invariant**: The transactional core (all current `cs` commands) must be
stateless one-shot invocations. Never introduce a daemon, background loop, or
persistent process.

**Where stated**: CLAUDE.md, architectural-invariants.md, ADR-016, THESIS.md.

**Current enforcement**: **Convention only**. No compile-time or CI check
prevents adding a `loop {}` or `tokio::spawn()` to a CLI command.

**Proposed enforcement**:
- **Architectural test**: Assert that no CLI command module imports
  `tokio::time::sleep`, `std::thread::sleep` with loop patterns, or runs
  event loops. Can be a grep-based CI step.
- **Lint rule**: Custom clippy lint or shell-based CI check scanning for
  daemon-like patterns in `crates/cosmon-cli/`.

---

### 7. Regime Boundaries (Inert / Propelled / Autonomous)

**Invariant**: Each command operates in exactly one regime. `cs tackle` is
Inert→Propelled only. `cs evolve` is Propelled/Autonomous only (worker
context). `cs done` is human-only.

**Where stated**: CLAUDE.md, architectural-invariants.md, ADR-016.

**Current enforcement**: **Partial**. The `evolve()` function validates molecule
status (Running/Queued), which implicitly gates on the molecule being in
Propelled state (it must have been tackled). But the regime itself is not a
first-class type — there's no `Regime` enum that commands check against.

**Proposed enforcement**:
- Add `Regime` enum (`Inert`, `Propelled`, `Autonomous`) to `cosmon-core`.
- Derive current regime from molecule state (Pending/no worker = Inert,
  Running/has worker = Propelled).
- Guard commands: `cs evolve` rejects Inert molecules, `cs tackle` rejects
  already-Propelled molecules.

---

### 8. Single Perimeter Per Command

**Invariant**: Each command has exactly one role and must not duplicate another
command's responsibility.

**Where stated**: CLAUDE.md (coherence checklist Q4), architectural-invariants.md.

**Current enforcement**: **Review-time only**. No automated check prevents a PR
from adding molecule-state mutation to `cs observe` or teardown logic to
`cs complete`.

**Proposed enforcement**:
- **Architectural decision records**: Already exist. Enforce via PR review
  checklist (partially automated).
- **Module-level `#[doc]` annotations**: Each command module's `//!` header
  declares its perimeter. A CI check could verify that no command module calls
  functions from another command's "private" scope.

---

### 9. Heterogeneous vs. Homogeneous Decay

**Invariant**: `cosmon_decay` is for homogeneous 1→N splitting (children share
parent's variables). Heterogeneous decomposition (distinct children with
individual topics) must use N calls to `cosmon_nucleate` with `--blocked-by`.

**Where stated**: deep-think.formula.toml (explicit, with regression guard from
delib-20260409-b22c and delib-20260409-f4e1).

**Current enforcement**: **NONE**. `cs decay` does not validate whether the
child formulas are semantically homogeneous. The `--blocked-by` link is
optional in `cosmon_nucleate`.

**Risk**: Orphaned children (no DAG edge to parent deliberation), invisible to
`cs deps --transitive`.

**Proposed enforcement**:
- In `cosmon_nucleate`, when the caller is nucleating from a deliberation
  context (detectable via source molecule's kind = `deliberation`), require
  `blocked_by` to be non-empty. This is complex — simpler alternative:
- **Post-nucleation validation in deep-think formula**: After Step 5 (outcomes),
  the formula's exit criteria should include `cs deps {delib_id} --transitive`
  output showing expected children. Make this a programmatic check in the
  evolve evidence validation.

---

### 10. Vocabulary Enforcement (Physics Naming)

**Invariant**: Use physics vocabulary exclusively: nucleate (not create/spawn),
evolve (not advance), collapse (not fail), freeze/thaw (not pause/resume),
ensemble (not status/inspect).

**Where stated**: CLAUDE.md, CONTRIBUTING.md, THESIS.md (Part V).

**Current enforcement**: **Partial**. The CLI commands and types use correct
vocabulary. But nothing prevents a contributor from adding a `create_molecule()`
function or a `--status` flag.

**Proposed enforcement**:
- **Banned-word lint**: CI grep check for banned terms in new Rust code:
  `spawn`, `create` (in molecule context), `advance`, `pause`, `resume` (for
  freeze/thaw), `status` (for observe/ensemble), `inspect`.
- **`cargo doc` review**: Automated check that public API docs use correct
  vocabulary.

---

### 11. `cs reconcile` After State Mutations

**Invariant**: After any operation that changes molecule state (nucleate, evolve,
collapse, freeze, thaw, decay, merge, transform), surfaces may drift. Run
`cs reconcile` to refresh.

**Where stated**: MCP INSTRUCTIONS (Tier 2 anti-pattern #4), every MCP tool
description, surface-sync-protocol.md.

**Current enforcement**: **NONE** (post-mutation). The `cs reconcile --check`
exists for CI, but nothing auto-runs reconcile after mutations. The MCP tool
descriptions remind callers, but this is prompt-only.

**Proposed enforcement**:
- **Auto-reconcile in MCP**: After any mutation tool (`cosmon_nucleate`,
  `cosmon_evolve`, `cosmon_freeze`, `cosmon_thaw`, `cosmon_collapse`,
  `cosmon_complete`), automatically run surface reconciliation. Already natural
  since the MCP server has access to the state store.
- **Alternative**: `cs done` already runs reconcile. Extend this pattern to
  all CLI mutation commands via a `--reconcile` flag (default on).

---

### 12. Conventional Commits Strict

**Invariant**: All commits must use conventional commit format: `feat:`, `fix:`,
`refactor:`, `test:`, `docs:`, `chore:`.

**Where stated**: CLAUDE.md (Git Rules), CONTRIBUTING.md.

**Current enforcement**: **NONE programmatic**. No commit-msg hook validates
the format.

**Proposed enforcement**:
- **Git commit-msg hook**: Regex check for conventional commit prefix.
- **CI check**: Validate all commits on PR branch match the pattern.

---

## P2 — Quality/Discipline Gaps

### 13. PR Max 400 Lines

**Invariant**: Pull requests must not exceed 400 lines changed.

**Where stated**: CLAUDE.md (Git Rules).

**Current enforcement**: **NONE**. No CI check or GitHub bot enforces line count.

**Proposed enforcement**: GitHub Action that comments/blocks PRs exceeding the
threshold.

---

### 14. CHANGELOG.md Updated With Every User-Visible Change

**Invariant**: Every user-visible change must update CHANGELOG.md.

**Where stated**: CLAUDE.md (Git Rules).

**Current enforcement**: **NONE**.

**Proposed enforcement**: CI check that any PR touching `crates/` also touches
`CHANGELOG.md` (with override label for internal-only changes).

---

### 15. 90%+ Code Coverage on cosmon-core

**Invariant**: Target 90%+ coverage on cosmon-core crate.

**Where stated**: CLAUDE.md (Quality Rules), CONTRIBUTING.md.

**Current enforcement**: **NONE in CI**. Coverage tools mentioned (tarpaulin,
llvm-cov) but no CI gate.

**Proposed enforcement**: Add `cargo llvm-cov --workspace --fail-under 90` to CI
for cosmon-core.

---

### 16. Doc Comments on Every `pub` Item

**Invariant**: `///` doc comments on every `pub` type, trait, and function.
Doc comments explain WHY, not WHAT.

**Where stated**: CLAUDE.md (Quality Rules), CONTRIBUTING.md.

**Current enforcement**: **Partial**. `#![deny(missing_docs)]` is declared for
cosmon-core, which enforces presence but not quality (WHY vs WHAT). Other crates
may not have this attribute.

**Proposed enforcement**: Extend `#![deny(missing_docs)]` to all library crates
in the workspace.

---

### 17. Bootstrap Prompt "DO NOT" Violations

**Invariant**: Workers must not pause between steps, ask for confirmation,
summarize what they did, or wait for user input.

**Where stated**: Bootstrap prompt in `tackle.rs` (7 explicit "DO NOT" items).

**Current enforcement**: **NONE**. These are behavioral instructions to the LLM.
By definition, they are suggestions.

**Risk**: Low — these affect UX/efficiency, not system integrity.

**Proposed enforcement**: **Accept as prompt-only** — these are inherently
LLM behavioral instructions that cannot be programmatically enforced. The
correct mitigation is monitoring: detect stalled workers via `cs patrol` and
nudge them.

---

### 18. `--json` on Every CLI Command

**Invariant**: Every CLI command must support `--json` for NDJSON output
(agent-first interface).

**Where stated**: CLAUDE.md (Conventions).

**Current enforcement**: **NONE**. No test verifies all subcommands accept
`--json`.

**Proposed enforcement**: Integration test that runs every subcommand with
`--json --help` and verifies the flag is recognized.

---

### 19. No `unwrap()` / `expect()` in Library Code

**Invariant**: Library crates must return `Result` everywhere, no panics.

**Where stated**: CLAUDE.md (Rust Rules), CONTRIBUTING.md.

**Current enforcement**: **Partial**. Convention enforced by review. Note:
`tools.rs:976` uses `unwrap_or_else` with a fallback `unwrap()` inside —
technically a violation.

**Proposed enforcement**:
- **Custom clippy config** or CI grep: `rg 'unwrap\(\)' crates/cosmon-core/`
  should return zero results.
- Exclude test modules from this check.

---

### 20. `#![forbid(unsafe_code)]` in Every `lib.rs`

**Invariant**: Every library crate's `lib.rs` must have `#![forbid(unsafe_code)]`.

**Where stated**: CLAUDE.md (Rust Rules).

**Current enforcement**: **Per-crate** — only enforced if each crate's `lib.rs`
actually contains the attribute. No workspace-wide check.

**Proposed enforcement**: CI check: `grep -rL 'forbid(unsafe_code)' crates/*/src/lib.rs`
should return empty.

---

## Summary Matrix

| # | Invariant | Severity | Current Enforcement | Proposed Mechanism |
|---|-----------|----------|--------------------|--------------------|
| 1 | Principal separation (evolve = worker-only) | P0 | None | COSMON_WORKER_ID check in MCP |
| 2 | Agent-attacks-molecule-itself | P0 | Partial (hidden steps) | Gate evolve behind worker cred |
| 3 | Workers cannot self-destroy (cs done) | P0 | None | COSMON_WORKER_ID guard in done.rs |
| 4 | Surfaces are read-only projections | P0 | Partial (reconcile --check) | Pre-commit hook |
| 5 | Artifacts in mol_dir, not worktree | P0 | None | Yield gate scan in cs done |
| 6 | No daemon in Layer A | P1 | Convention | Architectural grep test |
| 7 | Regime boundaries | P1 | Partial (status check) | Regime enum + command guards |
| 8 | Single perimeter per command | P1 | Review only | Module boundary enforcement |
| 9 | Heterogeneous vs homogeneous decay | P1 | None | blocked_by requirement for delib children |
| 10 | Physics vocabulary | P1 | Partial (CLI/types) | Banned-word lint |
| 11 | Reconcile after mutations | P1 | None (post-mutation) | Auto-reconcile in MCP/CLI |
| 12 | Conventional commits | P1 | None | commit-msg hook |
| 13 | PR max 400 lines | P2 | None | GitHub Action |
| 14 | CHANGELOG.md updates | P2 | None | CI check |
| 15 | 90%+ coverage | P2 | None | CI coverage gate |
| 16 | Doc comments on all pub items | P2 | Partial (cosmon-core only) | Extend deny(missing_docs) |
| 17 | Bootstrap DO NOT violations | P2 | None | Accept as prompt-only + patrol |
| 18 | --json on every command | P2 | None | Integration test |
| 19 | No unwrap() in library code | P2 | Convention | CI grep |
| 20 | forbid(unsafe_code) in every lib.rs | P2 | Per-crate | CI grep |

---

## Recommended Priority

**Phase 1 (immediate, high leverage)**:
- #1 + #2: Add worker credential check to `cosmon_evolve` in MCP
- #3: Add self-destruction guard to `cs done`
- #5: Implement misplaced-artifact scan in `cs done` yield gate

**Phase 2 (next sprint)**:
- #4: Pre-commit hook for surface files
- #11: Auto-reconcile after MCP mutations
- #12: Conventional commit hook
- #19 + #20: CI grep checks for unwrap/unsafe

**Phase 3 (architecture hardening)**:
- #7: Regime enum as first-class type
- #9: Blocked-by enforcement for deliberation children
- #6: Daemon-pattern detection
- #10: Vocabulary lint

**Accept as prompt-only** (no programmatic fix possible):
- #17: Bootstrap behavioral instructions — mitigate via patrol monitoring
