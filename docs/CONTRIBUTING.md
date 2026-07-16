# Contributing to Cosmon

## Getting Started

```bash
cargo check --workspace   # Build
cargo test --workspace    # Test
cargo clippy --workspace -- -D warnings  # Lint
cargo fmt --all -- --check  # Format
```

## Quality Rules

Every PR must satisfy these rules. They are enforced by CI and code review.

### 1. Literate Programming

The code reads like a document. A new contributor should understand the
architecture from `cargo doc` alone.

- **`///` doc comments** on every `pub` type, trait, and function
- Doc comments explain the **WHY**, not the WHAT
  ```rust
  /// Unique identifier for a molecule instance.
  ///
  /// Uses a timestamp-seeded format (`cs-YYYYMMDD-xxxx`) to enable
  /// chronological sorting while remaining human-readable in logs.
  pub struct MoleculeId(String);
  ```
- **`//!` module headers** explain the module's role in the architecture
  ```rust
  //! State persistence for molecules, agents, and sessions.
  //!
  //! All storage is behind the [`StateStore`] trait. The default
  //! implementation is [`FileStore`] (JSON files, zero dependencies).
  //! See ADR-001 for the rationale.
  ```
- `cargo doc --open` must produce navigable, coherent documentation

### 2. Tests = Executable Specification

Tests document the expected behavior. They are the specification.

- **Readable names**: `test_molecule_evolve_moves_to_next_step`, not `test_1`
- **Each test is a usage example** of the type or trait it tests
- **Doc examples compile**: `cargo test --doc` must pass
  ```rust
  /// Create a new molecule from a formula.
  ///
  /// # Example
  /// ```
  /// use cosmon_core::molecule::Molecule;
  /// use cosmon_core::id::FormulaId;
  ///
  /// let formula = FormulaId::new("patrol").unwrap();
  /// let mol = Molecule::nucleate(formula);
  /// assert_eq!(mol.status(), "active");
  /// ```
  pub fn nucleate(formula: FormulaId) -> Molecule<Active> { ... }
  ```

### 3. Coverage

- **Target**: 90%+ on `cosmon-core`
- **Tool**: `cargo tarpaulin` or `cargo llvm-cov`
- Measured at each PR — regressions block merge

### 4. README Driven

- Code examples in README.md must compile
- If a README example doesn't work, it's a bug
- When you add a feature, update the README example

## The cosmon-without-neurion CI gate (mandatory, blocking)

Cosmon's wedge — *git-composable, one binary, no daemon* — requires the
substrate to cold-boot on a fresh machine with **no neurion service
installed**. If neurion is ever promoted from an optional index into a
bootstrap dependency, cosmon has silently drifted into Universe D: the
operator clones the repo, runs `cs`, and nothing happens.

The `cosmon-without-neurion` CI job exists to make that drift loud. It
runs as a blocking check in `.github/workflows/ci.yml` after
`cargo test --workspace`.

### What the gate enforces

- **Bootstrap monotonicity (grep).** No `$(neurion …)` shell
  substitution inside the files that install to
  `~/Library/LaunchAgents/com.cosmon.*` or `~/.config/cosmon/`. The
  plist / TOML must point at absolute filesystem paths, never at a
  registry lookup that requires neurion to already exist. Enforced by
  `crates/cosmon-cli/tests/bootstrap_monotonicity.rs`.
- **Restart-fidelity (future, sibling `task-20260418-fb87`).** A
  scripted cosmon trajectory with `PATH` stripped of neurion, killed
  mid-flight and resumed, must reach the same final state as an
  uninterrupted run. Wired into the same recipe; activates the moment
  the sibling test file lands.

### How to run the gate locally

```bash
just test-without-neurion
```

The recipe deterministically strips every `PATH` entry that currently
resolves the `neurion` binary (it does **not** blank `PATH` — the
toolchain, git, and shell utilities remain reachable), then runs the
in-scope tests.

### Why the gate is blocking (not advisory)

Per [delib-20260418-1f29][delib] Child C (Gödel + Einstein both flagged
it non-negotiable for any GO path on `cs tick`) and
[`docs/architectural-invariants.md` §7c (Markov property)][inv], a
silent failure mode here cannot be caught by any single-PR review — it
only appears across trajectories. A blocking CI check is the cultural
enforcement mechanism. If this gate ever becomes infeasible to keep
green, raise a **successor ADR** before merging the change that broke
it.

[delib]: ../.cosmon/state/archive/2026/04/delib-20260418-1f29/synthesis.md
[inv]: architectural-invariants.md

## Architecture Decisions

Significant decisions are recorded as ADRs in `docs/adr/`. Read them before
proposing changes to the areas they cover:

- [ADR-001: State Storage — JSON First](adr/001-state-storage-json-first.md)

## Vocabulary

Cosmon uses physics terminology. Use the correct terms:

| Action | Term | Not |
|--------|------|-----|
| Create molecule | **nucleate** | pour, create, spawn |
| Advance molecule | **evolve** | advance, step, progress |
| Fail molecule | **collapse** | fail, abort, error |
| Pause molecule | **freeze** | pause, suspend |
| Resume molecule | **thaw** | unpause, resume |
| Connect molecules | **entangle** | link, connect, associate |
| Fleet status | **ensemble** | status, list |
| Inspect molecule | **observe** | inspect, show, describe |

## Agent Interface

Every CLI command supports `--json` for structured output:

```bash
cs ensemble              # human-readable
cs ensemble --json       # NDJSON for agents
```

This is the foundation for agent scripting and the future MCP server.

## Rust Rules

These are enforced by CI and non-negotiable:

| Rule | Enforcement |
|------|-------------|
| `#![forbid(unsafe_code)]` in every `lib.rs` | CI: `cargo clippy` |
| `#![deny(missing_docs)]` in `cosmon-core` | CI: `cargo doc` |
| No `unwrap()` / `expect()` in library code | Code review |
| `pub(crate)` for internal helpers | Code review |
| Explicit re-exports in `lib.rs` | Code review |
| Workspace deps centralized | CI: `cargo deny` |
| MSRV: `rust-version = "1.82"` | CI: toolchain check |
| `cargo deny` for licenses + supply chain | CI step |

## Git Rules

- **Conventional Commits** (strict): `feat:`, `fix:`, `refactor:`, `test:`, `docs:`, `chore:`
- **PR max 400 lines** — split larger changes into stacked PRs
- **CHANGELOG.md** updated with every user-visible change (Keep a Changelog format)
- **Co-Authored-By** trailer for AI-assisted commits

## How to Contribute a Handbook Question

The operator handbook (`docs/handbook.md`, exposed as `cs help guide`) is a
curated Q&A that prevents concrete operator errors. Entries are accepted only
if they meet every acceptance criterion below.

### Acceptance criteria

- **Prevents a concrete operator error.** State which error in the question
  itself. "When should I run `cs done`?" is acceptable; "What is a molecule?"
  is not.
- **Has a `Try it:` block.** Include a runnable command, an `Expect:` line
  describing the observable outcome, and a `Falsified if:` line naming the
  symptom that would invalidate the answer.
- **Cites an authoritative source.** Link to a specific `file:line` in
  `docs/`, `crates/`, or a chronicle entry. No dangling claims.
- **≤4 sentences of prose** before the `Try it:` block. The handbook
  rewards density — if you need more, write an ADR instead.
- **No "best practice" / "recommended" language.** The handbook states what
  the system does and what breaks if you deviate. It does not advise.
