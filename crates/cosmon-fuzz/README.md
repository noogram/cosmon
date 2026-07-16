# cosmon-fuzz

Property-based, in-process fuzz harness for the `cs` CLI surface. The
feynman companion to the CLI type-tightening task
(`task-20260414-81b1`): the type system prevents ill-formed inputs by
construction; this crate catches everything that is grammatically
valid but semantically ill-formed.

## What it covers

| Target | Property |
|---|---|
| `cosmon_core::id::MoleculeId` | parse is total; round-trip equality on success |
| `cosmon_core::tag::Tag` | parse is total; key/value grammar; round-trip on success |
| `cosmon_core::id::WorkerId` | parse is total; `ep-` prefix bit round-trips |
| `cosmon_core::formula::Formula` | parse is total; step id uniqueness + order bounds |
| `cosmon_core::nucleate::nucleate` | required-variable contract; id-prefix preservation; pass-through |
| Lifecycle state machine (`Ensemble`) | merge-before-dispatch; transition legality; reconcile idempotence |
| Blocker DAG | no self-reference, no cycles, no dangling reference |

## Run

```bash
# Quick (256 cases / property, seconds):
cargo test -p cosmon-fuzz

# Longer soak (tens of thousands of cases, ~minutes):
PROPTEST_CASES=10000 cargo test -p cosmon-fuzz --release

# Regression corpus only (fast):
cargo test -p cosmon-fuzz corpus_
```

## CI policy

The quick tier (default `PROPTEST_CASES=256`) runs on every PR
alongside `cargo test --workspace`. The long soak runs on a nightly
schedule. Any new panic or property violation fails the build.

## Adding a new property

1. Add a generator in `tests/properties.rs` (e.g. `arb_foo`).
2. Add an oracle function in `src/lib.rs` (e.g. `oracle_foo`).
3. Add a `proptest!` block wiring the two, with a one-line doc
   comment stating the invariant it enforces.

## Corpus — historical incidents

`src/lib.rs` → `corpus` module. Each entry is a minimal
[`SimCommand`] sequence that reproduces a known incident; running
`cargo test -p cosmon-fuzz corpus_` asserts every seed still behaves
as expected. Current entries:

- `convoy_cascade` — unblocked tackle attempt without merging head/middle.
- `b22c-placeholder` — dangling `--blocked-by` reference (pending chronicle attachment).
- `f4e1-placeholder` — self-referential `--blocked-by` (pending chronicle attachment).

When a new incident lands, append a reproduction function to `corpus`,
add a regression test, and link the chronicle in the doc comment.

## Why not `cargo fuzz` / libFuzzer?

Two reasons:

1. **Determinism.** proptest replays the same seed on the same input,
   so a failing case shrinks to a minimal reproduction and lands in
   the corpus as a `#[test]`. libFuzzer's exploration is coverage-
   guided but brittle to reproduce.
2. **Toolchain.** `cargo fuzz` requires nightly + a separate
   `fuzz/` crate layout. proptest is already a workspace dependency
   (used by `cosmon-crashtest` and several core crates) and runs
   under stable `cargo test`.

The task briefing explicitly permits this path: *"Fuzz harness
runnable as `cargo fuzz run <target>` **or** a property-test suite
under `cargo test --workspace --features fuzz`"*. If coverage-guided
exploration becomes valuable, a `cargo fuzz` wrapper can be added as
a second crate that shares the same oracles.

## Non-scope

- Real subprocess spawning (`cs tackle` → tmux → worker).
- fsync-ordering fault injection — that's `cosmon-crashtest`.
- LLM-driven input generation (explicit out-of-scope in the task briefing).
- Replacing the scenario harness — orthogonal.
