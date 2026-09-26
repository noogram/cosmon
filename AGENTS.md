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
- An unmerged branch left by a collapsed molecule requires an audit verdict
  before deletion, never a guess.

No agent-harness session identifier, thread identifier, or conversation
deep-link may appear in any artefact this repository publishes — commit
messages (trailers included), PR titles and bodies, issue titles and bodies,
review comments, release notes, or any tracked file. Known shapes are examples
of the class, not its definition: `Claude-Session:`, `claude.ai/code/session_…`,
`chatgpt.com/codex/…`, `Codex-Thread:`, `Session-Id:`, and any
`https://…/session[_/]<opaque-id>` deep link into a vendor console. Such an
identifier names a private conversation the repository's readers cannot and
must not open, and a public repository is permanent — so it is a durable
pointer at something private, which is a leak even while the endpoint refuses
anonymous readers. A harness may *instruct its agent* to append such a trailer
by default (Claude Code does); this rule overrides that default here. Put
provenance where it is public, stable and meaningful instead: the molecule id
and the merge shape `Merge branch 'feat/<mol_id>'`, plus the durable molecule
directory and the ledger (ADR-052 §I9). A vendor URL is not provenance; it is
a bookmark in somebody's browser. Enforced by
`scripts/check-no-session-ids.sh` (commit messages and PR body, over what a
change adds) and `publish.sh --check` rule G (tracked files).

## Verification

Two speeds, both in the `justfile`:

```text
just quick    # every gate except the test suite — ~90 s warm, run after each edit
just gates    # the whole contract below — ~7 min warm, ~15 min cold, run once before merging
```

The split is not cosmetic. Measured on 2026-07-30, the test suite is about
nine tenths of the wall-clock of a full run, so lifting it out is what makes an
edit-and-verify loop usable at all. `just check` is an alias of `gates`; it
previously ran four of the seven gates while being named as if it ran them all.

Warm vs. cold matters here, and the two figures above are not interchangeable.
A **warm** run reuses an already-populated `target/` — a developer's own
checkout after the first build. A **cold** run starts from an empty
`target/` — every `cs tackle` worker's worktree, every time. Measured on this
machine (`task-20260925-fd07`, cold, ambient fleet load 12–250 on 16 cores):
`just gates` cold is **≈15.4 min**; the same steps warm are **≈7 min**, almost
all of it the test run itself (compile steps drop to seconds warm). Quote
whichever figure matches the situation you're describing.

The individual commands, which is what those two recipes run:

```text
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt --all -- --check
RUSTDOCFLAGS='-D warnings' cargo doc --workspace --no-deps
```

There is no standalone `cargo check --workspace` step. On the target set both
would compile (default: lib + bins, no `--all-targets`), clippy's `-D
warnings` is the strictly stronger gate — every `rustc` error `check` would
raise, plus every clippy lint. Measured twice: `task-20260925-fd07` §2 (B→C,
−27 s cold, no lost coverage) and re-verified in `task-20260926-70fa` with two
deliberate compile errors — one in a test-only target, one behind a
non-default feature — that `cargo check -p <crate>` and this recipe's `cargo
clippy -p <crate> -- -D warnings` missed identically, before and after.
Neither gap is new: a test-only compile error is still caught downstream by
`cargo test --workspace` in `gates`; a non-default-feature compile error is
not built by any gate today (nothing passes `--features <name>`) and this
change does not touch that. Widening clippy to `--all-targets` would close
the first gap, but it also turns on clippy's lint pass over every existing
test file, and at least one already fails it on an unrelated, pre-existing
lint — so that widening is deliberately not folded into this change.

The doc gate is not redundant with the others: clippy compiles code without
resolving a doc link, and clippy is not rustdoc. A broken intra-doc link
passes every other gate and fails only in CI on the trunk.

Both recipes run their `cargo` steps with `CARGO_INCREMENTAL=0` — a one-shot
gate run never revisits its own artifacts, so the incremental cache is pure
disk cost with no time benefit (measured: −9.6 GB per worktree, wall time
unchanged). It is scoped to the `env` invocation inside `quick`/`gates`
themselves, not exported globally, so a plain `cargo check` or `cargo test`
run outside `just` keeps incremental compilation for its own edit-and-rerun
loop.

Run them through `just`, not by hand. Both recipes wrap every step in
`scripts/no-pilot-env.sh`, the boundary between a worker's *pilotage*
environment and its gates. `cs tackle` steers a worker with environment
variables — `COSMON_EGRESS_POLICY`, `CB_DEPTH`, `ANTHROPIC_MODEL` and the rest
of `cosmon_core::pilot_env::PilotVar` — and a bare `cargo test` inherits them,
so tests read their parent's instructions as their own configuration. That has
produced three false verdicts and collapsed one healthy molecule
(`task-20260804-2bbb`, 2026-08-06; the work was intact at `226b9b0d`). A scoped
run needs the same wrapper: `./scripts/no-pilot-env.sh cargo test -p <crate>`.
Forget it and one named test fails with the offending variables listed — not
eleven unattributable ones.

The strip list is not maintained by hand: `cs tackle` emits every variable
through `PilotVar::name()`, so a variable absent from that enum cannot be
injected and everything in it is stripped. The boundary is a **gate** mechanism
only — it must never appear on a runtime path, where stripping
`COSMON_EGRESS_POLICY` would weaken a real jail.

`cargo` is not the whole contract. Two more gates run in CI and are not
subsumed by the four above:

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
string is named by path and line with a truncated digest, and a contact address
by its domain with the local part digested — never the value itself.
Some of its rules need a waiver from time to time — cosmon's own leak detector
must contain the shapes it detects — and the waiver is the inline marker
`publish: allow — <reason>` on that single line. Per line, never per file: a
whole-file exclusion is a blind spot nobody sees again, while a marker is a
sentence someone had to write and a reviewer reads in the diff.

Two formats cannot hold that marker: a key container has no comment syntax, and
a lockfile is rewritten wholesale by its tool, which erases any marker on the
next run. Those get a tracked `<path>.publish-allow` sidecar stating the reason
and pinning the waived blob by hash — so the waiver lapses when the file
changes, and someone re-reads it before restating the hash.

The other two release referees, `scripts/release-checklist.sh` and
`scripts/confidentiality-lint.sh`, are broader but cannot be hard gates on a
bare clone: their secret scan needs `gitleaks` installed and their content
denylist is operator-private by construction, so both honestly report PEND
without them. `publish.sh` covers only the structural subset — decidable from a
fresh clone with git and python3 and nothing else — which is what lets it fail
the build.
