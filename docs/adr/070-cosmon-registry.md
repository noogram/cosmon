# ADR-070 — Cosmon galaxy-name registry (`cosmon-registry`)

**Status:** Accepted
**Date:** 2026-04-23
**Parent:** delib-20260423-95fe (urgent-reflex deep-think, architect panel)
**Sibling:** task-20260423-XXXX (`cs ask` conversational ingress)
**Supersedes:** none

## Context

Under time pressure, the operator still types `claude` in
`/srv/cosmon/<name>/` instead of `cs …`. The panel's primary remedy
(delib-20260423-95fe) is a conversational ingress — `cs ask "<free
text>"` — whose first job is to resolve a short galaxy name embedded
in prose into a concrete `(path, fleet, default formula)` triple.

Without an in-process name → path index, every `cs ask` invocation
would have to walk `$HOME`, open `.cosmon/config.toml` on every
candidate directory, and guess. That is the daemon-shaped problem
architect refuses to solve with a daemon (ADR-054).
We need the stateless alternative.

## Decision

Introduce a new crate `cosmon-registry` exposing a **read-only**
public API:

```rust
pub struct Galaxy {
    pub name: String,
    pub path: PathBuf,
    pub fleet: String,
    pub claude_md_digest: Option<String>,
    pub default_formulas: HashMap<MoleculeKind, FormulaId>,
}

pub trait GalaxyIndex {
    fn resolve(&self, name: &str) -> Option<Galaxy>;
    fn list(&self) -> Vec<Galaxy>;
    fn default_formula(&self, galaxy: &str, kind: MoleculeKind) -> Option<FormulaId>;
}
```

Two backends ship today:

* **`TomlGalaxyIndex`** — source of truth. Reads
  `~/.config/cosmon/galaxies.toml` (honoring `$XDG_CONFIG_HOME`).
  Default. Missing file → empty index (not an error) so a fresh
  environment does not fail lookups.
* **`NeurionBackedGalaxyIndex`** — fallback behind feature flag
  `neurion-fallback`. Reads the `repos` table of the neurion
  SQLite inventory. Same crate, not a required dependency.

TOML schema (v0):

```toml
[[galaxy]]
name = "mailroom"
path = "/srv/cosmon/mailroom"
fleet = "default"
default_formulas = { task = "task-work", idea = "idea-to-plan", deliberation = "deep-think" }
```

Unknown `default_formulas` keys (not a valid `MoleculeKind`) are
rejected at load time — typos surface immediately.

CLI surface:

* `cs galaxies registry list [--json]` — list every registered galaxy
  (NDJSON with `--json`, one entry per line).
* `cs galaxies registry resolve <name> [--json]` — resolve one name;
  exit status 1 on miss so scripts can gate on it.

## Consequences

* `cs ask` (sibling molecule) can now depend on a cheap,
  synchronous, stateless `(name) → (path, fleet, formula)` lookup.
* Operator owns the registry by editing TOML. No daemon, no socket,
  no background reload. Cold-load is sub-millisecond on a 10-entry
  file.
* Two sources of truth exist — the TOML (human-declared) and the
  neurion `repos` table (machine-observed). We accept this
  asymmetry: drift between them is itself a useful signal, to be
  caught by a later drift-check formula, not papered over at load
  time.

## Explicit non-goals

* **No write API.** The operator edits TOML by hand. A future
  `cs galaxies register <name>` verb is a separate molecule.
* **No cache invalidation.** The optional `claude_md_digest`
  field is UI-informational only; it is never load-bearing.
* **No identity, auth, or presence.** Out of scope per architect's
  gap analysis.
* **Not a coupling path for cross-galaxy messaging.** Names
  resolve to paths; nothing else flows through here (ADR-047,
  ADR-064).

## Alternatives considered

* **Daemon with filesystem watch** — rejected (ADR-054). The
  daemon-shaped solution hides state in RAM and makes crash
  recovery non-trivial. A TOML file is git-checkable, offline,
  zero-dependency.
* **Walk `$HOME` on every `cs ask`** — rejected. O(galaxies)
  directory walks + TOML parses per invocation is the problem
  the registry exists to eliminate. Walking is the fallback
  semantic (`cs init` walk-up), not the steady-state ingress.
* **Put galaxies in `.cosmon/config.toml` per galaxy** — rejected.
  Per-galaxy config is where that galaxy declares its own
  shape. The *catalog* of galaxies is a user-level concern and
  belongs in `$XDG_CONFIG_HOME`.
