# ADR-028: `cosmon-observability` crate and `cs peek` command

## Status
Accepted (2026-04-12)

## Context

The pilot's current fleet-observation workflow relies on zsh aliases: `tl`
(tmux list-sessions across sockets) and `tff` (fzf session jumper). They
show tmux session names but nothing about the molecule behind each session —
no role, no status, no step, no staleness, no energy. To see what a worker
is doing one must *attach* to its tmux, which is intrusive and breaks the
"observer does not perturb" invariant.

Deliberation `delib-20260412-7457` (panel: torvalds, tolnay, jobs,
architect, shannon) framed the replacement design question and converged on
a semi-interactive TUI with a shared read-only query layer. The converged
outcome resolved one decision hinge: **`cosmon-cockpit-http` already exists
in this workspace**, so the second consumer of a fleet-observability query
layer is real — not aspirational. This justifies a Rust library path over
torvalds's shell-script preference, *and* justifies an ADR before any
implementation lands.

Four structural questions were raised that are load-bearing for every
future extension of the observation surface:

1. What *kind of thing* is the observability layer in the architecture —
   a new sensor plane, a projection, or something else?
2. What is the port shape — how many traits, sync or async, how is drift
   prevented?
3. What is the command perimeter — is `cs peek` an additional command
   alongside `cs watch`, or does it absorb it?
4. How does the TUI share its query layer with `cosmon-cockpit-http`
   without reintroducing the MCP-vs-CLI drift the project paid dearly to
   eliminate?

This ADR answers all four before the crate scaffold or the `cs peek`
command are written. It is Phase 0 of the integration plan in
`delib-20260412-7457`'s outcomes.md.

## Decision

### 1. `cosmon-observability` is a read-only adapter over the existing data plane — not a new sensor plane

The temptation, when adding pane-capture and cross-socket aggregation, is
to frame tmux scrollback as a third plane alongside control (DAG) and data
(filesystem). **Reject this framing.** `tmux capture-pane` is a *volatile
read* of process stdout that the data plane already owns — the tmux ring
buffer is a transient cache, not a durable channel. Modelling it as a new
plane would multiply future infrastructure cost (new mailbox semantics,
new retention policy, new persistence story) for zero new information.

Instead, `cosmon-observability` is structurally analogous to
`cosmon-surface`: a read-only projection/adapter over state that already
exists. `cosmon-surface` projects `.cosmon/state/` into `STATUS.md` /
`ISSUES.md` / GitHub Issues; `cosmon-observability` projects
`.cosmon/state/` + tmux scrollback + claudion logs into a `FleetSnapshot`
value and a `PaneCapture` blob. Both are **pure readers**.

This placement satisfies the architectural coherence checklist (from
CLAUDE.md §"Coherence checklist"):

| # | Rule | Status |
|---|---|---|
| 1 | Stateless | ✓ each snapshot rebuilds from sources |
| 2 | Idempotent | ✓ pure reads, twice = once |
| 3 | Regime-aware | ✓ *reports* the regime, alters none |
| 4 | Single perimeter | ✓ observation only (see §4 for `cs watch` absorption) |
| 5 | Symmetric undo | N/A — read-only |
| 6 | Runtime-compatible | ✓ Resident Runtime becomes an additional `FleetObserver` impl |
| 7 | Worker/human boundary | ✓ human-first TUI; `--json` for workers, default-scoped to own fleet |
| 8 | Write-read asymmetry | ✓ never writes; no coupling-report-on-write |
| 9 | Merge-before-dispatch | N/A — no dispatch |
| 10 | CLI-first for workers | ✓ workers continue using `cs` CLI; observability is for *external* observers and the operator |

The regime placement under ADR-016: the observability layer is **regime-
neutral**. It is safe to invoke in Inert, Propelled, or Autonomous. The
Resident Runtime (ADR-016 Phase 3+) becomes *a source* for the
`FleetObserver` trait, not a replacement for it — in that future, the
`TmuxFsObserver` adapter is joined by a `RuntimeObserver` adapter,
selected at startup the same way ADR-023's `DashboardView` plans its
`RuntimeCockpitView`.

### 2. Two-trait port on the cold/hot axis, synchronous, with `FakeObserver` as the anti-drift fixture

The port exposes **exactly two traits**, split on the cold/hot axis rather
than on query semantics:

```rust
pub trait FleetObserver {
    fn snapshot(&self, filter: &Filter) -> Result<FleetSnapshot, ObserveError>;
}

pub trait PanePeek {
    fn capture(&self, session: &SessionId) -> Result<PaneCapture, ObserveError>;
}
```

Rationale:

- `FleetObserver::snapshot` is the **cold** query: bounded work per call,
  rebuilds a `FleetSnapshot` from state + tmux session enumeration +
  claudion logs. Called on a 2 s auto-refresh tick in the TUI and per
  HTTP request in `cosmon-cockpit-http`. This is the view-model seam.
- `PanePeek::capture` is the **hot** query: pane capture is expensive,
  called *on-demand for the selected row only* (shannon's opportunistic
  policy at 1 Hz), never for all rows. Separating it keeps `FleetSnapshot`
  cheap and makes capture a distinct, disableable feature.

Five-queries-in-one was rejected: it would force every consumer to pay
for capture on every snapshot and would make `FakeObserver` (see below)
balloon.

**Traits are synchronous.** Rationale: the port contract must be usable
from ratatui's render loop (synchronous) without forcing an executor
onto the TUI. `cosmon-cockpit-http` (axum, async) wraps calls with
`tokio::task::spawn_blocking` at the adapter boundary. Async in the port
would cost the TUI for zero gain.

**`ObserveError` is a concrete enum** (`thiserror`), not an associated
type. Associated-type errors would break object-safety and prevent
`Arc<dyn FleetObserver>`. Concrete errors also simplify the HTTP adapter's
error-mapping and the test fixture.

**`#[non_exhaustive]` on every public struct and enum** in the port.
`FleetSnapshot` is the shared wire value between TUI and HTTP; any field
addition must be additive. This is enforced by the type system, not by
convention.

**Anti-drift mechanism: `FakeObserver` as a shared fixture.** The
discipline is not "both consumers import the same trait" — that is a weak
guarantee. The discipline is "both consumers are tested against the same
fixture". `FakeObserver` is a `pub` struct in `cosmon-observability`
constructable from a hand-authored `FleetSnapshot`. A cross-crate
integration test asserts:

- ratatui TestBackend renders `FakeObserver` into a frame with the
  expected column layout (snapshot test).
- `cosmon-cockpit-http` serializes the *same* `FakeObserver` snapshot
  into JSON with the expected shape.

If a field is added that one consumer renders and the other does not, the
test fails. This is the structural anti-drift invariant. The MCP-vs-CLI
drift this project eliminated in ADR-020 was expensive; `FakeObserver`
prevents the repeat.

### 3. `cs watch` is absorbed into `cs peek` — single-perimeter invariant

`cs watch` today renders a fleet summary in non-TUI mode. Keeping it as a
separate command after `cs peek` lands would be a direct violation of
**command perimeter rule 4**: "Single perimeter (not duplicating an
existing command's role)". Two commands rendering fleet state is a bug,
regardless of one being interactive and the other not.

Absorption:

- `cs peek` (default, TTY detected) → ratatui TUI.
- `cs peek --no-tui` / `cs peek --once` → non-interactive text render
  (replaces `cs watch`).
- `cs peek --json` → machine-readable NDJSON (replaces `cs watch --json`
  if that flag exists).

`cs watch` is deprecated with a one-release grace window: the command
remains callable, prints a deprecation banner to stderr, and internally
delegates to the `cs peek --no-tui` code path. After the grace window it
is removed. This is the same deprecation discipline used for
`cosmon-mcp` (CLAUDE.md §"Crate Structure").

jobs's "this tool is triage, not observation" critique is partially
honored by a secondary alias: `cs fleet` resolves to `cs peek` at the
clap level. One extra line, zero duplication, noun-shaped discoverability
for operators who think "I want to look at the fleet" rather than "I
want to peek".

### 4. Hexagonal sharing with `cosmon-cockpit-http` is via Rust library link — not HTTP, not subprocess

The canonical ways two processes could share a query layer:

1. **Subprocess** — `cosmon-cockpit-http` spawns `cs peek --json` and
   parses stdout. Used by ADR-023 for the *write* path (`cs nucleate`)
   where the transactional-core invariant demands a subprocess firewall.
2. **HTTP** — a separate observability service exposes JSON, both the
   TUI and the cockpit call it. Adds a new runtime dependency.
3. **Library link** — both consumers depend on `cosmon-observability`
   as a Rust crate and invoke the traits in-process.

**Library link is the correct choice here** because:

- There is no transactional-core invariant at stake — the observability
  layer is pure-read. The subprocess firewall (ADR-023) protects against
  backdoor writes; observation has no writes to firewall.
- The shared wire value (`FleetSnapshot`) *is* the Rust type. Serializing
  it through JSON only to parse it back would add a drift surface for
  zero gain — the two consumers could end up mapping to different
  view-models. The `#[non_exhaustive]` discipline works only if the type
  is the contract; round-tripping through JSON makes JSON the contract
  and the type a detail.
- The CLI-over-MCP invariant (CLAUDE.md §"CLI over MCP for workers")
  governs the **worker/state-store contract**, not library linking
  between two facing-the-operator components. Workers still use the `cs`
  CLI for every state mutation; `cosmon-cockpit-http` was already a
  library consumer of `cosmon-state` and `cosmon-core` (see ADR-023).
  Adding a `cosmon-observability` dependency is symmetric with that.
- `cosmon-cockpit-http` exposes `/api/sessions` by returning the
  `FleetSnapshot` JSON **verbatim** — no DTO layer, no re-serialization
  schema. The crate's `#[non_exhaustive]` discipline IS the wire
  contract. Any schema change is driven from the port crate and
  propagates additively to both consumers. ADR-023's DTO-flattening
  concern for the *write/mutation* path does not apply here because the
  core-free property of `DashboardView`'s DTOs was about isolating
  *mutation* callers from the domain enums; `FleetSnapshot` is a
  read-only value type whose fields are observations, not domain
  primitives, and may include domain-informed status strings without
  violating ADR-023's port-cleanliness spirit.

Depedency graph:

```
cosmon-observability (new crate)
 ├── cosmon-core        (IDs, domain types)
 ├── cosmon-state       (read-only access to .cosmon/state/)
 ├── cosmon-transport   (TmuxBackend session enumeration + capture)
 └── claudion           (per-worker energy/activity probe)
      ▲           ▲
      │           │
 cosmon-cli     cosmon-cockpit-http
 (cs peek)      (/api/sessions)
```

No cycles. Both consumers link the same crate version via the workspace
`Cargo.toml`.

## Consequences

**Positive**

- One concept — "read-only adapter over the data plane" — covers both
  surfaces and observability. No new mental model.
- Single anti-drift invariant, testable (`FakeObserver` cross-crate test).
- Single command perimeter for fleet observation (`cs peek`); `cs watch`
  leaves the surface area.
- Horizon (`cosmon-cockpit-http`) and the TUI evolve their view-model
  together — no MCP-vs-CLI-style drift possible.
- Resident Runtime (ADR-016) can become a new `FleetObserver` impl
  without changing the trait or either consumer.

**Negative**

- `cosmon-observability` pulls `cosmon-core`, `cosmon-state`,
  `cosmon-transport`, `claudion` into both `cosmon-cli` and
  `cosmon-cockpit-http` dependency trees. Both already transitively
  depend on `cosmon-core` and `cosmon-state`, so the marginal cost is
  `cosmon-transport` on the cockpit side. Acceptable — the cockpit
  already needs tmux liveness probing (ADR-023 §4) and currently
  reaches into `cosmon-transport` ad hoc; centralizing that access in
  `cosmon-observability` is an improvement, not a regression.
- Synchronous trait in an axum (async) adapter requires `spawn_blocking`.
  Measured cost is a thread hop per HTTP request; acceptable for a
  human-facing dashboard at human request rates.
- `cs watch` users must migrate. Mitigated by the grace-window
  deprecation banner and identical `--no-tui`/`--once` semantics.

**Neutral**

- `cs peek` and `cs fleet` are aliases. Slight CLI surface duplication;
  clap handles it in one line.
- `FakeObserver` is `pub` in the port crate (required for cross-crate
  test usage). Not a production attack surface because the crate has no
  write path.

## Alternatives considered

- **New sensor plane for tmux capture.** Rejected (§1): tmux scrollback
  is a volatile read of data the data plane already owns. Adding a third
  plane would multiply semantic cost (retention, persistence, mailbox
  rules) for no new information.
- **Five query methods on one trait.** Rejected (§2): forces every
  consumer to pay for pane capture on every snapshot; balloons the fixture.
- **Async traits in the port.** Rejected (§2): forces an executor onto
  the synchronous TUI render loop; `spawn_blocking` at the async
  boundary is the standard pattern.
- **Share via HTTP service.** Rejected (§4): introduces a new runtime
  dependency and a second copy of the view-model schema. No benefit over
  library link for an in-process observer.
- **Share via subprocess (`cs peek --json`).** Rejected (§4): the
  subprocess firewall exists to protect the transactional-core
  invariant; observation has no writes. Serializing `FleetSnapshot` to
  JSON only to parse it back makes JSON the contract and the Rust type
  a shadow — the `#[non_exhaustive]` discipline dissolves.
- **Keep `cs watch` alongside `cs peek`.** Rejected (§3): violates
  single-perimeter invariant. Two commands rendering fleet state is a bug.
- **Defer the crate; ship a bash script.** Rejected: the second consumer
  (`cosmon-cockpit-http`) exists *now*, resolving the D1 hinge in
  `delib-20260412-7457`. torvalds's scoping discipline is preserved
  instead as the MVP scoping law (see outcomes §c).

## References

- `delib-20260412-7457`
  — originating deliberation (panel: torvalds, tolnay, jobs, architect,
  shannon). See `synthesis.md` and `outcomes.md`.
- [ADR-016](016-autonomy-regimes-and-resident-runtime.md) —
  regime placement. The observability layer is regime-neutral; the
  Resident Runtime becomes an additional `FleetObserver` impl in Phase 3+.
- [ADR-023](023-cockpit-hexagonal-read-surface.md) — cockpit's
  retina framing. `cosmon-observability` extends the same pattern: a
  read-only adapter over the data plane, not a new plane.
- [ADR-020](020-mcp-project-agnostic-cwd-per-call.md) — MCP-vs-CLI
  drift lesson. `FakeObserver` is the structural prevention.
- CLAUDE.md §"Architectural Discipline", §"Coherence checklist",
  §"CLI over MCP for workers" — the invariants this ADR must not break.
- `docs/architectural-invariants.md` — the two-layer model the
  observability layer sits inside.

## Follow-ups

This ADR **blocks** (semantically, regardless of `--blocked-by` wiring):

1. `cosmon-observability` crate scaffold + trait defs + `FakeObserver`
   (Phase 1 in outcomes §e).
2. `cs peek` TUI V1 + `cs watch` deprecation (Phase 1).
3. `cosmon-cockpit-http` `/api/sessions` via library link (Phase 2).
4. Cross-adapter anti-drift integration test (Phase 3).

No implementation molecule should land before this ADR is merged. Any
deviation from the decisions above must file a successor ADR rather than
backdooring the change through a PR.
