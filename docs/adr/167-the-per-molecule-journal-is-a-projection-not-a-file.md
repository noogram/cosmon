# ADR-167 — The per-molecule journal is a projection, not a file

**Status:** Accepted (2026-07-30).
**Date:** 2026-07-30.
**Decider:** Noogram.
**Authoring task:** `task-20260730-7a74`.

**Entry artefact.** The operator's D4 on `delib-20260729-1d4e` (the
`cosmon-dev` spore retrospective): delete the spore's `trace` node — an agent
paid to be `cp`, holding a fleet slot, never once cited — and file the primitive
it was compensating for in the core, because patching an application for what
belongs in the core is forbidden.

**Related ADRs:**
[ADR-166](166-the-root-to-uid-demote-path-is-refused.md) (whose closing clause
this closes),
[ADR-055](055-cosmon-residence.md),
[ADR-030](030-cosmon-archive-model.md).

---

## Context

### What a molecule could say about itself, before this

Nothing, reliably. A molecule directory *sometimes* holds an `events.jsonl`:
164 of 389 directories in the development galaxy carry one, and 152 of those
164 contain nothing but `adapter_pane_signature_checked` — a diagnostic probe.
Four event types appear anywhere in them. It was never a journal; it was a
place some probes happened to land.

The galaxy ledger `.cosmon/state/events.jsonl` is the one real journal. It
exists from `cs init` onward and every verb appends to it.

### The case that has nothing to show

The molecule that most needs a journal is the one with the fewest artefacts. A
dispatch refused at the entry of `cs tackle` — ADR-166's root-spawn refusal —
produces no worker, no worktree, no branch, no responses and no molecule
directory to speak of. Everything an operator can learn about it is one ledger
row.

ADR-166 stated the consequence honestly and left it open: the refusal is
recorded once, at fleet scope, and "molecule-scoped tooling will not show why
that molecule never started." It also named the trap in the obvious repair —
*"Do not 'fix' that by creating the file here. Creating it is the residue."* A
root dispatcher creating a per-molecule file on a galaxy whose worker uid is
not root is exactly the residue the refusal exists to prevent.

### The shape that must not be rebuilt

`record_root_spawn_refusal` wrote its line to **two** sinks: the galaxy ledger
and, "defensively", a molecule-local one. That is the COSMON-DEV #20 defect in
miniature. Two writers means two truths, and the truth an operator reads is
whichever file happened to be writable — which is how a refusal came to *look*
recorded.

## Decision

**A molecule's journal is a view computed from the galaxy ledger. There is no
per-molecule journal file, and nothing writes one.**

`cosmon_state::journal::MoleculeJournal::project` folds ledger lines down to
the rows naming one molecule. `cs events journal <id>` renders it; `--json`
emits the projected rows verbatim.

The operator's five clauses are not five mechanisms. They are five consequences
of choosing a projection over a file:

- **A projection of the ledger, never a second file.** There is no second file
  to disagree with the first. The class of defect #20 belongs to is not guarded
  against; it is inexpressible.
- **Exists from nucleation.** `cs nucleate` appends `molecule_nucleated`, so
  the view is non-empty from that instant — with nothing created. This is the
  clause whose natural implementation is the wrong one, and the projection is
  what makes it safe: a view that is computed rather than stored leaves a root
  dispatcher nothing to leave behind.
- **Contains blockages where the worker produced nothing.** A refusal is a
  ledger row like any other. `is_blockage_type` classifies the failure family —
  `tackle_refused`, `worker_spawn_failed`, `gate_failed`, the `sf1`…`sfN`
  structured failures by prefix — so the render marks them rather than
  requiring the reader to know which type tags mean failure.
- **Survives teardown and archival.** The ledger lives in `.cosmon/state/`,
  not in the worktree `cs done` destroys. Archival additionally materialises
  the rendered projection as `journal.jsonl` inside the archive entry, so the
  history is readable from the archive alone.
- **Mechanically reconstructible.** The view *is* the reconstruction.

### Two consequences that follow, and are enforced

**The refusal recorder now writes one sink.** The molecule-local write is
removed. Molecule-scoped visibility comes from the projection instead, so
ADR-166's open consequence closes without adding a writer: `cs events journal`
on a molecule refused before it ever started prints the refusal and its cause.

**The fold reads JSON, not typed events.** `EventV2` is `#[non_exhaustive]` and
serde-tagged, so a row whose `type` this binary does not know fails to
deserialize — and `tackle_refused` is deliberately such a row, written as raw
JSON before any typed machinery exists. A typed projection would drop the one
entry the operator most needs. The journal's job is to lose nothing, not to
interpret everything.

### What is materialised, and why that is still one writer

`journal.jsonl` in the archive entry is a *rendering* of the fold, not an
independently authored record: every byte comes from
`MoleculeJournal::project` over the same ledger.
`an_archived_journal_equals_a_fresh_projection` asserts the file equals a fresh
projection, so it cannot drift into being a truth of its own. If that test ever
goes red, the file has stopped being a projection and the correct repair is to
delete the file, not to bless the test.

## What carries the claims

- `projection_is_reconstructible_from_the_ledger_alone` — render the view,
  delete it, rebuild it from the ledger, compare bytes. Without this test the
  projection claim is decoration.
- `projecting_a_journal_leaves_the_galaxy_byte_identical` (unit) and
  `projecting_a_journal_creates_nothing_and_appends_no_row_of_its_own`
  (through the real `cs` binary) — the residue property.

  The CLI one deliberately does **not** claim byte-identity, and the reason is
  ADR-166's own lesson. Every `cs` invocation appends one ambient
  `operator_present` row from `main` before any subcommand runs. A byte-identity
  claim would be false for a reason having nothing to do with this command, and
  the usual repair — narrowing to some convenient path — is precisely how
  ADR-166 acquired a test that passed while the residue it was written for went
  unnoticed. So the test pins what is actually about the projection: the set of
  paths is unchanged, and every row the ledger gained is an ambient presence
  row.
- `a_molecule_refused_on_its_first_dispatch_has_a_journal_that_says_why` — the
  case the primitive exists for.
- `a_molecule_that_was_only_nucleated_already_has_a_journal` — the
  exists-from-nucleation clause, with no file created.
- `an_archived_journal_equals_a_fresh_projection` — survives archival, and
  stays a projection while doing so.

## Consequences

- ADR-166's closing consequence ("molecule-scoped tooling will not show why
  that molecule never started; the fleet ledger is where the answer lives") is
  superseded in its *tooling* half. Its structural half is unchanged and is
  what this rests on: the fleet ledger is still where the answer lives — the
  journal reads it rather than copying it.
- Anything wanting per-molecule history calls the projection. A future
  contributor who finds a per-molecule file "missing" is looking at a caller
  the projection has not reached yet, not at a copy the ledger is missing.
- The scope this does **not** cover, named rather than implied: the ledger is
  the substrate, so a retention policy that prunes it prunes the journals of
  anything not yet archived. The archive materialisation is the durable half;
  a retention-aware projection is not attempted here.
- The `trace` node in `spores/cosmon-dev/` is deleted by a separate molecule on
  a serialized chain. This ADR does not touch the spore.
