---
title: "spore implementation: the DAG manifest the pilot foams"
status: design-note
relates_to: ADR-139, ADR-140, ADR-043, ADR-026, ADR-038, ADR-039
---

# Spore implementation: the DAG to foam

DELIVERABLE C of the `spore` design keystone. ADR-140 fixed the format and the
contract; this note is the **impl decomposition** the pilot will foam (the
nodes, the typed `blocked-by` edges, the gates). It is design only: it names the
work and its ordering, it does not do it.

Each node below is a future cosmon molecule (kind in brackets). The edges are
`blocked-by` (the arrow points from blocker to dependent). The whole graph is
gated by the cosmon Definition of Done (`cargo check` + `test` + `clippy` +
`fmt`), plus a public-surface banlist gate and an em-dash gate on outgoing
surfaces.

## The graph at a glance

```
N1 deterministic-trait+cache  (task)
        |
        v
N2 spore-schema-parser  (task) <----------------- (reads ADR-140 + annotated toml)
        |
        v
N3 expand()-pure-fn  (task)
        |
        +-------------------+-------------------+
        v                   v                   v
N4 seal-verify-contract  N5 cs-spore-run-CLI   N7 e2e-fixture  (task)
   (task)                   (task)                  |
        |                   |                       |
        |                   v                       |
        |               N6 cli-docs+help+man (task) |
        |                   |                       |
        +-------------------+-----------+-----------+
                                        v
                              N8 verify-gate  (task)
```

## Nodes

### N1: the `deterministic` trait + content-addressable cache  [task]
- **Does:** add `deterministic: bool` (default false) to `Formula` (parse from
  `.formula.toml`); implement the content-addressable molecule cache keyed by
  `BLAKE3(formula_id || resolved_vars || sorted(input_artifact_hashes))`,
  reusing ADR-043's input hashing. Deterministic molecule with a cache hit skips
  execution and links the cached artifact; miss runs and populates the cache.
  Couple `verify_requires_execution = !deterministic`.
- **Blocked-by:** none (foundational; absorbs OxyMake into cs).
- **TDD:** cache-hit-skips, cache-miss-runs-then-hits, agentic-never-cached,
  hash-changes-on-var-change. Stable tier (property test on hash stability).
- **Why first:** the cache trait is independent of the spore format and unblocks
  the determinism story the seal contract leans on.

### N2: the `spore.toml` schema parser  [task]
- **Does:** parse the ADR-140 schema into a `Spore` domain type in
  `cosmon-core`: `[spore]`, `[spore.seal]`, `[spore.params.*]`, `[spore.fleet]`,
  `[spore.formulas.*]` (with `deterministic`), `[[spore.node]]` (with `kind` +
  optional `[bounds]`), `[[spore.edge]]`, `[spore.astra]`. Reject: emergent node
  without `[bounds]`; edge cycle; unknown node kind; param type mismatch.
- **Blocked-by:** N1 (the formula trait the `[spore.formulas]` table references).
- **TDD:** the workshop prototype `spore.toml` parses; an emergent-without-bounds
  toml fails to load with a clear error; a cyclic edge set fails.

### N3: `expand(spore, params)` pure function  [task]
- **Does:** the ADR-140 D3 algorithm: validate params, resolve fixed nodes,
  expand pre-determined fan-outs over param lists, emit emergent-zone controller
  nodes carrying their bounds, topologically order by typed edges, return the
  ordered `[NucleateCall { formula, vars, blocked_by, alias }]`. Pure: no I/O,
  no clock, no randomness. Lives in `cosmon-core`.
- **Blocked-by:** N2.
- **TDD:** prototype expands to the expected ordered call list; same spore +
  same params yields a byte-identical list (determinism property test); missing
  required param refuses; the list replays top-to-bottom with every
  `--blocked-by` alias already defined.

### N4: the seal-verification contract  [task]
- **Does:** the ADR-140 D4 three-state contract (absent / present+checked /
  present+unchecked-honest). Detect TLC/JRE availability; run TLC on the seal
  when present; cache the verdict against `BLAKE3(spore.tla || spore.cfg)`.
  Default fail-closed on a present-but-unverified seal; `--allow-unchecked-seal`
  opts in. Never report a seal as verified when it is not.
- **Blocked-by:** N3 (`expand()` is what the seal gates).
- **TDD:** absent seal germinates with `seal: none`; present+JRE runs TLC and
  caches; present+no-JRE refuses by default and germinates under the flag with
  an honest `NOT verified` line; edited proof invalidates the cached verdict.
- **Dependency flag:** JRE/TLC is an external dependency the machine may lack.
  This node MUST surface that honestly, not silently pass. (The workshop seal has
  never been TLC-checked for exactly this reason.)

### N5: `cs spore run` CLI surface  [task]
- **Does:** the `cs spore` verb family. `cs spore run <ref> --var k=v ...`
  parses the spore (N2), validates + expands (N3), gates on the seal (N4), then
  executes the returned `cs nucleate ... --blocked-by ...` list against the live
  state store. `--json` NDJSON output (agent-first invariant). Sibling verbs as
  scoped: `cs spore validate` (parse + expand dry-run, no germination),
  `cs spore export` (content-addressed bundle + ASTRA emission, ADR-140 D6).
- **Blocked-by:** N3, N4.
- **TDD:** `cs spore validate` on the prototype prints the expansion without
  nucleating; `cs spore run` germinates a real polymer; `--json` is valid NDJSON.

### N6: CLI docs + `cs help` + `man cs`  [task]
- **Does:** fill the surfaces the audit found empty. `cs help spore`, `cs spore
  --help` per subcommand, the `man cs` `spore` section, and `docs/` CLI pages.
  Conceptual docs (vocabulary/README/book) arrive via the separate doc strand
  (`edit-20260628-e909`, already harvested); this node is the CLI reference.
- **Blocked-by:** N5 (document the surface that exists, not a guess).
- **Gate:** CLI-doc-sync discipline (any `cs` surface change updates `cs help` +
  `man cs` in the same change).

### N7: end-to-end fixture  [task]
- **Does:** wire the workshop `grace-business-analysis` bundle as the e2e fixture.
  `cs spore validate` then `cs spore run` on it must germinate a real polymer and
  emit an ASTRA. The fixture is citation-only (it lives in workshop); the test
  references it without copying proprietary content (the bundle is 100% public).
- **Blocked-by:** N5 (needs the runnable verb). Can develop in parallel with N6.
- **TDD:** full germination produces the expected node set; the seal gate fires;
  an ASTRA `ro-crate-metadata.json` is emitted.

### N8: the verify gate  [task]
- **Does:** the closing gate. `cargo check` + `test` + `clippy -D warnings` +
  `fmt --check` green across the workspace; public-surface banlist (zero
  client, fund, or private-domain names in shipped surfaces, per the release
  membrane allowlist); zero em dashes in outgoing surfaces (the spore bundle,
  public docs). Fail-closed.
- **Blocked-by:** N4, N6, N7 (gates the whole convergence).

## Ordering rationale

- **N1 first** because the cache trait is independent of the spore format and is
  the determinism foundation the seal contract (N4) leans on.
- **N2 -> N3** is the parse-then-expand spine; everything downstream needs a
  parsed spore and a pure expansion.
- **N4 / N5 / N7 fan out** from N3: the seal gate, the CLI, and the fixture are
  independent once `expand()` exists. N6 trails N5 (document what exists). N8
  converges them.
- This mirrors ADR-140's structural cut: format (N2) and expansion (N3) are the
  load-bearing new code; the cache (N1), the seal gate (N4), and the surfaces
  (N5/N6/N7) compose around them. No new scheduler, no new molecule type:
  `expand()` replays to existing `cs nucleate`, emergent fan-out is existing
  ADR-026 dynamic foaming under declared bounds.

## How the pilot foams it

```
# N1 has no blocker; the rest chain by blocked-by.
cs nucleate task-work --var brief="N1 deterministic trait + content cache (ADR-140 D5)"
cs nucleate task-work --var brief="N2 spore.toml parser (ADR-140 schema)"   --blocked-by <N1>
cs nucleate task-work --var brief="N3 expand() pure fn (ADR-140 D3)"        --blocked-by <N2>
cs nucleate task-work --var brief="N4 seal-verify contract (ADR-140 D4)"    --blocked-by <N3>
cs nucleate task-work --var brief="N5 cs spore run CLI"                     --blocked-by <N3>
cs nucleate task-work --var brief="N6 CLI docs + help + man"                --blocked-by <N5>
cs nucleate task-work --var brief="N7 e2e fixture (workshop prototype)"      --blocked-by <N5>
cs nucleate task-work --var brief="N8 verify gate (DoD + banlist + em-dash)" --blocked-by <N4>,<N6>,<N7>
# then: tmux new -d -s runtime cs run <N1> --poll-interval 5
```

Tag every child `temp:warm` on nucleation (decomposition-auto-tag discipline);
the pilot promotes to `temp:hot` as each becomes actionable. This manifest is
the design input to that foaming, refinable as the pilot learns.
