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

Two speeds, both in the `justfile`:

```text
just quick    # every gate except the test suite — ~90 s, run after each edit
just gates    # the whole contract below — ~8 min, run once before merging
```

The split is not cosmetic. Measured on 2026-07-30, the test suite is about
nine tenths of the wall-clock of a full run, so lifting it out is what makes an
edit-and-verify loop usable at all. `just check` is an alias of `gates`; it
previously ran four of the seven gates while being named as if it ran them all.

The individual commands, which is what those two recipes run:

```text
cargo check --workspace
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt --all -- --check
RUSTDOCFLAGS='-D warnings' cargo doc --workspace --no-deps
```

The doc gate is not redundant with the others: `cargo check` compiles code
without resolving a doc link, and clippy is not rustdoc. A broken intra-doc
link passes every other gate and fails only in CI on the trunk.

`cargo` is not the whole contract. Two more gates run in CI and are not
subsumed by the five above:

```text
python3 scripts/spdx-headers.py --check
scripts/publish.sh --check
```

Run `scripts/publish.sh --check` for release-bound changes. Runtime state,
credentials, machine paths, internal identifiers, and unreviewed binary assets
must never be tracked. A public release is produced from an isolated scrubbed
projection; never rewrite the development repository in place and never push
from an automated contributor session — which is why `--check` is the only mode
`publish.sh` has.

It reports what it found and never what it found it to be: a credential-shaped
string is named by path and line with a truncated digest, never with its value.
Two of its rules need a waiver from time to time — cosmon's own leak detector
must contain the shapes it detects — and the waiver is the inline marker
`publish: allow — <reason>` on that single line. Per line, never per file: a
whole-file exclusion is a blind spot nobody sees again, while a marker is a
sentence someone had to write and a reviewer reads in the diff.

The other two release referees, `scripts/release-checklist.sh` and
`scripts/confidentiality-lint.sh`, are broader but cannot be hard gates on a
bare clone: their secret scan needs `gitleaks` installed and their content
denylist is operator-private by construction, so both honestly report PEND
without them. `publish.sh` covers only the structural subset — decidable from a
fresh clone with git and python3 and nothing else — which is what lets it fail
the build.
