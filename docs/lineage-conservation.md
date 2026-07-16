# Lineage conservation — the auto-parent contract

**Scope:** `cs tackle`, `cs nucleate`, worker environment.
**Binds:** [ADR-037](adr/037-lineage-conservation-and-verification-architecture.md)
(lineage conservation & verification), [ADR-016](adr/016-autonomy-regimes-and-resident-runtime.md)
(autonomy regimes), [ADR-040](adr/040-runtime-cognition-architecture.md)
(runtime-cognition split).
**Motivation:** mailroom-20260414-cb10 orphan cascade — 17 pending
molecules nucleated without a parent edge because a worker forgot to pass
`--blocked-by` five times in a row.

## The problem

Cosmon's DAG is the control plane. A molecule with no incoming edge is,
by construction, a root — the runtime's ready-frontier policy treats it
as runnable only if nothing else keeps it inactive. When a worker
nucleates children from inside its own molecule step, the intended
meaning is almost always "these are my decay products, keep them tied
to me." Several formulas (`deep-think`, `mission-controller`,
`mission-plan`) encode that intent as a rule in the worker's prompt:

> **NEVER nucleate a child without passing `--blocked-by <parent_id>`.**

That rule is **declarative, not structural**. It survives only as long
as the LLM's cognition honors it. On 2026-04-14, a `mailroom` worker
decomposed a mission into five action-tracker children and forgot the
flag on every single call. The children landed in the store as roots,
the runtime's ready-frontier immediately ignored them (no parent
relationship means no lineage signal to act on), and they quietly
sedimented. Seventeen similar molecules accumulated across the
afternoon.

Root cause: **a contract that depends on cognition is fragile.** The
fix is to move the discipline from the prompt into the shell.

## The contract

When `cs tackle <mol>` spawns a worker, it now injects two environment
variables into the worker's shell:

| Variable | Meaning |
|----------|---------|
| `COSMON_MOL_DIR` | Path to the molecule's state directory (existing). |
| `COSMON_PARENT_MOL_ID` | **New.** Molecule id of the worker's own molecule. |

`cs nucleate`, on every invocation, consults `COSMON_PARENT_MOL_ID`. If
the variable is set **and** the operator did not pass an explicit
edge-declaring flag (`--blocks`, `--blocked-by`, `--decayed-from`) or
the opt-out `--no-parent`, nucleate auto-synthesizes a
`DecayedFrom { id: parent }` edge on the new molecule and the symmetric
`DecayProduct { id: new }` edge on the parent. A stderr hint records
the synthesis so operators can see it in the worker's log:

```
auto-linked to parent task-20260414-cb10 via DecayProduct (pass --no-parent to disable)
```

## Precedence

Given an invocation like `cs nucleate <formula> [flags...]`, the
resolver applies the following rules in order:

1. `--no-parent` → no auto-link. Always wins.
2. `--decayed-from <id>` → use it verbatim.
3. Any explicit `--blocks` or `--blocked-by` → no auto-link (the
   operator already declared an edge; the env layer stays silent so
   we never stack a second edge on top of an explicit one).
4. `COSMON_PARENT_MOL_ID` set → synthesize
   `DecayedFrom { id: <parent> }`.
5. Otherwise → nothing.

This is deliberately **opt-out**: the default under a tackled worker
is always to attach. Legitimate orphan nucleations (e.g. a worker that
spawns a truly unrelated top-level mission) must pass `--no-parent`.

## Why `DecayProduct`, not `BlockedBy`

`BlockedBy` means *the parent cannot progress until the child
completes*. That is almost always **false** at nucleation time: the
parent is in step N+1 when it spawns the child, and the runtime would
deadlock the parent if we said otherwise.

`DecayProduct` / `DecayedFrom` encodes *information lineage*: "this
child emerged from that parent's cognition." It is the right semantic
for emergent decomposition, and it composes with the runtime's lateral
drain pass (fix `dc66e2f`, 2026-04-14 morning) that already picks up
orphaned decay children when the parent is Active or Completed — the
orphans are rescued automatically.

The practical consequence: **a child produced under the auto-parent
contract is guaranteed to be schedulable**, either because the runtime
walks down the parent's `DecayProduct` edges on its next pass, or
because the operator explicitly promotes the edge to a blocking one.

## Failure modes and expected behavior

- **Worker sets `COSMON_PARENT_MOL_ID` itself.** Accepted — the
  override is a conscious choice.
- **Parent collapsed between env injection and nucleation.** The edge
  is still written, but the runtime ignores the child once it sees
  `Collapsed` in the parent (terminal state). Non-blocking; the child
  is a valid historical record.
- **Worker nucleates in a loop.** Every child gains the edge. This is
  not a bug but a load concern — `temp-review` and attention-budget
  warnings remain the throttle.
- **`COSMON_PARENT_MOL_ID` is malformed** (e.g. injected by the
  operator shell). `cs nucleate` refuses with an error that names the
  env var and points at `--no-parent` as the escape hatch.

## Interaction with `--from <path>`

`cs nucleate --from <declaration>` is git-tracked and is treated as an
explicit contract: the auto-parent contract does **not** apply.
Declarations must declare their own edges in the TOML file.

## What this does not replace

The auto-parent contract is a *robustness* layer, not a verification
layer. It guarantees the edge exists; it does not guarantee the
semantics are correct. Formulas that need a hard blocking dependency
between parent and child still have to pass `--blocked-by` explicitly.
ADR-037's lineage conservation principle (every claim must be traceable
to an imported source) is enforced at completion time by `cs verify`,
not at nucleation time.

## How to audit

- Inspect a worker's typed links: `cs observe <id> --json | jq '.typed_links'`.
- Verify a parent's decay products: `cs deps <parent_id> --transitive`.
- Search events for the hint: the `cs nucleate` stderr line
  `auto-linked to parent ...` is also captured in the worker's tmux
  pane when running inside a tackled worktree.

## Code entrypoints

| Concern | File |
|---------|------|
| Env injection | `crates/cosmon-cli/src/cmd/tackle.rs` (`spawn_and_prompt`) |
| Precedence resolver | `crates/cosmon-cli/src/cmd/nucleate.rs` (`resolve_decayed_from`) |
| Edge synthesis | `crates/cosmon-cli/src/cmd/nucleate.rs` (`nucleate_and_persist`) |
| Symmetric link idempotency | `crates/cosmon-cli/src/cmd/nucleate.rs` (`link_already_present`) |
| Integration regression | `crates/cosmon-cli/tests/cli.rs` (`test_nucleate_auto_parent_contract_from_env_var`) |
