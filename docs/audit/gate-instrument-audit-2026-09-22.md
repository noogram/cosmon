# Gate instrument audit — what each control actually observes

**Date:** 2026-09-22. **Molecule:** `task-20260922-8d48`.
**Predicate, applied once to every instrument:** *does this control observe the
thing it claims to measure, and over how much of the surface?*

**Why now.** Five instruments in one month were found reading as present while
looking elsewhere: a `MockBackend` that recorded agent id and cwd but not argv;
a published OpenAPI document declaring 14 paths against 27 mounted; an
`api_cli_coverage` test hand-wired to 3 of 42 live routes whose reverse check
fired only for `V0` rows; an `adapter_provides` that discards its capability
argument; a harness-fixture waiver granted per file rather than per line. The
shape is always the same — the instrument was written against the surface as it
stood, the surface grew, and nothing fails when an instrument stops covering
something. That is precisely the failure mode.

**This audit changes no gate.** It measures them. Every number below came from a
command run against this worktree at `feat/task-20260922-8d48`; claims that
could not be measured are marked UNMEASURED rather than inferred.

---

## 0 · The result in one paragraph

The defect is not distributed across the gates. It is concentrated at the top:
**cosmon's verification apparatus has almost no mechanical authority over what
reaches `main`.** 47 CI jobs run, 36 script-gates run inside them, ~40
conformance tests run — and today **zero** of them can block a merge, because
`required_status_checks` is not enabled on `noogram/cosmon@main`. The one gate
that does bind the merge path is `cs done`'s integrity cascade, which compiles
and nothing more, and which defaults to fail-open when it cannot verify. Below
that headline the individual instruments are in better shape than the month's
incident rate suggests: most are honestly scoped, several refuse rather than
pass when their prerequisite is absent, and the worst cases are documented
simplifications rather than hidden ones. The exceptions are counted in §2.

---

## 1 · The load-bearing finding: nothing mechanical gates `main`

### 1.1 Branch protection declares no required check

`scripts/apply-branch-protection.sh` is described in its own header as "the one
act that converts every CI radar into an exogenous GATE", and its
`REQUIRED_CHECKS` array names five job names (`Format`, `Clippy`, `Test`,
`Documentation`, `Artifact-map residence gate`). Measured against the live
repository:

```
$ gh api repos/noogram/cosmon --jq .private
false
$ gh api repos/noogram/cosmon/branches/main/protection/required_status_checks
{"message":"Required status checks not enabled", ... "status":"404"}
```

Protection *exists* — `required_signatures: true`, `enforce_admins: true`,
`allow_force_pushes: false`, one required approving review — so the object reads
present. It carries no status-check contexts at all. The script is runnable now
(the repo is public, which is the precondition its own preflight enforces), and
its payload has evidently never been applied: the live state also diverges from
the script on `require_code_owner_reviews` (live `false`, script `true`).

**Covers 0 of 47 CI jobs.** This is the audit's single most consequential
number, and it is the cheapest to fix — one script invocation.

### 1.2 Even when applied, the script would cover 5 of 47

The `REQUIRED_CHECKS` list names five checks. 47 jobs exist across the ten
workflow files. Thirteen ci.yml jobs run on every PR, can genuinely go red, and
are absent from the list — including two whose own inline comments call them
mandatory: `cosmon-without-neurion` (`ci.yml:312`) and
`restart-fidelity-without-neurion` ("do NOT merge to bypass it", `ci.yml:611`).
The script's own comment states the rule it then under-applies: *"A CI job not
in required_status_checks lets the operator merge red — it is theater."*

### 1.3 The merge path that is actually used runs no gate but the compiler

Work reaches `main` in this fleet through `cs done`, not through PRs, so CI is
post-hoc regardless of §1.1. What `cs done` enforces before merging:

- the publish / identity / confidentiality gates (in-process Rust, they do bind);
- the **integrity cascade** — `[gates].integrity_command`, else `cargo check`
  for a Rust change, else `[gates].build_command`
  (`crates/cosmon-harvest/src/transaction.rs:5594`);
- the `[hooks] pre_done` gate — **but only `if let Some(...)`**
  (`transaction.rs:2214`), and cosmon's own `.cosmon/config.toml` `[hooks]`
  block declares `post_merge` only.

So the merge is gated on **compilation**. Tests, clippy, fmt, doc and all 36
script-gates are advisory here: the `[gates]` block's own comment says those
commands are "injected into every worker prompt" — they are text the worker is
asked to run and self-report, not commands the harvest executes.

Further, `fail_closed_on_unverified` is `#[serde(default)]` on a `bool`
(`crates/cosmon-core/src/config.rs:1786`) and cosmon's config does not set it,
so the default is `false` — documented as "fail-open-loud" (ADR-158). When the
integrity cascade cannot verify a change, the merge proceeds with an advisory.

**Measured chain:** worker self-report → compile-only merge gate (fail-open on
unverified) → CI that cannot block. Each link is individually defensible and
documented; the composition is that no test result mechanically prevents
anything.

### 1.4 `just gates` covers 4 of 36 CI script-gates

CLAUDE.md presents `just gates` as "the whole contract" and then names two
additional CI gates (`spdx-headers.py`, `publish.sh --check`). Measured:

```
CI workflows invoke 36 distinct scripts/*.{sh,py}
just quick + just gates invoke 5 (one of which is the no-pilot-env wrapper)
overlap: 4  — check-no-session-ids.sh, publish.sh, spdx-headers.py,
              release/crossing.test.sh
```

Thirty-two script-gates that CI runs are invisible to a contributor who runs the
documented local contract — among them `sovereignty-gate.sh`,
`confidentiality-banlist.sh`, `check-docs-one-gate.sh`, `check-book-links.sh`,
`check-fixture-independence.sh`, `check-workflow-yaml.sh`,
`source-provenance.test.sh`. Given §1.1, those 32 currently bind nothing at
either end.

### 1.5 Twenty-two gate-shaped scripts are invoked by nothing at all

Of 122 scripts under `scripts/`, 81 are referenced by no workflow, no justfile
recipe and no lefthook stanza. Most of those are legitimately operator tools
(installers, Telegram routing, demos, curation). Filtering to names that claim a
gate role (`check-*`, `*-audit`, `forbid-*`, `*-lint`, `*.test.sh`, posture and
differential benches) leaves **22** that nothing runs:

```
architecture-audit.sh          check-verdict-provenance.py    coverage-ratchet.sh
assert-hits.sh                 completion-check.sh            cs-paste-nudge.test.sh
assert-hits.test.sh            confidentiality-lint.sh        curate-classify.test.sh
blind-eval.sh                  container-engine-posture.sh    forbid-matrix-features.sh
check-abandon-process-group.sh container-worker-doors-        forbid-matrix-send.sh
check-book-links.py              differential.sh              latex-audit.sh
check-galaxy-complete.sh       cosmon-tg-route.test.sh        latex-audit.test.sh
                                                              mutation-falsifier.test.sh
                                                              trace-sidecar.test.sh
```

Seven of these are `.test.sh` self-tests of gates — the meta-instrument that
proves a gate's red path still reddens. This is the same shape the justfile
itself documents for `hooks/`: "tracked for months with nothing installing it,
so every script in it was a gate nobody passed through."

### 1.6 The quarantine manifest is enforced by one local hook and nothing else

`docs/quarantined-commits.tsv` holds 14 entries. Grep over the whole tree
(excluding `target/` and `.git/`) finds exactly two references: `justfile` and
`hooks/pre-push`. No CI job reads it; no CI job verifies commit signatures
either. The manifest therefore binds only on a machine where `just install` ran
`install-hooks.sh`, and `git push --no-verify` or a fresh clone bypasses it
silently. The signature half of the same hook *is* backed server-side
(`required_signatures: true`, §1.1); the quarantine half has no server-side
counterpart.

---

## 2 · Formal and model-conformance instruments

| Instrument | Set derived from | Covers | Verdict |
|---|---|---|---|
| `tla-verify.yml` matrix | hand-written, 6 entries | 6 of 14 `docs/specs/*.cfg`; ~9 of ~26 named invariants | PARTIAL |
| `specs/tla/*.tla` (3 specs) | — no CI hook exists | 0 of 3 | **BLIND** |
| `spec_conformance.rs` | `Action::ALL`, a hand-written `const [Action; 13]` | 13 of 14 `Next` actions; 5 of ~9 invariants | PARTIAL |
| `typed_links_conformance.rs` | hand-written `Op` enum → `prop_oneof!` | 5 of 14 `MoleculeLink` variants | PARTIAL |
| `claude_model_coverage.rs` | synthetic `"model-a"` / `"model-b"` | 0 real model ids | **BLIND on its name** |
| `adapter_capability::adapter_provides` | argument discarded | 1 of 3 capability axes | **BLIND** |
| `run_state_ghosts.rs` | hand-written fixture table | 4 of 6 Rust `GhostKind`; 5 of 7 TLA+ values have a Rust counterpart | PARTIAL |
| `autonomy_attacks.rs` | hand-written, self-disclaiming | 4 of an unenumerable space | PARTIAL, declared |
| `event_v2_worker_spawn_roundtrip.rs` | scoped in its doc header | 5 of 5 stated scope | COVERS |

**`specs/tla/` is checked by nothing.** `tla-verify.yml`'s `paths:` filter
watches `docs/specs/*.tla` and `docs/specs/*.cfg` only
(`.github/workflows/tla-verify.yml:20-31`). `specs/tla/` is a different
directory holding `CosmonRuntime.tla`, `BoundedProgress.tla` and
`cs_pilot_interactive_fsm.tla` (ADR-115). No workflow references it. The only
evidence any of the three was ever model-checked is
`specs/tla/cs_pilot_interactive_fsm.check.log`, a manual TLC run.

**Eight of fourteen model configs never run.** The matrix hard-codes six, all
against `CosmonRun.tla`. Never invoked by CI: `CosmonRun_GovernanceGate.cfg`,
`CosmonRun_StepProgress.cfg`, `CosmonRunScheduler_{Normal,ConvoyCascade}.cfg`
(a whole second spec, checking the convoy-cascade regression its own comment
documents), `CosmonRunXGalaxy_{InBand,Adversarial}.cfg` (a third spec, I11–I15),
and `CosmonDocHarness{,_Safety}.cfg`. The `tlc-out-*.log` files sitting beside
them are stale local artifacts, which makes the directory *look* recently
checked.

**`I_StepProgress` has zero mechanical verification anywhere.** Two independent
gaps close over the same invariant — the one born from a molecule that sat
silent for four hours. `CosmonRun.tla` names `MarkStalled` 11 times, including
in `Next` with weak fairness. `crates/cosmon-core/src/spec.rs:126` declares
`pub const ALL: [Action; 13]` and the enum has no `MarkStalled` variant at all,
so the proptest refuter cannot generate it. And `CosmonRun_StepProgress.cfg`,
the config that would check the invariant in TLC, is one of the eight the CI
matrix omits. Neither instrument is individually alarming; their intersection
is that the invariant is asserted by nothing.

**`claude_model_coverage.rs` measures a different thing than its name.** Its two
tests use fabricated strings `"model-a"` / `"model-b"` and assert JSONL
turn-boundary deduplication (`claude_model_coverage.rs:14-47`). The file
contains no real model id and no enumeration of the catalog; a renamed or
retired model breaks nothing here. The parsing logic it does test is tested
correctly — the defect is the name, which invites a reader to check "model
coverage" off a list.

**`adapter_provides` discards its argument** — `let _ = capability;`
(`crates/cosmon-core/src/adapter_capability.rs:126`), returning
`!adapter_is_local(adapter_name)` for all three `WorkerCapability` values. This
is one of the five incidents that prompted the audit; it is still present and
is now documented as deliberate, the module doc naming the exact adapter class
that will break it (a sandboxed executor with a shell but no repository). The
21 assertions in its test (7 adapter names × 3 capabilities) cannot fail
differentially, because the function cannot tell the three capabilities apart.

---

## 3 · Security, isolation and egress instruments

The good news first, because it is the larger part. Four container posture
scripts route a missing docker daemon to `exit 2 / VERDICT INCONCLUSIVE` rather
than to success. `worker_env_hygiene.rs` tests an allow-list structurally — it
blocks a deliberately invented variable name (`COSMON_A_VARIABLE_INVENTED_
TOMORROW`), which is the shape that survives surface growth. The filesystem
sandbox is asserted per-tool and once across all five tools. The `MockBackend`
gap from issue #75 is genuinely closed: `crates/cosmon-transport/src/mock.rs:22`
now records `args` alongside `agent_id`, `command` and `cwd`, and
`library_executor_launch_argv.rs` asserts five launch clauses against the real
recorded argv.

The `no-pilot-env.sh` strip list deserves a correction to CLAUDE.md, in the
project's favour. CLAUDE.md says the list "is not maintained by hand" because
`cs tackle` emits every variable through `PilotVar::name()`. In fact the shell
script's `PILOT_VARS` is hand-copied — but `crates/cosmon-cli/tests/pilot_env_
boundary.rs:74` runs `no-pilot-env.sh --list` and diffs it against
`pilot_env::names()`, and `PilotVar::ALL` is pinned by an exhaustive
non-wildcard match asserting `len() == 11`. The property holds (11 of 11); the
mechanism described is not the mechanism present.

**The one BLIND finding: the only physical proof of egress denial never runs.**
Every other egress instrument proves *construction* — that `unshare --net` is
assembled with the right flags, or that a refusal/degrade decision is returned.
`egress_delegate.rs`'s five decision cells, `tackle.rs`'s three preflight cells,
and the four `autonomy_attacks.rs` attacks are all decidable at that layer, and
each says so. Exactly one test drives a live TCP probe through the real jail and
proves non-vacuity first by requiring the `AllowAll` baseline to reach the
network: `crates/cosmon-agent-harness/tests/exec_command_netns_e2e.rs`. It is
gated on `COSMON_NETNS_E2E=1` and `target_os = "linux"`, and:

```
$ grep -rn "COSMON_NETNS_E2E" .github/ justfile | wc -l
0
```

It is referenced only by its own file, the `cs-pilot-netns` Dockerfile and its
in-container script, and `scripts/cs-pilot-netns-egress-test.sh` — which is
itself one of the 22 scripts nothing invokes (§1.5). The chain "gates green →
egress physically denied" has a real, currently unexercised link. The guarantee
exists only when an operator runs that script by hand inside colima.

Two smaller notes. `scripts/cs-pilot-netns-egress-test.sh` and
`local-default-container-test.sh` `exit 0` when docker, the daemon, Ollama or
the model is absent — loudly logged as a yellow `SKIP`, so a human reading the
log is not deceived, but any automation reading only the exit code would record
a passing security test. And `exec_command_egress.rs`'s strict-mode phase is
`#[cfg(target_os = "macos")]`, so on Linux CI runners it is compiled out
entirely, with no skip line at all — unlike the netns file, which announces its
own abstention.

---

## 4 · Shell and Python check-scripts

Twenty-five scripts audited. Eleven are exhaustive over a surface derived from
git or from a compiled source and need no further comment:
`publish.sh --check` (2497 of 2497 tracked files, with a canary that reddens if
its own exclusion list drifts), `artifact-map-audit.py` (2497 of 2497),
`spdx-headers.py` (986 of 986 `.rs`), `license-network-boundary.py` (derived
from the `cargo metadata` graph, not a hand list), `check-book-links` (42 of 42
md), `check-workflow-yaml.sh` (10 of 10 workflows), `check-docs-one-gate.sh`
(whole `docs/book/src`), `confidentiality-banlist.sh`,
`release-version-conformance.sh` (4 canon binaries, cross-read by `release.yml`
and a Rust alignment test), `no-pilot-env.sh` (11 of 11, §3), and
`install-hooks.sh` (2 of 2).

`publish.sh` is worth naming as the model the rest should copy: it enumerates
from `git grep -- .` rather than from a list, and each exclusion carries a
canary that fails when the exclusion stops matching. That is an instrument
built so that surface growth reddens it instead of escaping it.

### 4.1 A CI gate that has never inspected a file

`sovereignty-gate.sh` runs in `ci.yml:64` under the check-run name "Sovereignty
gate (avatar-tenant-demo bundle)". Reproduced in this worktree:

```
$ git ls-files dist/avatar-tenant-demo/ | wc -l
0
$ ./scripts/sovereignty-gate.sh
sovereignty-gate: …/dist/avatar-tenant-demo absent — nothing ships,
                  gate holds vacuously
$ echo $?
0
```

The `build.sh` / `handoff.sh` sites its header describes do not exist anywhere
in the tree — `avatar-tenant-demo` appears only in the script, its allowlist,
its spec, two ADRs, an example TOML, the CHANGELOG and the CI wiring. The
script's own message is honest; the check-run name in the GitHub UI is not.
**Covers 0 of 0.** This is the purest instance of the class in the repository:
a green check for a bundle that was never wired up or has been retired, with
the gate left standing in CI.

### 4.2 Instruments guarding things that do not exist

- `forbid-matrix-features.sh` and `forbid-matrix-send.sh` target
  `crates/cosmon-matrix-tick`, absent from the workspace. Both print a clean
  pass for the missing crate; neither is referenced by CI or the justfile.
  Prospective scaffolding, currently inert.
- `assert-hits.sh` (the zero-hit injection primitive) is sourced by no script.
  Its own `.test.sh` proves the primitive works; as an installed control it
  guards **0 of 0** call-sites. Its three mentions outside itself are prose in
  two ADRs and a guide.
- `check-abandon-process-group.sh` is wired into nothing, and by default scans
  the operator's *installed* LaunchAgents rather than the repo's tracked
  templates. Pointed at the tracked templates it finds four real gaps —
  `letter-monday`, `session-route`, `session-to-spark`, `whisper-to-spark`, all
  `StartInterval=300`, none carrying `AbandonProcessGroup` — plus three
  `docker/launchd/*.plist` it cannot parse. The source-of-truth plists are
  checked by nothing.

### 4.3 Instruments narrower than their name suggests

- **`check-readme-quickstart.sh` covers 1 of 9 code blocks.** It extracts only
  the first ` ```bash ` block after `## A real session`
  (`check-readme-quickstart.sh:26`). README.md holds 9 bash blocks and 24
  distinct `cs <subcommand>` invocations; the extracted block carries 5 of
  them. The script's title says "Quickstart block", which is accurate — but
  "the README's CLI examples are drift-checked" is wrong for the other eight
  blocks.
- **`check-provenance.sh`'s ledger half has never bound once.** Under cosmon's
  `team` residence, `.cosmon/state/` is never tracked in any commit tree, so
  `git show <tip>:.cosmon/state/events.jsonl` has always resolved to nothing
  (`check-provenance.sh:57-84`). The script labels its verdicts "shape" rather
  than "ledger-verified" precisely because of this. Honest, and permanently
  inert — one of two advertised checks.
- **`coverage-ratchet.sh` covers 5 of 48 crates** and is wired into neither CI
  nor the justfile; it self-describes as a proposal at N=0.
  **`mutation-falsifier.sh` covers 1 of 48 crates**, schedule-only and
  `continue-on-error`. Neither misrepresents itself.
- **`confidentiality-lint.sh` is fail-open without an operator-local denylist**
  and is never run by CI — only `confidentiality-banlist.sh` is. CLAUDE.md
  already states this honestly; it is recorded here because the pair is easy to
  confuse by name.
- **`check-fixture-independence.sh` scans 295 files and excludes 548.** It looks
  at tracked `.rs` under `tests/` or matching `*fixture*`; inline
  `#[cfg(test)]` modules — where most of this workspace's unit tests live — are
  out of scope by design.

---

## 5 · CLI surface, parity and census tests

This is where the month's named incident lives, and it is the cluster in the
best shape — because the fix landed the right way. `api_cli_coverage.rs`
(commit `25bfcb87`) no longer folds a hand-written array of three; `live_routes`
now folds `crates/cosmon-rpp-adapter/data/surface_events.txt`, the same §8p
canon the router is built from, and `claims_shipped()` accepts `V0`/`V1`/`V2`/
`PARTIAL` rather than the `V0` written when V0 was the only cut. Seven tests
pass over a derived set. That is the shape every other instrument in this report
should be measured against.

Also derived, and therefore durable under surface growth: `help_goldens.rs`
(clap tree ↔ `--help` ↔ man page ↔ generated book pages ↔ book prose, via
`cs __help-tree` / `__man-page` / `__markdown-help`),
`injection_attribution_census.rs` (walks the whole `cosmon-cli/src` tree),
`shipped_binary_version_alignment.rs`, `formula_projection_parity.rs` (with an
explicit `checked > 0` vacuity guard), `operator_only_in_sync_with_adr.rs`
(parses ADR-080 §5.1 bidirectionally), and `coverage_exhaustive.rs`.

### 5.1 A gate that asserts a number against itself

`crates/cosmon-thin-cli/src/coverage.rs:198`:

```rust
let covered_exposable = covered.len();
let total_exposable = covered_exposable; // see field doc — drift is forbidden
```

and `crates/cosmon-thin-cli/tests/coverage_complete.rs:77`:

```rust
assert_eq!(covered, total, "covered_exposable == total_exposable");
```

The denominator is defined to be the numerator, so `ratio_covered == 1.0` and
`status == "COMPLETE"` cannot fail except in the vacuous zero-registry case.
Measured: `3 passed; 0 failed` over a property with no false branch.

What the module doc claims (`coverage.rs:17-20`) is a real and different
property — that every `#[verb]`-annotated function has a working dispatch arm
in `cosmon_thin_cli::cli::Command`. Nothing in the workspace checks it.
`Command` is a hand-written clap enum with hand-written match arms, independent
of the `linkme` registry that `covered_exposable` counts. A `#[verb]` function
registered but never wired into a `Command::X(args) => run_x(..)` arm — dead,
unreachable from the CLI — still reports `COMPLETE`. The nearest real
bijection in the workspace,
`cosmon-rpp-adapter/tests/api_surface_freeze.rs::routes_and_verbs_are_bijective`,
compares the verb registry to axum routes, which is a different pair than the
one this doc polices.

This is the same shape as the incident that prompted the audit: a green gate
whose two sides are one measurement wearing two names.

### 5.2 A lesson learned in one file and not propagated to its siblings

`coverage_exhaustive.rs:113` already diagnosed the silent-skip failure mode and
fixed it — its `require_cs()` hard-fails under `CI=1` when the `cs` binary is
absent, with a doc comment saying a missing-binary skip "is how the allowlist
drift this function guards was able to rot unnoticed."

Two files in the same directory still have the unguarded version.
`flag_parity.rs:129` and `parity_with_cs.rs:112` both `eprintln!` and `return`
with no `CI` check — four tests in the first, seven-plus in the second
(968 lines of byte-level `cs` ↔ `cs-thin` output parity) pass vacuously if `cs`
is not on the path. Today they do run for real, because `ci.yml:300` builds
`cs` before `cargo test --workspace`. The property is therefore held by the
continued presence and ordering of one build step in one workflow file, with no
fail-closed check inside the tests themselves — in a repository where, per §1.1,
that workflow blocks nothing anyway.

### 5.3 Hand-written sets that are correct today

`verdict_polarity_coherence.rs:196` pins `SPEAKERS: [&str; 3]` and
`public_attribution_domain.rs:15` pins eight public byline/maker file paths.
Both match reality as measured — 3 of the 52 `.cosmon/formulas/*.toml` files
co-mention `confirmed` and `CLEAN`, and the eight named files are the ones
carrying the slot. Neither regenerates its array, so neither would notice a
fourth formula speaking both vocabularies or a ninth public attribution slot.
Low severity: narrow regression pins over classes that grow rarely and under
review. Recorded because they are the same mechanism that produced the
`api_cli_coverage` incident, at a smaller scale.

---

## 6 · What the numbers say, and what to do

### 6.1 The ranking

| # | Finding | Measured | Cost to close |
|---|---|---|---|
| 1 | `required_status_checks` not enabled on `main` | 0 of 47 CI jobs can block a merge | one script run |
| 2 | `REQUIRED_CHECKS` would name 5 | 5 of 47, omitting two self-declared "mandatory" jobs | edit one array |
| 3 | `sovereignty-gate.sh` inspects an absent bundle | 0 of 0 files, green in CI | delete or re-point |
| 4 | `coverage_complete.rs` asserts a value against itself | ratio pinned to 1.0 by construction | implement the dispatch-arm check its doc claims |
| 5 | The only physical egress proof runs nowhere | `COSMON_NETNS_E2E` in 0 workflows | one CI job |
| 6 | `specs/tla/` outside every `paths:` filter | 0 of 3 specs checked | widen the filter |
| 7 | 8 of 14 TLC configs never run; `I_StepProgress` asserted by nothing | 6 of 14 configs, `MarkStalled` absent from `Action::ALL` (13) | extend matrix + enum |
| 8 | 22 gate-shaped scripts invoked by nothing | incl. 7 gate self-tests | wire or delete |
| 9 | `adapter_provides` discards its argument | 1 of 3 capability axes | documented; revisit on next adapter |
| 10 | `flag_parity` / `parity_with_cs` skip silently | 11+ tests, no `CI` guard | copy the sibling's `require_cs()` |
| 11 | `claude_model_coverage.rs` measures log parsing | 0 real model ids | rename, or make it cover models |
| 12 | `just gates` covers 4 of 36 CI script-gates | contributor's local contract is a quarter of one third | decide which are local |

### 6.2 The pattern underneath

Three distinct mechanisms produced these, and they want different remedies.

**The hand-written set beside a growing surface.** `Action::ALL`, the `Op` enum
in `typed_links_conformance`, the TLC matrix, `SPEAKERS`, the eight attribution
paths. Remedy: derive from the compiled source. `api_cli_coverage`'s fix and
`license-network-boundary.py` (which reads the `cargo metadata` graph) show it
is usually available. Where it is not, `publish.sh`'s canary pattern is the
fallback — pin the count and fail when the count moves.

**The instrument that outlived its surface.** `sovereignty-gate.sh`, the two
matrix-tick scripts, `assert-hits.sh`, `check-provenance.sh`'s ledger half.
These are not wrong; they are green over nothing. Remedy: a gate whose subject
is absent should exit non-zero or be deleted, never exit 0 with an explanatory
message that only a log reader sees. The container posture scripts already do
this correctly — `exit 2 / VERDICT INCONCLUSIVE` — and are the model.

**The enforcement gap between running and binding.** §1.1, §1.3, §5.2. An
instrument can be perfectly derived and still bind nothing. This is the
category with the largest measured gap and the cheapest fix, and it is the one
the incident narrative missed: the month's five instances were all read as
*instrument* defects, and the measurement says the dominant defect is
*wiring*.

### 6.3 One check worth adding

Every finding in §6.1 rows 3 and 8 would have been caught by a single gate that
does not exist: **an instrument that asserts every gate has a subject**. For
each CI job and each `check-*`/`*-gate` script, assert that the set it
enumerates is non-empty, and that every gate-shaped script under `scripts/` is
referenced by a workflow, the justfile or lefthook. That is a grep and a
directory walk. It is the same idea as `formula_projection_parity.rs`'s
`checked > 0` guard and `publish.sh`'s exclusion canaries, applied one level
up — to the gates themselves rather than inside them.

### 6.4 Scope of this audit

Measured: 47 CI jobs (all), 25 check-scripts (all assigned), 122 scripts
enumerated for wiring, 14 TLC configs, 10 formal/model-conformance instruments,
~19 security/isolation instruments, 17 CLI census/parity instruments out of 100
test files in the two CLI crates.

Not measured, and the honest residue: the full bodies of five large
`committee_*` / `*_reconcile_lint.rs` files (their module docs establish they
are behavioural proofs, not completeness claims, but an internal hand-written
allowlist in one of them was not ruled out); `crates/cosmon-harvest`'s trust and
authority surfaces beyond `ensure_trusted`; the credential/OIDC and notary
signature paths; `cosmon-rpp-adapter`'s tenant-isolation tests; and ~65
`cosmon-cli` test files whose names indicate scenario coverage rather than a
census role. Each is a candidate for the same integer treatment.
