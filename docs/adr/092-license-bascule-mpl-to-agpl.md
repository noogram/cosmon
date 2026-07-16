# ADR-092 — License bascule MPL-2.0 → AGPL-3.0 (cœur) + Apache-2.0 (frontières), abandon NPL-1.0

- **Status:** Accepted
- **Date:** 2026-05-09
- **Decider:** Noogram (operator), via question atomique 2026-05-09 → "oui"
- **Supersedes (partial):** task-20260415-100f (transitional MPL-2.0 sweep), draft NPL-1.0 v0.1
- **Aligns with:** noogram ADR-0001 §7, noogram ADR-0002 §5, noogram glossary,
  noogram inversions §10
- **Companion chronicle:** internal chronicles, 2026-05-09 — *Le trou
  doctrinal du brief de licence*

## 1. Context

The cosmon repository was placed under MPL-2.0 in transitional sweep
`task-20260415-100f`, on the path toward a custom copyleft variant
**NPL-1.0** (Noogram Public License). NPL-1.0 v0.1 is a draft: MPL-2.0
base + four narrow additions (§K4 attestation persistence, §K5 disclosure
on commercial redistribution, §K6 private-use carve-out, §K7 minor edits)
under an internal validation plan with a target switchover at
"Day J" (mid-October 2026).

In parallel, the noogram galaxy — which owns the federation-level
licensing doctrine for all artifacts touching the *commun* — has
inscribed a **non-negotiable invariant** in four places:

1. **noogram ADR-0001 §7** —
   > « Licence cœur : AGPL 3.0 (clause SaaS §13 + copyleft réseau passe
   > les deux tests adversariaux). Apache 2.0 aux frontières (SDK,
   > formats d'échange) ; CC-BY 4.0 textes ; OpenRAIL poids modèles. »

2. **noogram ADR-0002 §5** — table of *retained precedents*:
   > « **AGPL 3.0** (FSF, 2007) — Copyleft réseau + clause SaaS §13.
   > Licence cœur. Passe les deux tests adversariaux (juriste hostile +
   > chercheur public hostile). Apache 2.0 frontières, CC-BY 4.0 textes,
   > OpenRAIL poids modèles. »

3. **noogram `docs/glossary.md`** — entry *invariant*:
   > « L'invariant flèche-non-inversée est préservé par AGPL-3.0 :
   > aucune verticale ne peut sortir un fork propriétaire. »

4. **noogram inversions §10** —
   > « Le paradigme AI dominant pose : modèles fermés, weights
   > propriétaires, accès via API, monétisation par usage. Noogram
   > inverse (ADR-0001 §7) : AGPL 3.0 cœur (clause SaaS §13 + copyleft
   > réseau), Apache 2.0 aux frontières […] **L'ouverture est
   > l'invariant ; la fermeture serait l'erreur.** »

NPL-1.0 v0.1 is built on top of MPL-2.0. **MPL-2.0 has a structural
SaaS hole**: a wrapper SaaS service that exposes covered software over
the network triggers no obligation to share modifications. NPL-1.0
inherits this hole — none of its four additions close it. The only
copyleft licence that closes the SaaS hole at the source is AGPL-3.0,
via §13 (the *Affero* network clause).

The brief of NPL-1.0 was drafted *without* re-reading the noogram
upstream ADRs that already inscribed AGPL-3.0 as the federation invariant.
This is the same failure mode chronicled 2026-04-29 ("Le trou doctrinal
du brief de bootstrap") — a downstream brief silently weaker than the
upstream-galaxy doctrine that governs it.

## 2. Decision

Cosmon flips its licence partition to a **mixed-licence workspace**:

- **Core (AGPL-3.0-only)** — runtime, daemons, end-to-end binaries,
  MCP servers, all crates that execute the system's logic against the
  outside world. Default for the workspace.
- **Frontier (Apache-2.0)** — pure libraries / SDKs / type crates / contracts
  that a third party might `cargo add` to build *on top of* cosmon
  without wanting AGPL transitivity. Per-crate override in the package's
  own `Cargo.toml`.

The NPL-1.0 v0.1 draft is **rejected** as a structurally weaker
alternative to AGPL-3.0. Its draft text and validation plan are moved
to `docs/license/archive/` with a `REJECTED` preamble pointing here.

The transitional MPL-2.0 state (task-100f) is collapsed: the canonical
text of MPL-2.0 is preserved verbatim in `LICENSE-MPL-archive` for
historical traceability, but `LICENSE` now contains the AGPL-3.0
canonical text (FSF, 2007).

## 3. Partition test

> **Note 2026-05-17:** the per-crate table below records the original
> decision at the time of ADR-092 acceptance (60 crates). The
> *living* table — kept in sync with the workspace by
> `scripts/license-table.sh` and gated by CI — lives in
> [`docs/license/INDEX.md`](../license/INDEX.md). When the two
> disagree, the living table wins (workspace evolves; the ADR is
> immutable).

For any cosmon crate, the placement test is:

> **« Un tiers ferait-il `cargo add cosmon-X` sans vouloir tout
> AGPL-iser son projet ? »** Yes → frontier (Apache-2.0). No → core
> (AGPL-3.0-only).

The shape of the answer depends on what the crate ships:

- **Core (AGPL-3.0-only)** — anything that boots, executes, talks to
  agents, or wraps the runtime end-to-end:
  - `cs` CLI (`cosmon-cli`).
  - Resident runtime, daemons, supervisors, schedulers
    (`cosmon-runtime`, `cosmon-daemon`, `cosmon-daemon-supervisor`,
    `cosmon-scheduler`, `schedulerd`).
  - Cockpits and HTTP cockpits (`cosmon-cockpit`, `cosmon-cockpit-http`).
  - SaaS endpoint (`cosmon-saas`).
  - MCP servers (`cosmon-mcp`, `almanac`, `foundry-mcp`,
    `neurion-mcp`, `topon-mcp`).
  - Ops-only binaries (`cosmon-archive`, `cosmon-crashtest`,
    `cosmon-fuzz`, `cosmon-rpp-adapter`, `cosmon-scenario`,
    `cosmon-ask`, `cosmon-voice-bridge`, `cosmon-matrix-tick`,
    `cosmon-oidc-testkit`, `claudion`, `apps-transport-http`,
    `noogram-mycelial-monitor`).

- **Frontier (Apache-2.0)** — pure types / libs / SDKs / boundary
  contracts:
  - `cosmon-core`, `cosmon-client`, `cosmon-api`, `cosmon-hash`,
    `cosmon-graph`, `cosmon-state`, `cosmon-filestore`, `cosmon-style`,
    `cosmon-thin-cli`, `cosmon-thin-macro`, `cosmon-verify`,
    `cosmon-notary`, `cosmon-observability`, `cosmon-provider`,
    `cosmon-registry`, `cosmon-transport`, `cosmon-bridge-claude`,
    `cosmon-bridge-gastown`, `cosmon-surface`.
  - Internalised library halves: `almanac-resolver`,
    `foundry-core`, `foundry-kernel`, `foundry-lean-syntax`, `foundry-probe`,
    `ga-core`, `ga-island`, `ga-operators`, `ga-proof-search`,
    `neurion-core`, `topon-core`,
    `mailroom-voice-core`, `mailroom-voice-gridco`,
    `mailroom-voice-tts`, `mailroom-voice-whisper`.

### Per-crate table (60 crates)

| Crate | Tier | Licence | Rationale |
|-------|------|---------|-----------|
| `cosmon-cli` | core | AGPL-3.0-only | end-to-end CLI |
| `cosmon-runtime` | core | AGPL-3.0-only | resident runtime |
| `cosmon-daemon` | core | AGPL-3.0-only | long-lived daemon |
| `cosmon-daemon-supervisor` | core | AGPL-3.0-only | daemon supervisor |
| `cosmon-cockpit` | core | AGPL-3.0-only | TUI cockpit |
| `cosmon-cockpit-http` | core | AGPL-3.0-only | HTTP cockpit |
| `cosmon-saas` | core | AGPL-3.0-only | SaaS endpoint |
| `cosmon-mcp` | core | AGPL-3.0-only | MCP server |
| `cosmon-scheduler` | core | AGPL-3.0-only | scheduler engine |
| `schedulerd` | core | AGPL-3.0-only | scheduler daemon |
| `cosmon-archive` | core | AGPL-3.0-only | archive binary |
| `cosmon-crashtest` | core | AGPL-3.0-only | crash test harness |
| `cosmon-fuzz` | core | AGPL-3.0-only | fuzzer |
| `cosmon-rpp-adapter` | core | AGPL-3.0-only | RPP adapter binary |
| `cosmon-scenario` | core | AGPL-3.0-only | scenario runner |
| `cosmon-ask` | core | AGPL-3.0-only | ask binary |
| `cosmon-voice-bridge` | core | AGPL-3.0-only | voice bridge runtime |
| `cosmon-matrix-tick` | core | AGPL-3.0-only | Matrix bridge runtime |
| `cosmon-oidc-testkit` | core | AGPL-3.0-only | OIDC test kit binary |
| `claudion` | core | AGPL-3.0-only | session probe binary |
| `apps-transport-http` | core | AGPL-3.0-only | runtime HTTP transport |
| `almanac` | core | AGPL-3.0-only | almanac MCP server |
| `foundry-mcp` | core | AGPL-3.0-only | foundry MCP server |
| `neurion-mcp` | core | AGPL-3.0-only | neurion MCP server |
| `topon-mcp` | core | AGPL-3.0-only | topon MCP server |
| `noogram-mycelial-monitor` | core | AGPL-3.0-only | runtime monitor |
| `cosmon-core` | frontier | Apache-2.0 | pure types |
| `cosmon-client` | frontier | Apache-2.0 | client SDK |
| `cosmon-api` | frontier | Apache-2.0 | API types |
| `cosmon-hash` | frontier | Apache-2.0 | hash primitives |
| `cosmon-graph` | frontier | Apache-2.0 | DAG primitives |
| `cosmon-state` | frontier | Apache-2.0 | state store trait |
| `cosmon-filestore` | frontier | Apache-2.0 | filestore backend |
| `cosmon-style` | frontier | Apache-2.0 | UI primitives |
| `cosmon-thin-cli` | frontier | Apache-2.0 | thin CLI lib |
| `cosmon-thin-macro` | frontier | Apache-2.0 | thin proc-macro |
| `cosmon-verify` | frontier | Apache-2.0 | verification lib |
| `cosmon-notary` | frontier | Apache-2.0 | notary primitives |
| `cosmon-observability` | frontier | Apache-2.0 | observability lib |
| `cosmon-provider` | frontier | Apache-2.0 | provider trait |
| `cosmon-registry` | frontier | Apache-2.0 | registry types |
| `cosmon-transport` | frontier | Apache-2.0 | transport trait |
| `cosmon-bridge-claude` | frontier | Apache-2.0 | Claude bridge lib |
| `cosmon-bridge-gastown` | frontier | Apache-2.0 | GasTown bridge lib |
| `cosmon-surface` | frontier | Apache-2.0 | surface projection |
| `almanac-resolver` | frontier | Apache-2.0 | citekey resolver lib |
| `foundry-core` | frontier | Apache-2.0 | formal-methods core |
| `foundry-kernel` | frontier | Apache-2.0 | foundry kernel |
| `foundry-lean-syntax` | frontier | Apache-2.0 | Lean syntax adapter |
| `foundry-probe` | frontier | Apache-2.0 | foundry probe lib |
| `ga-core` | frontier | Apache-2.0 | GA primitives |
| `ga-island` | frontier | Apache-2.0 | GA island model |
| `ga-operators` | frontier | Apache-2.0 | GA operators |
| `ga-proof-search` | frontier | Apache-2.0 | GA proof search |
| `neurion-core` | frontier | Apache-2.0 | neurion core lib |
| `topon-core` | frontier | Apache-2.0 | topon core lib |
| `mailroom-voice-core` | frontier | Apache-2.0 | voice core lib |
| `mailroom-voice-gridco` | frontier | Apache-2.0 | gridco adapter |
| `mailroom-voice-tts` | frontier | Apache-2.0 | tts adapter |
| `mailroom-voice-whisper` | frontier | Apache-2.0 | whisper adapter |

Total: **60 crates**. Core: **26**. Frontier: **34**.

## 4. Why AGPL-3.0 closes the SaaS hole

MPL-2.0 (and therefore the NPL-1.0 draft built on top of it) requires
sharing of modifications **only when binaries are distributed**.
A SaaS deployment that wraps the software behind an HTTP endpoint
distributes nothing in the MPL sense — the user receives an HTTP
response, never the binary — and is not bound to share modifications.

AGPL-3.0 §13 closes this exactly:

> "Notwithstanding any other provision of this License, if you modify
> the Program, your modified version must prominently offer all users
> interacting with it remotely through a computer network […] an
> opportunity to receive the Corresponding Source of your version […]"

For cosmon, this is structural: the system is *designed* to be wrapped
behind agentic SaaS surfaces (cockpit-http, cosmon-saas, future remote
pilots). MPL-2.0 leaves a fork-by-SaaS escape hatch that contradicts the
*flèche-non-inversée* invariant inscribed in noogram glossary —
"aucune verticale ne peut sortir un fork propriétaire."

Apache-2.0 at the frontier preserves the *adoption surface*: the
cosmon-core types, the cosmon-client SDK, the noogram-mycelial-monitor
exception aside, can be embedded in third-party (including proprietary)
projects without forcing them to AGPL. That is the boundary noogram
ADR-0001 §7 already named ("Apache 2.0 aux frontières (SDK, formats
d'échange)").

## 5. Why NPL-1.0 v0.1 is rejected

Three structural reasons:

1. **Structurally weaker than the upstream invariant.** NPL-1.0 v0.1 is
   MPL-2.0 + §K4/§K5/§K6/§K7. It does not close the SaaS hole. The
   upstream noogram doctrine has *already inscribed* AGPL-3.0 as the
   answer; NPL-1.0 would be a downstream weakening that contradicts a
   federation invariant. Same failure mode as 2026-04-29 (the bootstrap
   brief that silently dropped invariants). Surface the brief, do not
   patch the application.

2. **Custom licence cost > custom licence benefit.** NPL-1.0 needed
   €400–€800 of FR/CH IP counsel review, OSI submission decision, peer
   review by 2-3 OSS licensing experts, and a six-month validation plan.
   AGPL-3.0 is FSF-canonical, OSI-approved, fifteen-year-tested, and
   already inscribed by the upstream galaxy. Adopting AGPL-3.0 saves the
   entire validation budget and ships the invariant *now* rather than
   on Day J.

3. **The §K4/K5/K6/K7 additions do not add structural protection that
   AGPL-3.0 lacks.** §K4 (attestation persistence), §K5 (commercial
   disclosure), §K6 (private-use carve-out), §K7 (minor edits) are all
   either redundant under §13 of AGPL-3.0, expressible in the upstream
   noogram registry rather than the licence, or out of scope for a
   software licence (better placed in noogram phase-2 governance).

The NPL-1.0 draft is preserved in `docs/license/archive/` for the
historical record. Any future custom licence must (i) be a strict
*superset* of AGPL-3.0, never an MPL-derived weakening, and (ii) be
proposed via a successor ADR, not a brief.

## 6. Consequences

### Mechanical changes (this PR)

1. `LICENSE` → AGPL-3.0 canonical text (FSF, 2007).
2. `LICENSE-MPL-archive` → previous MPL-2.0 text preserved verbatim.
3. `LICENSE-APACHE` → new file with Apache-2.0 canonical text.
4. `Cargo.toml` workspace default → `license = "AGPL-3.0-only"`.
5. 34 frontier crates → per-crate `license = "Apache-2.0"` override.
6. `deny.toml` → `AGPL-3.0-only` added to allow-list (first-party use).
7. `README.md` License section → updated.
8. `NOTICE` → updated.
9. `docs/license/INDEX.md` → rewritten (AGPL-3.0 + Apache-2.0 partition).
10. `docs/license/archive/` → NPL-1.0 draft + validation plan with
    `REJECTED` preamble.
11. Internal chronicles → entry 2026-05-09.
12. `.license/HEADER-AGPL-3.0.txt` and `.license/HEADER-APACHE-2.0.txt` → new
    canonical SPDX headers.

### Out-of-scope (follow-ups, `temp:warm`)

- Noogram-side chronicle citing cosmon ADR-092 (nucleate in noogram).
- Syzygie peer notification (mailroom, showroom) — chronicle-lint
  patrol picks it up.
- Third-party redistribution monitoring — same plan as NPL-1.0
  validation-plan §7 but adapted for AGPL-3.0 (mostly *do nothing*; FSF
  and SFC handle enforcement).
- THESIS.md / CONSTITUTION.md sweep if licence terms appear there.

### Invariants preserved

- Workspace builds clean (`cargo check --workspace`,
  `cargo clippy --workspace -- -D warnings`, `cargo fmt --all -- --check`).
- No silent residue of MPL-2.0 in code or doc — `grep -r MPL-2.0`
  outside `LICENSE-MPL-archive/` and `docs/license/archive/` returns
  empty.
- `flèche-non-inversée` (no proprietary fork via SaaS) holds at the
  source level for all core crates.
- Adoption surface preserved: any third party can `cargo add` a frontier
  crate and ship under any compatible licence.

## 7. References

- noogram ADR-0001 §7 — `/srv/cosmon/noogram/docs/adr/0001-noogram-fondateur.md`
- noogram ADR-0002 §5 — `/srv/cosmon/noogram/docs/adr/0002-positionnement-strategique-vs-precedents.md`
- noogram glossary — `/srv/cosmon/noogram/docs/glossary.md`
- noogram inversions §10
- task-20260415-100f — transitional MPL-2.0 sweep (now superseded)
- AGPL-3.0 canonical text — https://www.gnu.org/licenses/agpl-3.0.txt
- Apache-2.0 canonical text — https://www.apache.org/licenses/LICENSE-2.0.txt
- Companion chronicle — internal chronicles, 2026-05-09
