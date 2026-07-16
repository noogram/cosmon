# .cosmon/ Directory

This directory contains cosmon's project-local state: formulas, molecule
declarations, runtime state, and surface projection configuration.

## Directory structure

```
.cosmon/
  surfaces.toml           # Surface projection config (git-tracked)
  formulas/               # Formula templates (git-tracked)
    idea-to-plan.formula.toml
    task-work.formula.toml
  molecules/              # Molecule declarations (git-tracked, TOML)
  registry.sqlite         # Project-local nervous system (git-ignored)
  .gitignore              # Ignores state/, registry, locks, tmp files
  state/                  # Runtime state (git-ignored)
    fleet.json            # Fleet definition (workers, roles)
    events.jsonl          # Event log
    surfaces.snapshot.json  # Last projection hashes (for 3-way divergence)
    fleets/
      {fleet-name}/
        molecules/
          {molecule-id}/
            state.json    # Molecule runtime state
            briefing.md   # Step briefing for the assigned worker
            log.md        # Execution log
    surfaces/
      github/
        {owner-repo}/
          {molecule-id}.json  # Local mirror of projected GitHub Issue
```

## What is git-tracked vs git-ignored

| Path | Tracked | Purpose |
|------|---------|---------|
| `surfaces.toml` | yes | Declares which surfaces to project |
| `formulas/` | yes | Formula templates shared across the team |
| `molecules/` | yes | Molecule declarations (TOML, not runtime state) |
| `.gitignore` | yes | Keeps runtime state out of git |
| `state/` | **no** | Runtime state, rebuilt by cosmon commands |
| `registry.sqlite` | **no** | Nervous system DB, rebuilt from declarations |
| `*.lock`, `*.tmp` | **no** | Transient files |

## Key commands

```bash
# Project internal state onto surface files (STATUS.md, ISSUES.md, etc.)
cs reconcile

# Check if surfaces are up to date (dry-run, exit code 0 or 1)
cs reconcile --check

# Fetch GitHub Issue state before comparing
cs reconcile --fetch

# Full check with remote state
cs reconcile --fetch --check

# View fleet and molecule status
cs ensemble
```

## Further reading

- [docs/surface-sync-protocol.md](../docs/surface-sync-protocol.md) -- full sync protocol guide
- [THESIS.md Part XVI](../THESIS.md) -- Surface Observability theory
- [docs/adr/013-particle-convergence.md](../docs/adr/013-particle-convergence.md) -- the ADR
