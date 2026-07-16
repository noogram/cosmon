# cosmon-crashtest

Property-based crash-resilience harness for cosmon. Implements turing's
bisimulation property (see deliberation `delib-20260414-8c82`): a crashed +
resumed run must produce the same canonical lifecycle trace as an
uninterrupted run, modulo timestamps and opaque LLM content.

## Run

```
cargo test -p cosmon-crashtest --release
```

Nightly CI job — not per-commit (1000 proptest cases).

## What this covers

- DAG traversal with `blocked_by` dependencies.
- Per-step artifact emission under a deterministic LLM stub.
- Crash injection (`SIGKILL`-equivalent abort) at a random step index.
- Atomic rename semantics for state persistence — no torn writes.
- Resume-from-disk producing identical terminal lifecycle states and
  identical canonical event traces.

## What this does NOT cover (yet)

- **fsync-ordering fault injection** — needs `dm-flakey` + a Linux CI lane.
- **Real subprocess `SIGKILL`** — the current harness simulates crashes in
  process. When the sibling tasks `task-20260414-3b36` (`cs resume`) and
  `task-20260414-8acc` (idempotence guards) land, the `run()` driver can be
  swapped for a real-subprocess driver that spawns `cs tackle`, signals
  `SIGKILL` after N steps, and invokes `cs resume`. The canonicalization
  and property statement do not change.
- **Byzantine concurrent-writer fuzz** — separate concern.

## Adding a new bisimulation case

1. Add a generator or a targeted DAG shape in `src/lib.rs` (examples:
   `arbitrary_linear_dag`, `arbitrary_diamond`). Keep shrinkers cheap.
2. Add a `#[test]` or a `proptest!` block in `tests/recovery.rs` that
   drives `run()` through the new shape, injects a crash, and asserts
   `canonicalize(trace_resumed) == canonicalize(trace_uninterrupted)`.
3. If the shape needs a new lifecycle invariant, extend `canonicalize()`
   rather than weakening the assertion — the property statement is the
   contract, not the test body.
4. Re-run with `PROPTEST_CASES=10000` before merging to flush any
   low-frequency flake.

## Design notes

- `DeterministicLlmStub` makes synthetic artifact bytes reproducible from a
  seed. This lifts the equivalence from content-free bisimulation to
  structural equality on artifact presence classes.
- `canonicalize()` is a no-op in the in-process model (the driver is
  single-threaded and deterministic) but is kept as a named function so the
  property statement reads identically once real subprocess mode lands.
- Fleet dirs allocated under `std::env::temp_dir()`, which is tmpfs on Linux
  CI runners. One trial ≈ sub-millisecond; 1000 cases run in a few seconds.
