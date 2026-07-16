# cs-thin test coverage rapport

Status as of **2026-05-05** (T-CS-THIN-TEST-COVERAGE-GAP).

This document tracks how the cs-thin test suite covers each category of
risk, with a per-test mapping. **It is mechanical** — the
`flag_parity` / `operator_ux` / `coverage_complete` test files are the
source of truth, and their assertions either pass (the surface is
covered) or fail (the surface drifted). The table below summarises
which test pins which axis.

## 1. Why this rapport exists

The 2026-05-05 cross-container live test surfaced a category-K gap
(wheeler delib-20260504-0913): the operator typed
`cs-thin ensemble --tag cs-thin --json` (out of muscle memory from
`cs --json ensemble`) and the binary returned a raw clap error
"unexpected argument '--json'". The 34 tests in place all guarded
**JSON output shape** (parity with cs) and **happy-path verb dispatch**
— none of them tested the **clap-flag set** or the **operator-typing
patterns**. The fix is below.

## 2. Risk taxonomy

The risk axes — five structurally independent concerns, each its own
test file:

1. **Output parity (cs ↔ cs-thin JSON shape).** Same molecule, same
   JSON envelope. Owned by `parity_with_cs.rs`.
2. **Flag-set parity (clap surface).** For every cs verb, what flags
   does cs-thin expose / refuse / silently accept? Owned by
   `flag_parity.rs`.
3. **Operator UX (paper cuts).** When the operator types a flag by
   habit, what does cs-thin do? Owned by `operator_ux.rs`.
4. **End-to-end live (docker stack).** Is the published binary
   reachable across containers, with real OIDC and real network?
   Owned by `dist/rpp-live/test-cross-container.sh` and
   `cross_container_live.rs` (feature `live-docker`).
5. **Operator-only refusal (ADR-080 §5.1).** Are the 11 operator-only
   verbs absent from cs-thin's clap surface? Owned by
   `operator_only_in_sync_with_adr.rs` and the `flag_parity.rs`
   allowlist.

## 3. Coverage table

The table below pins the test that owns each risk. Drift in any axis
fails the named test. **A blank cell is a structural bug** — every
risk × verb pair must have at least one test.

| Risk axis                 | observe | nucleate | tag | ensemble | collapse | freeze | thaw | stuck | verbs |
|---------------------------|---------|----------|-----|----------|----------|--------|------|-------|-------|
| Output parity             | `parity_observe` | `parity_nucleate` | `parity_tag` | `parity_ensemble` | `parity_collapse` | `parity_freeze_thaw` | `parity_freeze_thaw` | `parity_stuck` | n/a |
| Flag-set parity (vs cs)   | `flag_sets_match_modulo_allowlist` | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | n/a (only in cs-thin) |
| Habit-flag tolerance      | `habit_flag_config_is_rejected` | ✓ | ✓ | ✓ all/cluster/json | ✓ | ✓ | ✓ | ✓ | ✓ |
| Missing required arg      | `observe_without_molecule_id_is_rejected` | `nucleate_without_formula_is_rejected` | `tag_without_molecule_id_is_rejected` | n/a (no required) | `collapse_without_reason_is_rejected` | `freeze_without_args_is_rejected` | `thaw_without_args_is_rejected` | `stuck_without_reason_is_rejected` | n/a |
| Missing JWT / base-url    | `missing_jwt_env_var_returns_exit_3` `missing_jwt_file_returns_exit_3` `missing_base_url_returns_exit_1_with_hint` (verb-agnostic; covers all verbs via run_with) | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | n/a |
| 404 / nonexistent id      | `observe_nonexistent_id_returns_exit_1_with_body` | (covered by HTTP `read_envelope`) | (idem) | n/a | (idem) | (idem) | (idem) | (idem) | n/a |
| Refused (operator-only)   | n/a | n/a | n/a | n/a | n/a | n/a | n/a | n/a | n/a (verb registered) |
| Live cross-container      | `Scenario D` | `Scenario C` | `Scenario E` | `Scenario G` | `Scenario K` | `Scenario I` | `Scenario I` | `Scenario J` | `Scenario A` |

✓ = covered by the same test that's named in the row's first cell
(the test sweeps every verb mechanically).

## 4. cs verbs absent from cs-thin

92 cs verbs total; 10 modelled in cs-thin, 82 allowlisted as absent —
18 of those classed `operator_only` and 64 `out_of_scope` (§8p subset
strict). Each row in `tests/cli-flag-allowlist.toml` carries a written
`reason = "..."` and a `class` (`operator_only` → ADR-080 §5.1,
`out_of_scope` → §8p subset). The narrower *closed list* of ADR-080
§5.1 itself holds 11 entries, mirrored verbatim in
`cosmon_thin_cli::coverage::OPERATOR_ONLY` — that list and the
`operator_only` row count are different things and are allowed to
differ.

**The count is over every real verb, hidden or not.** It comes from
`cs __help-tree --all`, not from the `Commands:` block of `cs --help` —
that block lists only `hide = false` verbs (72 of the 92 today), and
reading it as if it were the verb set is what produced the earlier
figure of "73 total" and, in `coverage_exhaustive.rs`, a gate that
denounced 19 live-but-hidden verbs as deleted
(task-20260716-f862). Help visibility is a book decision; verb
existence is not.

The `coverage_exhaustive::exhaustive_verb_partition` test fails CI if
any cs verb sits in no bucket, or if an allowlist row names a verb
neither CLI has, so the table is self-maintaining. It skips only when
no `cs` binary exists, and never in CI (`require_cs`).

## 5. Per-cs-thin-verb flag inventory

Source of truth: `cli-flag-allowlist.toml`. Each row below derives
mechanically from the clap surfaces of the two CLIs.

| verb (cs)   | flags (cs only)                                                                                  | flags (cs-thin only) | shared           | allowlisted divergence | test id                              |
|-------------|--------------------------------------------------------------------------------------------------|----------------------|------------------|------------------------|--------------------------------------|
| observe     | --all, --formula, --notes, --search, --status, --tag, --worker, --config, --json, --verbose      | (none)               | (positional id)  | yes (cs is list+single, cs-thin is single) | `flag_sets_match_modulo_allowlist`   |
| nucleate    | --assign, --blocked-by, --blocks, --decayed-from, --energy-budget, --expires-at, --expiry-policy, --fleet, --formulas-dir, --from, --no-parent, --refines, --role, --store-dir, --ttl, --config, --json, --verbose | --formula            | --kind, --var, --tag | yes (cs has formula positional + many edges) | `flag_sets_match_modulo_allowlist`   |
| tag         | --config, --json, --verbose                                                                      | (none)               | --add, --remove, (positional id) | yes (globals only)        | `flag_sets_match_modulo_allowlist`   |
| ensemble    | --all, --cluster, --cluster-root, --config, --json, --verbose                                    | --status, --kind, --fleet | --tag    | yes (filters differ)   | `flag_sets_match_modulo_allowlist`   |
| collapse    | --ops-dir, --config, --json, --verbose                                                           | (none)               | --reason, --cause, --account, --kind, (positional id) | yes (--ops-dir cs-only)   | `flag_sets_match_modulo_allowlist`   |
| freeze      | --by, --no-tmux, --timeout, --config, --json, --verbose                                          | (none)               | --reason, (positional id) | yes (worker-process plumbing cs-only) | `flag_sets_match_modulo_allowlist`   |
| thaw        | --continue, --no-tmux, --config, --json, --verbose                                               | (none)               | (positional id)  | yes (worker-process plumbing cs-only) | `flag_sets_match_modulo_allowlist`   |
| stuck       | --config, --json, --verbose                                                                      | (none)               | --reason, (positional id) | yes (globals only)        | `flag_sets_match_modulo_allowlist`   |

## 6. Discipline — meta-rule

> **When a new test reveals a gap, this document updates.**

That is the mechanical contract. Concretely:

- A new flag added to cs but not cs-thin → `flag_sets_match_modulo_allowlist`
  fails → choose: (a) add it to cs-thin, (b) add a `[[flag_only_in_cs]]`
  row to `cli-flag-allowlist.toml`. Either way, this table updates in
  the same PR.
- A new cs verb → `every_cs_verb_is_modelled_or_allowlisted` fails →
  choose: (a) wire it to cs-thin, (b) add a `[[verb_absent_from_thin]]`
  row with `class` and `reason`. Either way, this table updates.
- A new operator paper-cut → `every_verb_has_a_paper_cut_test` fails
  → add a missing-arg / habit-flag test in `operator_ux.rs`. Update
  this table.

## 7. Open gaps (acknowledged, tracked)

| Gap                                                                | Tracker                                  |
|--------------------------------------------------------------------|------------------------------------------|
| `cs ensemble` doesn't expose `--status`, `--kind`, `--fleet` flags. cs-thin does (it mirrors `GET /v1/molecules` query params). Allowlisted as `[[flag_only_in_thin]]`; bring cs into line in V1. | future-V1                                |
| `cs nucleate` takes formula as a positional arg; cs-thin requires `--formula`. Allowlisted; reconcile in V2 (server-side formula resolution constraint). | future-V2                                |
| `--json` is a global no-op on cs-thin (silent), but per-verb `--json` is rejected. The asymmetry is intentional — operator typing the global form must succeed; per-verb form lets clap surface the divergence. Documented behaviour. | accepted                                 |
| `--config`, `--verbose` rejected on cs-thin (no equivalent). The operator gets a clear clap error naming the offending flag (pinned by `habit_flag_config_is_rejected`). | accepted                                 |
| `flag_parity.rs` still enumerates cs verbs by scraping the root help's `Commands:` block, so it only checks the non-hidden verbs: the flag-set parity of the ~20 hidden-but-live verbs (`events`, `presence`, `stitch`, `note`, …) is never compared. It is green because it under-checks, not because the parity holds. `coverage_exhaustive.rs` was moved to `cs __help-tree --all` in task-20260716-f862; moving `flag_parity.rs` too will surface that unexamined drift for the first time. | task-20260716-0396 |

## 8. Counts

- **Test files** in `crates/cosmon-thin-cli/tests/`: 10.
- **Test functions** total (modulo features): 66 `#[test]` /
  `#[tokio::test]` attributes across those 10 files.
- **Allowlist rows** in `cli-flag-allowlist.toml`: 82 verb-absent +
  2 verb-only-in-thin + 54 flag-only-in-cs + 8 flag-only-in-thin
  = **146**.
- **cs verbs** covered by the inventory: 92 (every top-level verb in
  `cs __help-tree --all`, hidden ones included — see §4).
- **cs-thin verbs**: 10.

These counts are **hand-maintained and not machine-checked.** An
earlier revision of this section asserted the opposite — that "every
count above is a function of the source files" and that drift "would
surface as a count mismatch in the next CI run." Nothing computes them,
so nothing did: the verb count sat at 73 while cs grew to 92. The only
count any test enforces is `allowlist_counts_are_plausible`, which
asserts a floor (`verb_absent_from_thin >= 11`), not equality. Treat
the numbers here as a snapshot with a date, not as a gate; the gates
are the tests named in §3, and they compare sets, not totals.
