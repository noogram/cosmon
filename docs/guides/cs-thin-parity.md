# cs ↔ cs-thin parity (T-CST-PARITY)

> *« le test n'est pas du wrapper, c'est du système »* — wheeler,
> [delib-20260504-0913](../../.cosmon/state/molecules/delib-20260504-0913)
> §Mécanisme de mesure.

The local `cs` binary (operator-paid) and the `cs-thin` HTTP client
(JWT-paid, ADR-080 §8j Remote Pilot Port) MUST be byte-equivalent on
the intersection of verbs that crosses the wire — the §8p
"API surface ⊊ CLI surface" invariant.

This guide explains what the parity gate enforces, how the mechanism
works, and how to debug a mismatch.

## The invariant

For every verb annotated `#[cosmon_thin_macro::verb(...)]` (currently:
`observe`, `nucleate`, `tag`):

```text
output(cs --json <verb> <args>)  ≡  output(cs-thin <verb> <args>)
                                 modulo allowlist
```

The allowlist (`crates/cosmon-thin-cli/tests/parity-allowlist.toml`)
names the small set of fields that *must* differ — for structural
reasons, not implementation drift. Every entry carries a written
rationale; there is no
*ignore-everything-that-differs* escape hatch.

## How the gate is composed

Three independent tests, all required to be green in CI:

### 1. Bijection test — `tests/api_surface_freeze.rs::routes_and_verbs_are_bijective`

Asserts that the set of `(method, path)` pairs in
`cosmon_rpp_adapter::frozen_api_surface` equals the set in
`cosmon_thin_cli::registry::all`. A new `#[verb]` annotation that
forgets to mount a route — or a route that forgets the annotation —
fails this test. Path templates are normalised so `:id` (Express,
used by the macro) and `{id}` (axum 0.7+) compare equal.

This is the *structural* bijection: surface ↔ verbs.

### 2. NDJSON comparator — `cosmon_thin_cli::parity`

Pure function `compare(left, right, verb, allowlist) -> Vec<Diff>`
walking two `serde_json::Value` trees. Reports every divergence as a
typed `Diff { path, left, right, kind }`. Allowlist templates support
literal paths, single-segment `*`, and trailing `**` for descendants.

Audited in `cosmon-thin-cli/src/parity.rs::tests` — every code path is
exercised in isolation so a CI failure can be reproduced offline from
the captured byte streams alone.

### 3. End-to-end parity — `tests/parity_with_cs.rs`

For each of the three V0 verbs, the test:

1. Provisions a `TenantWorkspaces` tempdir with `.cosmon/state/` (and
   `.cosmon/formulas/` for `nucleate`).
2. Plants a fixture molecule (`observe`, `tag`) or a `task-work`
   formula (`nucleate`).
3. Boots the rpp-adapter in-process on a random `127.0.0.1` port.
4. Mints a JWT via `cosmon-oidc-testkit::OidcMock`.
5. Invokes `cs --json <verb>` as a real subprocess with
   `COSMON_STATE_DIR` / `COSMON_FORMULAS_DIR` / `HOME` env vars
   pinned to the tempdir, and `COSMON_PARENT_MOL_ID` cleared so the
   worker harness's auto-DecayProduct contract does not pollute the
   nucleate output.
6. Invokes `cs-thin <verb>` via the in-process `run_with` entry
   point — exactly the function the `cs-thin` binary calls in
   `main.rs`. No mock dispatcher.
7. Parses both outputs as JSON and feeds them to
   `parity::compare(.., verb, allowlist)`. Asserts the diff vec is
   empty.

The cs binary is located via, in order:

1. `COSMON_THIN_PARITY_CS_BIN` env var (CI, scripted runs).
2. Workspace `target/{debug,release}/cs` walked up from
   `CARGO_MANIFEST_DIR`.
3. `cs` on `PATH`.

If none resolve, the test prints a SKIP notice on stderr and returns
successfully. CI is responsible for building cs first — see §CI below.

## The allowlist — why each entry exists

| Verb | Path | Why it must differ |
|------|------|--------------------|
| `observe` | `molecule_dir` | cs prints absolute filesystem path; rpp-adapter substitutes the molecule id to avoid leaking server FS layout. |
| `*` | `request_id` | Server-generated UUID per HTTP request; cs has no envelope. |
| `nucleate` | `id` | Each invocation creates a new molecule; the id IS the difference. |
| `nucleate` | `created_at` | Wall-clock at call site; cs and cs-thin run sequentially, never simultaneously. |
| `observe` | `updated_at` | Same wall-clock concern when re-read between calls. |
| `*` | `tags.*` | Compared structurally; identical tag set asserted by the per-verb scenario rather than by paths. |

Adding an entry MUST land an ADR amendment (or, for V0 trivia, a
chronicle paragraph in an internal chronicle). The allowlist is a
soft contract — operators read it; CI does not police rationale.

## Debugging a mismatch

When CI prints:

```text
parity[observe] = MISMATCH
3 unallowed diff(s):
  - [value_mismatch] formula: cs="task-work" | cs-thin="other-work"
  - [missing_right] tags.0: cs="temp:hot" | cs-thin=<absent>
  - [type_mismatch] total_steps: cs=3 | cs-thin="3"
```

The triage tree:

1. **`value_mismatch` on a stable field** (formula, status, total_steps)
   → either cs and cs-thin called against different fixtures, or one
   side miscoded the field. Check that both sides target the SAME
   molecule id (or the same canonical action) and that
   `cosmon_state::ops::*` is the only producer of that field.
2. **`type_mismatch`** → almost always a serialisation drift. Check
   the `Serialize` impl in `cosmon-state::ops::<verb>::*Json`; it is
   the single source of truth.
3. **`missing_left` / `missing_right`** → field added to one renderer
   but not the other. Both `cs --json` (`cosmon-cli/src/cmd/<verb>.rs`)
   and the rpp-adapter handler
   (`cosmon-rpp-adapter/src/routes/molecules.rs`) consume the SAME
   `*Json` struct; if a field is missing on one side, the projection
   layer added/dropped it.
4. **A field that genuinely should differ** → add an allowlist entry
   with rationale. Land it in the same PR as the field change.

## CI integration

The parity test is part of `cargo test --workspace --locked` (the
existing `test` job in `.github/workflows/ci.yml`). The job also
runs `cargo build --bin cs -p cosmon-cli --locked` *before* the test
step so `find_cs_binary()` resolves the workspace `target/debug/cs`.

If you want to run the gate locally:

```sh
# One-time per branch — build the cs binary cs-thin parity will exec
cargo build --bin cs -p cosmon-cli

# All three parity tests (≈ 5 s on a warm cache)
cargo test -p cosmon-thin-cli --test parity_with_cs

# Plus the bijection
cargo test -p cosmon-rpp-adapter --test api_surface_freeze
```

Or, if cs is installed system-wide and on PATH:

```sh
cargo test -p cosmon-thin-cli --test parity_with_cs
# (find_cs_binary falls through to PATH on the third try)
```

## Scope and known limits

- **V0 covers three verbs.** When `#[verb]` is added to a fourth
  function, both the bijection test and the parity scenarios must
  grow. The bijection grows automatically; the parity scenario must
  be added by hand (different verbs need different fixtures).
- **In-process rpp-adapter.** The parity test exercises the *real*
  rpp-adapter library, just bound on a `127.0.0.1` listener instead of
  a docker container. The full docker stack lives at
  `dist/rpp-live/`; running the bash harness `dist/rpp-live/test-curl.sh`
  is a complementary gate (manual, not a `cargo test`).
- **Two cs subprocesses, in-process cs-thin dispatcher.** The cs-thin
  side calls `run_with` directly because it IS the binary's entry
  point — there is no semantic gap. The cs side spawns a real
  subprocess because `cosmon-cli` does not export its CLI dispatcher
  as a library function.

## Related artefacts

- Substrate: `delib-20260504-0913` — three-axis taxonomy that produced
  the parity gate as the *measurement* leg.
- ADR-080 §4.2 (R3 in §12) — bijection between surface and ops.
- `crates/cosmon-thin-cli/src/verbs.rs` — *the duplication is
  intentional*: the comment block there names this guide as the gate
  that makes the duplication safe.
- `crates/cosmon-rpp-adapter/tests/api_surface_freeze.rs` — bijection
  test home.
- `crates/cosmon-thin-cli/tests/parity_with_cs.rs` — parity scenarios.
- `crates/cosmon-thin-cli/tests/parity-allowlist.toml` — explicit
  divergence list.
- `crates/cosmon-thin-cli/src/parity.rs` — pure NDJSON comparator.
