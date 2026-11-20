# Gate slots and worker admission

Admission is checked when `cs tackle` would spawn a worker. If the active
molecule count reaches the configured cap or free space falls below the
configured floor, `cs tackle` returns an error before creating the worker's
worktree, branch, or session. Gate slots apply when a command is ready to
run. If all slots in its pool are held, the command waits for one to free up.
See the [project configuration reference](../project-config.md).

## Run commands in two independent pools

Set the pool sizes in the galaxy's `.cosmon/config.toml`.

```toml
[gate_slots]
full_gate = 2

[gate_slots.pools]
docker_build = 1
```

From worktrees that resolve to the same galaxy state directory, run full
verification in `full_gate` and container builds in `docker_build`.

```sh
cs gates slot -- just gates
cs gates slot --pool docker_build -- docker build .
```

If two worktrees are running `just gates` through `cs gates slot`, a third
full gate waits. One container build can run at the same time. A second
container build waits for `docker_build`, even if `full_gate` has a free slot.
Commands participate by taking a slot in the chosen pool.

Each slot is a file lock under the shared state directory. `cs gates slot`
holds it while the command runs and passes back the command's exit status.
The lock is released when the holder exits, including if it is killed.
When every slot is held, the CLI reports that the run is queued on stderr
and reports the slot number once acquired. Waiter order is not specified.
See [ADR-190](../adr/190-bounded-verification-and-train-landing.md).

## Read the background runner's receipts

In cosmon's own repository, `just gates-bg` uses `scripts/run-gates-bg.sh`.
The script checks whether `full_gate` is bounded and, when it is, wraps the
bundle in `cs gates slot --pool full_gate`. With an unbounded pool it runs
the bundle directly.

The launcher reports `gates queued for a slot of pool full_gate` when it
returns while the run is waiting. Look in `$COSMON_MOL_DIR`, or in
`./.cosmon-gates-runs` outside a molecule, for the receipts.

| File | What it records |
|------|-----------------|
| `gates.queued` | The detached runner's PID and pool name on separate lines while it waits. With an unbounded pool this receipt is absent. |
| `gates.pid` | The detached runner's PID. In a successful start with a bounded pool, it appears at acquisition and the runner removes `gates.queued`. |
| `gates.log` | Combined output, including the CLI's queue and acquisition messages when it had to wait. |
| `gates.exit` | The run's exit code, published when it finishes. |

Use `gates.exit` for the result. While queued, the run has yet to produce a
gate verdict.
