# ADR-082 — Architecture baseline

**Status:** Accepted
**Date:** 2026-05-03
**Parent deliberation:** `delib-20260503-81dd`
**Panel that converged on the verdicts:** forgemaster · tolnay · godin · karpathy · knuth · godel · architect-as-Blake-stand-in
**Substrate witness:** cosmon (governance_tier `substrate` — see §Tier-aware applicability)
**Companion artefacts:** `tests/inv/`, `scripts/architecture-audit.sh`, `docs/architecture-baseline.md`

## Context

Cosmon hosts a federation of galaxies (lumen, agora, workshop, knowledge, mailroom, …) each governed by its own `CLAUDE.md` and ad-hoc local hygiene. Gridgame — a non-cosmon-managed Rust workspace authored by Blake — exhibits an architectural-discipline package (per-INV test files in `tests/inv/`, two ADRs encoding `Options Considered` as deliberation records, `anti-corruption.js` enforcing publish-side hygiene, a 110-line `publish.sh`, a 138-line `test-self-invariants.sh`) that an external architect can verify mechanically in five minutes. The pilot's question on 2026-05-03: should this discipline be promoted from a one-galaxy artefact into a doctrine that the cosmon federation can offer to its galaxies — and what is the right shape so the doctrine teaches rather than punishes, allows tier-appropriate strictness, and (substrate-galaxy obligation) survives being applied to cosmon itself?

The seven-persona deep-think panel (`delib-20260503-81dd`) ran the question. Convergences: 12 named (C1-C12). Divergences: 6 named (D1-D6). Surprising insights: 9 named (S1-S9). Atomic verdicts (a)-(e) cleanly converged. This ADR ratifies the verdicts and constitutes the same-PR substrate witness Gödel demanded.

The doctrine **must pass its own `INV-ADR-OPTIONS-CONSIDERED` rule**: if it cannot, it is not allowed to ship (panel non-negotiable, S4). The Options Considered section below is therefore not a courtesy — it is the load-bearing test of the proposal.

## Options Considered

### Option 1 — Per-galaxy `CONSTITUTION.md` only, no cosmon-side ADR (rejected)
Encode the doctrine as a `CONSTITUTION.md` file at the root of every adopting galaxy. No cosmon-side canonical record.
- **Pros:** Discoverable from the galaxy root by any clone; self-contained; no central authority.
- **Cons:** "Constitution" carries decree-not-deliberation tone (panel rejected the word — Architect, Karpathy, Godin converge); no canonical version, every galaxy diverges silently; no shared audit script; no record of the deliberation that produced the rules.
- **Why rejected:** Panel verdict (d) is **NO** to `CONSTITUTION.md`. Tone is wrong, divergence is unbounded, deliberation is invisible.

### Option 2 — Cosmon-side ADR only, extend existing `docs/architectural-invariants.md` (rejected)
Restate the six INVs as additional sections inside cosmon's existing `docs/architectural-invariants.md` aggregate document. Do not introduce per-galaxy artefacts.
- **Pros:** Reuses existing surface; zero new files; cosmon already lints this document.
- **Cons:** Doctrine is invisible to galaxies cloned independently of cosmon; no portable audit substrate; conflates cosmon-lab aggregate (which exists for cosmon-the-codebase, not cosmon-the-doctrine) with the federation-wide rule set.
- **Why rejected:** Forgemaster's *parametric audit* point (D6) shows the two encodings can coexist, but the federation-wide doctrine needs a portable, citable artefact (the ADR); extending the existing aggregate would silently bury the doctrine.

### Option 3 — Flat 3-INV minimum doctrine (rejected)
Ship only the three syntactically-checkable INVs (`PORT-ADAPTER-NAMING`, `DOMAIN-PURE-NO-IO`, `PUBLIC-SURFACE-SCRUBBED`); drop the cultural and meta INVs.
- **Pros:** Smaller contract; faster to write; less audit-drift surface.
- **Cons:** Drops `INV-ADR-OPTIONS-CONSIDERED` (Godin's load-bearing INV — the one that changes cultural posture from decree to record); drops `INV-NAMED-INVARIANT-HAS-TEST` (the closure rule that prevents folklore). Without these, the audit becomes a directory-layout linter — Potemkin-immune (S5) but trust-shallow.
- **Why rejected:** Three INVs is a check, not a doctrine. The cultural/meta INVs are what the federation actually needs.

### Option 4 — Unbounded style guide as Markdown prose (rejected)
A style-guide document with N rules, no test files, no audit script, no PASS/FAIL matrix.
- **Pros:** Cheap to author; expressive; no contract pressure.
- **Cons:** No mechanical verification; "doctrine without enforcement is a moral lecture" (Godin); rules drift; the audience is internal collaborators, not external architects who form a 5-minute verdict.
- **Why rejected:** Audit-as-curriculum requires mechanical checks. Prose-only invariants are a known failure mode (forgemaster's predicted failure #2).

### Option 5 — `cs audit-galaxy` Rust subcommand from day one (rejected)
Promote the audit to a first-class `cs` subcommand. Implement in Rust inside `cosmon-cli`.
- **Pros:** Strong typing; discoverable via `cs --help`; integrates with cosmon's UX.
- **Cons:** Over-coupling on day one (Tolnay D3: bash V0 first); ties the doctrine's contract version to cosmon's release cadence; cosmon's cs binary is not available in galaxies that do not have cosmon installed (which is the federation-wide assumption); promoting a 250-line bash one-shot into a multi-crate Rust subcommand inflates the contract surface 10× and violates subtractive-design.
- **Why rejected:** Promote only if the script outgrows ~500 lines or develops state (Tolnay's discipline). Day one is bash.

### Option 6 — Ship without same-PR cosmon remediation (rejected)
Land the doctrine; retrofit cosmon (build `tests/inv/`, etc.) in a follow-up PR.
- **Pros:** Smaller PR; faster to ship.
- **Cons:** Substrate-galaxy obligation (Gödel D6, Architect S4): cosmon cannot have a waiver because cosmon *is* the rule. Shipping the doctrine without cosmon's own conformance is the exact pathology Hilbert feared and Gödel exhibited (S4 + S6 — cosmon scores 40-60/100 today). Doctrine incoherence on cosmon itself = forgemaster's predicted failure #5.
- **Why rejected:** Non-negotiable per panel.

### Option 7 (chosen) — Cosmon-side canonical ADR + per-galaxy `architecture-baseline.md` + vendored bash audit + tier-aware applicability + same-PR cosmon witness
The shape this ADR ratifies. Verdicts (a)-(e) of `delib-20260503-81dd`.

**Decision Outcome:** Option 7. Options 1, 2, 3, 4, 5, 6 are explicitly rejected by name above; their rejection rationales are recorded in this section verbatim per `INV-ADR-OPTIONS-CONSIDERED`.

## Decision Drivers

- **D-DECREE-VS-RECORD** — Move from "every galaxy's hygiene is invented locally" to "any qualified outsider can mechanically verify any participating galaxy's hygiene in 5 minutes." The cultural shift from decree to record is the load-bearing change (Godin).
- **D-CONSISTENCY-OVER-COMPLETENESS** — Pick a narrow, stable contract over a comprehensive but unstable one. Aim for two cosmon-years without contract bump (Tolnay, Gödel).
- **D-AUDIT-AS-CURRICULUM** — Failure messages are the artefact. Every audit FAIL is a 60-second architecture lesson with file:line + why-the-rule + how-to-fix + ADR link (Karpathy).
- **D-TIER-AWARE** — Galaxies declare their applicability tier; the audit projects the appropriate strictness. No blanket waivers; the tier ladder encodes the obligations (Gödel, Knuth, Forgemaster).
- **D-SUBSTRATE-WITNESS** — Cosmon must obey the doctrine it ships, in the same PR (Gödel, Architect).
- **D-SOFT-FOR-MOST-HARD-FOR-IRREVERSIBLE** — Soft signal for the verdict; hard for the meta-rule (`INV-NAMED-INVARIANT-HAS-TEST`); hard for the irreversible (`INV-PUBLIC-SURFACE-SCRUBBED` gates publish.sh) (Gödel).

## Decision

Adopt **Option 7**: a cosmon-side canonical ADR (this document), per-galaxy `docs/architecture-baseline.md` (1-page summary with named exemptions), a vendored bash audit script (`scripts/architecture-audit.sh`), six named architecture invariants with Knuth-rewritten semantics, tier-aware applicability declared in each galaxy's `CLAUDE.md`, and same-PR cosmon remediation (this PR ships `tests/inv/`, `scripts/architecture-audit.sh`, `docs/architecture-baseline.md`, and a `governance_tier='substrate'` declaration in cosmon's `CLAUDE.md`).

## The six invariants

The naming convention is `INV-<DOMAIN>-<ASSERTION>`. Each invariant is paired with a witness — a mechanical check that produces PASS, FAIL, or SKIP (with a rationale).

### INV-DOMAIN-PURE-NO-IO

**Assertion.** Domain crates (those housing the project's core types and rules) declare no I/O dependency, perform no ambient time/entropy reads, and declare their allocation tier explicitly.

**Three sub-checks.**
1. `cargo tree --no-default-features` over each domain crate excludes a published I/O-provider blocklist (`tokio`, `reqwest`, `hyper`, `async-std`, `smol`, `mio`, `notify`, `rusqlite`, …). Galaxies extend the blocklist via `.architecture.toml` if needed.
2. Source contains no call to `Instant::now`, `SystemTime::now`, `std::time::SystemTime::now`, `thread_rng`, `rand::random`, `OsRng::default`. These must be input parameters (clock, RNG injected at the boundary).
3. The crate declares its allocation tier in `Cargo.toml` `[package.metadata.architecture]` as one of `no_alloc | alloc_only | std`. Default is `std` (declared explicitly).

**Why this knob (and not `no_std`).** The panel established (S1) that gridgame's own `gridgame-core` is *not* `#![no_std]` — it uses `std::collections::BTreeSet`. The label `no_std` is cargo-cult relative to gridgame's own implementation; the right knob is "no I/O, no clock, no unseeded RNG." This is the rewrite forgemaster, knuth, and gödel all converged on (verdict T4).

**Witness.** `tests/inv/inv-domain-pure-no-io.sh` runs the three sub-checks against every crate marked `domain` in the workspace. WASM build is a *consequence* available to galaxies with a WASM target, not a requirement.

### INV-PORT-ADAPTER-NAMING

**Assertion.** Every port is a Rust trait whose API mentions only domain types. Every adapter is a struct implementing exactly one port-trait. Every port has at least one alternative implementation (mock counts).

**Two-implementation witness.** A port with one impl is an indirection, not a port (Knuth). The witness scans for traits in `*-port*.rs` or workspace-declared port crates, then checks each has ≥2 implementations across the workspace (real adapter + mock test double, or two real adapters).

**Layout is recommendation, not rule.** Multi-crate workspaces satisfy this via the Cargo dep graph (the dependency edges *are* the port/adapter boundary — forgemaster). Single-crate projects use `lib/ports/` + `lib/adapters/` as the recommended layout. Cosmon's existing `cosmon-core` (domain) + `cosmon-filestore` / `cosmon-transport` / `cosmon-mcp` (adapters) shape satisfies the rule by Cargo dep graph.

**Witness.** `tests/inv/inv-port-adapter-naming.sh` walks the workspace, identifies port traits (by naming convention `*Port` or trait-on-domain-types signature), counts implementors, FAILs if any port has fewer than 2 distinct implementors.

### INV-NAMED-INVARIANT-HAS-TEST

**Assertion.** Every `INV-<NAME>` cited in `docs/` (in any ADR, invariant doc, baseline) has a corresponding test under `tests/inv/inv-<lowercase-dashed-name>.sh` (per-INV encoding) or is named in `docs/architectural-invariants.md` (aggregate encoding — cosmon's choice). Both encodings are auditable mechanically (Forgemaster D6).

**Soft warning on the converse.** A test file `tests/inv/inv-foo-bar.sh` whose name is not cited anywhere in `docs/` is reported as a soft warning ("candidate for retirement"), not a FAIL (Gödel D1: graceful retirement of INVs without same-commit cleanup pressure).

**This is the meta-rule.** Closure between the doctrine surface (the Markdown citation) and the witness layer (the test file). It is **non-negotiable** wherever an `INV-` name is cited (Gödel's hard layer #1).

**Witness.** `tests/inv/inv-named-invariant-has-test.sh` greps `docs/` for `INV-[A-Z][A-Z0-9_-]+`, dedupes, then for each citation looks up the corresponding test file or aggregate-document section. This file is itself the closure of the rule it states.

### INV-ADR-OPTIONS-CONSIDERED

**Assertion.** Every ADR (`docs/adr/NNN-*.md`) contains:
1. A `Status:` field (Proposed/Accepted/Superseded/Deprecated).
2. An `## Options Considered` section with at least two named non-trivial alternatives, each with a Pros/Cons body.
3. A `## Decision` (or `## Decision Outcome`) section that **cites at least one rejected option by name**.
4. A `## Consequences` section with at least one item explicitly framed as a risk, downside, or negative trade-off.

**Why this is the load-bearing INV.** Of the six, this is the only one that **changes the cultural posture from decree to record** (Godin S8). The others are directory layouts and CI gates. This one changes how the team writes — and therefore, six months later, how the team thinks. The Blake feedback was not about courtesy to outsiders — it was the signal that private discipline had become invisible to the future reader, the only reader who matters.

**This ADR is its own first witness.** The §Options Considered above enumerates seven alternatives, six rejected by name, with Pros/Cons and rejection rationale. Decision Outcome cites all six rejected options by name. This is the recursive self-test demanded by the panel (S4, T1).

**Grandfather clause.** Older ADRs (pre-2026-05-03) that lack Options Considered are *exempt* from FAIL. The audit reports them as `WARN` with note "predates ADR-082, retrofit on next substantive edit." Going forward, `Status: Accepted` requires conformance.

**Witness.** `tests/inv/inv-adr-options-considered.sh` parses every ADR after the cutoff date for the four required structural elements; emits FAIL with file:line on first missing element.

### INV-PUBLIC-SURFACE-SCRUBBED

**Assertion.** No private path, internal hostname, gitignored content, or operator credential reaches a public surface (a published Rust crate, an open-sourced repo, a public website artefact).

**Audit row is `SKIP — delegate to publish.sh --check`.** The publish pipeline is the source of truth. The audit script does not reimplement leak-guard logic — different blast radius, different deps, subtractive design (Tolnay C6, Knuth, Forgemaster converge). The proof obligation is **history scan**, not working-tree scan: the gate must live CI-side on the publish path (Knuth — `git rm` from working tree without scrubbing history is a leak).

**Hard-for-irreversible.** Publish is irreversible (a published crate cannot be unpublished without rotation cost). This is Gödel's hard layer #2: when the audit's SKIP delegates to `publish.sh --check`, `publish.sh --check` is a hard gate (exit 1 blocks publish) on the receiving end. Pre-push hooks can be `--no-verify`'d; the publish-side check cannot.

**For galaxies without a publish pipeline.** Cosmon today has no `scripts/publish.sh`. The audit emits `SKIP — no publish pipeline; INV not enforceable here. File a temp:warm bead if a publish pipeline is added.` This is correct behavior: the INV applies if-and-only-if the galaxy has a public surface to scrub.

**Witness.** `tests/inv/inv-public-surface-scrubbed.sh` shells out to `scripts/publish.sh --check` if it exists; emits SKIP with rationale if it does not.

### INV-PRIVATE-FILE-RM-CACHED-NOT-RM

**Assertion.** Privatising a file (moving from tracked to gitignored) preserves the file in the worktree. Equivalently: after privatisation, the file is `untracked` in `git status` — not absent.

**Why the rename was rejected.** The task brief uses the original name `INV-PRIVATE-FILE-RM-CACHED-NOT-RM`. The panel (Knuth, Karpathy, T3) recommended renaming to `INV-PRIVATISATION-PRESERVES-FILE` to encode intent rather than git command syntax. The brief is authoritative on naming for v1; the rename is recorded in §Future invariants under consideration as a v1.1 candidate.

**Why this is in the doctrine and the procedure is in the runbook.** The INV catches data loss (the worktree state property). The procedural "use `git rm --cached`" instructions live in `docs/runbook/privatisation.md` because procedures belong in runbooks (Knuth, Karpathy converge — C1, T3).

**Witness.** `tests/inv/inv-private-file-rm-cached-not-rm.sh` cross-references commits that introduce a path into `.gitignore` against `git log -- <path>`: if the same commit also runs `git rm <path>` (full rm, not `--cached`), FAIL. If `git rm --cached <path>` was used, PASS. If no privatisation event found in the recent history window, SKIP.

## Tier-aware applicability

Each galaxy declares its `architecture_tier` (also recorded as `governance_tier` for backward compatibility with the deliberation vocabulary; see Note below) at the top of its `CLAUDE.md`. The audit script reads this declaration and projects tier-appropriate strictness.

| Tier | Audit posture | Substrate-galaxy obligation | Use case |
|------|--------------|---------------------------|----------|
| `exploration` | Advisory only — all FAILs report as INFO; no exit code | Not applicable | New galaxies, prototypes, pre-shape codebases |
| `stable` | Bronze — `INV-NAMED-INVARIANT-HAS-TEST`, `INV-ADR-OPTIONS-CONSIDERED`, `INV-PRIVATE-FILE-RM-CACHED-NOT-RM` are hard; rest are soft | Same-PR remediation only on the three hard INVs | Galaxies with declared discipline, single-author rate |
| `production` | Gold — all six INVs hard, with named-waiver mechanism per INV | Same-PR remediation on every INV unless waiver is named in `docs/architecture-baseline.md` | Galaxies with multi-contributor or external-consumer surface |
| `substrate` | Cosmon's tier — same as production, with the additional obligation that every named waiver in `architecture-baseline.md` carries a remediation plan and target date | Cosmon obeys its own doctrine in the same PR that introduces or modifies the doctrine; waivers are visible, dated, and time-boxed | Cosmon (and any future galaxy that ships the architectural baseline to its dependents) |

**Exemption-by-default for `exploration`.** A galaxy that is still finding its shape should not have the architectural-baseline projected onto empty space (forgemaster's predicted failure #5). Exemption defaults to `exploration` if the tier is unset.

**Note on `governance_tier` vs `architecture_tier`.** Cosmon's existing ADR-009 introduces a different `governance_tier` ladder (`Full | Light | GuardedMain | Micro | AppendOnly`) for *merge governance* (review/CI/merge-queue). The two ladders are orthogonal — a galaxy can be `Full` merge-governance and `exploration` architecture-tier (e.g., a new multi-contributor experiment). To avoid the field-name collision, this ADR introduces `architecture_tier` as the canonical name; `governance_tier` is accepted as a synonym during a 60-day grace period for the deliberation vocabulary, after which the audit deprecates the synonym (warns once, then errors at v2.0). Galaxies SHOULD declare both: `governance_tier` (ADR-009, merge) and `architecture_tier` (ADR-082, baseline).

## Consequences

### Positive
- **Mechanical verifiability.** Any qualified outsider clones a galaxy and runs `bash scripts/architecture-audit.sh --check` to form a 5-minute verdict. The audit is the artefact (Godin C5).
- **Cultural posture shifts from decree to record.** `INV-ADR-OPTIONS-CONSIDERED` makes deliberation visible. Six months from now, future-pilot reads the rejected options and understands why a decision held under pressure (Godin S8).
- **Audit-as-curriculum.** Failure messages teach the architecture; CI failure becomes a 60-second architecture lesson (Karpathy S7).
- **Tier-aware applicability respects the federation.** A pure-math galaxy (sandbox) cannot have Hexagonal projected onto it; a prototype gallery cannot be held to substrate standards. The doctrine adapts (Gödel C9).
- **Substrate-galaxy obligation honoured.** Cosmon obeys the doctrine it ships in the same PR. No Hilbertian split between authority and witness.

### Negative / Risks
- **Ceremony tax.** Six INVs is a non-trivial ongoing surface. Even with grandfather clauses, ADR authors will spend additional minutes on Options Considered. The bet: this minute compounds to hours saved by future-pilots reading the record (Godin's load-bearing claim — falsifiable by absence of citation in retrospectives over the next 6 months).
- **Audit-drift risk.** As the federation grows, individual galaxies will demand new INVs. The contract is `Contract version: 1`; bumps are coordinated migrations. If we bump faster than once per year, the doctrine has lost its narrow lean (Tolnay D4 — falsifiable).
- **Goodhart on syntactic checks.** A galaxy can game `INV-PORT-ADAPTER-NAMING` with shell ports (Gödel S5: the `noogram/potemkin` thought experiment scores 100/100). The audit is a *signal*, not a *proof*. The two-implementation witness raises the gaming cost but does not eliminate it. **A green audit is a signal, not a proof. Read this row in §Epistemic posture before drawing structural conclusions from any audit-report.md.**
- **Field-name collision risk** between `governance_tier` (ADR-009) and `architecture_tier` (this ADR). Mitigated by 60-day synonym window.
- **`CONSTITUTION.md` muscle memory.** The panel rejected the word, but galaxies opening with `vim CONSTITUTION.md` is an attractor; the per-galaxy `architecture-baseline.md` filename + README link is the mitigation. Falsifiable: count `CONSTITUTION.md` files created in the federation over the next 6 months; if >0, the rename was insufficient.
- **`exploration` tier as audit-skip-loophole.** Galaxies could declare `exploration` to escape ceremony. Mitigated by: (a) the tier is declared in CLAUDE.md (visible in version control), (b) `production` is the tier expected of any galaxy with external consumers, (c) Forgemaster's "do not project structure onto empty space" is the invited use, not a loophole. Audit can flag prolonged residency in `exploration` as a soft warning in v1.1.

## Future invariants under consideration

The panel (D4, T9) named six gaps that any external architect would expect, but which were deliberately omitted from v1 to honour Gödel's *consistency over completeness* lean. These are candidates for the first non-breaking addition once v1 has been running on lumen + agora + cosmon for one cosmon-month:

- **INV-CLEAN-CLONE-RUNS-GREEN** (Architect S2, D2, T9). The integration test of all the other invariants and the single highest-value signal an outsider has. Lumen likely passes; agora likely doesn't. **First candidate for v1.1 addition.**
- **INV-DEP-VERSION-PINNING** (Knuth). `Cargo.lock` checked in for binary crates; reproducible build property.
- **INV-COMMIT-PR-CONVENTIONS** (Knuth). Conventional Commits enforced; PR size cap (cosmon's 400-line cap as the canonical example).
- **INV-LINT-FORMAT-PREREQUISITE** (Knuth). The audit refuses to run on a repo that does not pass `cargo clippy` and `cargo fmt`. **Folded as prerequisite to the audit script in v1**, not a separate INV; promotion to a named INV is a v1.1 candidate.
- **INV-PER-CRATE-LICENSE** (Knuth, Architect S2). Every published crate declares a license; LICENSE file at root matches the declaration.
- **INV-SELF-DECLARED-CONFORMANCE** (Knuth). Galaxy declares its `architecture_tier`. **This is partially shipped in v1 (the tier declaration is required for non-exploration audit posture); promotion to a named INV with mechanical check is a v1.1 candidate.**
- **INV-PRIVATISATION-PRESERVES-FILE** (Knuth, Karpathy rename, T3). Rename of `INV-PRIVATE-FILE-RM-CACHED-NOT-RM` to encode intent. **v1.1 rename candidate.**

The decision on which (if any) to land as v1.1 will be taken at the end of week 4 of the rollout (per delib verdict (e)), not before.

## Named waivers

Substrate-tier discipline says: *every violation visible, dated, and either
remediated or explicitly waived.* The structured allowlist that mutes specific
audit FAILs into WARNs lives at the galaxy root in
[`.architecture-waivers.toml`](../../.architecture-waivers.toml). The audit
script reads it and tags matching rows as `historical waiver`.

Historical waivers — commits or facts predating the doctrine — are preserved
because rewriting history (`git filter-repo`) costs more than the documentary
benefit. Go-forward discipline is non-negotiable; the chronicle entry that
accompanies each waiver is the trail.

### INV-PRIVATE-FILE-RM-CACHED-NOT-RM — historical commit `21cf165`

**Violation.** Commit
`21cf165da2cd81793b726ee49e2c4321c80dbfc3` (2026-04-10, *fix: remove
accidentally tracked .worktrees, fix cockpit probe type mismatch*) combined
`git rm` of three `.worktrees/*` entries with adding `.worktrees` to
`.gitignore` in the same commit. This is exactly the pattern the doctrine
forbids — the pattern gridgame later re-discovered (delib-20260503-81ac /
2f98) and which motivated the doctrine's existence.

**Status.** Waiver granted, pre-baseline (commit predates ADR-082's
2026-05-03 cutoff). Go-forward discipline applies in full: any
post-cutoff commit hitting the same heuristic is a hard FAIL with no
implicit waiver.

**Why a waiver and not a rewrite.** `git filter-repo` would re-write every
commit hash from 2026-04-10 onward, breaking every external citation
(chronicles, ADR cross-refs, deliberation hashes, in-flight pull requests).
The internal chronicle entry
*L'audit substrate a accusé son propre fondateur* preserves the trail; the
audit reports WARN; the doctrine is intact.

**Self-referential note.** The audit test that detects this pattern was
written *because* of the gridgame re-discovery. Running it on cosmon's own
history surfaced the very commit that, months earlier, had inspired the rule.
The substrate galaxy does not exempt itself from its own doctrine — it
records the historical violation as a waiver and applies the discipline
go-forward. (Chronicle 2026-05-03.)

### INV-PRIVATE-FILE-RM-CACHED-NOT-RM — historical commit `a99a3cd`

**Violation.** Commit
`a99a3cd52e691eb76c6847650cd46decc5bad3de` (2026-04-04, *fix: remove
auto-generated runtime files from git and update .gitignore (hq-hmi01)*)
combined `git rm` of `.runtime/agent.lock`, `.runtime/session_id`,
`.beads/redirect` with adding `.runtime/` and `.beads/redirect` to
`.gitignore` in the same commit. Discovered by the same audit pass that
surfaced `21cf165`.

**Status.** Waiver granted, pre-baseline (predates 2026-05-03 cutoff).
Same rationale as `21cf165`: rewriting history is more costly than the
documentary benefit. Go-forward discipline applies in full.

## Same-PR substrate witness

This PR ships the same-PR cosmon remediation that the substrate-galaxy obligation requires (Gödel D6, T2):

1. **`docs/adr/082-architecture-baseline.md`** — this document.
2. **`tests/inv/`** — six test scripts (one per INV), one of which (`inv-named-invariant-has-test.sh`) is the closure meta-test that asserts every cited `INV-<NAME>` has a corresponding test.
3. **`scripts/architecture-audit.sh`** — vendored bash audit script (~250 lines, two flags, six rows, file:line on FAIL, `Contract version: 1` header).
4. **`docs/architecture-baseline.md`** — cosmon's per-galaxy 1-page summary (created in this PR; lists exemptions and waivers).
5. **`CLAUDE.md`** — declares `architecture_tier='substrate'` and (during the synonym window) `governance_tier='substrate'`.

Cosmon's existing `docs/architectural-invariants.md` continues to play its established role as the cosmon-lab aggregate document. The new ADR is the canonical authoritative statement of the six INVs; the existing aggregate document is the cosmon-flavoured encoding (Forgemaster D6 — both encodings are valid, the audit is parametric).

## Epistemic posture

A green audit is a **signal, not a proof** (Gödel S5). The Potemkin galaxy (`noogram/potemkin`) — empty `ports/` + `adapters/` directories, vestigial core crate, 8000 lines of unstructured I/O in a non-core crate — scores 100/100. The audit cannot detect whether the structure carries weight. Conversely, the Sage galaxy (`noogram/sage`, a 200-line Clojure backup script well-factored for scope) scores 0/100; the audit cannot detect appropriate scope-economy.

Therefore: every audit-report.md is read alongside a one-paragraph external-architect note, not in isolation. Doctrine verdicts are *probabilistic*, not deterministic — the formal posture is **soft signal** with two narrow hard exceptions (`INV-NAMED-INVARIANT-HAS-TEST` non-negotiable wherever cited; `INV-PUBLIC-SURFACE-SCRUBBED` gates publish.sh via the existing `git_remote_blocklist` posture).

## Rollout sequence (informational)

This ADR is Week 1 of a four-week rollout (delib verdict (e)). Subsequent weeks are nucleated as separate molecules:

- **Week 1 (this PR)** — Cosmon doctrine + audit script + cosmon-self remediation.
- **Week 2** — Lumen vendoring + the two declared-and-broken fixes (LICENSE file, `.github/workflows/`).
- **Week 2-3** — Agora vendoring + the canonical README wedge (separate trivial PR).
- **Week 4** — Chronicle entry; observe; decide on v1.1 first non-breaking addition (likely `INV-CLEAN-CLONE-RUNS-GREEN`).

The third galaxy decision is deferred to end of week 4. The doctrine's narrow lean is a feature, not a bug (Tolnay).

## References

- Parent: `delib-20260503-81dd/synthesis.md` — verdicts (a)-(e).
- Companion (cosmon-lab): [`docs/architectural-invariants.md`](../architectural-invariants.md) — aggregate document, cosmon-flavoured encoding.
- Companion (per-galaxy): [`docs/architecture-baseline.md`](../architecture-baseline.md) — cosmon's 1-page summary with named waivers.
- Tooling: [`scripts/architecture-audit.sh`](../../scripts/architecture-audit.sh) — vendored bash audit, Contract version 1.
- Tier governance: [ADR-009](009-governance-tiers.md) (`governance_tier` / merge governance — orthogonal axis).
- Substrate-galaxy obligation precedent: [ADR-049](049-cosmon-ward-feedback-flow.md), CLAUDE.md "le réacteur apprend de ce qu'il brûle".
- Surface-scrubbing precedent: cosmon `.cosmon/config.toml` `[git_remote_blocklist]`, CLAUDE.md "Git remote allowlist — structural anti-leak invariant".
- Exemplar: gridgame (Blake's repository) — `tests/inv/`, `publish.sh`, `anti-corruption.js`, ADR-0023, ADR-0027.
