# `cs verify` — proof-of-work chain verification (MVP)

Re-checks a completed molecule's **proof-of-work chain** after the fact:
artifact hashes, formula gate replay, and the existing event-log chain.
Implementation: [`crates/cosmon-cli/src/cmd/verify.rs`](../crates/cosmon-cli/src/cmd/verify.rs),
[`crates/cosmon-cli/src/pow.rs`](../crates/cosmon-cli/src/pow.rs).

## Model

Each completed molecule carries a sealed manifest `verify.json`, written
by `cs complete`. It contains BLAKE3 hashes of the durable markdown
artifacts present at completion time:

```
prompt.md      → nucleation seed
briefing.md    → step-by-step plan
frame.md       → (formula-specific) framing
synthesis.md   → final artifact
log.md         → append-only history
```

`cs verify <mol>` recomputes the hashes, compares against the seal,
replays the formula's `command =` gates (shell + native), cross-checks
the `events.jsonl` chain, and writes `verify-report.md` to the molecule
directory. Exit `0` on pass, `1` on any failure.

## Usage

```
cs verify <mol>                 # per-molecule audit (human report + exit code)
cs verify <mol> --json          # NDJSON report for scripts
cs verify --federation          # fleet-wide cross-galaxy provenance audit
cs verify <mol> --federation    # per-molecule, plus federation provenance
cs verify --invariants          # fleet-wide archived⇒terminal ghost audit
cs verify <mol> --invariants    # per-molecule, plus the invariant check
```

### What is checked

1. **Artifact chain.** For every entry in `verify.json.artifacts`,
   recompute BLAKE3 and compare to the sealed hex. Missing files are a
   FAIL (the seal is the source of truth — you cannot "optionally drop"
   `synthesis.md`).
2. **Gate replay.** Every formula step with a `command =` or `native =`
   field is re-run in the molecule's worktree. Non-zero exit = FAIL.
3. **Event-log chain.** The existing `events.jsonl` hash chain is
   re-walked from genesis to `Completed`.
4. **Federation provenance (opt-in via `--federation`).** Walk the
   fleet-wide `events.jsonl` (or the molecule's local one when
   `<mol>` is given), classify each event as cross-galaxy, and FAIL
   any cross-galaxy event whose `federation_provenance` is `None`.
   Four event variants are scanned:

   - `MergeDispatched` / `MergeCompleted` — cross-galaxy iff the
     `molecule_id` or `branch` carries a foreign galaxy alias
     (Oracle B subject-mark per ADR-105 §D3).
   - `ChronicleAdded` — cross-galaxy iff `cites_galaxies` mentions a
     non-cosmon peer (W7 ADR-105 machinery).
   - `AdrInscribed` — cross-galaxy iff `cites_galaxies` mentions a
     non-cosmon peer (W7 ADR-105 machinery).

   The discipline is named in
   [ADR-105](adr/105-i9-prime-federation-provenance.md) (I9'). Use
   `--legacy-tolerate-before YYYY-MM-DD` to downgrade pre-cutoff
   events to SKIP during a backfill window — escape hatch only,
   revert once `cs delegate` has restamped legacy lineage.
5. **State-machine invariants (opt-in via `--invariants`).** Enforce
   `archived ⇒ status.is_terminal()`: an archived molecule must carry
   a terminal status (`completed`/`collapsed`). A row with
   `{archived: true, status: running}` is a *ghost* — torn down
   out-of-band without terminalizing — and is a FAIL. Fleet-wide when
   no `<mol>` is given (the galaxy-wide audit), per-molecule when one
   is. Detect-only; heal the on-disk rows with
   `cs reconcile --heal-invariants`. See `idea-20260618-1b10`.

### What is NOT checked (yet)

- **LLM determinism.** Claude output is not byte-equal across runs;
  verifying semantic equivalence is a separate formula
  (future `verify-semantic`).
- **Cross-molecule dependencies.** Verifying a leaf does not re-verify
  its blockers. Walk the DAG with `cs deps <mol> --json` + `xargs cs
  verify` if you need that.
- **External world state.** Citations, URLs, and third-party services
  are the `ClaimEmitted`/`ClaimVerified` pipeline's job
  ([events-claim.md](events-claim.md)), not `cs verify`'s.

## Example

```
$ cs verify task-20260414-a78b
ok    artifact  prompt.md       blake3 cafebabe
ok    artifact  briefing.md     blake3 deadbeef
ok    artifact  synthesis.md    blake3 1337...
ok    gate      step 2          cargo check --workspace
ok    events    chain           genesis → completed  (42 events)
PASS  task-20260414-a78b

$ echo $?
0
```

A failing verify leaves `verify-report.md` in the molecule directory
describing exactly which artifact, gate, or event broke.

## See also

- [spore-reproducibility](design/spore-reproducibility.md): the trust spectrum
  that generalizes this surface one scale up. A molecule's seal certifies one
  past trace; a [`spore`](vocabulary.md#spore)'s seal certifies the safety of a
  whole orchestration over the space of bodies it can germinate (ADR-139).
- [molecule artifacts in CLAUDE.md](../CLAUDE.md) — proof-of-work trail
- [events-claim.md](events-claim.md) — runtime claim verification (IFBDD)
- [events-jsonl-merge.md](events-jsonl-merge.md) — event log chain rules
