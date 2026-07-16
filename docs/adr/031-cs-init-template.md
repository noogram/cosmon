# ADR-031: `cs init --template` Phase 2 design envelope

**Status:** Proposed
**Date:** 2026-04-12
**Parent deliberation:** `delib-20260412-30d7`

## Context

Phase 1 of the init scaffolding ships as a shell script
(`scripts/quickstart-wikipedia.sh`) that sets up a cosmon project for a
known recipe. Phase 2 promotes the scaffold into the binary so that
projects can be bootstrapped without relying on out-of-band scripts and
so templates become first-class, discoverable artifacts.

We need to reserve the flag envelope *now*, before a second template
lands and forces a rushed decision that we later pay for in semver
breakage. The design must leave clear room for variable substitution,
prompts, remote registries, and nested composition — without committing
to any of them in v1.

The full semver analysis is in
`.cosmon/state/fleets/default/molecules/delib-20260412-30d7/responses/tolnay.md`;
the governing synthesis is D2 (insight #1) in the same molecule's
`synthesis.md`.

## Decision

Phase 2 introduces a single command with a minimal, tolnay-disciplined
surface:

```
cs init [PATH] --template <NAME> [--from <SOURCE>] [--force]
```

### Flags

- **`--template <NAME>`** — named resolution only. `NAME` must match
  `[a-z0-9][a-z0-9-]*` (kebab-case). This is the happy path and the
  only surface most users will see.
- **`--from <SOURCE>`** — escape hatch. Accepts a filesystem path or a
  git URL and bypasses name resolution entirely. Mutually exclusive
  with `--template`. Exists so power users can iterate on a template
  without publishing it, and so CI can pin a template by commit SHA.
- **`--force`** — overwrite on conflict. Default behaviour when a
  target file already exists is **refuse and print a diff**, not
  silently overwrite. This is the tolnay refuse-on-conflict
  invariant: destructive actions require an explicit opt-in.
- **`PATH`** — target directory. Defaults to `.`. Must be empty or
  pass the conflict check.

### Resolution order for `--template <NAME>`

The first hit wins:

1. `$CS_TEMPLATES_DIR/<name>/` — environment override (CI, tests,
   local dev).
2. `./.cosmon/templates/<name>/` — project-local templates (a
   monorepo can ship its own without global install).
3. `$XDG_CONFIG_HOME/cosmon/templates/<name>/` (falling back to
   `~/.config/cosmon/templates/<name>/` on hosts without XDG) —
   user-level templates.
4. **Embedded** — templates compiled into the `cs` binary. Guarantees
   that `cs init --template quickstart` works on a fresh machine with
   no configuration.

### Non-goals for v1

- **No variable substitution.** Templates are copied verbatim. No
  `{{name}}`, no Handlebars, no Tera, no `.env` interpolation. A
  template that needs per-project values publishes a post-init hook
  or documents a manual edit. Adding substitution later is a
  **major** version bump (see *Semver risk*).
- **No interactive prompts.** Non-interactive by construction; the
  CLI stays scriptable.
- **No remote registries.** `--from <git-url>` is the only remote
  path, and it is explicit.
- **No nested templates / inheritance.** One template = one tree.

## Reserved names and flags

To preserve design room we explicitly reserve the following surface.
None of these ship in Phase 2; introducing any of them is a deliberate
future decision, not a drive-by PR.

**Commands:** `cs quickstart`, `cs new`, `cs fleet create`,
`cs init --bare`.

**Flags:** `--preset`, `--profile`, `--recipe`, `--registry`,
`--template-dir`.

Claiming these slots now prevents a second contributor from landing
`cs quickstart` next week and splitting the mental model.

## Rejected alternatives

- **`cs quickstart`** — a second top-level verb for what is
  structurally `cs init` with a default template. Doubles the
  surface area; rejected.
- **`cs fleet init --template`** — conflates fleet configuration
  (runtime concern) with project scaffolding (bootstrap concern).
  Rejected.
- **`--preset` / `--profile` / `--recipe` soup** — three synonyms
  for one concept. Pick one word (`template`) and stick with it.

## Semver risk

With this scope, semver risk is **low**: `--template` and `--from`
are additive; `--force` is a no-op unless opted into.

The following each trigger a **major** bump when added later, which
is exactly why they are out of v1:

- Variable substitution (changes the file-copy contract).
- Paths inside template names (changes the name grammar).
- Interactive prompts (changes invocation semantics: non-interactive
  scripts must keep working).
- Remote template registries (changes the trust model).

## Migration

`scripts/quickstart-wikipedia.sh` continues to work. It is not
deprecated by this ADR. When the `quickstart` embedded template lands
in Phase 2, the script becomes a thin shim that invokes
`cs init --template quickstart` and eventually goes away — tracked
separately.

## Consequences

- Contributors have a fixed envelope: template-bearing work goes
  through `cs init --template`, and everything else is a future ADR.
- The binary ships at least one embedded template (`quickstart`) so
  the default works offline.
- Future work items unblocked by this ADR: embedded template
  registry layout, conflict-diff renderer, `--from <git-url>`
  fetcher, post-init hook protocol.
