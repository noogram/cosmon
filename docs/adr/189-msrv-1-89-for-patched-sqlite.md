# ADR-189: Rust 1.89 for the patched bundled SQLite dependency

**Status:** Accepted (2026-11-09), as a dependency requirement of
`task-20261109-dfd7`.

**Amends:** the Rust 1.88 floor in [ADR-065](065-msrv-bump-1-88.md).

## Context

RUSTSEC-2026-0459 affects the bundled SQLite in `libsqlite3-sys` before
0.39.0. The requested upgrade to the latest `rusqlite`, 0.41.1, pulls the
patched library and retains the `bundled` feature. Its published manifest
requires Rust 1.89.0. Keeping the workspace's advertised floor at 1.88
would therefore misstate the toolchain needed to build the shipping CLI.

## Decision

Raise `workspace.package.rust-version` to 1.89 and update the current build
instructions and toolchain comment to match. Keep the toolchain on `stable`.
This records the dependency's minimum requirement; verification on current
stable does not establish that every workspace dependency supports 1.89.

## Alternatives considered

- Retain `rusqlite` 0.35: leaves the reported vulnerability in bundled SQLite.
- Keep the old declared floor while upgrading: advertises an unsupported build.
- Link system SQLite: drops the requested `bundled` contract and makes the
  patched version depend on the host installation.

## Consequences

Source builds now require at least Rust 1.89. Prebuilt binaries and database
formats are unchanged. This maintenance change introduces no broader Rust
API migration or unrelated dependency upgrade.

Sources: [RustSec advisory](https://rustsec.org/advisories/RUSTSEC-2026-0459),
[rusqlite 0.41.1 manifest](https://docs.rs/crate/rusqlite/0.41.1/source/Cargo.toml).
