# Cosmon Templates

Fleet templates are reusable project archetypes that encode role structures,
workflow formulas, dev recipes, and documentation skeletons. They are the
**horizontal gene transfer** mechanism — proven patterns absorbed from living
colonies and templatized for reuse.

## Available Templates

| Template | Source Colony | Description |
|----------|-------------|-------------|
| [wikipedia-production](../../templates/wikipedia-production/) | wiki2, wiki3 | 5+2 role fleet for Wikipedia-quality encyclopedia articles (Justfile, README, CLAUDE.md absorbed from wiki3) |
| [formal-research](../../templates/formal-research/) | foundry | 3+1 role fleet for formal verification with LLM firewall |

## Usage

Templates are copied literally — there is no variable substitution engine.
Sentinels marked `<CHANGE_ME_*>` must be replaced manually.

```sh
# 1. Initialize a new cosmon project
cs init

# 2. Copy the template
cp -r templates/formal-research/* .

# 3. Replace sentinels
grep -r 'CHANGE_ME' . | # find all sentinels and replace them

# 4. Copy fleet.toml to .cosmon/fleets/
cp fleet.toml .cosmon/fleets/

# 5. Copy formulas to .cosmon/formulas/
cp formulas/*.formula.toml .cosmon/formulas/
```

## Anatomy of a Template

Each template directory contains:

| File | Purpose |
|------|---------|
| `fleet.toml` | Fleet configuration: roles, prompts, channels, grades |
| `formulas/*.formula.toml` | Colony-specific workflow formulas (NOT builtins) |
| `Justfile.tmpl` | Dev workflow recipes (`just dev`, `just check`, etc.) |
| `MISSION.md.tmpl` | Mission frontmatter skeleton |
| `README.md.tmpl` | Project README template |
| `docs/` | Documentation skeleton (vocabulary, coding-rules, ADRs, chronicles) |

## Absorption Vocabulary

- **Absorb** — extract a reusable pattern from a colony into a template
- **Colony** — a living cosmon project that has developed proven patterns
- **Horizontal gene transfer** — importing a specific pattern from one project
  to another without merging the entire codebase (biology term)
- **Sentinel** — a `<CHANGE_ME_*>` placeholder in a template file

## Creating a New Template

Use the `absorb` formula to extract patterns from a colony:

```sh
cs nucleate absorb --var colony="/path/to/colony" --var template_name="my-template"
cs tackle <id>
```

The formula scans the colony, identifies absorbable artifacts, templatizes them
with `<CHANGE_ME_*>` sentinels, and verifies the result deploys cleanly.
