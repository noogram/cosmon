# Cosmon contributor guide

Cosmon is a Rust workspace for persistent agent identity, typed lifecycle
management, and crash recovery. Public artifacts are maintained by Noogram
(noogram.org).

## Required reading

Before changing command behavior, read `THESIS.md`,
`docs/architectural-invariants.md`, and the applicable ADRs. The domain core is
I/O-free; filesystem, process, transport, and network behavior belongs behind
injectable ports.

## Conventions

- Use newtypes for identifiers and typestate for lifecycle transitions.
- Preserve the physics vocabulary used by the CLI: nucleate, evolve, collapse,
  freeze, thaw, entangle, ensemble, and observe.
- Return `Result` from library code; do not introduce `unwrap()` or `expect()`.
- Document every public item, explaining why it exists.
- Add readable tests that serve as executable usage examples.
- Keep workspace dependencies centralized in the root `Cargo.toml`.
- Update the CLI/UI parity audit when changing a user-facing command.
- Use Conventional Commit prefixes such as `fix:`, `feat:`, and `docs:`.

## Verification

Run the configured gates before submitting a change:

```text
cargo check --workspace
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt --all -- --check
```

Run `scripts/publish.sh --check` for release-bound changes. Runtime state,
credentials, machine paths, internal identifiers, and unreviewed binary assets
must never be tracked. A public release is produced from an isolated scrubbed
projection; never rewrite the development repository in place and never push
from an automated contributor session.
