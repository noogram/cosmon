# docs/ index

Curated, complete entry point into the cosmon documentation. Newcomers should
start with the concise on-ramp — the [physics vocabulary](book/src/explanation/physics-vocabulary.md)
and [crash recovery](book/src/explanation/crash-recovery.md) in the book — then
read [CLAUDE.md](../CLAUDE.md) for conventions. [THESIS.md](../THESIS.md) is an
optional deep-dive, not required first reading. This index tells you where each
top-level doc and subsystem lives; every top-level `docs/*.md` is listed below
exactly once.

## Operator handbook & workflow

- [handbook.md](handbook.md) — the operator Q&A (exposed as `cs help guide`): nucleate → tackle → wait → done, peek, temp tags, surfaces.
- [CONTRIBUTING.md](CONTRIBUTING.md) — the single contributor entry: PR discipline + quality rules. (The former `developer-workflow.md` was superseded and moved to [archive/](archive/).)
- [DAG-GUIDE.md](DAG-GUIDE.md) — tutorial: building and running polymers (DAGs).
- [formula-cookbook.md](formula-cookbook.md) — recipes for writing formulas.
- [do-not-do.md](do-not-do.md) — register of rejected choices.
- [pilot-refusal-register.md](pilot-refusal-register.md) — living append-only refusal register (ADR-052).

## Architecture & invariants

- [architectural-invariants.md](architectural-invariants.md) — the governing rulebook. Read before adding a command.
- [architecture.md](architecture.md) — high-level architecture overview.
- [architecture-baseline.md](architecture-baseline.md) — per-galaxy ADR-082 baseline summary with named waivers.
- [distribution-mechanism.md](distribution-mechanism.md) — kernel + plugins assembly (refines ADR-132).
- [reconciliation-model.md](reconciliation-model.md) — desired/observed/effective surface projection model.
- [surface-sync-protocol.md](surface-sync-protocol.md) — STATUS.md / ISSUES.md / GitHub mirror.
- [events-jsonl-merge.md](events-jsonl-merge.md) — event-log merge semantics and hash chain.
- [lineage-conservation.md](lineage-conservation.md) — auto-parent conservation contract (binds ADR-037/016).
- [runtime.md](runtime.md) — how the runtime behaves.
- [watchdog.md](watchdog.md) — propulsion and liveness probes.
- [spark-capture.md](spark-capture.md) — spark discipline (ties to the P_external axiom).

## Command references (per-feature deep dives)

- [cs-verify.md](cs-verify.md) — `cs verify` proof-of-work chain verification (MVP).
- [cs-spore.md](cs-spore.md) — `cs spore`: germinate a whole polymer from a shareable `spore.toml` template (ADR-140).
- [cs-recover.md](cs-recover.md) — crash-recovery scan for stranded molecules. ⚠ documents a phantom `cs recover` verb (drift D1; scheduled fix in modernization P0).
- [cs-demo-design.md](cs-demo-design.md) — design doc for the shipped `cs demo` command.
- [archive.md](archive.md) — `cs archive` command reference.
- [hooks.md](hooks.md) — hook contract.
- [events-claim.md](events-claim.md) — `ClaimEmitted` / `ClaimVerified` IFBDD instrumentation.
- [tmux-paste-buffer.md](tmux-paste-buffer.md) — per-call unique buffer name invariant.

## Contracts & schemas

- [EMBEDDING.md](EMBEDDING.md) — normative contract: how a project consumes cosmon.
- [project-config.md](project-config.md) — `.cosmon/config.toml` reference.
- [molecule-metadata.md](molecule-metadata.md) — durable metadata (tags/notes) schema.
- [whisper-frontmatter-schema.md](whisper-frontmatter-schema.md) — whisper YAML frontmatter schema.
- [observability.md](observability.md) — EventV2 schema; cockpit, peek, metrics.

## Specifications

- [spec-suite.md](spec-suite.md) — L1/L2/L3 executable-spec layering.
- [spec-bindings.md](spec-bindings.md) — generated binding tests across crates (do-not-edit banner).

## Release & publication

- [RELEASE-CHECKLIST.md](RELEASE-CHECKLIST.md) — pre-launch checklist.
- [WEBDOCS.md](WEBDOCS.md) — mdBook deploy runbook. ⚠ stale repo URL / page list (drift D2; scheduled fix in modernization P0/P5).
- [release/](release/) — release-staging material (crate cartography, membrane, migration).

## Design notes (draft / speculative)

- [design-creativity-interface.md](design-creativity-interface.md) — creativity-interface design (draft).
- [codeberg-mirror.md](codeberg-mirror.md) — codeberg mirror runbook (`temp:warm`, prepared not executed).

## Patterns (cross-galaxy shapes — prose, non-normative)

- [patterns/upstream-contract-gating.md](patterns/upstream-contract-gating.md) — capability typestate at the galaxy boundary: gate a sibling galaxy's not-yet-frozen contract with a witness type, not a feature flag.
- [patterns/latency-budgets.md](patterns/latency-budgets.md) — per-domain hot-path budgets the portability of any shared primitive pulls against.

## Governance

- [governance/](governance/) — governance docs.
- [adr/](adr/) — architecture decision records (see [`adr/INDEX.md`](adr/INDEX.md)).
- [founding/](founding/) — founding thesis (parent project).

## Narrative & vocabulary

- [lore/CHRONICLES.md](lore/CHRONICLES.md) — dated moments that illuminated a principle.
- [vocabulary.md](vocabulary.md) — physics naming glossary.
- [appendix-physics-inspiration.md](appendix-physics-inspiration.md) — why these verbs (non-normative).
- [easter-eggs.md](easter-eggs.md) — cultural references (non-canonical).
- [visual-charter.md](visual-charter.md) — visual charter source-of-truth.

## Guides & examples

- [guides/](guides/) — how-to guides (peek-zoom, syzygie, scratch, …); see [`guides/README.md`](guides/README.md).
- [examples/](examples/) — copy-paste config templates (`daemons.toml.example`, `codex-adapter.toml.example`, `voice-commands.toml.example`).

## Sister subsystems

- [`crates/cosmon-crashtest/README.md`](../crates/cosmon-crashtest/README.md) — bisimulation proptest harness.
- [neurion/](neurion/) — Neurion MCP in-tree subtree: [CLAUDE_NEURION.md](neurion/CLAUDE_NEURION.md), [TUTORIAL_NEURION.md](neurion/TUTORIAL_NEURION.md), [MCP_INSTRUCTIONS_NEURION.md](neurion/MCP_INSTRUCTIONS_NEURION.md), `neurion-adr/`, `neurion-defaults.toml`.

## Archive

- [archive/](archive/) — retired point-in-time notes, superseded docs, and historical research kept for provenance (peek/help audits, GasTown mapping + deep-dive, provider-morphology heuristics, deprecated-MCP channel study, overdue peek-bash spike eval, the superseded `developer-workflow.md`, operator-feedback capture, the archived topon-cli crate).
