# Cosmon — Architecture baseline (per-galaxy summary)

**Doctrine:** [ADR-082 — Architecture baseline](adr/082-architecture-baseline.md)
**Tier:** `substrate` (declared in `CLAUDE.md`)
**Audit:** `bash scripts/architecture-audit.sh --check`
**Encodings used:** per-INV test files (`tests/inv/`) **and** aggregate document
([`docs/architectural-invariants.md` §9](architectural-invariants.md))

This is the 1-page summary. The full doctrine is in
[ADR-082](adr/082-architecture-baseline.md). Cosmon's tier is `substrate`
because cosmon ships the doctrine to its dependents — it must obey the rules
it imposes (Gödel substrate-galaxy obligation, ADR-082 §Decision Drivers).

## How to verify in 5 minutes

```bash
git clone <cosmon-repo>
cd cosmon
bash scripts/architecture-audit.sh --check       # CI mode, exit 1 on FAIL
bash scripts/architecture-audit.sh --report /tmp/r.md && cat /tmp/r.md
```

The audit produces six rows of `{PASS | FAIL | SKIP | WARN}` with file:line on
FAIL. There is no score, no tier badge, no Bronze/Silver/Gold framing. Read the
rows. Read the failure messages. Form your own verdict.

## Named waivers (substrate-tier obligation: visible, dated, time-boxed)

Cosmon at `substrate` tier ships the doctrine knowing it has the following
known violations. Each waiver carries a remediation plan and target date. The
waivers are visible in version control because the panel demanded the
substrate galaxy be the same-PR witness (Gödel D6, Architect S4).

### W1 — `INV-DOMAIN-PURE-NO-IO` partial waiver (clock only)

**Audit signal.** Since `task-20260622-3144` the audit's purity grep is
complete — it now scans for filesystem, process, and network I/O **and** the
wall clock, not just the rng half it caught before (that blindness is what let
the violations below pass CI silently). With that complete measurement the
audit reports a single remaining class: **31 `Utc::now()` call sites** in
`crates/cosmon-core/src` (outside `#[cfg(test)]`), spread across ~18 files —
the hottest being `molecule.rs` and `event_v2.rs` (5 each), `creativity.rs`
(4), `rig.rs` (3). (The raw grep including test modules is ~106; the audit
excludes `#[cfg(test)]` blocks, so the production figure is 31.)

**What is *no longer* a violation.** The earlier waiver named two things that
are now gone:

- **Entropy.** `MoleculeId::generate()` already takes an injected
  `Rng` (`generate<R: Rng + ?Sized>(prefix, rng)`); there is no `thread_rng()`
  / `OsRng` / `rand::random` in `cosmon-core` production code. The stale
  "`rand::thread_rng()` at id.rs" signal in the previous version of this
  waiver was incorrect.
- **Filesystem & process spawns.** `cosmon-core` previously carried
  `std::fs::File::open` (the attestor-ledger `read_all`) and
  `std::process::Command` (`RealCommandRunner`, the Darwin `IoregSensor`).
  `task-20260622-3144` moved all three to adapter crates — `read_all` →
  `cosmon_state::attestor_log`, `RealCommandRunner` →
  `cosmon_transport::command_runner`, `IoregSensor` →
  `cosmon_transport::presence_sensor` — leaving only the ports (traits) in
  core. The fs/process/net portion of the gate is now **PASS-clean**.

**Why the clock is still a violation.** Functions like `dispatch()`,
`Molecule::evolve()`, and `MoleculeId::generate()` read ambient time
(`Utc::now()`) directly inside the domain crate. The Knuth rewrite of the INV
says this should be an injected input (a `Clock` trait threaded at the
boundary, the symmetric twin of the `Rng` already injected).

**Why we are shipping anyway.** Threading a `Clock` through every nucleate /
evolve / collapse / stamp call site is an estimated 30+ signatures and 100+
touch points — a meaningful refactor that warrants its own molecule and ADR
(testing strategy, transition plan, backward compatibility). Blocking on it
would stall unrelated work.

**Remediation plan.** A separate molecule
`task-cosmon-domain-purity-clock` will:
1. Introduce a `cosmon_core::clock::Clock` trait (port) — the twin of the
   already-injected `Rng`.
2. Thread it through `MoleculeId::generate()`, `dispatch()`, `evolve()`, and
   the other stamping sites (the 31 above).
3. Provide a `SystemClock` adapter impl in an adapter crate (`cosmon-state` or
   `cosmon-transport`); `RealClock` already exists in
   `cosmon_core::harness` and can move out with it.
4. Update call sites in `cosmon-cli` and tests.

**Target date.** End of Q2 2026. Tracked as a `temp:warm` molecule. If it has
not landed by the target date, escalate to `temp:hot`.

**Audit posture during waiver.** The `INV-DOMAIN-PURE-NO-IO` row remains FAIL
at substrate tier (the audit honestly reports the 31 clock sites). CI does not
gate on this row until remediation lands; the waiver is declared visibly here.
This is the named-waiver discipline: the violation is in the open, fully
measured, not buried — which is precisely the correction
`task-20260622-3144` made to both the gate and this count.

### W2 — `INV-NAMED-INVARIANT-HAS-TEST` future-invariants citations

**Audit signal:** the v1 audit may report `INV-CLEAN-CLONE-RUNS-GREEN` (and
the other v1.1 candidates) as cited in ADR-082's "Future invariants under
consideration" section without a corresponding test file.

**Why this is intentional.** The ADR's future-invariants appendix lists
candidates for v1.1 that are explicitly not active in v1. They are cited in
prose to make the omission visible per `consistency over completeness`
(Gödel D4).

**Why this is not a real violation.** Cosmon's `docs/architectural-invariants.md`
§9 carries stub aggregate-doc sections for each future-invariant candidate,
which the audit's parametric closure check accepts as a valid witness
(Forgemaster D6). The citation closes against the aggregate-doc section, not
against a test file. The audit reports PASS once those sections are in place
(this PR ships them).

**Remediation plan.** When a v1.1 invariant is promoted from candidate to
active, its aggregate-doc section is replaced by a real test file in
`tests/inv/`. No further action.

### W3 — `INV-PUBLIC-SURFACE-SCRUBBED` gate now active, FAILs on the live tree

**History.** Through v1 this row read SKIP — `scripts/publish.sh` did not exist,
and the W3 waiver said SKIP was the honest verdict for a galaxy with no public
surface to scrub. That was true *as a waiver*, but it also meant the audit
reported green while the private surface leaked (BLOCKER B3 premortem,
`task-20260616-c789`). The waiver has been retired: the inert gate is now
activated.

**Audit signal:** `scripts/publish.sh --check` exists and the row reports FAIL
on cosmon's live working tree. FAIL is the **correct and expected** verdict
here, not a regression. The premortem grep set (operator home paths, private
sibling-galaxy and crate names, client names, the operator's homeserver domain)
is densely present in cosmon's private development tree
by design — formulas, chronicles, ADRs, CLAUDE.md. The gate answers one
question: *"if this tree were published as-is, would private surface leak?"* For
the live private tree the answer is yes, so the gate says so out loud instead of
hiding it behind SKIP.

**Why a FAIL is not a baseline breach.** The publishable artefact is **not** the
live tree — it is the scrubbed clone produced by the genericisation chain under
`scripts/release/` (preflight → rename-clients → filter-repo message rewrite →
`cosmon-release-audit`). `publish.sh --check` passes on that scrubbed clone, and
only there. The remedy for the FAIL is never to scrub the live private tree; it
is to publish the clone, never the tree. The `.cosmon/config.toml`
`[git_remote_blocklist]` posture (CLAUDE.md "Git remote allowlist — structural
anti-leak invariant") remains the structural sibling at the `cs done` merge gate;
`publish.sh --check` is its fast, single-invocation kernel on the publish path.

**Vendoring.** `scripts/publish.sh` carries the same Contract-version-1 vendoring
header as `scripts/architecture-audit.sh`: a downstream galaxy preparing a public
release copies it verbatim, and the gate forces a real scrub before the first
publish (PASS once clean, FAIL while any pattern leaks). A galaxy with
intentional occurrences extends the path allowlist via `.publish-allowlist.txt`.

### W4 — `INV-PRIVATE-FILE-RM-CACHED-NOT-RM` historical commit `21cf165`

**Audit signal:** the v1 audit flags commit
`21cf165da2cd81793b726ee49e2c4321c80dbfc3` for removing `.worktrees`
entries while adding `.worktrees` to `.gitignore` in the same commit.

**Why this is a real violation, not a false-positive.** Earlier framing
called this a heuristic false-positive on ephemeral directories. That
framing was wrong. The commit (2026-04-10, *fix: remove accidentally tracked
.worktrees …*) is exactly the pattern the doctrine forbids: `git rm` of
tracked paths combined with `.gitignore` add in one commit. Months later,
the gridgame galaxy re-discovered the same trap (delib-20260503-81ac /
2f98), and that re-discovery is what motivated
`INV-PRIVATE-FILE-RM-CACHED-NOT-RM`. Ironic and instructive: the substrate
galaxy contains the historical violation that justified its own doctrine.

**Why we accept the waiver.** Rewriting history with `git filter-repo`
would invalidate every external commit-hash citation (chronicles, ADR
cross-refs, deliberation hashes, in-flight PRs). The cost of rewriting
exceeds the documentary benefit. The chronicle entry
*L'audit substrate a
accusé son propre fondateur* (2026-05-03) — preserves the trail; the
structured allowlist [`.architecture-waivers.toml`](../.architecture-waivers.toml)
is read by the audit and mutes the row from FAIL to WARN with the tag
`historical waiver`.

**Go-forward discipline.** Non-negotiable. Any post-cutoff (2026-05-03)
commit hitting the same heuristic is a hard FAIL with no implicit waiver.
Adding a new entry to `.architecture-waivers.toml` is an ADR-grade
decision (file a deliberation; do not patch silently).

## Audit posture summary (today)

| INV | Status | Note |
|-----|--------|------|
| INV-DOMAIN-PURE-NO-IO | FAIL → waiver W1 | refactor planned, target Q2 2026 |
| INV-PORT-ADAPTER-NAMING | PASS | Cargo dep graph: 18+ workspace crates depend on cosmon-core |
| INV-NAMED-INVARIANT-HAS-TEST | PASS | aggregate-doc encoding (§9) covers future candidates |
| INV-ADR-OPTIONS-CONSIDERED | WARN | 103 ADR(s) grandfathered (pre-2026-05-03); ADR-082 itself passes |
| INV-PUBLIC-SURFACE-SCRUBBED | FAIL → expected W3 | gate now active (`scripts/publish.sh --check`); FAIL on the live private tree is correct — publish the scrubbed `scripts/release/` clone, not the tree |
| INV-PRIVATE-FILE-RM-CACHED-NOT-RM | WARN → waiver W4 | historical commit `21cf165` (pre-baseline); allowlist in `.architecture-waivers.toml` |

## Re-running the audit

The audit is expected to run on every CI build of cosmon (when CI is set up).
For now, it is a manual gate:

```bash
bash scripts/architecture-audit.sh --check
echo "Exit: $?"   # 0 if no FAIL beyond declared waivers
```

When a new INV is added, a new ADR is introduced, or a structural change
modifies the audit's verdict, this file MUST be updated in the same PR
(ADR-082 §Tier-aware applicability, substrate obligation).

## Fleet config — `organization_type` (advisory, no code branches)

`fleet.toml` accepts an optional top-level `organization_type` field
since `task-20260509-8416` (parent: `delib-20260509-18df` §D-C, layer-C
deferral). The field is **purely advisory IFBDD instrumentation**:

- **Free-form `String`** — no Rust enum, no canonical list, no
  validation beyond "is a string".
- **Never matched on by code** — there is no `match organization_type`
  anywhere in the codebase, no template loading, no behaviour change.
- **Emits one event** — when `cs fleet resolve` loads a fleet that
  carries the field, an [`EventV2::FleetTyped`] is appended to
  `events.jsonl`. That trail is the only output.

```toml
# fleet.toml — advisory only
organization_type = "editorial-board"  # free-form, change at will

fleet = "my-fleet"
version = 1
# … rest of fleet config …
```

The field exists so that a future re-evaluation can read the event
log and answer empirically:

1. Do **≥3 fleets** converge on the same value with the same operational
   meaning (operator → operator citation in chronicles confirms semantic
   agreement)? OR
2. Do **N≥2 distinct human operators** exist with observable preference
   divergences (game theory becomes non-degenerate, von-neumann §5)? OR
3. Has a concrete user need named **exactly which code path** needs to
   branch on `organization_type` (then implement that one `match`,
   two arms — torvalds § evolution path step 3)?

Until one of those triggers fires, **no enum, no template loading, no
trait/impl pattern, no code branching**. Re-evaluation is a deliberate
operator decision — not a silent code-path drift. See
[ADR-082](adr/082-architecture-baseline.md) for the substrate-tier
obligation that gates how this field can later evolve.

[`EventV2::FleetTyped`]: ../crates/cosmon-core/src/event_v2.rs

## References

- [ADR-082 — Architecture baseline](adr/082-architecture-baseline.md)
- [`docs/architectural-invariants.md` §9](architectural-invariants.md) — aggregate-doc encoding
- [`scripts/architecture-audit.sh`](../scripts/architecture-audit.sh) — vendored bash audit
- [`tests/inv/`](../tests/inv) — six per-INV test files
- Parent deliberation: [`delib-20260503-81dd`](../.cosmon/state/archive/2026/05/delib-20260503-81dd/synthesis.md)
- Layer-C deferral on `organization_type`: [`delib-20260509-18df`](../.cosmon/state/molecules/delib-20260509-18df/synthesis.md) §D-C / [`task-20260509-8416`](../.cosmon/state/molecules/task-20260509-8416/)
