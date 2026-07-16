# ADR-133 — One-repo flip: the artifact-map membrane supersedes the deny-by-default allowlist

**Status:** Accepted (doctrine) — **the flip is GATED, not authorised** (see §6).
**Date:** 2026-06-24
**Accepted:** 2026-06-24 (ratification deliberation
`delib-20260624-8d8f`,
7-seat adversarial panel). The **membrane doctrine** — one repo, a residence
audit over `git ls-files`, deny-by-default polarity — is ratified by the whole
panel and is Accepted. The **release sequence is amended**: the panel caught a
load-bearing flaw — *the membrane audits the **tree**, but a public flip ships
**history***, and two seats reproduced confidential content reachable only in
history. The flip is therefore **blocked behind a hard pre-flip history-purge
gate** (§6, amendment B1) plus the accompanying pre-flip blockers B2–B7 from the
synthesis. Accepting the doctrine does **not** authorise the flip.
**Decider:** Noogram
**Authoring molecule:** `task-20260623-feeb` (🔧 task)
**Accepting molecule:** `task-20260624-6355` (📐 decision)
**Operator vision source:** operator decision 2026-06-24 — "cosmon must be ONE
public repo (43 crates, all public-audience) that flips public, NOT a maintained
public/private pair."

**Supersedes:**
- ADR-127 — the deny-by-default *allowlist*
  membrane (`.cosmon/release-allowlist.toml` + `bless-allowlist.sh`). Its
  polarity-inversion insight is retained (default-refuse beats default-ship);
  its *mechanism* (a per-path allowlist over a confidential live tree projected
  to a scrubbed public tree) is retired with the projection model itself.

**Builds on:**
- [ADR-132](132-kernel-plugin-catalog-ecosystem.md) — cosmon is a kernel; the 28
  formerly-private crates are an installable plugin catalog living in their home
  galaxies / future plugin repos, not in cosmon. This is *why* cosmon can be one
  public repo: the private crates that forced the public/private split are gone.
- [ADR-126](126-crate-frontier-two-gates.md) / ADR-113
  — the crate frontier that the catalog extraction resolved.

**Adapts (the source topology):**
- oxymake **ADR-017** (`/srv/cosmon/oxymake/docs/adr/017-artifact-residence-topology.md`)
  — "Topology B": one repo, one artifact map, one audit proving residence over a
  single `git ls-files`. cosmon ports the membrane (`.cosmon/artifact-map.toml`
  + `scripts/artifact-map-audit.py`) and the command-backed release projection
  (oxymake délib-20260529-13a7, Q-REL-4/5 — janis's *"the corpse is the exit
  code"* discipline).

**Retains / re-frames:**
- [ADR-057](057-genre-and-artifact-map.md) — the *genre* axis survives; its
  **five-audience** scheme (`public/team/author+agent/partner:<name>/solo`)
  collapses to **two** (`public`, `solo`). The orphan-`narration` residence for
  `author+agent`/`partner` is gone (no second tree to host it).
- [ADR-055](055-cosmon-residence.md) — residence; `.cosmon/state/` stays solo.
- [ADR-128](128-d7-attribution-vacuum-and-publish-gate.md) — the D7
  confidential-content publish gate already in `cs done` is **unchanged** and is
  the runtime twin of the checklist's pre-flip forbid-strings gate.

---

## 1. The problem in one sentence

The old release model was a **projection**: a confidential live tree
(`/srv/cosmon/cosmon`) scrubbed by a chain (`cosmon-release-resync.sh`: purge
history → rename clients → scrub messages → audit) into a clean public tree,
gated by a deny-by-default allowlist (ADR-127). A projection means **two
versions to keep in sync forever** — and a sync invariant no one enforces is a
silent-leak generator. The operator's decision retires the projection: cosmon
becomes **one repo** whose *live tree is the public tree*.

## 2. Why the allowlist model goes with the projection

ADR-127's allowlist was the right answer to the *projection's* question — *"which
of the confidential tree's paths are cleared to copy into the public
projection?"* Once there is no projection, that question dissolves:

- The allowlist (`.cosmon/release-allowlist.toml`, ~15 000 lines bootstrapped)
  enumerated the *entire* shipping frontier with per-path clearance. It is an
  operator artifact that itself **can never ship** (it leaks the shape of the
  private/public frontier — ADR-127 §8). In a one-repo world it is pure
  overhead: every public path would need a permit, and the permit file is
  bigger than the tree it guards.
- The deny-by-default *polarity* (new file → no permit → RED → forces a human to
  look) is preserved — but at the **residence** layer, not the path layer: a new
  *confidential* file classifies `solo` (or unmapped) and goes RED by
  construction, with no list to maintain.

## 3. The decision

**Adopt oxymake's Topology B.** One repository, one artifact map, one exogenous
audit, proving residence over a single `git ls-files`. The membrane is:

1. **`.cosmon/artifact-map.toml`** — classifies every tracked path by
   `genre → audience → residence`. Two audiences: `public` (ships) and `solo`
   (regenerable/local-only, never tracked). Longest-fixed-prefix-match wins; a
   `code` catch-all `**/*` is declared **last** so every path classifies
   (totality I1). This is byte-compatible with the existing
   `cosmon-core/src/artifact_map.rs` parser (`public`/`solo` are pre-existing
   `Audience` variants) — `cs inspect` keeps working.
2. **`scripts/artifact-map-audit.py`** — the **exogenous** gate (not the `cs`
   binary): plain stdlib Python, no skip-env, no continue-on-error. RED if any
   tracked path is unmapped (I1) **or** classifies non-public (residence). Wired
   into CI as `.github/workflows/artifact-map.yml` (job name *"Artifact-map
   residence gate"*). A gate that *is* the audited binary auditing its own
   release is the self-referee pathology cosmon already named (ADR-127 §6); the
   Python walker sidesteps it and runs from a fresh clone with nothing but
   Python 3.11+.
3. **`scripts/release-checklist.sh`** — the command-backed projection of every
   pre-flip referee, in two bins only: `[GATE]` (a non-zero command blocks the
   flip) and `[ADVISORY]` (reported, never blocks — honest demotion). The single
   source of truth is the **script's exit code**, never a human tick-list. The
   fixed sequence is **clean → flip → protect → enforce** — where, per the
   ratification amendment (§6, B1), **`clean` means tree-clean AND
   history-clean**. A history-purge GATE precedes the flip; a green tree audit
   alone does not satisfy `clean`.
4. **`scripts/apply-branch-protection.sh`** — the post-flip wiring that converts
   each CI radar into a binding `required_status_check` on `noogram/cosmon`
   (`enforce_admins=true`, so the gate binds the operator too).

### Two membranes, two layers (not one)

| Layer | Question | Mechanism |
|-------|----------|-----------|
| **Residence** | Is this whole *file* allowed on the public surface? | `artifact-map-audit.py` (RED on any tracked `solo` path) |
| **Content** | Does a confidential *string* hide inside a public file? | gitleaks (secrets) + `release-checklist.sh` gate 4 (client/domain/infra denylist) + the D7 publish gate in `cs done` (ADR-128) |

The denylist of confidential **strings** is sourced **externally** (env /
gitignored `scripts/.release-denylist.local` / the now-untracked
`.cosmon/release-rules.toml` on disk), never inlined into the shipping script —
the *detector-is-its-own-leak* lesson (ADR-127 §6) carried forward.

> **The artifact-map is a TREE-membrane, not a HISTORY-membrane (ratification
> amendment, B1).** Both layers above audit the **working tree** (`git ls-files`
> / file contents at HEAD). **A public flip ships HISTORY, not just the tree** —
> every reachable past commit travels with the repo. So "tree-clean" is **not**
> "publish-safe": a blob purged from HEAD but reachable in an ancestor commit is
> still published. The ratification panel reproduced two such leaks (Sci-Hub
> index reachable via ancestor `<pinned-rev>`; the client-identity scrub
> dictionary). Therefore a **third membrane** — a history-purge gate (§6, B1) —
> is a **hard pre-flip precondition**, not a layer the tree audit can supply.

## 4. What is retired, and how

- **`.cosmon/release-allowlist.toml`** — `git rm --cached` + gitignored. Kept on
  disk only as input to the legacy local audit; never tracked, never shipped.
- **`.cosmon/release-rules.toml`** — `git rm --cached` + gitignored. It carries
  confidential client/domain literals (`noogram.dev`, `a private sibling crate`, …) and is
  the local source the forbid-strings gate derives its denylist from. Untracked
  for the same reason the allowlist is: it can never ship.
- **`scripts/release/bless-allowlist.sh`**, **`scripts/release/cosmon-release-resync.sh`**
  — marked DEPRECATED in-file (header banner pointing here). Not deleted: they
  are retained to reproduce the **frozen cosmon-private history archive** (§5),
  a one-time act, never a maintained second version. The broader resync chain
  (`purge-history.sh`, `rename-clients.sh`, `scrub-messages.sh`, …) is obsolete
  for the public flip for the same reason and should be treated as archive-only.

## 5. cosmon-private is a FROZEN HISTORY ARCHIVE, not a second version

The pre-trim full history (28 removed crates, client docs, the operator's
narration) is preserved **once** as `cosmon-private` — a frozen snapshot for
provenance, **never a maintained pair** with the public repo. There is no
ongoing sync, no projection, no second `main` to keep clean. The 28 removed
crates live in their home galaxies / future plugin repos (ADR-132), not in
cosmon and not in a maintained private cosmon.

> **Resurrection condition (B7, godel).** "Frozen archive" is honest **only
> while the public `main`'s history is also frozen at the flip point.** The
> post-flip history rewrite of §6a/B1, *if re-snapshotted back into a maintained
> private pair*, resurrects exactly the two-versions-to-sync model this ADR
> retires. Discipline: provenance and plugin extraction **read** from the frozen
> archive into a **third** repo — they never **write back** to recreate the pair.

## 6. Consequences

- **One tree, one audit, one truth.** No projection to keep in sync; the leak
  surface is `git ls-files` of `main`, full stop.
- **New confidential file → RED by construction.** It classifies `solo` (or
  unmapped) and the audit fails closed (`PUBLIC_AUDIENCES = {public}` is a
  positive allow-list).
- **The flip is still gated by a clean LIVE tree, which today it is NOT.** The
  initial run of `release-checklist.sh` is RED: the tree still carries client
  names, private domains and operator `$PATH` strings (the projection chain used
  to scrub these at copy-time; now they must be scrubbed *in place* or
  relocated). This is surfaced, not hidden — the corpse is the exit code.
- **The history rewrite is a HARD PRE-FLIP GATE, not out-of-scope (amendment
  B1).** The full-history rewrite that removes confidential blobs from *history*
  (not just the tree) is an escalated, operator-run step (it rewrites shared
  object stores — same constraint oxymake ADR-017 records). It is correctly out
  of scope **for the MQ-landed task that introduced this membrane** — but it is
  **NOT** out of scope **for the flip**. See §6a: `clean` is not satisfied, and
  the flip must not proceed, until history is purged.
- **The residence membrane is already GREEN.** This task's artifact-map +
  `git rm --cached` of 65 tracked `solo` artifacts (cargo-mutants output,
  rehearsal scratch, a wrangler cache carrying the operator's Cloudflare account
  id, stray `.cosmon/state` residue, the retired allowlist/rules TOMLs) brought
  the residence layer to 2492/2492 public. **This is a TREE result; it certifies
  nothing about history** (§6a).

### 6a. Hard pre-flip gates (ratification amendment, delib-20260624-8d8f)

The membrane doctrine is ratified, but the ADR as originally written authorised
an **irreversible flip** that — verified by two independent seats — would ship
private content under the operator's real legal identity. The flip is **blocked**
until the following pre-flip gates close. **B1 is the load-bearing amendment this
acceptance turns on; B2–B7 are the accompanying blockers from the synthesis.**

- **B1 — HISTORY-CLEAN IS A FLIP GATE (CRITICAL, the load-bearing amendment).**
  Confidential content is reachable in `main`'s **history**, not its tree: a
  Sci-Hub index source (via an ancestor commit) and the client-identity scrub
  dictionary. The residence audit walks the **tree** and is structurally blind to
  history; §5/§6 originally scoped the history rewrite *out*. **Amend:** make
  **full-history-clean a hard GATE** in `release-checklist.sh` that runs
  *before* any flip — `clean = tree-clean AND history-clean`. Two acceptable
  mechanisms (operator's choice): publish from a **fresh orphan/squashed root**
  (zero pre-trim reachability by construction), **or** run a `git filter-repo`
  history purge of the catalog-crate paths + Sci-Hub-index shape + the scrub
  dictionary; then add a gate that greps `git log --all` for those paths/shapes
  and **FAILs** (not PENDs) if any pre-trim blob is reachable. *§6's "history
  rewrite is out of scope for this task" is fine for the task; it is **not**
  acceptable as a property of the flip.*
- **B2 — scrub the treasure-map coordinate.** Remove the private-archive commit
  hash from the public files that publish it (the distribution mechanism doc, the
  avatar example profile, the smithy CMB before promotion). Replace with a
  symbolic `<pinned-rev>` resolved from a gitignored local file — the same
  externalisation as the denylist, and the tree-side twin enforced on the ADR-132
  side (ADR-132 §6 F5). Publishing the *coordinate* of the history leak hands the
  reader both lock and key.
- **B3 — the scrub dictionary must not ship (CRITICAL).** The client-rename
  script (`scripts/release/rename-clients.sh` + the deprecated resync chain) is
  tracked, classifies `public`, and is excluded from the gate-4 content scan via
  `:(exclude)scripts/release/*`. Reclassify `scripts/release/**/*` to `solo` +
  `git rm --cached`, **or** drop the exclude from gate 4, **or** relocate the
  chain into the `cosmon-private` archive it exists to reproduce. Any one closes
  it; do it before the flip. (This is ADR-127 §6's *detector-is-its-own-leak*
  pathology recurring inside the very ADR that cites §6 as escaped.)
- **B4 — deny-by-default at the residence layer.** The `code` catch-all `**/*`
  defaults `public`; the ADR *claims* deny-by-default but the implementation is
  **allow-by-default**. Invert the catch-all to `solo` (every unmapped path RED
  until a public genre claims it), **or** mirror every confidential `.gitignore`
  entry with a `solo` genre + a CI check that the two lists stay in lockstep.
  This also closes the partner-deliverable leak (a partner-deliverable file
  must map to `solo`, not fall through to `public` — the lost `partner:<name>`
  audience of ADR-057 maps to `solo`, not a third tier).
- **B5 — content gates FAIL CLOSED.** Gate 4 (and gitleaks) must **FAIL**, not
  PEND, when their denylist/tool is unresolvable in the flip-authorising context
  (a fresh CI clone has no gitignored denylist by construction). No denylist, no
  flip. A gate that cannot run is a gate that failed — PEND-is-not-FAIL is correct
  only for a gate whose referee is *structurally elsewhere*, wrong for one whose
  *input is missing*.
- **B6 — fix the enforcement target.** The enforcement scripts target
  `noogram/cosmon`; origin is `noogram/cosmon`. Add **gate 0**: assert
  `git remote get-url origin` slug == `REPO_SLUG`, FAIL otherwise — else branch
  protection (incl. `enforce_admins`) lands on the wrong repo while the
  actually-public repo ships unguarded.
- **B7 — prose-honesty (non-blocking, folded in).** State the self-referee escape
  (§2 / `apply-branch-protection.sh`) as **silent→attributable, not absolute**:
  `enforce_admins=true` binds the *merge* but not the *editing of the referee*; an
  N=1 admin means consistency is **monitored, not enforced** — name the missing
  second-admin witness. And §5's "frozen archive" is honest **only while main's
  history is also frozen**: §6/B1's post-flip history rewrite, if re-snapshotted,
  resurrects the maintained pair (§5 amendment below). Provenance/plugin
  extraction must **read into a third repo**, never write-back.

Operator reading: **ratify the doctrine; do not flip** until B1 (and B2–B7)
close. B1 and B3 are confirmed leaks of real private content under the operator's
real legal identity into an irreversible public repo. A green tree audit cannot
see the leak.

## 7. Coherence checklist (CLAUDE.md)

stateless ✔ (audit + checklist are one-shot reads) · idempotent ✔ ·
regime-aware ✔ (observers; no regime transition) · single perimeter ✔ (audit is
read-only; `apply-branch-protection.sh` is the separate write tool) · symmetric
✔ (the artifact map declares; `git rm --cached` reverses a tracking) ·
write-read asymmetry preserved ✔ (no command both writes state and returns a
coupling report).

## 8. The one-sentence ADR

*cosmon stops projecting a clean public tree out of a confidential one and
becomes a single public repo whose membrane is a residence audit over its own
`git ls-files` — the allowlist that guarded the projection retires with the
projection.*
