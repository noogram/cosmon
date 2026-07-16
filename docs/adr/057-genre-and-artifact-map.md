# ADR-057 — Genre and the Artifact Map

**Status:** Proposed (2026-04-20)
**Scope:** classification axis for every tracked artifact in a cosmon
galaxy. Defines the noun `genre`, the TOML table `.cosmon/artifact-map.toml`,
and the two commands (`cs inspect`, `cs artifacts audit`) that read it.
Answers the question *"what kind of thing is this file, and who is it
for?"* — the layer **above** ADR-055 residence.

**Parent deliberations:**
- `delib-20260420-74b8`
  — Artifact Map panel (wheeler, jobs, torvalds, feynman, shannon,
  godin, knuth). Converged on nomenclature, CLI shape, and the minimal
  TOML surface.
- `delib-20260420-fbe4`
  — Neurion hypergraph vs TOML. Verdict 5-0 for TOML; wheeler's
  graduated vote reconciled as *future citation-graph projection over
  TOML* (not v0).

**Binds:**
- [ADR-055](055-cosmon-residence.md) — residence is the *where*. Genre
  is the *what*. Every genre declares an audience; residence is
  derived from audience. Genre is a strict layer above residence: a
  galaxy with a single residence can still classify its files into
  many genres.
- [ADR-047](047-event-log-protocol-v0.md) — `events.jsonl` is a genre
  in its own right (`code` in v0; may earn its own genre in v1 if the
  rotation policy diverges from source files).

**Blocks (follow-ups, not in scope here):**
- `cs migrate --genre github-surface` — move already-tracked surface
  files to the residence implied by their genre. v0 declares; v1
  enforces.
- Citation-graph projection over `artifact-map.toml` (wheeler's vote,
  deferred as future work).

## 1 · Context

Eight thousand files live under `.cosmon/state/` (before the ADR-055
migration), six thousand under `docs/`, and a growing number of
operator-visible artifacts — chronicles, addl videos, ADRs, guides —
sit at various places in the tree. Residence (ADR-055) tells us *where
a galaxy's memory lives*, but it does not tell us *what kind of thing
each file is* or *who it is for*.

The operator's precipitating question (2026-04-20): **docs/surfaces/issues.md
and prs.md are about to pollute the git log with a `chore(surfaces):
refresh` every time `cs reconcile` runs on noesis. Can these be routed
to an orphan branch instead?**

The architectural answer is that those files have a *different kind*
than, say, a chronicle: a chronicle is a piece of durable narration
written once by the operator; a surface is a regenerable mirror of the
ledger. They should not live in the same git history because they
obey different social contracts. Naming this axis — the `genre` of a
file — is the prerequisite for routing it correctly.

The two deliberations converged on a minimal v0: **five genres plus a
`code` catch-all, two required fields per genre (`location`,
`audience`), residence derived**. The CLI surface is one verb:
`cs inspect <path>`. Audit is a separate verb: `cs artifacts audit`.

## 2 · Decision

### 2.1 Nomenclature: `genre`

The noun is **`genre`** (French and English, identical spelling and
meaning, classifier of form and audience). Five alternatives were
considered and rejected:

| Candidate | Rejected because |
|-----------|------------------|
| `category`  | bureaucratic; suggests a drop-down menu, not an identity. |
| `kind`      | already used in cosmon for molecule kinds (💡/🔧/📐/🐛/⚡/🧠); collision. |
| `class`     | OOP load; wrong register for operator docs. |
| `artifact_type` | two words, snake_case, and redundant: every tracked thing is an artifact. |
| `residence` (re-use) | conflates *where* with *what*; ADR-055's noun. |

Wheeler: *genre is a noun, not an adverbial*. Jobs: *one syllable in
both languages, no translation loss*. Godin: *genre is what a reader
expects of a work — the right register for deciding audience*.
Shannon measured the redundancy across candidates; `genre` added the
least ambiguity to an already-overloaded vocabulary.

### 2.2 The six v0 genres

```text
chronicle         — operator narration (docs/lore/*.md)
adr               — architecture decision records (docs/adr/*.md)
addl              — partner deliverables (docs/addl/<partner>/...)
github-surface    — regenerable GitHub mirror (docs/surfaces/*, STATUS.md, ISSUES.md)
deliberation      — multi-persona panel synthesis (.cosmon/.../synthesis.md)
code              — everything else (catch-all — default genre for any path)
```

Per genre, the map declares two fields:

- **`location`** — list of glob patterns. Longest-match wins ties.
- **`audience`** — who the file is addressed to. One of:
  - `public` — the world (`main` branch, open source).
  - `team` — the collaborators on the galaxy's git remote.
  - `author+agent` — only the operator and their agents.
  - `partner:<name>` — a specific external partner (captured from the
    path glob when `<name>` appears as a path component).
  - `solo` — the operator, no one else.

**Residence is derived from audience** (delib-74b8 C3):

| Audience            | Residence    |
|---------------------|--------------|
| `public`            | `Team` (tracked on `main`)       |
| `team`              | `Team` (tracked on `main`)       |
| `author+agent`      | `Team` (orphan narration branch) |
| `partner:<name>`    | `Team` (orphan narration branch, optionally `Encrypted`) |
| `solo`              | `Solo` (local-only, excluded from push) |

A genre therefore selects one residence out of the four ADR-055
offers. The six genres above exercise three: `Team`, `Team`-with-orphan,
and `Solo`.

### 2.3 CLI surface — one verb per atom

The panel (jobs + torvalds + feynman + shannon unanimous, knuth on
the audit split) converged on a single inspection verb plus a
separate audit verb:

```text
cs inspect <path>              # classify one path, print genre + audience + residence + rot
cs inspect <path> --json       # NDJSON for scripting
cs inspect <path> --verbose    # explain the glob that matched + residence derivation

cs artifacts audit             # walk git ls-files, report counts per genre, list unclassified
cs artifacts audit --json      # NDJSON
```

Explicitly rejected:

- `cs classify` — verb is not a noun's home; inspect *is* classification.
- `cs genre <subcommand>` — pushes the noun into the verb slot.
- `cs inspect --audit` — mixes two atoms; knuth's cut.
- A pre-commit hook — out of scope for v0 (declare, don't enforce).

### 2.4 TOML shape — ~14 lines, deliberate

```toml
# .cosmon/artifact-map.toml
# Classifies every tracked file. See ADR-057.

[chronicle]
location = ["docs/lore/**/*.md"]
audience = "author+agent"

[adr]
location = ["docs/adr/**/*.md"]
audience = "public"

[addl]
location = ["docs/addl/<name>/**/*"]
audience = "partner:<name>"

[github-surface]
location = ["docs/surfaces/**/*.md", "STATUS.md", "ISSUES.md"]
audience = "solo"

[deliberation]
location = [".cosmon/state/fleets/*/molecules/*/synthesis.md"]
audience = "author+agent"

[code]
location = ["**/*"]
audience = "public"
```

`code` is the catch-all; every file unmatched by the other five
genres lands here. Totality is guaranteed by construction (invariant
I1 below). When `.cosmon/artifact-map.toml` is **absent**, every file
is treated as `code` (backwards-compatible default — zero surprise
for existing galaxies).

### 2.5 Why `github-surface` defaults to `solo`

The operator's precipitating question has a specific, architectural
answer. `docs/surfaces/*.md` (and its cousins `STATUS.md`,
`ISSUES.md`) are a **regenerable mirror** of the ledger — `cs reconcile`
rewrites them on every sync. If they are tracked on `main`, every
sync produces a `chore(surfaces): refresh` commit whose sole content
is a projection of state already held on disk elsewhere.

The `github-surface` genre says: these files are *addressed to nobody
but the operator's local machine*. Their audience is `solo`, their
residence is `Solo`, and they belong in `.git/info/exclude` (the
local ignore that is never pushed). GitHub itself reads them via
`cs reconcile --github` using a different push channel (Issues API),
not the git index. The mirror stays on disk; the noise disappears
from `git log`.

### 2.6 Invariants (knuth-drafted)

Four structural properties the audit verb checks:

- **I1 — Totality.** Every path tracked by git matches at least one
  genre. Guaranteed by the `code` catch-all glob `**/*` at the bottom
  of the map.
- **I2 — Unique classification by glob specificity.** When two genres
  could match a path, the longer glob wins (more path components
  fixed = more specific). Ties on length are resolved in declaration
  order (earliest wins). No path has ambiguous genre.
- **I3 — Residence is well-typed.** The audience of a genre maps to
  a residence that actually exists in ADR-055. No genre may declare
  an audience whose residence is not in
  `{Solo, Team, Encrypted, Remote}`.
- **I4 — Audience–residence compatibility.** A genre declaring
  audience `public` cannot be placed in a `Solo` residence (would
  contradict "public reachable"). A genre declaring audience `solo`
  cannot be placed in anything but `Solo`. The audit verb refuses to
  pass when a migration in flight would violate this.

`cs artifacts audit` checks all four and exits `0` on pass, `1` on
violation (with a per-violation line in the output).

### 2.7 The cut list

The deliberation explicitly removed these from v0:

- **`visibility`.** Redundant with `audience` (0.3 bits of additional
  information, shannon measured). Dropped.
- **`rot`** (declared field). Must be *measured* from git history,
  not declared. Shannon falsified a draft that declared `rot = "never"`
  on ADRs by measuring the actual commit rate. In v0, `rot` is a
  **computed** field returned by `cs inspect`, read from `git log -n 1
  --format=%ct <path>` — never a TOML input.
- **`inheritance`.** Genre A *extends* genre B. Knuth:
  *inheritance over globs is undecidable in general — two globs can
  partially overlap in ways no finite rule resolves*. Dropped.
- **`multi-location`.** A single genre spanning more than one residence.
  Postponed to v1 (only `deliberation` is a realistic candidate, and
  its v0 default works).
- **`--publish`.** A flag on `cs inspect` that would *move* a file to
  the residence its genre declares. v0 declares; v1 migrates. Pre-commit
  enforcement also deferred.

### 2.8 Partner capture

A single path-level parameter: when a glob contains `<name>`, that
token captures the matching path component and parameterises the
audience. Example:

```
docs/addl/bob/videos/demo.mp4
  ↓ matches docs/addl/<name>/**/*
  ↓ <name> = "bob"
  ↓ audience = "partner:bob"
  ↓ residence = Team (orphan narration)
```

Only one capture group per glob; no regex, no positional args. If a
glob needs more expressive power, split it into two genres.

## 3 · Relation with ADR-055 (residence)

Genre layers **above** residence. The decomposition is:

```
     path
       │
       ▼
  [artifact-map.toml]   ◀── ADR-057 (this)
       │
       ▼
     genre
       │
       ▼
    audience
       │
       ▼
   residence            ◀── ADR-055
       │
       ▼
  {Solo, Team, Encrypted, Remote}
```

A galaxy in `Team` residence still holds six genres worth of files;
they all obey the same `Team` contract at the git-level. A galaxy in
`Solo` residence would have the `github-surface` genre collapse with
the overall residence (no distinction). Genre does not override
residence; it refines it.

## 4 · Feynman 1-week experiment

This ADR is **proposed**, not accepted. The falsifier is mechanical:

1. Land `.cosmon/artifact-map.toml` with the six v0 genres.
2. For seven consecutive days, run `cs artifacts audit` daily on
   cosmon-le-repo.
3. Count unclassified files per day (should be 0 — the `code` catch-all
   guarantees it, so this is a build-health check, not a design check).
4. **The design check** — count files whose classification *surprises*
   the operator when inspected. If the count trends toward zero and
   the six genres cover every case that mattered in the week, the
   classification is sound. If new candidates emerge (e.g.
   `delivery`, `experiment`, `research-pad`), add genres until
   coverage is stable.

The audit is successful when:
- Unclassified count = 0 (totality holds), and
- Surprise count ≤ 1 across the week (classification is useful).

If either fails, revise the map *before* marking this ADR accepted.

## 5 · Consequences

**Gained**

- One noun, one TOML, two verbs. Every file answers *what am I and
  who am I for?* without reading any ADR.
- The operator's precipitating question (`docs/surfaces/*` pollution)
  has a one-line answer: it's a `github-surface` genre, solo audience,
  excluded from git.
- A base vocabulary for v1 enforcement (`cs migrate --genre …`): we
  now know what "this file is in the wrong residence" means.
- The `code` catch-all means a galaxy with no `artifact-map.toml`
  keeps working unchanged.

**Lost / constrained**

- Six genres is not six forever. v1 will likely add 1–2 more; the cost
  of each is a glob and an audience declaration.
- No enforcement in v0. An operator can tag a file `github-surface`
  and still track it on `main`; the audit catches the inconsistency
  but does not fix it. v1 layers `cs migrate --genre` on top.
- `rot` is *computed* per inspection — a fast operation (one
  `git log -1`), but it does make `cs inspect` git-dependent. Galaxies
  that are not git repos return `rot = unknown`.

**Open (deferred)**

- Citation-graph projection over the TOML (wheeler's future work). A
  TOML edit is still a graph edit — the projection can happen later
  without breaking v0.
- `events.jsonl` as its own genre (v1 candidate if rotation diverges).
- Multi-location genres (spanning more than one residence).
- `--publish` flag and pre-commit hook (enforcement layer).

## 6 · References

- Deliberation synthesis — Artifact Map:
  `delib-20260420-74b8`
- Deliberation synthesis — TOML vs Neurion:
  `delib-20260420-fbe4`
- Chronicle: *Le triangle qui s'est résolu* (commit `f84f6c8e5`)
- Residence axis: [ADR-055](055-cosmon-residence.md)
- Event-log substrate: [ADR-047](047-event-log-protocol-v0.md)
- Operator question (2026-04-20): docs/surfaces/*.md pollution on
  noesis
- Follow-up: `task-20260420-4125` (chronicle genre orphan branch —
  potential successor molecule)

## The one-sentence genre

*A file's genre is what kind of thing it is and who it is for — the
artifact map declares it, `cs inspect` reads it, residence follows.*
