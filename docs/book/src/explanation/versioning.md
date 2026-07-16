# Versioning policy

Cosmon is pre-1.0. Its version numbers follow **semantic versioning**, read the
way every `0.x` project reads it: while the leading number is zero, the *minor*
number carries the weight a *major* number normally would.

- **`0.x.y` → `0.x+1.0`**: a minor bump may include breaking changes. Before
  1.0, the API surface (CLI verbs, flags, JSON output shape, on-disk state
  format) can shift between minor releases. Read the changelog before upgrading.
- **`0.x.y` → `0.x.y+1`**: a patch bump is fixes and additive changes only.
- **`1.0.0` onward**: once cosmon reaches 1.0, the usual semver contract
  applies: breaking changes wait for a major bump, and the CLI surface,
  `--json` output, and state format become stable promises.

## What "stable" will mean at 1.0

Three surfaces are the public contract, and they are what 1.0 will freeze:

1. **The CLI**: command names, their flags, and their semantics. The generated
   [reference](../reference/overview.md) is a projection of the actual tool, so
   the reference and the binary cannot silently disagree.
2. **The `--json` output**: the agent-first interface. Every command honours
   `--json`, and its shape is part of the contract because other tools parse it.
3. **The on-disk state format**: the JSON files in `.cosmon/state/`. Because
   these files *are* the source of truth (see
   [Why a stateless CLI](./stateless-cli.md)), their format is as much a public
   API as any function signature.

## No version switcher, yet

This documentation site describes one version: the one on `main`. There is
deliberately **no multi-version switcher** on the site today. Building one is
premature until there is a real `0.x → 1.0` break worth navigating between; the
scheme is stated here in prose now, and the switcher is deferred to the first
version boundary that actually needs it. This is the same "document the policy
first, build the tooling when the need is concrete" discipline cosmon applies
everywhere.
