# `cs archive` — durable proof-of-work trail

Every time a molecule reaches a terminal transition (`cs done`,
`cs collapse`, `cs freeze`, `cs stuck`), cosmon drops a canonical,
hash-sealed snapshot under `.cosmon/state/archive/YYYY/MM/<molecule-id>/`.
The snapshot outlives worktree teardown and branch deletion: a fresh
`git clone` sees every merged molecule's prompt, edges, responses, and
synthesis without running `cs`.

Implementation: [`crates/cosmon-state/src/archive/`](../crates/cosmon-state/src/archive/),
[`crates/cosmon-cli/src/cmd/archive.rs`](../crates/cosmon-cli/src/cmd/archive.rs).
Governing ADR: [ADR-030](adr/030-cosmon-archive-model.md).

## Why it exists

`cs done` tears down the worktree, removes the feature branch, and
purges the worker from the fleet. Without an archive, the molecule's
proof-of-work trail — the nucleation prompt, the per-persona responses,
the synthesis, the causal edges to other molecules — evaporates the
moment teardown runs. A reader who clones the repo a month later sees
a merged commit but no record of *why* the decision was taken or
*which panel* argued for it.

The archive closes that gap without a new daemon, a new database, or a
new top-level command. It is a side-effect of verbs that already exist,
writing plain JSON + markdown to a tracked subdirectory of
`.cosmon/state/`. Fresh-clone readers and CI auditors get a contract;
operators get a knob (`[archive.retention]`) to control disk growth.

## Enabling the archive

The archive is **opt-in per project**. Add to `.cosmon/config.toml`:

```toml
[archive]
enabled = true

# Optional retention policy (defaults keep everything forever).
[archive.retention]
keep_all      = false              # safety switch; must be false to prune
max_age_days  = 180                # 0 disables the age rule
max_total_mb  = 512                # 0 disables the size rule
keep_kinds    = ["decision", "deliberation"]
```

With `enabled = false` (the default), terminal transitions are no-ops
against `.cosmon/state/archive/`. With `enabled = true`, every
`cs done` / `cs collapse` / `cs freeze` / `cs stuck` writes one entry.

Writes are **non-fatal**: an archive failure logs to stderr prefixed
with `archive:` and lets the terminal transition succeed. The archive
catches the honest reader, not a motivated adversary.

## Layout

```
.cosmon/state/archive/
  SCHEMA_VERSION                          # stamp — current: see cosmon-state::SCHEMA_VERSION
  YYYY/MM/<molecule-id>/                  # one directory per terminal molecule
    molecule.json                         # canonical MoleculeData (sorted keys)
    edges.json                            # typed links touching this molecule
    manifest.json                         # {formula_pin, schema_version,
                                          #  response_hashes, synthesis_hash}
    responses/*.md                        # per-persona responses (deliberations)
    synthesis.md                          # final synthesis (when present)
    events.jsonl                          # per-molecule transition log
  events/
    events-YYYY-MM.jsonl                  # fleet-level rolled transition stream
```

Ordering is deterministic: JSON files are serialized with sorted keys
(via a `BTreeMap<String, Value>` intermediate) and trailing LF, so
`git diff` tracks semantic change, not serializer whims.

### Manifest schema

```json
{
  "schema_version": "1",
  "formula_pin": "task-work",
  "molecule_id": "task-20260413-cb69",
  "status": "completed",
  "response_hashes": {
    "wheeler.md": "4f8c...a3",
    "torvalds.md": "9e2d...7b"
  },
  "synthesis_hash": "1b3a...f0"
}
```

`response_hashes` and `synthesis_hash` are SHA-256 hex digests computed
at archive time. `cs archive verify` recomputes them and flags any
drift — including file deletion, which surfaces as an empty `current`
hash.

### What is deliberately excluded

- `registry.sqlite*` (derived index, rebuilt from JSON)
- `*.lock`, `*.tmp`, `tmux-*.log`, `captures/` (apparatus noise)
- In-flight pending-molecule state (lives in `state/`, not `archive/`)
- Binary artifacts (PDFs, screenshots) — v1 stores path + hash only;
  LFS is reserved for a future milestone

The gitignore rule that makes this work:

```gitignore
.cosmon/state/fleet.json
.cosmon/state/**/state.json
.cosmon/state/**/runtime.lock
.cosmon/state/**/*.lock
.cosmon/state/**/*.pid
.cosmon/state/**/pty.log
.cosmon/state/**/tmux-capture.log
.cosmon/state/**/*.log
```

Notice what is **not** ignored: `.cosmon/state/archive/` and every
canonical artifact under it. Apparatus is gitignored; the archive is
tracked.

## Reading the archive

### List archived entries

```bash
cs archive list                          # every archived molecule
cs archive list --year 2026              # scoped to one year
cs archive list --year 2026 --month 04   # scoped to one month
cs archive list --json                   # structured form for scripts
```

Plaintext output is a table (`YEAR / MONTH / MOLECULE / STATUS / FORMULA`)
sorted by `(year, month, molecule_id)`. JSON output is an object with
`archive_root`, `count`, and `entries[]`, where each entry carries
`molecule_id`, `year`, `month`, `status`, `formula`, `schema_version`,
and `path`.

### Inspect one entry

```bash
cs archive show <molecule-id-or-prefix>
```

Prints the manifest, the causal edges, and the artifact inventory. The
argument accepts a unique ID prefix so operators do not have to type
full `task-20260413-cb69` each time.

### Verify integrity

```bash
cs archive verify <molecule-id-or-prefix>
```

Recomputes the SHA-256 of every file listed in `manifest.json`'s
`response_hashes` plus `synthesis.md` (when `synthesis_hash` is
sealed). Exits `0` on a clean chain, `1` on any mismatch or deletion.

Verification is a **trace, not a lock** — no `chmod`, no PKI, no
signatures. Anyone with filesystem access can still rewrite both the
file and its hash. The seal catches the accidental post-hoc edit and
the lazy shadow contract; it is not a tamper-evident vault. This
mirrors the `git status` model: the working tree may diverge from the
last commit; `git status` tells you.

## Retention — controlling disk growth

The archive grows monotonically by design (invariant **I4**, ADR-030).
On a busy project that ships a hundred molecules a month, the
prompt / briefing / synthesis markdown accumulates to hundreds of
megabytes a year. `[archive.retention]` is the controlled escape
valve.

### Policy knobs

| Key | Default | Meaning |
|-----|---------|---------|
| `keep_all` | `true` | Safety switch. Must be `false` for `cs archive prune` to delete anything. |
| `max_age_days` | `0` | Entries older than this become deletion candidates. `0` disables. |
| `max_total_mb` | `0` | Soft cap on total archive size. Above cap, oldest non-kept entries are evicted first. `0` disables. |
| `keep_kinds` | `["decision", "deliberation"]` | Molecule kinds that are never deleted, regardless of age or size. |

### Hash-chain integrity

Retention runs a **BFS closure** over the typed-link graph. If kept
molecule *B* references molecule *A* as a parent via `DecayedFrom`,
`BlockedBy`, or `MergedFrom`, then *A* is promoted to kept even when
policy would have deleted it. Chains *C → B → A* survive as long as
*C* is kept. No kept entry is ever orphaned from its causal history.

Implementation: [`crates/cosmon-state/src/archive/retention.rs`](../crates/cosmon-state/src/archive/retention.rs).

### Applying the policy

```bash
cs archive prune --dry-run   # print the plan (kept / promoted / deleted)
cs archive prune             # execute the plan
```

Planning is **pure**: `cs archive prune --dry-run` never touches disk.
Execution runs `fs::remove_dir_all` on every entry classified `Delete`
and emits a JSON / plaintext summary with sizes freed.

## Fresh-clone semantics

Clone a repo that has `[archive] enabled = true` and the archive is
already populated from git; `state/` is empty. The first `cs`
invocation lands in the **Inert regime** (ADR-016). `cs observe` and
`cs reconcile` work read-only over `archive/`. A future `cs replay`
will reconstruct a projected `state/` index from `archive/events/` for
faster scans; the v1 layout is designed to support it.

## CI gate

Projects that care about archive integrity add a job that pipes
`cs archive list` into `cs archive verify`:

```bash
# Verify the current month on every push.
cs archive list --year "$(date +%Y)" --month "$(date +%m)" --json \
  | jq -r '.entries[].molecule_id' \
  | xargs -I{} cs archive verify {}

# Weekly scheduled job: verify the full history.
cs archive list --json \
  | jq -r '.entries[].molecule_id' \
  | xargs -I{} cs archive verify {}
```

Exit code `1` from any `verify` fails the job. Scoping the per-push
check to the current month keeps CI cost bounded on long-lived
projects; a weekly run over the full history catches drift that
survives a month-rollover.

## Upgrading an existing project

1. **Opt in.** Add `[archive] enabled = true` to `.cosmon/config.toml`.
2. **Back-fill.** Run `cs migrate --archive-past` to iterate every
   terminal molecule currently in `state/` and write its archive entry
   idempotently. Pre-existing molecules will lack response hashes for
   files that have since been deleted; the migrator records only what
   is still on disk.
3. **Verify.** `cs archive list --json | jq -r '.entries[].molecule_id' | xargs -I{} cs archive verify {}`
   should exit `0` on the back-filled tree. A post-migration
   `git status` should show the new `archive/` subtree ready to commit.
4. **Arm retention** (optional). Once the back-fill looks right, flip
   `[archive.retention] keep_all = false` and set `max_age_days` /
   `max_total_mb` to the operator's preference. Always run
   `cs archive prune --dry-run` before the first real prune.

Projects that started after archive shipped already have the subsystem
wired and `[archive] enabled = true` included in the `cs init` template
when the operator opts in.

## Invariants preserved

From ADR-030, these are the archive's **soft-contract invariants**:

- **I1 Terminal closure.** Every terminal molecule's identity, kind,
  final status, timestamp, responses, and incoming edges from other
  terminal molecules are recoverable from `archive/` alone.
- **I2 Causal DAG preservation.** The subgraph induced on terminal
  molecules by typed links is reconstructible exactly. Edges touching
  non-terminal peers record both endpoints; projections are lossless.
- **I3 Content addressability.** Every referenced artifact is either
  inlined or pinned by SHA-256.
- **I4 Monotonicity.** Archive grows monotonically. Corrections are
  append-only records, never in-place edits. `cs archive prune` is the
  single sanctioned exception, and operates under an explicit policy.
- **I5 Surface replayability.** `cs reconcile` run against `archive/`
  alone produces byte-identical `STATUS.md` / `ISSUES.md` projections
  over terminal molecules.

## See also

- [ADR-030 — Cosmon Archive Model](adr/030-cosmon-archive-model.md) — the governing decision record.
- [`docs/architectural-invariants.md`](architectural-invariants.md) — the soft-contract discipline (§8b *propose mechanisms of verification, do not impose them*).
- [`docs/cs-verify.md`](cs-verify.md) — the sibling proof-of-work chain for live molecules (briefing seals).
- [`cs help archive`](../crates/cosmon-cli/src/cmd/examples.rs) — inline help and examples emitted by the CLI.
