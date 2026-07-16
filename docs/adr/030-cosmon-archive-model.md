# ADR-030: Cosmon Archive Model

## Status
Accepted (Implemented) — landed over six milestones (M1 → M6) between
2026-04-12 and 2026-04-19. Derived from deliberation
`delib-20260412-692b` (panel: wheeler, torvalds, tolnay, architect,
knuth, feynman, shannon). Synthesis:
`.cosmon/state/fleets/default/molecules/delib-20260412-692b/synthesis.md`.

Shipping commits (`git log main --oneline | grep archive`):

| Milestone | Commit | Subject |
|---|---|---|
| M1 config + gitignore | `68eaf3e8` | `feat(config): [archive] section + gitignore negation` |
| M2 atomic write path | `aeb9a12e` | `feat(archive): atomic snapshot writer` |
| M3 terminal triggers | `bcfc2d22` | `feat(terminal): archive write at terminal transitions` |
| M3 read commands | `a1440ba2` | `feat(archive): read commands (list / show / verify)` |
| M4 retention + prune | `04c5941f` | `feat(archive): retention + prune` |
| M5 verify + CI gate | `6af6c970` | `feat(archive): cs archive verify + CI gate` |
| M6 documentation | _this commit_ | `docs(archive): ADR-030 closeout + operator manual` |

Operator manual: [`docs/archive.md`](../archive.md). CLI examples live
in `cs help archive` and `man cs`.

## Context

`.cosmon/state/` is currently fully gitignored. Consequence: every
artifact cosmon produces — molecule JSON, deliberation syntheses, panel
responses, event logs — is invisible outside the machine that ran the
worker. A fresh clone sees declarations and formulas but not a single
trace of decisions already taken. This defeats the non-cosmon reader
(future collaborator, auditor, LLM opening the repo cold), and it
conflicts with the project's own maxim that immutable facts belong in
git (`CLAUDE.md`, Chronicles).

A quick fix — un-gitignore selected path patterns — was considered and
rejected as a long-term model because it conflates mutability tiers and
drifts silently. The deeper question: what is the right archive model?

Three forces in tension drove the deliberation:

1. **Smallest API surface** (torvalds/tolnay). Every new command, every
   new public path is a forever contract. Prefer `.gitignore` rules and
   existing verbs.
2. **Formal reconstruction correctness** (knuth). If `state/` is lost,
   the tracked content must be sufficient to reconstruct the causal DAG
   of terminal molecules up to isomorphism.
3. **Architectural invariant compliance** (architect). The archive must
   compose with surface-sync (invariant #8 write-read asymmetry),
   merge-before-dispatch, the stateless core, and ADR-016's two-layer
   model.

wheeler reframed the question as **reader taxonomy** — apparatus,
events, narrative — three readers, three surfaces. feynman reduced it
to one sentence: *"What did this project decide, do, and learn —
readable a year from now without running `cs`?"* shannon quantified
the irreducible payload (~14 KB per deliberation; responses are
~85% of that). The integrated answer:

## Decision

Adopt a **two-directory split** inside `.cosmon/`, where the boundary
encodes a *mutability invariant* and a *reader contract* — not a
pattern-match convention:

```
.cosmon/
  state/       # gitignored — live apparatus
               #   per-fleet registry.sqlite, locks, in-flight molecule JSON,
               #   tmux captures, worker pids, reconcile caches
  archive/     # tracked, append-only — terminal record
               #   archive/YYYY/MM/<molecule-id>/{molecule.json, edges.json,
               #     manifest.json, responses/, synthesis.md, events.jsonl}
               #   archive/events/events-YYYY-MM.jsonl  (fleet-level, rolled)
               #   archive/INDEX.md                      (generated)
               #   archive/SCHEMA_VERSION
```

### Invariants the archive preserves

Formalized by knuth, adopted as the specification:

- **I1 Terminal closure.** Every terminal molecule's identity, kind,
  final status, timestamp, response artifacts, and incoming edges from
  other terminal molecules are recoverable from `archive/` alone.
- **I2 Causal DAG preservation.** The subgraph induced on terminal
  molecules by typed links is reconstructible exactly. Edges touching
  non-terminal peers record **both endpoints** so the projection is
  lossless.
- **I3 Content addressability.** Every referenced artifact is either
  inlined or pinned by sha256 to a git-reachable object.
- **I4 Monotonicity.** Archive grows monotonically; corrections are
  append-only records, never in-place edits.
- **I5 Surface replayability.** `cs reconcile` run against `archive/`
  alone produces byte-identical STATUS.md / ISSUES.md projections over
  terminal molecules.

### Promotion mechanism (hybrid)

Terminal transitions (`cs done`, `cs collapse`, `cs freeze`, `cs stuck`)
write the archive **atomically** with the state transition:

1. Write canonical `molecule.json` (sorted keys, stable LF) to
   `archive/YYYY/MM/<id>/`.
2. Write `edges.json` (edge manifest, both endpoints), `manifest.json`
   (formula pin + parent branch SHA + response hashes).
3. Copy `responses/*.md` and `synthesis.md` verbatim.
4. Append the terminal transition to `archive/events/events-YYYY-MM.jsonl`.
5. Fsync, then mark the molecule archived in `state/`.

`cs reconcile` projects **derived** artifacts (`archive/INDEX.md`,
STATUS.md, ISSUES.md) and enforces append-only via `--check`. It does
**not** write individual molecule archives — that would fuse two
decisions into one invocation and break invariant #8.

No new top-level command. Promotion is a side-effect of verbs that
already exist.

### Minimal archived set per terminal molecule

```
archive/YYYY/MM/<id>/
  molecule.json     # canonical terminal state (no status_history; derivable)
  edges.json        # every typed link touching <id>, both endpoints
  manifest.json     # {formula_pin, parent_branch_sha, response_hashes}
  responses/*.md    # irreducible NL payload (shannon: ~85% of H)
  synthesis.md      # deliberations only
  events.jsonl      # per-molecule transition log
```

Explicitly **excluded** from `archive/`: `registry.sqlite*`, `*.lock`,
`*.tmp`, `tmux-*.log`, `captures/`, intermediate JSON snapshots,
in-flight pending-molecule state.

### Clone semantics

Fresh clone: `archive/` populated from git, `state/` empty. First `cs`
invocation lands in the **Inert regime** of ADR-016. `cs observe` and
`cs reconcile` work read-only over `archive/`. A future `cs replay`
reconstructs derived state/ indexes from `archive/events/`; not a v1
requirement, but the archive layout is designed to support it.

### Format decisions

- Plain JSON + markdown. No content-addressable blob store (shannon:
  "reinventing git inside git"; git already does content-addressed
  dedup and delta pack compression).
- Canonical on write: sorted keys, LF endings, no embedded wall-clock
  paths. Meaningful diffs are the goal.
- Binary artifacts (PDFs, large captures): out of scope for v1; store
  sha256 + path in `manifest.json`. Add LFS only when concrete
  pressure appears.

## Consequences

### What improves

- **Non-cosmon readers gain a contract.** The archive schema is public,
  versioned (`SCHEMA_VERSION`), and markdown-first. A `git clone` plus
  a text editor is sufficient to follow a project's reasoning.
- **Crash recovery generalizes to clone recovery.** The same mechanism
  that survives a `tmux` crash now survives re-cloning the repo on a
  new machine.
- **Surface-sync discipline extends naturally.** `archive/` is declared
  in `surfaces.toml`; `cs reconcile --check` gates its consistency in
  CI just like STATUS.md today.
- **Chronicles and ADRs are joined by a first-class molecule record.**
  Every deliberation's full panel, synthesis, and outcome is readable
  inline with the code it shaped.

### What changes

- `.cosmon/state/` no longer contains terminal molecules past a
  `cs done`/`cs collapse`/`cs freeze`. Scripts that grep `state/` for
  terminal molecules must read `archive/` instead.
- Terminal-transition verbs gain an archive-write side-effect. The
  archive write must be idempotent; running `cs done` twice must equal
  once (existing invariant; gated on an `archived:true` flag in
  `state/` before the transition is removed).
- Disk footprint grows linearly with history (shannon: ~6 KB packed
  per deliberation; 1000 deliberations ≈ 6 MB). Acceptable.

### What is deliberately not done

- ~~No `cs archive` command.~~ **Revised at M3.** The initial rejection
  was correct for the *write* path (promotion remains a side-effect of
  terminal-transition verbs). But operators need a *read* path that
  does not require grep-walking `archive/YYYY/MM/…` by hand, and the
  retention policy needed an explicit execute verb. The M3+M4 compromise
  added `cs archive list / show / verify / prune` as read-only +
  retention surfaces with zero write-path impact. The composability
  principle is preserved: there is still no `cs archive promote` and
  no new promotion verb.
- No content-addressable blob format. Rejected by shannon; redundant
  with git.
- No archival of `registry.sqlite`. Derived index; rebuilt from JSON.
- No pre-emptive monthly rollover of per-molecule `events.jsonl`;
  only the fleet-level stream rolls monthly.
- No binary-artifact subsystem in v1.
- No decision yet on wheeler's blade (track only events, regenerate
  terminal JSON on demand). Deferred pending a `cs replay` benchmark.

### Migration plan (as implemented)

1. **M1 (2026-04-12) — config + gitignore.** `[archive] enabled = false`
   ships as default, `ArchiveConfig` lives in `cosmon-core::config`,
   `.gitignore` switches from blanket `.cosmon/state/` exclusion to
   explicit per-file patterns so `.cosmon/state/archive/` is tracked
   without a leading negation rule. (`68eaf3e8`.)
2. **M2 (2026-04-13) — write path.** Atomic snapshot writer
   (`cosmon-state::archive::write`) with canonical JSON, SHA-256
   manifest, tempfile + rename, and non-fatal `write_non_fatal`
   variant. (`aeb9a12e`.)
3. **M3 (2026-04-13) — wiring + read commands.** `cs done`,
   `cs collapse`, `cs freeze`, `cs stuck` call `write_non_fatal`.
   `cs archive list / show / verify` added as read-only operator
   surface; `cs migrate --archive-past` back-fills pre-existing
   terminal molecules. (`bcfc2d22`, `a1440ba2`.)
4. **M4 (2026-04-19) — retention.** `[archive.retention]` policy with
   `keep_all`, `max_age_days`, `max_total_mb`, `keep_kinds`.
   `cs archive prune [--dry-run]`. Hash-chain integrity closure
   (`DecayedFrom` / `BlockedBy` / `MergedFrom`). (`04c5941f`.)
5. **M5 (2026-04-19) — verify + CI gate.** `cs archive verify`
   recomputes manifest hashes; CI gate pipe
   (`cs archive list --since-days 7 | xargs cs archive verify`).
   (`6af6c970`.)
6. **M6 (2026-04-19) — documentation.** Operator manual
   ([`docs/archive.md`](../archive.md)), ADR-030 status flipped to
   Accepted (Implemented), `man cs` synopsis example.
   (_this commit_.)

Default remains `enabled = false` — flipping to `true` by default is
deferred to a future minor release after the archive has matured on
opt-in projects. The gitignore pattern shipped is per-file (not the
`!.cosmon/archive/` negation originally sketched) because the
two-directory split in the final design is `state/` (live apparatus,
with nested durable archive) rather than `state/` + sibling `archive/`.

### Compliance with invariants

| Invariant | Compliance |
|---|---|
| Stateless core | ✓ Archive writes are one-shot, idempotent, JSON-on-disk |
| Merge-before-dispatch | ✓ Archive write lands with the `cs done` merge commit |
| Surface-sync (invariant #8, write-read asymmetry) | ✓ Promotion is in the terminal-transition verb; `cs reconcile` only projects derived files |
| Two-layer model (ADR-016) | ✓ `archive/` is shared substrate for the resident runtime, like `state/` |
| Three regimes | ✓ Fresh clone = Inert regime with authoritative archive |
| Composability principle | ✓ No new commands, no new plugins; extension via `surfaces.toml` declaration |
| CLI-first for workers | ✓ Archive is read/written through the same walk-up-discovered paths |

## Open questions (deferred to follow-up ADRs)

- **wheeler's blade** — spike `cs replay` performance; if events + git
  log can reconstruct terminal JSON cheaply, drop the tracked JSON and
  save ~4 KB/molecule. Deferred: v1 ships both for safety.
- **Archive schema version governance** — who owns `SCHEMA_VERSION`
  bumps? First bump will open a successor ADR; current stamp tracks
  `cosmon_state::SCHEMA_VERSION`.
- **GitHub surface coupling** — does `archive/` feed GitHub Issue
  closure messages via `cs reconcile`? Not wired in v1. Spec to live
  in a follow-up ADR when GitHub mirror grows a "closed-with-synthesis"
  rendering.
- **Binary artifact subsystem (LFS)** — v1 stores path + SHA-256 in
  manifests only. When a concrete pressure appears (attaching a PNG,
  a PDF review, a large dataset), the next ADR adds the blob backend.
- **Default flip** (`enabled = true`) — deferred to a minor release
  after opt-in adoption feedback lands.

## Alternatives rejected

- **Single directory + selective gitignore** (torvalds, tolnay). Elegant
  diff, zero new paths, minimal semver impact. Rejected because
  selective gitignore is a pattern-match heuristic that drifts silently
  (knuth I4 risk) and is hostile to surface-sync's `--check` gate
  (architect). Its merits are preserved by migrating behind a config
  flag and minimizing public CLI surface.
- **Three-way split `state/` + `history/` + `docs/`** (wheeler).
  Correct taxonomy but overspecifies v1. Existing `docs/` already
  handles the human-narrative surface; `archive/` can grow a
  three-reader discipline internally without a top-level rename.
- **Content-addressable archive** (option F in the question). Rejected
  (shannon, torvalds, feynman): cargo-cult of git's internals; git
  already provides content addressing via blob hashing and pack
  delta. Adds no bits, adds complexity.
- **`cs archive` command** (option B alternative). Rejected
  unanimously: promotion is a property of terminal transitions, not a
  new verb. Adding it violates the composability principle.
