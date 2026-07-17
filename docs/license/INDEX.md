# docs/license/ — Cosmon licensing workstream

## Current state — AGPL-3.0 (cœur) + Apache-2.0 (frontières)

The cosmon repository is a **mixed-licence workspace** since 2026-05-09
(see [ADR-092](../adr/092-license-bascule-mpl-to-agpl.md)) and aligned
with the noogram federation invariant
([noogram ADR-0001 §7](../../../noogram/docs/adr/0001-noogram-fondateur.md)):

- **Core (AGPL-3.0-only)** — runtime, daemons, end-to-end binaries,
  MCP servers. Default for the workspace via
  `[workspace.package] license = "AGPL-3.0-only"`.
- **Frontier (Apache-2.0)** — pure libraries / SDKs / contracts a third
  party would `cargo add`. Per-crate override `license = "Apache-2.0"`.

The canonical licence texts live at the repo root:

- [`LICENSE`](../../LICENSE) — AGPL-3.0 (FSF, 2007).
- [`LICENSE-APACHE`](../../LICENSE-APACHE) — Apache-2.0.
- [`LICENSE-MPL-archive`](../../LICENSE-MPL-archive) — historical MPL-2.0
  text, preserved verbatim for traceability of the transitional sweep
  (task-20260415-100f → ADR-092).

The placement test (ADR-092 §3) is:

> **« Un tiers ferait-il `cargo add cosmon-X` sans vouloir tout
> AGPL-iser son projet ? »** Yes → frontier (Apache-2.0). No → core
> (AGPL-3.0-only).

ADR-092 §3 inscribes the original decision (60 crates, 2026-05-09). The
**living table below** is the current state of the workspace, regenerated
from `crates/*/Cargo.toml` by `scripts/license-table.sh`. When the two
disagree, the living table wins (workspace evolves; the ADR is immutable).
A CI gate fails the build if the table is stale or if a new crate has been
added without a curated row in `scripts/license-rationales.tsv`.

<!-- BEGIN LICENSE TABLE -->
## Per-crate partition (machine-generated — do not edit by hand)

_Last generated: 2026-07-17, 44 crates._

_Run `bash scripts/license-table.sh --write` to refresh._

### Core (AGPL-3.0-only) — 37 crates

| Crate | Tier | Licence | Rationale |
|-------|------|---------|-----------|
| `cosmon-agent-harness` | core | AGPL-3.0-only | agent harness — layer ④ engine (delib-ca76) |
| `cosmon-api` | core | AGPL-3.0-only | API — code-links AGPL core/state/filestore, not pure-network (delib-ca76) |
| `cosmon-archive` | core | AGPL-3.0-only | archive binary |
| `cosmon-ask` | core | AGPL-3.0-only | ask binary |
| `cosmon-bridge-claude` | core | AGPL-3.0-only | Claude bridge — layer ④ engine, code-links core (delib-ca76) |
| `cosmon-cli` | core | AGPL-3.0-only | end-to-end CLI binary `cs` |
| `cosmon-cockpit-http` | core | AGPL-3.0-only | HTTP cockpit |
| `cosmon-cockpit` | core | AGPL-3.0-only | TUI cockpit |
| `cosmon-core` | core | AGPL-3.0-only | kernel — the moat; AGPL closes the strip-mine hole (delib-ca76) |
| `cosmon-crashtest` | core | AGPL-3.0-only | crash test harness |
| `cosmon-daemon-supervisor` | core | AGPL-3.0-only | daemon supervisor |
| `cosmon-daemon` | core | AGPL-3.0-only | long-lived daemon |
| `cosmon-filestore` | core | AGPL-3.0-only | filestore backend — layer ② persistance (delib-ca76) |
| `cosmon-fuzz` | core | AGPL-3.0-only | fuzzer |
| `cosmon-graph` | core | AGPL-3.0-only | DAG primitives — layer ① noyau (delib-ca76) |
| `cosmon-hash` | core | AGPL-3.0-only | hash primitives — layer ① noyau (delib-ca76) |
| `cosmon-mcp` | core | AGPL-3.0-only | MCP server |
| `cosmon-notary` | core | AGPL-3.0-only | notary primitives — layer ② (delib-ca76) |
| `cosmon-observability` | core | AGPL-3.0-only | observability — layer ⑥ (delib-ca76) |
| `cosmon-oidc-testkit` | core | AGPL-3.0-only | OIDC test kit binary |
| `cosmon-ops-tools` | core | AGPL-3.0-only | ops tools — layer ③ product harness (delib-ca76) |
| `cosmon-pilot` | core | AGPL-3.0-only | pilot UX — layer ⑥ cockpit (delib-ca76) |
| `cosmon-process-witness` | core | AGPL-3.0-only | process-identity witness for adapters — same kernel tier as the adapters it guards |
| `cosmon-provider` | core | AGPL-3.0-only | provider trait — layer ④ engine; closes the --features llama SPDX lie (delib-ca76) |
| `cosmon-registry` | core | AGPL-3.0-only | registry — code-links AGPL core (delib-ca76) |
| `cosmon-remote` | core | AGPL-3.0-only | remote CLI — code-links AGPL core+surface-canon, not pure-network (delib-ca76) |
| `cosmon-rpp-adapter` | core | AGPL-3.0-only | RPP adapter binary |
| `cosmon-runtime` | core | AGPL-3.0-only | resident runtime |
| `cosmon-scenario` | core | AGPL-3.0-only | scenario runner |
| `cosmon-scheduler` | core | AGPL-3.0-only | scheduler engine |
| `cosmon-state` | core | AGPL-3.0-only | state store trait — crash-recovery wedge, layer ② (delib-ca76) |
| `cosmon-style` | core | AGPL-3.0-only | UI primitives — layer ⑥ cockpit (delib-ca76) |
| `cosmon-surface-canon` | core | AGPL-3.0-only | surface canon — layer ② (delib-ca76) |
| `cosmon-surface` | core | AGPL-3.0-only | surface projection — layer ② (delib-ca76) |
| `cosmon-transport` | core | AGPL-3.0-only | transport trait — layer ② (delib-ca76) |
| `cosmon-verify` | core | AGPL-3.0-only | verification — layer ③ product harness (delib-ca76) |
| `cosmon` | core | AGPL-3.0-only | name-reservation crate (publish=true) — ships product metadata, AGPL |

### Frontier (Apache-2.0) — 7 crates

| Crate | Tier | Licence | Rationale |
|-------|------|---------|-----------|
| `apps-transport-http` | frontier | Apache-2.0 | HTTP-on-Tailscale transport — vendored sibling-galaxy lib, Apache ⑦ |
| `claudion` | frontier | Apache-2.0 | session-energy probe lib (vendored in-tree) |
| `cosmon-client` | frontier | Apache-2.0 | client SDK |
| `cosmon-thin-cli` | frontier | Apache-2.0 | thin CLI lib |
| `cosmon-thin-macro` | frontier | Apache-2.0 | thin proc-macro |
| `neurion-core` | frontier | Apache-2.0 | neurion core lib |
| `topon-core` | frontier | Apache-2.0 | topon core lib |
<!-- END LICENSE TABLE -->

## History — rejected branches

| Branch | Status | Why rejected |
|--------|--------|--------------|
| MPL-2.0 (transitional) | Superseded 2026-05-09 | SaaS hole; aligned doctrine inscribed AGPL-3.0 upstream. |
| NPL-1.0 v0.1 (custom copyleft) | Rejected 2026-05-09 | Built on MPL-2.0; inherits the SaaS hole; structurally weaker than the upstream noogram invariant. |

The NPL-1.0 draft and its validation plan are preserved under
[`archive/`](archive/) with `REJECTED` preambles.

## Files

| File | Purpose |
|------|---------|
| [`../adr/092-license-bascule-mpl-to-agpl.md`](../adr/092-license-bascule-mpl-to-agpl.md) | The decision: MPL-2.0 → AGPL-3.0 + Apache-2.0, NPL-1.0 rejected. |
| [`archive/NPL-1.0-draft-v0.1.md`](archive/NPL-1.0-draft-v0.1.md) | Rejected draft, preserved for the record. |
| [`archive/NPL-1.0-validation-plan.md`](archive/NPL-1.0-validation-plan.md) | Rejected validation plan, preserved for the record. |

## Why AGPL-3.0 at the core

MPL-2.0 (and the NPL-1.0 superset) closes file-level redistribution but
leaves the network/SaaS escape hatch open: a wrapper SaaS that exposes
covered software via HTTP triggers no obligation to share modifications.

AGPL-3.0 §13 closes that hatch at the source. For cosmon — a runtime
designed to be wrapped behind agentic SaaS surfaces (cockpit-http,
cosmon-saas, future remote pilots) — that is structural, not stylistic.

The *flèche-non-inversée* invariant inscribed in noogram glossary
("aucune verticale ne peut sortir un fork propriétaire") requires it.

## Why Apache-2.0 at the frontier

The frontier crates (cosmon-core, cosmon-client, cosmon-api, …) are the
adoption surface: a third party should be able to embed them in *any*
project, including a closed-source one, without forcing AGPL onto the
host. Apache-2.0 preserves that surface and is what noogram ADR-0001 §7
explicitly names ("Apache 2.0 aux frontières (SDK, formats d'échange)").

## References

- [ADR-092](../adr/092-license-bascule-mpl-to-agpl.md) — the decision.
- [noogram ADR-0001 §7](../../../noogram/docs/adr/0001-noogram-fondateur.md) — federation licence invariant.
- [noogram ADR-0002 §5](../../../noogram/docs/adr/0002-positionnement-strategique-vs-precedents.md) — retained precedents.
- [noogram glossary](../../../noogram/docs/glossary.md) — *invariant flèche-non-inversée préservé par AGPL-3.0*.
- noogram inversions §10 — *AGPL est l'invariant, la fermeture serait l'erreur*.
- Companion chronicle — 2026-05-09, *Le trou doctrinal du brief de licence*.
