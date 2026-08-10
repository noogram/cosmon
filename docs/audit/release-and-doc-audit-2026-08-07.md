# Release and documentation audit since v0.5.0

**Date:** 2026-08-07. **Molecule:** `task-20260807-942f`.
**Baseline:** `v0.5.0`, tagged 2026-07-31.
**Range measured:** `v0.5.0..HEAD` at the time of writing — 141 non-merge
commits, 333 files, +48 715 / −2 147 lines.

**This audit publishes nothing and tags nothing.** It is a decision file. The
release gesture belongs to the operator.

> **Amended 2026-08-10** (`task-20260810-75e9`), while executing §2.5. The
> body below is left as it was written; the three corrections are §5, which
> is appended at the end rather than woven in, so that what this audit
> concluded on 2026-08-07 stays readable as what it concluded then. In short:
> six further deliveries landed after the audit (including a rename that is
> itself part of the MINOR case), the range is now 229 commits / 150
> non-merge, and step 3 of §2.5 rested on a grep that matched two third-party
> crates. The **`v0.6.0` recommendation is unchanged and is now stronger.**

---

## 1 · What was delivered, by theme

Seven themes account for the 141 commits. Ordered by how much of the public
surface each one moves.

### 1.1 · Mission co-pilotage — a new top-level command, M1 → M8

The largest single delivery, and the only one that adds a **new public
surface**: `cs sessions`, with eleven verbs
(`discover`, `list`, `show`, `attach`, `peers`, `send`, `inbox`,
`checkpoint`, `drift`, `takeover`, `hook`). Nothing existing changed shape.

| Milestone | What landed |
|---|---|
| M1 | `session-probe-core` port + Claude and Codex adapters; content-addressed log references |
| M2 | reciprocal pilot presence and a traced mailbox |
| M3 | `PilotCheckpoint` publication + **tri-valued** drift comparison (`AGREE` / `FINDING` / `INCONCLUSIVE`) |
| M4 | the PRIMARY lease, its epoch, request/grant; guard wired onto the lifecycle gestures |
| M5 | the cockpit behind one plural verb |
| M6 | the hook that runs without being typed |
| M7 | dogfood shadow — the measured friction list ADR-173 rests on |
| M8 | the hand-over procedure, its refusal table, and its rollback |

The authority model is the load-bearing part. Since
[ADR-171](../adr/171-the-operator-gesture-is-a-signature-not-a-string.md) the
operator gesture is a **minisign signature**, not a string an agent can type:
`--sign-with` hands the signing to `minisign(1)`, cosmon owns no signer and
never sees the passphrase, and every ledger line is re-checked on read against
the key pinned at `.cosmon/takeover.pub`. Related:
[ADR-168](../adr/168-a-co-pilot-inherits-the-session-substrate-not-its-delivery-contract.md),
[ADR-172](../adr/172-done-authority-is-an-operator-sealed-capability.md).

### 1.2 · Fixes from external contributions

Every one of these was found by someone outside the fleet, against the
**signed `v0.5.0` binaries**, and every one is still unreleased.

| Issue / PR | Defect | Reporter |
|---|---|---|
| #33 | `cargo test` does not compile on linux-musl — the musl-only safety test calls two private functions, so the guard never ran on the target it guards | external tester |
| #35 | a `kill -9`'d worker keeps its seat; four defects in crash recovery | external tester |
| #36 | hardcoded `/usr/bin/true` breaks `cosmon-daemon-supervisor` on BusyBox; `signal_cascade`'s escalation contract silently untested on Alpine | external tester |
| #37 | six `cosmon-runtime` resident tests die on `Deadline` instead of naming the missing `python3` | external tester |
| #38 | a transport test pins the demote uid absolutely instead of relative to the tester | external tester |
| #43 | three `cs` unit tests read the runner's terminal; on a real TTY one **hangs forever** and one enters raw mode on the operator's own terminal | external tester |
| #39 / #41 / #42 | external rate-measurement data (400 trials, aarch64 Colima); the published README's pooled bounds and reproduction claim were wrong and were **retracted** | @jdthaler |
| #44 | OIDC login only worked against Forgejo; replaced by standard discovery with explicit issuer validation at both hops and both token paths | **@ph-lean** |
| #32 | the container walkthrough was not followable on `v0.5.0` — circular steps 4/5, and a missing `git init -b main` that made `cs done` refuse at the very end | external tester |
| #26 | an unsubmitted `cs done` in a worker composer; injections are now attributed at the transport | @jdthaler |

### 1.3 · The hermetic worker/gates boundary

`cs tackle` steers a worker with environment variables; `cargo test` inherited
them, so tests read instructions addressed to their parent. Three false
verdicts and one collapsed healthy molecule (`task-20260804-2bbb`, work intact
at `226b9b0d`). The pilot-variable list now lives once as
`cosmon_core::pilot_env::PilotVar`; `cs tackle` **emits** through it and
`scripts/no-pilot-env.sh` strips the same list, so it is the producer's list,
not a denylist someone must remember to update. Falsified both ways in
`tests/pilot_env_boundary.rs`, including the inverse: the boundary must never
appear on a runtime path.

### 1.4 · Performance, measured

- **`[profile.dev] opt-level = 1`**: 1010.4 s → 299.9 s wall on the real gate,
  7489 passed / 0 failed on both sides. Cost: cold test-world build 877 →
  2335 CPU-seconds. **No workflow touched** — CI adoption stays gated on a
  runner-side cold-build measurement.
- **Event-log scan cursor checkpoint**: `cs observe` 2.90–3.68 s → 0.01–0.03 s
  in a galaxy with a 151 MB log. The sidecar is a cache of a pure fold, never
  a source of truth.
- Cross-process tests stopped shelling out to `cargo`.

### 1.5 · The test-speed studies, including the refutation

Three write-ups under `docs/measurements/`: where the suite's wall clock
actually goes; the second-seat speed study and the two-seat comparison; and
the `cosmon-cli` rig consolidation — **built, then refuted by measurement**
rather than quietly dropped. This is the theme most at risk of being read as
"work that produced nothing"; the refutation is the result.

### 1.6 · CI infrastructure corrections

Four instruments that reported green while measuring nothing: a step name
containing a colon made a whole workflow unparsable; the external-PR
assert-guard interpolated the PR **body** as shell source (surfaced by #44,
where a body with backticked `client_id` executed); the provenance gate judged
GitHub's synthetic merge commit instead of the PR head; and the three
instruments behind the chronic nightly red. Local `RUSTFLAGS` now match CI's.

### 1.7 · Everything else

`cs spore install`; adapter-capability gating on formulas (exit 17,
noogram/cosmon #4 clause 2); `cs peek --phase harvestable`; the galaxy's
declared target repository (ADR-170); algorithmic provenance on every
conclusion (ADR-169); committee contract-hash verification against the body;
`cs purge` failing closed on unharvested work; nine ADRs, 165 → 174.

---

## 2 · Is a release necessary?

**Yes. Recommendation: `v0.6.0`.** Three criteria were instructed, not
presumed.

### 2.1 · Do external users await a fix? — Yes, nine of them

Nine defects reported by people outside the fleet are fixed on the trunk and
absent from every released artifact. Two are not cosmetic:

- **#43** makes `cargo test` **hang forever** on a live interactive terminal.
  Someone evaluating cosmon by cloning and running the suite in their own
  shell — the first thing an evaluator does — meets an unexplained freeze.
- **#33** means `cargo test` does not *compile* on linux-musl at all. The
  Alpine/musl path is exactly the container path Harbor-style deployments and
  the container walkthrough push people toward.

Both are met on first contact, by the population least equipped to diagnose
them, and both are fixed here. **Only 2 issues remain open** (#40, #45), and
neither is a v0.5.0 regression.

### 2.2 · Has the public surface changed? — Yes, `cs sessions` is new

Eleven new subcommands, plus `cs peek --phase harvestable`, `cs spore
install`, and a new refusal exit code (17). All additive.

### 2.3 · Is there a risk in *not* releasing? — Yes, and it is the sharp one

The container walkthrough (#32) was measurably not followable on `v0.5.0`, and
the docs that describe the fixed path are on the trunk. Anyone installing
`v0.5.0` today follows a guide that no longer matches the binary they have.
The gap widens with every day the fixes stay unreleased, and it costs the
external contributors who found these bugs a second round of the same
diagnosis.

### 2.4 · Why `v0.6.0` and not `v0.5.1`

Under semver a patch release carries **no new public surface**. `cs sessions`
is eleven new verbs. That alone forecloses `0.5.1`. Two further reasons:

- `cs run`'s JSON summary counter `briefless_parked` became
  `permanently_parked` — a rename in a machine-read contract.
- `cs reconcile` is now a deprecated alias printing a removal notice.

Pre-1.0, MINOR is where both additive surface and small contract breaks
belong. `v0.6.0` is the correct bump.

### 2.5 · What the release still costs

The gates that can be run from a bare clone are **green as of this audit**:
`cargo check` / `test` / `clippy` / `fmt` / `doc`,
`python3 scripts/spdx-headers.py --check`, and `scripts/publish.sh --check`
(all six rules PASS — *"the tracked tree would produce a clean public
projection"*).

Remaining, all mechanical:

1. Bump `Cargo.toml:140` to `0.6.0` (the workspace version every shipped
   binary prints — the CHANGELOG's pinned contract).
2. Regenerate both man pages (`crates/cosmon-cli/man/cs.1`,
   `crates/cosmon-remote/man/cosmon-remote.1`) — each embeds `0.5.0` twice
   and is generated, never hand-edited.
3. Refresh `supply-chain/config.toml` (two `0.5.0` rows).
4. Promote `## [Unreleased]` to `## [0.6.0] — <date>` in `CHANGELOG.md`.
5. `scripts/release-checklist.sh`, then a signed tag on the trunk (the
   ordinary path since the 2026-07-30 amendment — the scrubbed projection is
   the exceptional case, not the route).

`scripts/release-checklist.sh` and `scripts/confidentiality-lint.sh` report
PEND without `gitleaks` and the operator-private denylist; that is by
construction and is the operator's step, not this audit's.

---

## 3 · Documentation — verified, not sampled

### 3.1 · Corrected in this molecule

| Gap | State |
|---|---|
| `CHANGELOG.md` had no record of the largest deliveries since v0.5.0 — `[Unreleased]` covered six items out of seven themes, and none of `cs sessions`, the external-contribution series, the hermetic boundary, `opt-level=1`, or the CI corrections | **Fixed.** `[Unreleased]` now carries Added / Performance / Changed / Fixed / Documentation covering all seven. Every `docs/` link in the section was checked to resolve. |
| `docs/adr/INDEX.md` stale by 27 rows (including 165, 166, 167, 168, 170, 173, 174) | **Fixed** — regenerated. See §3.2, the defect was structural. |
| Every one of the index's 130 links was dead — written `docs/adr/<file>` from a file living *in* `docs/adr/`, resolving to `docs/adr/docs/adr/<file>` | **Fixed** in `render_adr_index`, with a unit regression test. |

### 3.2 · The generated-file-with-no-generator defect, instructed

The mission asked how the ADR index is regenerated, because the generator
existed (`cosmon-surface::render_adr_index`, surface `project.decisions` in
`.cosmon/surfaces.toml`) and no command appeared to reach it.

**Measured cause.** `cs reconcile`'s classification loop
(`crates/cosmon-cli/src/cmd/reconcile.rs::render_for_surface`) matched four
referents and returned `None` for everything else. `classify_all` `continue`s
on `None`, so the `project.decisions` surface never entered the plan — and
`project_surfaces`, which *does* implement directory rendering, is only ever
handed the surfaces in the plan. So the code path existed and was
**unreachable from any command**. Confirmed by running `cs reconcile` before
the fix: *"Projected 4 surfaces"*, `docs/adr/` absent.

A second, smaller defect sat behind it: the classifier joined the surface's
declared path directly, which for a directory surface is a directory, so
`read_to_string` failed and the current content always read as empty.

**Fix.** A `project.decisions` arm in `render_for_surface` (via a new public
`render_adr_index_content`), and a `surface_target` helper that resolves a
directory surface to the `INDEX.md` inside it — the same target
`project_surfaces` writes. `cs project` now reports *"Projected 5 surfaces"*.

**Is it wired into a gate? It was not; it is now.** A regenerating command
nobody runs is the same defect one layer up. The gate is a test,
`crates/cosmon-surface/tests/adr_index_freshness.rs`: it re-renders the index
from `docs/adr/` and compares bytes with what is committed. Decidable from a
bare clone — no fleet state, no network, no `cs` binary — which is what lets
it fail the build. Verified load-bearing: it fails on the pre-fix index with
the message naming `cs project`, and passes on the regenerated one.

### 3.3 · Verified up to date — no action needed

- **`cs help`.** `crates/cosmon-cli/src/root_help.rs` documents the lease
  verbs, `cs sessions takeover` in both its one-command and three-step forms,
  `--sign-with`, and the `.cosmon/takeover.pub` trust root.
- **`man cs`.** Carries the same lease section and lists `cs sessions` in
  `SUBCOMMANDS`. (It embeds the version string, so it is a release-step
  regeneration, not a doc gap.)
- **`docs/book`.** `reference/tools.md` carries `cs sessions` and all eleven
  subcommands including the four `checkpoint` verbs and `takeover` — because
  the book Reference is generated by `cs __markdown-help` from the clap tree
  and `crates/cosmon-cli/tests/help_goldens.rs` fails when it drifts. This is
  the pattern §3.2 was missing, already working elsewhere.
- **The container walkthrough** (#32). Re-read: the image creates
  `/srv/mission` at build time owned by uid 10001, `git init -q -b main` is in
  step 5 with *"-b main is load-bearing"* beside it, and the reason has its
  own section. Followable.
- **`THESIS.md:906` — *"No built-in UI dashboard."*** **Still true after
  ADR-173.** ADR-173's own scope line reads: *"This ADR changes no CLI
  surface, no flag, no output byte, and writes no UI."* Its decision is that a
  cockpit is a **command surface**, which is what the THESIS sentence already
  claims. No edit needed — and editing it would have been the wrong reading.

### 3.4 · Named as remaining work, not corrected here

**The CLI/UI parity audit that `CLAUDE.md` makes mandatory cannot be updated,
because it is not in this repository.** ADR-173 §2.1 measured this: `git log
--all -- '*ux-cli-parity-audit.md'` returns nothing — the file is absent from
the working tree *and* from every ref, having been relocated to the knowledge
galaxy on 2026-07-14. Six live references still point at the missing path,
three of them from `docs/architectural-invariants.md` itself, including the
Markdown link **inside §8l's own rule text** (line 1875). An invariant whose
instrument is a 404 is not a weak invariant; it is a sentence.

ADR-173 §2.2 decided the remedy (accepted 2026-08-06) and **none of it is
implemented**:

| Bead | Cost | Why not done here |
|---|---|---|
| Create `docs/guides/ux-cli-parity-registry.md` — one row per user-facing `cs` verb, a coverage cell per declared peer | ~1 day: ≈40 verbs to enumerate and classify against the worker-boundary rule | Requires deciding which verbs are user-facing vs worker-only. Arbitration. |
| Amend §8l — replace *"the native pilot apps (mac-pilot, ios-pilot)"* with *"every surface declared a parity peer in the registry"*, and repoint the link | ~2 h | Amending an architectural invariant is not a mechanical edit. |
| Half-gate A — assert every user-facing clap verb has a registry row | ~half a day | Depends on the registry existing and on the boundary decision above. |
| Half-gate B — peer-published coverage attestation, referenced never gated | unscoped | Needs a peer repository to publish it. |

**Consequence for this release:** `cs sessions`' eleven verbs are shipping
with no parity row, exactly the breach half-gate A exists to catch. This does
not block `v0.6.0` — 32 of 32 pre-existing verb rows were already red, so
coverage is nil rather than degraded — but it should be filed before the
registry is written, not after, or the eleven new rows get back-filled by
someone who was not there.

---

## 4 · Summary

| Question | Answer |
|---|---|
| What shipped? | Seven themes; `cs sessions` (11 verbs) is the only new public surface |
| Release needed? | **Yes** — nine external-reported fixes are unreleased, two met on first contact |
| Which version? | **`v0.6.0`** — new surface forecloses a patch; two small contract breaks fit MINOR pre-1.0 |
| Docs up to date? | `cs help`, `man cs`, `docs/book`, container walkthrough, THESIS: **yes**. CHANGELOG and ADR index: **were not, now fixed**. CLI/UI parity registry: **remaining work, ~1.5 days, arbitration required** |

---

## 5 · Amendment, 2026-08-10 — executing §2.5

Written by `task-20260810-75e9`, the molecule that carried out the mechanical
half of §2.5. Three things the audit could not have known, and one it got
wrong.

### 5.1 · The range grew, and the MINOR case grew with it

`v0.5.0..HEAD` is now **229 commits (150 non-merge), 397 files,
+52 947 / −2 688 lines** — up from the 141 / 333 / +48 715 measured on
2026-08-07. Six deliveries landed after this audit was committed
(`0bf1cfc0`):

| Commit | What landed |
|---|---|
| `c4f8e18e`, `4c2d0731` | **`cs session` → `cs journal`** ([ADR-175](../adr/175-the-operator-carnet-is-cs-journal.md)) — the carnet moves out of `cs sessions`' prefix; old verb survives hidden with a deprecation line; store `.cosmon/state/sessions/` → `.cosmon/state/journals/` |
| `8cb99fac` | patrol propels a stalled worker only on the provider's own typed `api_error`, never on inferred idleness |
| `fce0c60f` | `cs tackle` sites a child worktree at the galaxy, never inside a parent worktree (five nestings on 2026-08-08) |
| `15edce4c` | versioned cockpit surface canon |
| `3209619c` | mdBook search highlight dismisses itself (561 `<mark>` on one page) |
| `4ec284aa` | `cs sessions` help says what the surface lets you do, not which verbs it has |

The rename is not cosmetic bookkeeping: it is a **third** entry in §2.4's
list of small contract breaks that foreclose a patch release. `cs journal`
joins `permanently_parked` and the `cs reconcile` deprecation. All three fit
MINOR pre-1.0, so **§2's `v0.6.0` verdict stands and is now better
supported**, not weakened.

### 5.2 · §2.5 step 3 is withdrawn — no cosmon row in `supply-chain/config.toml`

The audit instructed *"Refresh `supply-chain/config.toml` (two `0.5.0`
rows)"*. Measured: `grep -n cosmon supply-chain/config.toml` returns
**nothing**. The two lines are `[[exemptions.dirs-sys]]` and
`[[exemptions.heck]]` — third-party crates that happen to be at their own
version 0.5.0. Editing either would have falsified a `cargo vet` exemption
and refused a crate whose audit is honest.

The corroborating evidence is the previous release commit: `971f75c5`
(*chore(release): v0.5.0*) touched six files and `supply-chain/config.toml`
is not among them. **Step 3 was a grep artefact and is not part of a release
cut.**

### 5.3 · §2.5 step 2 was one site short

Beyond the two man pages, the workspace version is also embedded in an insta
snapshot, `crates/cosmon-cli/tests/snapshots/cs_help_grouped_reference.snap`
(`cs 0.5.0 — Cosmon agent orchestrator`). `971f75c5` reblessed it, and a cut
that misses it fails the test gate rather than shipping wrong — but it
belongs on the list. Regenerated here with `INSTA_UPDATE=always`.

Two further `0.5.0` strings were examined and deliberately **not** touched:

- `crates/cosmon-rpp-adapter/openapi/v1.yaml` `info.version` — an API
  document version, `"0.5.0"` since the initial public commit when the
  workspace was at `0.1.0`. It does not track the workspace and never has.
- `crates/cosmon-cockpit/data/cockpit_views.*` `"introduced": "0.5.0"` — a
  historical fact about when a view appeared, not a current version.

`docs/book/src/getting-started/install.md`'s `ver=0.5.0` **was** bumped: it
is a worked example telling the reader to fetch a release that exists, and
after the tag it names the current one.

### 5.4 · What this molecule did not do

It did not tag. The signed tag is the operator's gesture and only theirs, per
the header of this file. It did not write the acknowledgements section for
the external contributors of §1.2 — that slot is a marked `TODO-ACK` comment
in the `[0.6.0]` heading block of `CHANGELOG.md`, for the operator to fill
before the tag. §3.4's parity registry remains untouched and remains the
named remaining work.
