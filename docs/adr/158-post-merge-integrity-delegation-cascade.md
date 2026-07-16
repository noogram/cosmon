# ADR-158 — Post-merge integrity gate: delegate the WHAT, own the WHEN

**Molecule:** task-20260714-2b34 (increment-2), hardened in task-20260715-ff5b
(increment-3: 5 in-scope inc-2 defects, incl. the critical delegated-shell
egress hole and the D1 deviation). · **Parent deliberation:**
`delib-20260714-7605` (topology/policy) building on `delib-20260714-559a`
(inc-1 scoping) and the operator-ratified D1
(`delib-20260714-559a`). **Status:** Accepted (2026-07-15). **Decision owner:**
Noogram.

Supersedes the Rust/Cargo-hardcoded shape of the `cs done` post-merge gate
(RR-SAFE-1) and extends the same fix to the `cs validate` tier-2 gate. Reuses
ADR-156's defect-fails-closed / legibility-stays-advisory split (§131-133) and
ADR-016's regime model. Governed by Principle 1 (Transport ≠ Cognition).

---

## Context — a fact about *another* project baked into cosmon's binary

`cs done` runs a fast integration gate after a branch merges but before
`merged_at` makes the landing observable to the resident runtime: if the merged
tree does not compile, main is reset to its pre-merge revision and the DONE is
refused, so the scheduler never advances dependents past a broken main.

The gate worked — but it *authored* the verdict bit with knowledge of cosmon's
own layout. It hardcoded `crates/cosmon-core` as a special fan-out root and
`crates/` as the member directory. Those are **propositions about the
orchestrated project compiled into cosmon's binary** — false in any repository
laid out differently, and meaningless in a Python or Go galaxy. feynman's
executable test (delib-559a): *a project-specific fact baked into cosmon's
compiled binary is cognition (a leak); a fact supplied by the project and
executed is transport (fine).* `run_bounded → exit-code → one bit` is
transport; the two path literals were cognition.

einstein's reframe names *why* this matters: the integrity gate **is the DAG
edge in disguise** — a 1-bit channel where cosmon carries the bit and external
cognition computes its value, exactly cosmon's universal control-plane pattern.
The gate violated that pattern by authoring the bit itself.

Increment-1 (PR-A/PR-B, molecules `…-aa2e` / `…-e0a6`) already de-hardcoded the
cargo path via live `cargo metadata`, split the overloaded `Skipped` into
`{ Verified, NothingToVerify, Unverified }`, and made the `Unverified` outcome a
durable `ok:unverified` witness in `events.jsonl` (verified and unchecked merges
are no longer byte-identical in the log). **This ADR is increment-2:** it lifts
the ceiling for *any* galaxy shape by letting the galaxy declare the WHAT.

## Decision

Cosmon owns the **WHEN** (merge → gate → reset-on-red → only-then stamp
`merged_at` — untouched); the galaxy declares the **WHAT** through a resolution
cascade over the existing `[gates]` config. No trait, no gate-kind enum on the
wire, no new config section — a different verification command is a *value*, not
a *strategy* (tolnay). The fix is net-negative architecture: a foreign body
removed, not a mechanism added.

### 1. The post-merge cascade (rung order = declaration ▸ inference ▸ fallback)

`run_post_merge_gate` resolves exactly one command to run on the **combined**
(already-merged) tree, from the repo root, under one shared wall-clock deadline
(`POST_MERGE_GATE_TIMEOUT`, 5 min — a hang-guard, not a test budget):

1. **`[gates].integrity_command`** — explicit declaration, run verbatim. Exit 0
   → `Verified`; non-zero → a gate *error* → the merge is rolled back. A
   declared verifier saying "broken" is exactly as load-bearing as `cargo check`
   saying "broken".
2. **cargo-metadata auto-detect** — the zero-config inference (the reverse-dep
   diff-scoper), run only where cosmon legitimately understands the toolchain.
   A repo cargo cannot resolve from the root *declines* (falls through); it is
   not a verdict.
3. **`[gates].build_command`** — the polyglot declaration fallback. `build_command`
   is the worker's cheap pre-completion self-check; `integrity_command` is the
   authoritative merge gate; they *coincide* for cosmon but a galaxy may want
   them to differ, so the fallback is composition, never a rename.
4. **loud `Unverified{expected:false}`** — nobody declared how to verify this
   tree. The residual hole named without euphemism: cosmon cannot manufacture a
   net, only be honest about its absence.

A *declared* command (rungs 1 & 3) runs **unconditionally** — cosmon does not
pre-filter "is this diff build-relevant?" for it, because that judgment needs
language knowledge cosmon must not bake in. Only the cargo rung short-circuits a
documentation-only diff to `NothingToVerify` (cosmon understands cargo
relevance; it does not understand Python's) — **and only when no `build_command`
fallback is declared.** When a fallback *is* declared, the cargo rung's "no Rust
changed" short-circuit is suppressed: it is Cargo cognition and must not gate the
polyglot path, so a non-Rust change declines to rung 3 and the declared command
runs (inc-3 Defect 3). Likewise, cargo that cannot be *spawned/resolved* (absent
from `PATH`, spawn error) declines to rung 3 — a mere absence of cargo is
auto-detect declining, never a hard error that rolls the merge back (inc-3
Defect 4).

**Trusted-config source, combined-tree execution (inc-3 Defect 2).** The command
*text* is resolved from the **pre-merge, trusted** `.cosmon/config.toml` (loaded
before the merge, `done.rs`), and its *execution* happens on the **combined
(already-merged) tree**. This split is deliberate and safer than reading the
command from the combined tree: reading `[gates].integrity_command` from the
untrusted merged tree would let a merged branch *inject* the command to run,
compounding the delegated-shell hole (Defect 1). So the WHAT is authored by the
trusted operator config; only the WHEN/where (the merged tree) comes from the
branch. The residual — a *trusted* command whose *script* the branch modified —
is closed by the egress jail (§4).

Precedence is *derived from the invariant* (einstein): a declaration outranks an
inference, an inference outranks a blind check. The `pre-merge-commit` git-hook
rung and `Unverified` provenance/adequacy auditing are deferred seams
(delib-7605 §6).

### 2. Fail-closed policy (D1) — the ratified expected-gate discriminator

Every `Unverified` outcome carries an `expected: bool` — kahneman's expected-gate
discriminator: `true` when a gate *was* expected (a cargo workspace resolved, or
a command was declared) but a code diff still went unchecked (the dangerous,
defect-shaped case); `false` when nothing was declared.

**Ratified rule (delib-20260714-559a, operator's choice):**

```
fail_closed = expected || fail_closed_on_unverified
```

- `Unverified { expected: true }` — a gate was expected but a code diff went
  unchecked — fails **CLOSED by default** (reset + `error:` witness + non-zero
  exit), protecting cosmon-on-cosmon's own net with no opt-in required.
- `Unverified { expected: false }` — nobody declared how to verify this tree —
  stays fail-**open**-loud (the merge lands, witnessed durably as `ok:unverified`,
  exit 0) *unless* the operator opt-in `[gates].fail_closed_on_unverified = true`
  promotes it to a rollback.

> **Correction (inc-3 Defect 5).** Increment-2 shipped `fail_closed =
> fail_closed_on_unverified` — the flag alone, `expected` ignored for policy —
> and this ADR previously *documented that deviation as if it were the decision*.
> That contradicted the operator-ratified discriminator. The deviation is
> removed: the ratified `expected || flag` rule is now implemented and is the
> canonical statement above. `expected` is no longer merely legibility — it is
> load-bearing for the default fail-closed of the defect-shaped case. The inc-1
> binding acceptance test that pinned the confirmed-workspace-unmapped case to
> *land* fail-open is superseded by this ratification (that case is `expected:
> true`, so it now rolls back by default).

### 3. Tier-2 symmetry (`cs validate`)

`cs validate` carried the identical leak (hardcoded cargo stages). It now builds
its stage list from the `[gates]` commands (`setup`/`typecheck`/`test`/`lint`/
`format`), falling back to the cargo defaults only when a slot is unset. The
mutation falsifier stays a cosmon-specific extra, gated on its wrapper script's
presence so a non-cosmon galaxy skips it. cosmon's own config declares those
slots as cargo commands, so cosmon-on-cosmon behaviour is unchanged. Its
repo-supplied stages run under the **same egress jail** as the `cs done`
delegated commands (§4).

### 4. Security — B5 trust gate *and* egress jail on repo-supplied shell

`integrity_command` / `build_command` / the `cs validate` gate commands come
from the repo's own `.cosmon/config.toml` — repo-supplied shell, so they are
refused in an untrusted clone (B5, RCE-by-clone; `cs trust` is the gate). Unlike
the advisory `post_merge` hook (which runs after the irreversible teardown and
merely warns), these run *before* `merged_at`, so a trust refusal is a safe,
reversible `Unverified{expected:true}` the caller can roll back — never a silent
exec of untrusted shell. The cargo rung is cosmon's own binary, not repo-supplied,
so it needs no trust grant (cosmon rides it unchanged).

**Egress jail — the delegated command runs under the same sandbox as an ordinary
agent subprocess (inc-3 Defect 1).** Trust hashes the *config* + formula TOMLs,
**not the scripts** a trusted `integrity_command` invokes (`trust.rs`). Increment-2
therefore had a critical hole: the delegated command executed as a plain `sh -c`
with only `current_dir` set — no `EgressJail`, no preflight, no runtime probe —
so a merged branch could modify a *trusted* `integrity_command` script
(`./ci/integrity.sh`) and execute arbitrary code from the combined tree with full
host filesystem + network access (credential exfiltration). Ordinary agent
subprocesses do **not** have this hole (`exec_command` wraps them). Increment-3
routes the delegated command (and the `cs validate` stages) through the *same*
discipline: policy resolved from `COSMON_EGRESS_POLICY`, the C1-F3 netns runtime
probe, and the pre-spawn `EgressJail::preflight`. On a host that cannot
kernel-enforce a required `deny-external` policy for an **exposed multi-tenant**
dispatch (`COSMON_EGRESS_EXPOSED` / the RPP `COSMON_API_REQUEST` marker, or the
operator's `COSMON_EGRESS_REQUIRE_NETNS`), the gate **refuses fail-closed** — a
gate error → the merge rolls back — rather than run the shell unconfined
(mirroring the `cs tackle` preflight refusal, `egress.rs`). With
`COSMON_EGRESS_POLICY` unset (the trusted single-operator default) the wrapped
command is byte-identical to the pre-fix `sh -c`, so cosmon-on-cosmon is
unchanged. The shared helper is `cmd::egress_delegate`.

## Consequences

- **Ceiling lifted** for every repo that declares HOW to verify itself — any
  Rust layout, Python, Go, JS. The special-case Rust subsystem dissolves into an
  instance of cosmon's carry-the-bit control-plane pattern.
- **Residual, un-closeable and named:** a polyglot repo with no declared command
  and no resolvable cargo workspace merges *unverified*. No reframe can close
  this — integrity requires someone to define "broken"; here no one did. The
  win is precise and only that: a **silent false-clean** becomes a **loud honest
  `ok:unverified`**.
- **Adequacy is not policed:** cosmon guarantees the declared check *ran green*,
  never that it is *adequate* (`test_command="true"` passes over a broken tree).
  Judging adequacy would re-import cognition; the honest mitigation is provenance
  (the witness records which command authored the verdict), not policing.
- **Unconfined-merged-tree-shell residual — now mitigated (inc-3 Defect 1).**
  The delegated command executes on the combined tree, so its *script* body is
  merged-branch content even when the *command text* is trusted config. Before
  inc-3 this ran unconfined; it now runs under the egress jail (§4), and an
  exposed-multi-tenant dispatch that cannot enforce the jail is refused
  fail-closed. The residual that remains is the ceiling of the jail itself —
  advisory (unenforced) egress on a single-operator non-Linux host (the honest
  interim until native macOS enforcement, ADR-155) — not an unconfined shell.
- **Merge-before-dispatch survives structurally** — it is enforced by
  write-ordering (`frontier.json` written only after the gate passes), a property
  of the *ordering*, not the *checker*. Swapping the WHAT changes what verdict the
  bit carries, not when it flips.
- **cosmon unchanged:** its `[gates]` leaves `integrity_command` unset, so `cs
  done` rides the cargo rung exactly as before; `fail_closed_on_unverified`
  defaults false; every inc-1 acceptance test stays green.

## Falsifiers

- `integrity_command_green_lands_clean_ok` / `integrity_command_red_rolls_back_and_fails`
  (e2e) — rung 1 is used and its exit code is the verdict.
- **`default_config_expected_true_unverified_fails_closed`** (e2e, inc-3
  Defect 5) — with default config, an `expected:true` Unverified on a code diff
  rolls back. Reverting `fail_closed = expected || flag` to the flag alone
  reddens it (the merge would land `ok:unverified`).
- **`fail_closed_flag_promotes_expected_false_unverified_to_rollback`** (e2e) —
  the FLAG isolated on `expected:false`; dropping the flag disjunct reddens it.
- **`post_merge_gate_unverified_expected_false_lands_with_ok_unverified_witness`**
  (e2e) — the `expected:false` default stays fail-open-loud.
- **`build_command_runs_for_non_rust_change_in_polyglot_repo`** (e2e, inc-3
  Defect 3) + `cargo_autodetect_declines_for_non_rust_change_when_build_command_declared`
  (unit) — a declared `build_command` runs for a non-Rust change; reverting the
  `has_fallback` fall-through reddens them.
- **`cargo_absent_falls_through_to_build_command`** (e2e, inc-3 Defect 4) —
  cargo absent from `PATH` falls through to `build_command`; reverting
  `cargo_metadata_bounded`'s spawn-error → `Ok(None)` reddens it (rollback).
- **`deny_external_exposed_unenforceable_is_refused`** and the sibling
  `cmd::egress_delegate` unit tests (host-independent, inc-3 Defect 1) +
  `delegated_command_egress_refused_on_unenforceable_exposed_host` (e2e, guarded)
  — the delegated shell is refused fail-closed rather than run unconfined.
- `empty_gates_fall_back_to_cargo_and_need_no_trust` /
  `declared_gates_delegate_and_are_trust_gated` (validate unit) — tier-2 symmetry.
