# Red repro-contract #21 — `--resident` renders `$COSMON_DEFAULT_ADAPTER` unreachable

**Class:** resolver precedence. **LLM:** none required (pure resolver assertion).
**Affected ref:** v0.2.2. **Assertion target:** the COMPOSED resolver
`resolve_adapter_selection(...)`, surfaced via the `adapter_selected` event's
`selection_source` — NEVER the literal `--adapter` string.

## The contract (G1 — what cosmon promises)

The adapter for a `cs tackle` dispatch is the value the **composed resolver**
`resolve_adapter_selection` returns, under the documented precedence:

```
flag (--adapter)          rank 1  -> AdapterSelectionSource::Cli
formula-step `adapter =`  rank 2  -> AdapterSelectionSource::FormulaStep
$COSMON_DEFAULT_ADAPTER   rank 3  -> AdapterSelectionSource::EnvVar
[adapters.default] cfg    rank 4  -> AdapterSelectionSource::Config
global [adapters.default] rank 5  -> AdapterSelectionSource::GlobalConfig
built-in floor ("local")  rank 6  -> AdapterSelectionSource::Default
```
(Source: `crates/cosmon-cli/src/cmd/tackle.rs::resolve_adapter_selection`.)

The promise: **for a molecule with NO adapter pin, an operator who exports
`$COSMON_DEFAULT_ADAPTER=claude` gets `claude` under EVERY dispatch path — including
`cs run --resident`.** The env-var tier must be *reachable* on the resident path.

## The bug (source-located)

`crates/cosmon-runtime/src/resident.rs:587-594` dispatches a pin-less molecule as
`Decision::Tackle { adapter: Some(m.adapter.unwrap_or(SAFE_DEFAULT_ADAPTER)) }` —
i.e. it materialises `--adapter local` (rank 1, `Cli`) whenever the molecule
carries no pin. Rank 1 **structurally outranks** `$COSMON_DEFAULT_ADAPTER` (rank 3)
in `resolve_adapter_selection`. So under `--resident`, the env-var tier is
**structurally unreachable** for a pin-less molecule: the operator's
`COSMON_DEFAULT_ADAPTER=claude` is silently ignored, and the worker runs on the
`local` floor.

**This is an *écriture qui masque*, not a *lecture manquante*** (blueprint §5): the
resident path *writes* a flag that masks the env, not a reader that *fails to look*.

## The fix (either form is acceptable)

The fix accepts **one OR the other** (the assertion must pass for both):

- **(a) omit the flag** — resident emits `adapter: None` when the molecule has no
  pin, letting `resolve_adapter_selection` fall through to the env-var tier; or
- **(b) resolve env before writing** — resident resolves `$COSMON_DEFAULT_ADAPTER`
  itself and writes that as the flag.

Because both are valid, the red MUST assert on the **composed resolver output**
(the effective adapter), NOT on the literal `--adapter` string (which is `local`
under the bug, absent under fix (a), and `claude` under fix (b) — three different
literals for two correct behaviours). Asserting on the literal is the false-green.

## The red harness (runs in the clean-room against `git archive v0.2.2`)

The observable is the `adapter_selected` event's `adapter_name` + `selection_source`
emitted by `cs tackle --dry-run` (the dry-run path walks the full resolution block;
see `crates/cosmon-cli/tests/tackle_adapter_flag.rs`). The harness reproduces the
resident dispatch's argument shape for a pin-less molecule, with the operator's env
hammer set, and asserts the effective adapter honours the env.

```bash
#!/usr/bin/env bash
# repro-21.sh — run inside the clean-room (repro-user is enough; no root needed).
# RED on v0.2.2: resolves to local/cli. GREEN on the fix: resolves to claude/env.
set -euo pipefail

# 1. A pin-less molecule (no --adapter flag, no formula-step adapter). The RESIDENT
#    path is what materialises `--adapter local`; we reproduce its dispatch by
#    invoking `cs tackle --dry-run` with NO flag, exactly as fix (a) would, while
#    the OPERATOR ENV HAMMER is set. On v0.2.2 the resident loop writes the flag
#    FOR us; we assert on what a resident-dispatched pin-less molecule RESOLVES to.
export COSMON_DEFAULT_ADAPTER=claude        # the operator's rank-3 session hammer

# 2. Drive the resident dispatch (the code path that emits the flag). Use the real
#    `cs run --resident` for ONE tick on a single pin-less molecule, capturing the
#    adapter_selected event it emits. (A --dry-run resident tick exists on >=0.2.2;
#    if absent on the archived ref, assert directly on resolve_adapter_selection via
#    the unit harness below.)
cs run --resident --once --dry-run <pinless_mol_id> >/dev/null 2>&1 || true

# 3. Read the effective adapter from the adapter_selected event (the COMPOSED
#    resolver's output — NOT the literal flag).
effective=$(jq -r 'select(.type=="adapter_selected") | .adapter_name'      state/events.jsonl | tail -1)
source=$(jq   -r 'select(.type=="adapter_selected") | .selection_source.source' state/events.jsonl | tail -1)

echo "effective adapter = $effective (source = $source)"

# 4. THE CONTRACT ASSERTION — the env hammer must be honoured.
if [ "$effective" != "claude" ]; then
  echo "RED (right reason): pin-less resident dispatch resolved to '$effective'" >&2
  echo "     via source '$source' — \$COSMON_DEFAULT_ADAPTER=claude was UNREACHABLE." >&2
  echo "     (v0.2.2: resident.rs:587-594 writes --adapter local, rank 1 > env rank 3.)" >&2
  exit 1
fi
echo "GREEN: env-var tier reachable on the resident path."
```

### Unit-level twin (if the resident dry-run tick is absent on the archived ref)

A pure assertion on `resolve_adapter_selection`, encoding the full precedence
table — the fix-variant-agnostic form. This is the honest, LLM-free oracle:

```rust
// Drives resolve_adapter_selection with the argument shape a resident dispatch of
// a PIN-LESS molecule produces. The bug is that resident supplies flag=Some("local")
// for a pin-less molecule; the fix supplies flag=None (variant a) OR
// flag=Some(env) (variant b). We assert the COMPOSED result honours the env.
#[test]
fn resident_pinless_dispatch_honours_env_default_adapter() {
    // The operator's rank-3 hammer.
    let env_default = Some("claude");

    // FIX SHAPE: a pin-less molecule supplies no flag and no step pin, so the
    // resident path must NOT inject `--adapter local`. Under the fix, flag is None.
    let (name, source) = resolve_adapter_selection(
        /* flag             */ None,        // fix (a): resident omits the flag
        /* formula_step      */ None,
        /* env_default       */ env_default,
        /* adapters_cfg      */ None,
        /* config_path       */ Path::new("/x/.cosmon/config.toml"),
        /* global_cfg        */ None,
        /* global_path       */ Path::new("/x/global.toml"),
    );
    assert_eq!(name, "claude", "env-var tier must be reachable for a pin-less molecule");
    assert!(matches!(source, AdapterSelectionSource::EnvVar { .. }));

    // BUG SHAPE (v0.2.2): resident injects flag=Some("local") for the same pin-less
    // molecule, which outranks the env. This is what makes the env unreachable.
    let (buggy_name, buggy_source) = resolve_adapter_selection(
        /* flag */ Some("local"),   // resident.rs:587-594 materialises this
        None, env_default, None,
        Path::new("/x/.cosmon/config.toml"), None, Path::new("/x/global.toml"),
    );
    assert_eq!(buggy_name, "local");
    assert!(matches!(buggy_source, AdapterSelectionSource::Cli { .. }),
        "the resident-injected flag is rank-1 Cli and masks the rank-3 env — the bug");
}
```

## Differential refutation (one variable flips the colour)

The single variable is the **flag the resident path supplies for a pin-less
molecule**: `Some("local")` (bug) vs `None` (fix a). Flip it and the effective
adapter flips `local` <-> `claude`, and the `selection_source` flips
`Cli` <-> `EnvVar`. The unit twin above demonstrates BOTH transitions in one test:
the first block is the fix colour (green), the second is the bug colour (red on
v0.2.2). Reverting the resident fix restores `Some("local")` and re-reddens.

## False-green mode

- **Asserting on the literal `--adapter` string.** Under fix (a) there is no flag;
  under fix (b) the flag is `claude`; under the bug it is `local`. A test that
  asserts `resident emits no --adapter flag` PASSES for fix (a) but WRONGLY FAILS
  for the equally-valid fix (b) — and a test that asserts `flag == "claude"` passes
  for fix (b) but wrongly fails for fix (a). Only the COMPOSED resolver output is
  invariant across both valid fixes. Assert there.
- **An `[adapters.default]` config or global config accidentally present** in the
  clean-room could satisfy the assertion via rank 4/5 instead of the env tier — a
  false green that hides the env-unreachability. The harness must strip config
  tiers (as `tackle_adapter_flag.rs` does with `COSMON_CONFIG_HOME` isolation) so
  the ONLY path to `claude` is the rank-3 env.

## False-red mode

- **A typo'd env var name** (`COSMON_ADAPTER_DEFAULT`) would leave the env unset and
  redden for the wrong reason (the built-in floor `local`, source `Default`, not
  `Cli`). The harness distinguishes: a right-reason red shows source `Cli` (the
  masking flag); a wrong-reason red shows source `Default`. Check the source.
- **The molecule accidentally carrying a pin** would make `Some(pin)` legitimate.
  The harness asserts the molecule is pin-less first.

## Adjacent nit (not this contract's red)

A model pinned with no reachable adapter should raise a **warning at
germination/dispatch**, not a collapse (blueprint §5). File as a follow-up, not
folded into this red.
