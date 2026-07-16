# Coding Rules

<!-- Absorbed from foundry colony (2026-04-13). Replace package names with your crate names. -->

Rust conventions, testing discipline, and semver policy for <CHANGE_ME_PROJECT>.

## Unsafe Policy

- `#![forbid(unsafe_code)]` in `<CHANGE_ME_CORE_PKG>` and `<CHANGE_ME_KERNEL_PKG>`.
  These crates form the trust anchor — the kernel's sovereignty depends on
  the Rust type system being unsubverted. `forbid` (not `deny`) because it
  cannot be overridden by inner attributes.
- Other crates (`<CHANGE_ME_PROBE_PKG>`, `<CHANGE_ME_SYNTAX_PKG>`): `unsafe` is discouraged.
  If required, it must be justified by an ADR, wrapped in a dedicated module
  with `// SAFETY:` comments on every block, and covered by Miri in CI.

## Documentation

- `#![deny(missing_docs)]` in `<CHANGE_ME_CORE_PKG>` and `<CHANGE_ME_KERNEL_PKG>`.
  Every public type, trait, function, and constant must have a `///` doc comment.
- `#![warn(missing_docs)]` in `<CHANGE_ME_PROBE_PKG>` and `<CHANGE_ME_SYNTAX_PKG>`.
  Docs are expected but won't block the build during exploration.
- Doc comments explain the **WHY**, not the what. The type signature says what;
  the doc says why it exists and when you'd reach for it.
- Every module gets a `//!` header explaining its role in the architecture
  (literate programming discipline).
- `cargo doc --no-deps` must produce navigable, coherent documentation.

## Testing Tiers

Testing expectations scale with the crate's role in the trust chain.

| Tier | Crate | Requirements |
|------|-------|-------------|
| **Production** | `<CHANGE_ME_KERNEL_PKG>` | Unit + integration + property (`proptest`) + doc tests + mutation testing (`cargo mutants`). CI-gated: build fails if mutation score drops below threshold. The kernel IS the TCB — every surviving mutant is a potential unsoundness. |
| **Stable** | `<CHANGE_ME_CORE_PKG>` | Property-based tests for invariants (serialization roundtrips, term construction, level arithmetic). `cargo mutants` run locally; mutation score >60%. |
| **Exploration** | `<CHANGE_ME_PROBE_PKG>`, `<CHANGE_ME_SYNTAX_PKG>` | Type-driven assertions. At least 1 end-to-end test per public entry point. `proptest` welcome but not required. |

## Semver Discipline

### `PROOF_HASH_VERSION`

A `pub const PROOF_HASH_VERSION: u32` in `<CHANGE_ME_CORE_PKG>` tracks the serialization
format of proof terms. Bumped on any change affecting byte representation.

### `TermWire`

Serialization uses an explicit `TermWire` type — never a derived `Serialize`
on domain types. The wire format is a deliberate, versioned contract.

### Trait restrictions on `Term`

- **No derived `Hash`** on `Term`. If hashing is needed, go through `TermWire`.
- **No derived `Ord`** on `Term`. Use explicit comparators if sorting is needed.
- **`#[non_exhaustive]`** on every public enum.

### Golden hash corpus

Committed set of canonical terms with expected hashes. CI compares:
- Hashes match: pass.
- Hashes diverge + `PROOF_HASH_VERSION` bumped: pass (new baseline).
- Hashes diverge + `PROOF_HASH_VERSION` NOT bumped: **fail**.

## TCB Budget

`<CHANGE_ME_KERNEL_PKG>` is the Trusted Computing Base:
- **Target:** <CHANGE_ME_TCB_BUDGET> lines of Rust (excluding tests and comments).
- **Hard cap:** 1500 lines.
- **Measurement:** `tokei` on `crates/<CHANGE_ME_KERNEL_PKG>/src/`.

## Style

### Commits

Conventional Commits strict: `feat:`, `fix:`, `refactor:`, `test:`, `docs:`, `chore:`.

### PR size

400 lines max. Split larger changes into stacked PRs.

### Error handling

- No `unwrap()` or `expect()` in library code — return `Result`.
- `thiserror` for error types, `serde` for serialization.
- Panics are acceptable only in tests and in `main()` (via `anyhow`).

### General

- `pub(crate)` for internal helpers, not `pub`.
- Re-exports explicit in `lib.rs` (no `pub use module::*`).
- Workspace dependencies centralized in root `[workspace.dependencies]`.

## CI Gates

| Gate | Command | Scope |
|------|---------|-------|
| Format | `cargo fmt --all -- --check` | Workspace |
| Lint | `cargo clippy --workspace -- -D warnings` | Workspace |
| Build | `cargo check --workspace` | Workspace |
| Test | `cargo test --workspace` | Workspace |
| Supply chain | `cargo deny check` | Workspace |
| Mutation (production) | `cargo mutants --check` | `<CHANGE_ME_KERNEL_PKG>` |
| Golden hashes | Golden hash corpus regression test | `<CHANGE_ME_CORE_PKG>` |
| TCB budget | `tokei` line count ≤ 1500 | `<CHANGE_ME_KERNEL_PKG>/src/` |
