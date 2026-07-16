# `ClaimEmitted` / `ClaimVerified` — IFBDD instrumentation

Two events in [`cosmon-core::event`](../crates/cosmon-core/src/event.rs)
that carry the verifiable-claim pipeline: **Instrument First, Before
Drawing Dragons**. Parent deliberation:
[`delib-20260412-2946`](./lore/).

## Why these events exist

Before we spend compute on verifiers (DOI lookup, cargo-check, URL
reachability, …) we need to measure the noise channel itself — how many
claims each molecule emits, how many are verified, and what the per-kind
latency cost looks like. `ClaimEmitted` makes the raw extraction
observable; `ClaimVerified` closes the loop with a verdict and a cost.

The correlation key is `claim_id` (a `ClaimId` newtype). Every
`ClaimVerified` references the `ClaimId` of the `ClaimEmitted` it
resolves. A claim emitted but never verified is a **channel leak** and
shows up directly in event-log aggregation.

## Event shapes

### `ClaimEmitted`

```rust
ClaimEmitted {
    claim_id: ClaimId,              // correlation key
    molecule_id: MoleculeId,        // source molecule
    step_index: usize,              // producing step (0-based)
    claim_type: ClaimType,          // Citation | Numeric | Code | Link | Factual
    claim_text: String,             // verbatim excerpt
    source_span: Option<(usize, usize)>, // byte offsets in source artifact
}
```

Emitted by the claim-extraction pass, **before** any verifier runs.

### `ClaimVerified`

```rust
ClaimVerified {
    claim_id: ClaimId,              // matches the ClaimEmitted above
    verifier_kind: String,          // e.g. "doi-lookup", "cargo-check"
    verdict: Verdict,               // Confirmed | Refuted | Inconclusive
    cost_latency_ms: u64,           // wall-clock of the verifier
    evidence_ref: Option<String>,   // URL, path, or CAS hash
}
```

Emitted once per verifier invocation. Several verifiers MAY run against
one claim (voting); each emits its own `ClaimVerified`.

## Producer & consumer roles

- **Producer (worker-side).** A formula step that extracts claims
  (regex on citations, code-fence capture, numeric tokenizer, …) calls
  the adapter in
  [`cosmon-cockpit::adapter`](../crates/cosmon-cockpit/src/adapter.rs)
  to emit `ClaimEmitted` onto the fleet event log.
- **Consumer (verifier-side).** A verifier tool reads pending
  `ClaimEmitted` events (those with no matching `ClaimVerified` yet),
  resolves them against an external oracle, and emits `ClaimVerified`.
  Verifiers are independent molecules or external processes — cosmon
  itself does not privilege any verifier kind.

## Minimal example

```rust
use cosmon_core::event::{Event, ClaimType, Verdict};
use cosmon_core::id::ClaimId;

let cid = ClaimId::new_v4();

// Worker extracts a citation from its synthesis.md:
Event::ClaimEmitted {
    claim_id: cid.clone(),
    molecule_id: mol_id.clone(),
    step_index: 2,
    claim_type: ClaimType::Citation,
    claim_text: "Paleologo (2025), §4.2".into(),
    source_span: Some((1204, 1228)),
};

// A DOI-lookup verifier responds:
Event::ClaimVerified {
    claim_id: cid,
    verifier_kind: "citekey-resolve".into(),
    verdict: Verdict::Confirmed,
    cost_latency_ms: 87,
    evidence_ref: Some("zotero://select/items?key=ABCD1234".into()),
};
```

## Aggregation

```
# count emitted vs verified per molecule
grep '"kind":"claim_emitted"'  .cosmon/state/fleets/default/events.jsonl | jq -s 'length'
grep '"kind":"claim_verified"' .cosmon/state/fleets/default/events.jsonl | jq -s 'length'

# per-verifier cost distribution
grep '"kind":"claim_verified"' .cosmon/state/fleets/default/events.jsonl \
  | jq -r '[.verifier_kind, .cost_latency_ms] | @tsv' \
  | sort | awk '{ sum[$1]+=$2; n[$1]++ } END { for (k in sum) printf "%s\t%d\t%d\n", k, n[k], sum[k]/n[k] }'
```

## Relationship to `cs verify`

`cs verify` checks the **artifact chain** of a completed molecule
(hashes + gates + event-log). `ClaimEmitted`/`ClaimVerified` check the
**content** of artifacts against the outside world. They are
complementary and both exit 0/1 independently.

## Invariants

- Both variants are `#[non_exhaustive]` at the `ClaimType` / `Verdict`
  level: new kinds ship without breaking downstream `match` sites.
- Every `ClaimVerified.claim_id` MUST reference a prior `ClaimEmitted`
  on the same event log. Orphan verifieds are a bug.
- `cost_latency_ms` is wall-clock of the verifier invocation only — not
  queue time. This is the cost signal for the IFBDD trade-off.

## See also

- [`cs verify`](cs-verify.md) — artifact proof-of-work chain
- [events-jsonl-merge.md](events-jsonl-merge.md) — event log semantics
- [ifbdd-methodology](lore/2026-04-12-ifbdd-methodology.md)
