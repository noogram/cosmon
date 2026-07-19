# ADR-160: `cs spore export` at conjugation — the ex-post output manifest, the RO-Crate bundle, and `cs spore verify`

**Status:** Accepted (2026-07-20). Doc-only; extends
[ADR-139](139-spore-shareable-polymer-template.md) D2 and narrows
[ADR-140](140-spore-format-expand-deterministic-cache-astra.md) D6. It
**supersedes nothing**: every ADR-140 decision stands as written.
**Date:** 2026-07-20.
**Decider:** Noogram (operator canonisation).
**Scope discipline.** This ADR fixes the **ex-post artefact format, the export
bundle layout, the verification contract, and the ASTRA deferral**. It lands
**no implementation code**. Where ADR-139 fixed the noun and ADR-140 fixed the
contract the *germination* code must satisfy, ADR-160 fixes the contract the
*conjugation* code must satisfy.

**Entry artefact.** Design note
`design-note-expost-manifest.md` produced by noogram molecule
`task-20260627-1176` (parent `delib-20260627-eee8`, Q3/INHERIT-3). This ADR is
the cosmon-ward canonisation the note's §5 requested; its arbitrations are
adopted, not re-litigated, except where they collide with shipped code (§D0).

**Builds on (realize, do not reinvent):**
- [ADR-139](139-spore-shareable-polymer-template.md) D2: portability is
  *conjugation* (export/import); the seal is the ex-ante property. This ADR
  adds the orthogonal ex-post axis to that same table.
- [ADR-140](140-spore-format-expand-deterministic-cache-astra.md) D4 and D6:
  the honest seal-status contract, and the "a spore emits an ASTRA at share
  time" emission point. D6 pinned the **seed-side** payload — its deliverable
  B, `docs/design/spore-toml-annotated.toml`, already fixes
  `[spore.astra] profile = "ro-crate"`, and the shipped exporter writes exactly
  that. What D6 left open is the **harvest-side** payload (nothing describes the
  outputs) and whether a normative `astra.yaml` is also owed. This ADR closes
  the first and answers the second.
- [ADR-039](039-fleet-composability.md) §1: *content-addressing is the
  registry*. The output manifest is that principle applied one scale later —
  to the harvest rather than to the seed.
- ADR-043: the input-hashing machinery; BLAKE3 stays the native key. No new
  hashing primitive is introduced.

---

## Context

A spore proves things **before** it grows. The `.tla` seal gates `expand()` and
fails closed (ADR-139 D3, ADR-140 D4). That flank is strong.

The flank that is open is the one **after** growth. Today, the shipped
`cs spore export <ref>` (crates/cosmon-cli/src/cmd/spore.rs) emits a
content-addressed bundle hash and an RO-Crate-shaped descriptive layer **for
the seed**: name, version, params, nodes, edges, seal presence. That is a
faithful description of the *plan*. It says nothing about what the germinated
mission actually **produced**.

So a recipient who receives a harvest — the synthesis, the report, the figures
— has no way to answer the two questions that matter to someone who will not
re-execute:

1. *Are these bytes the ones that came out of that mission, unaltered?*
2. *Did they exist by a given date?*

The current answer to (2) is the papers pipeline's `ots stamp <pdf>`: one
OpenTimestamps stamp per file, which scales linearly and leaves every unstamped
sibling unattested. The current answer to (1) is: nothing.

The design note's framing, adopted verbatim as the image of record:

> The spore is a seed that proves *before* it grows. What is missing is the
> certificate it emits *after* having grown — the small sealed label stapled to
> the parcel **at the moment of shipping**, never during growth.

The deliberation that produced the note also fixed *where the label must not
go*. shannon refused provenance inside the germination core: a spore already has
deterministic reproduction (`same spore + same params ⇒ same polymer`), so for
ourselves a manifest is parity bits on a noiseless channel. architect countered
that the **external** recipient — the one who will not or cannot re-execute — needs
exactly the redundancy shannon calls waste. wheeler resolved it: *the seed emits
its own stamp at the end of the course.* Per-output provenance lives at
export/share, never in `expand()`.

---

## Decision

### D0 — One verb, two halves of one crate: `--mission` extends `export`, it does not fork it

The design note writes the new gesture as `cs spore export <mission-id>`. The
shipped verb is `cs spore export <spore-ref>`. Dispatching one positional
argument on whether it *looks like* a mission id would be a type-pun in the CLI
surface, and it would silently change the meaning of a command users already
run.

**Decision:** keep the positional `<REF>` meaning exactly what it means today
(the seed), and add an optional flag:

```
cs spore export <ref> [--mission <mission-id>] [--out DIR] [--profile ro-crate|astra]
```

> **Flag naming.** The new flag is `--profile`, not `--emit`. `emit` is already
> taken, with different semantics and a different type: `[spore.astra] emit`
> is a **boolean** governing *automatic* emission at run / mission completion,
> explicitly not the hand-invoked verb (`spore.rs` documents this). `--profile`
> selects the **payload format** and mirrors the stanza's own `profile` key,
> whose sole v0 value is `ro-crate`. Reusing `emit` for an enum would collide
> two unrelated meanings on one word.

- **without `--mission`** — unchanged from today: seed description only. The
  crate carries the plan. Backward-compatible byte-for-byte on the fields it
  already emits.
- **with `--mission`** — the crate additionally carries the **harvest**: the
  ex-post output manifest of §D1, one record per output, plus the OTS stamp of
  that manifest.

The two halves are the same crate because they describe the same object at two
moments. A recipient reads the plan and the result from one directory. This is
a **monotone extension**: no field is removed, no default changes, and an
export without `--mission` produces what it produced before.

> **Deferred, and named honestly.** `cs spore export --mission <id>` cannot
> today resolve the seed from the mission alone, because germination records no
> link back. `germinate()` does tag every nucleated molecule (`temp:warm`, plus
> the `needs-review*` and `reviewer-adapter:*` tags when cross-provider review
> is on), but **no tag or field carries the spore's bundle hash**, so a mission
> cannot name the seed it grew from. Closing that requires one cheap addition — germination stamps each
> molecule with a `spore:<bundle-hash>` tag, reusing the existing tag primitive
> and no new field. Until that lands, `<ref>` stays **required**. When it lands,
> `<ref>` becomes optional-if-`--mission`-resolves, which is again monotone.
> This is a follow-up task, not part of this ADR.

### D1 — The output manifest: canonical JSON, per-output, double-hashed

The payload is a single file, `manifest.json`, in **canonical JSON**: keys
sorted, LF endings, UTF-8, no floats. Canonical because the manifest is itself
content-addressed, and a serializer with latitude would break that. Not YAML,
for the same reason.

```jsonc
{
  "manifest_version": "1",
  "kind": "spore-output-manifest",

  // upstream identity: the ex-ante chain, REFERENCED, not re-proved
  "spore": {
    "id": "blake3:<bundle_hash of the .spore>",
    "sealed": true,
    "seal": "blake3:<hash of the .tla module>",
    "seal_verified": false
  },
  "params_digest": "blake3:<hash of the canonical-JSON params>",
  "mission_id": "mission-20260627-xxxx",

  // the payload: one record PER output
  "outputs": [
    {
      "path": "outputs/synthesis.md",
      "blake3": "<hash>",
      "sha256": "<hash>",
      "bytes": 14213,
      "media_type": "text/markdown",
      "produced_by": {
        "molecule": "task-20260627-1176",
        "formula": "task-work",
        "formula_seal": "blake3:<existing prompt_seal>"
      }
    }
  ]
}
```

Four format decisions:

- **D1a — double hash, each with a job.** `blake3` is the native key: every
  cosmon seal already speaks BLAKE3 (`prompt_seal`, `briefing_seals`,
  `cs verify`, `FleetId` ADR-039, and `bundle_hash` in the shipped exporter).
  `sha256` is the interop hash: it is what the RO-Crate vocabulary and the
  OpenTimestamps ecosystem expect. Emitting both is cheaper than either forcing
  the outside world to speak BLAKE3 or renouncing our own idiom internally.
- **D1b — the manifest is itself content-addressed, and that is what gets
  stamped.** Its BLAKE3 is the identity of the harvest; its SHA-256 is what
  `ots stamp manifest.json` anchors. **One stamp covers every output.** This is
  the precise upgrade over today's papers pipeline: `ots stamp <pdf>` timestamps
  *one* file; stamping the manifest timestamps *the set*, and each output stays
  individually tamper-detectable through its manifest row.
- **D1c — the ex-ante chain is referenced, never re-derived.** `spore.seal`
  says *the seed was sealed*; it does not carry the proof. A recipient wanting
  the proof fetches the `.spore`, which contains it. `seal_verified` is present
  and honest, carrying forward the ADR-140 D4 rule verbatim: **the manifest
  never claims a seal was verified when TLC did not run.** The manifest is the
  descriptive layer; the spore remains the lossless channel.
- **D1d — no `decision` field.** `decision` is a false friend across the two
  vocabularies (a methodological choice in ASTRA; a governance verdict in
  cosmon). It is disarmed by *absence*. The manifest speaks files and hashes and
  nothing else. The annotation layer (decision / options / rationale) is a
  separate future object.

### D2 — `cs spore verify`: trust without re-execution, fail-closed

A fourth verb joins `validate` / `run` / `export`:

```
cs spore verify <bundle-dir | bundle.zip>
```

Contract:

1. recompute `blake3` + `sha256` for every output listed in the manifest;
2. compare against the manifest rows;
3. verify the manifest matches its own content address;
4. if `manifest.json.ots` is present **and** the network is reachable, verify
   the timestamp; if absent or offline, report `timestamp: not checked` — never
   `verified`;
5. **PASS / FAIL, fail-closed**: any divergence, any missing listed output, any
   unparseable row is FAIL with a non-zero exit (ADR-119 exit-code contract).

An output present in the bundle but *absent* from the manifest is a **WARN, not
a FAIL** — the manifest attests what it lists; unlisted bytes are simply
unattested, and conflating "unattested" with "tampered" would make honest
partial exports impossible. The distinction is printed, never silent.

> **Deliberate divergence from the entry note.** The design note writes
> *« tout écart = FAIL, exit ≠ 0 »*. This ADR keeps that verdict for every
> **listed** output (rows 1-3 above are strictly fail-closed) and carves out the
> single case of an *unlisted extra file*, which is not a divergence between
> manifest and bytes but an absence of a claim. Under the note's literal rule a
> recipient could never add a README to a bundle without turning it red. The
> fail-closed property that matters — no listed row may silently pass — is
> untouched.

`verify` is the recipient's path to trust at the cost of a hash-and-compare, so
it must run on a **bare bundle**, with no cosmon state store and no fleet. It
reads only the bundle directory. It is the ex-post twin of `cs verify` (which
certifies prompt seals ex-ante), and it is deliberately **not** the same verb:
one certifies a process, the other certifies bytes.

### D3 — RO-Crate is the bundle; the manifest travels inside it

**This layout is the `--mission` form only.** Without `--mission`, D0 holds and
the output is unchanged from today: a single `ro-crate-metadata.json` written
into `--out` (or the manifest directory), describing the seed. Adding
`--mission` is what promotes the output from one file to a bundle directory,
because only then is there a harvest to carry.

With `--mission`, the export writes a directory (zippable) conforming to
RO-Crate 1.1, profile *Workflow Run RO-Crate*:

```
<mission-id>-crate/
├── ro-crate-metadata.json   # JSON-LD, generated
├── manifest.json            # the §D1 manifest, carried as a data file
├── manifest.json.ots        # the OpenTimestamps stamp
├── spore.toml               # the seed (wiring manifest, ADR-139/140)
├── seal.tla                 # the TLA+ module, if the spore was sealed
└── outputs/                 # the harvest
```

Mapping table (Provenance Run Crate profile, extended):

| cosmon object | RO-Crate / Schema.org term | note |
|---|---|---|
| `spore.toml` | `File` + `ComputationalWorkflow` | the profile's "workflow" |
| germination → mission | root `CreateAction` (`instrument` → the workflow) | one run, one action |
| each molecule of the polymer | child `CreateAction` | per-step granularity |
| each output | `File` with `contentSize`, `encodingFormat`, `sha256` | hash taken from the manifest |
| `params_digest` | `PropertyValue` on the root action | digest, not values, when confidential |
| fleet / roles / personas | `SoftwareApplication` as `agent` / `instrument` | cosmon has this layer |
| `seal.tla` | `File` + `CreativeWork`, `subjectOf` the root action | **outside the standard vocabulary**: custom term `cosmon:formalSeal` in the crate's local JSON-LD context. The profile does not normalize it and this ADR does not pretend it does. |
| `manifest.json` | `File`, `about` → the root action | the label travels in the parcel |

- **D3a — flat JSON generation, zero ontological dependency.** The
  `ro-crate-metadata.json` is JSON-LD *written as JSON*: a context URL plus one
  namespaced custom term. **No LinkML, no OWL, no triple store, no reasoner** —
  the deliberation's refusal, honored. A template and a loop over the manifest
  suffice, which is what the shipped `build_astra()` already does.
- **D3b — the crate embeds the manifest, it does not replace it.** The manifest
  is the minimal verifiable object (a thirty-line checker); the crate is the
  archivable, citable envelope (Zenodo, WorkflowHub). Two consumers, two layers,
  one content. A recipient in a hurry checks the manifest; an archivist deposits
  the crate.
- **D3c — the seal travels as an honest attachment.** We do not invent a
  formal-proof vocabulary inside RO-Crate. We attach the `.tla` and describe it
  under a `cosmon:` namespace. Flat files until proven insufficient by a real
  consumer.

### D4 — ASTRA is deferred, and the deferral is fail-closed

ADR-140 D6 says a spore emits "an ASTRA-compatible descriptive layer" and the
annotated schema pins `profile = "ro-crate"`. This ADR resolves the remaining
ambiguity in that sentence, in the only honest direction available:

**RO-Crate now; normative `astra.yaml` deferred.** Three reasons:

1. The normative ASTRA schema **could not be read** — its sub-pages 404, and the
   fields the deliberation worked from were inferred from a rendered example.
   Emitting a file claiming conformance to a schema nobody has read is
   manufacturing conformance: form without substance.
2. ASTRA's naming is still molten upstream. RO-Crate is stable and versioned.
3. The day of real contact with the ASTRA authors, "a spore emits an ASTRA"
   becomes an hour of mapping *with their schema in hand*, and a relational
   gesture rather than a guess.

Surface consequence: `cs spore export --profile astra` is **reserved and fails
closed** with an honest message —

```
error: ASTRA schema not yet pinned (normative spec unread; see ADR-160 D4).
       Use --profile ro-crate (default).
```

Fail-closed extends into interop: a flag that would produce a plausible-looking
file against an unread schema refuses instead. `--profile ro-crate` is the default
and the only value that produces bytes.

This **narrows** ADR-140 D6 without contradicting it: the emission *point*
(share time, `cs spore export`) and the *compose-do-not-reinvent* stance are
unchanged; only the payload is now pinned to the one schema we have actually
read.

### D5 — Export is terminal-only, optional, and idempotent (core non-contamination)

Four invariants, stated so that a future implementer can fail a review against
them:

1. **`expand()` does not change by one line.** Signature, semantics, TLA+ seal:
   untouched. The manifest is never an input nor an output of `expand()`.
2. **The seal is neither weakened nor extended.** It keeps gating germination
   (ex-ante). The manifest proves *nothing* about behaviour; it attests *what
   came out* (ex-post). Two objects, two moments, zero coupling.
3. **A spore without an export is a complete spore.** Export is optional and
   idempotent — same mission, same bytes, same manifest. No internal path
   (`nucleate` / `tackle` / `wait` / `done`, feedback loops) may ever depend on
   a manifest existing.
4. **Emission at end of course only.** `--mission` fails closed unless the
   mission is in a **terminal, harvested** state. One does not stamp an organism
   still growing — the same rule that makes the papers pipeline stamp only after
   the visual gate.

### D6 — The ex-post axis extends ADR-139 D2's state table, orthogonally

ADR-139 D2 gave a spore three seal-and-sharing states: *unsealed*,
*sealed-but-private*, *sealed-and-shared* — all of them properties of the
**seed**. This ADR adds an axis about the **harvest**:

| ex-post state | meaning |
|---|---|
| **harvest-unattested** | outputs exist; no manifest. Today's world. |
| **harvest-manifested** | `manifest.json` present; every output content-addressed and `verify`-able offline. |
| **harvest-stamped** | manifest additionally OTS-stamped; existence anchored in time. |

The axis is **orthogonal to the seal**: a sealed spore can have an unattested
harvest, and an unsealed spore can have a stamped one. They answer different
questions — *will it behave?* versus *is this what came out?* — so no state on
one axis implies anything about the other. Speaking them as one would be exactly
the conflation this ADR exists to prevent.

Resulting card:

| | before | after |
|---|---|---|
| ex-ante | TLA+ seal gates `expand()` — **strong** | identical, **intact by construction** (D5) |
| ex-post | one OTS stamp on one file — **weak** | per-output tamper-detectable manifest, offline `verify` without re-execution, archivable RO-Crate, one stamp covering the set — **flank closed** |

---

## Consequences

- **The ex-post flank is closed at low cost and with no bespoke format.** BLAKE3
  is already ours, SHA-256 and RO-Crate are already the world's, OpenTimestamps
  is already in the papers pipeline. The only new bytes are the manifest schema.
- **A recipient can trust without re-executing.** `cs spore verify` on a bare
  bundle, no cosmon runtime required, is the whole recipient-side story.
- **One OTS stamp replaces N.** Stamping the manifest instead of each artefact
  turns a linear cost into a constant one, and makes previously-unstamped
  siblings attested.
- **The germination core is provably untouched.** D5's four invariants are
  written as review-failable statements, not as intentions.
- **ADR-140 D6 is narrowed, not reversed.** The emission point stands; the
  payload is now the one schema we have read.
- **The CLI surface grows by one verb and one flag.** `cs spore verify` is new;
  `cs spore export` gains `--mission` and `--profile`. Per the repo convention,
  the implementing change must update `cs help` and `man cs` in the same PR.
- **Rollback path:** doc-only. `git revert` of the introducing commit. Every
  cited primitive (`expand()`, the seal gate, `bundle_hash`, `build_astra`,
  BLAKE3 seals) is untouched and remains available regardless.

### Explicit non-goals (v0)

- **No `decision` / `options` / `rationale` annotation layer.** Separate object;
  the false friend is disarmed first (D1d).
- **No provenance inside `expand()` or the germination core.** Refused: parity
  bits on a noiseless channel (D5.1).
- **No LinkML / OWL / JSON-LD as a hard dependency.** Refused until a real
  consumer demands it (D3a).
- **No normative `astra.yaml`.** Deferred, fail-closed, until the schema is read
  (D4).
- **No consuming another spore's stamp as a trust input** (the "commons"). Named
  by the deliberation, designed separately.
- **No migration of the papers `bake.sh` pipeline onto this manifest.** Same
  pattern (stamp-the-set), but papers are not spores. The pattern is noted; the
  migration is not a goal here.
- **No registry.** How a bundle is published and discovered remains the separate
  design question ADR-139 and ADR-140 both flagged.

---

## References

- Entry design note: noogram `task-20260627-1176`
  `design-note-expost-manifest.md` (parent deliberation `delib-20260627-eee8`,
  Q3/INHERIT-3; personas architect §Q3-Q4, shannon §5, wheeler's resolution).
- [ADR-139](139-spore-shareable-polymer-template.md) — the `spore` primitive,
  `germinate`, D2 (seal + conjugation), D3 (the seal gates `expand()`).
- [ADR-140](140-spore-format-expand-deterministic-cache-astra.md) — the format,
  `expand()` semantics, D4 (honest seal status), D6 (share-time emission).
- [ADR-039](039-fleet-composability.md) §1 — *content-addressing is the
  registry*.
- [ADR-119](119-adapter-exit-code-contract.md) — the non-zero-exit contract
  `cs spore verify` inherits.
- [`docs/design/spore-toml-annotated.toml`](../design/spore-toml-annotated.toml)
  — `[spore.astra]`, whose `profile = "ro-crate"` this ADR confirms as the only
  emitting value for v0.
- `crates/cosmon-cli/src/cmd/spore.rs` — the shipped `validate` / `run` /
  `export` surface this ADR extends (`bundle_hash`, `build_astra`).
- RO-Crate 1.1 and the Workflow Run RO-Crate profile
  (researchobject.org / WorkflowHub) — the community standard composed with.
