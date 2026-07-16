## ADR-096 — OpenClaw, claw-code, and NemoClaw treated as bibliography, never as dependency

**Status:** Accepted — 2026-05-17.
**Date:** 2026-05-17
**Parent task:** `task-20260517-a226`
— pirate sub-verdict: three parallel research deliverables informing
future cosmon-runtime + smithy containerization work.
**Companion notes (read first if you have not):** captured in internal
architecture notes (OpenClaw + NemoClaw reads, plus smithy's NemoClaw
containerization lessons).

## Context

Three public agent-runtime projects were read during this task:

- **OpenClaw** (`github.com/openclaw/openclaw`) — a multi-channel
  personal-assistant gateway, NodeJS, daemon-shaped.
- **claw-code / claw-code-rust / claw-code-parity**
  (`github.com/instructkr/claw-code`,
  `github.com/ultraworkers/claw-code-parity`) — a Korean
  clean-room rewrite of the leaked Claw Code TypeScript source,
  with an in-progress Rust port.
- **NemoClaw** (`github.com/NVIDIA/NemoClaw`) — NVIDIA's
  opinionated hardening stack built on its general-purpose
  agent runtime **OpenShell**.

Each one solves a problem adjacent to cosmon's. Each one has
useful patterns. Each one is **the wrong category of system to
depend on** for cosmon:

- OpenClaw is daemon-shaped; cosmon's wedge is stateless CLI.
- claw-code is a single-agent harness, not an orchestrator.
- NemoClaw embeds Kubernetes; cosmon does not orchestrate
  containers.

The companion lore notes already capture what we learned. This
ADR is the **governance layer**: how we make sure the reading
informs cosmon without contaminating it.

## Decision

### 1. The three repos are **bibliography**, not dependencies.

No crate in the cosmon workspace may declare a runtime or build
dependency on:

- any package under `@openclaw/*` or `openclaw*` on npm,
- any crate from `instructkr/claw-code` or `ultraworkers/*`,
- any image under `ghcr.io/nvidia/openshell-*` or
  `ghcr.io/nvidia/nemoclaw*`,
- any sibling `~/dev/git/{openclaw,claw-code,claw-code-rust,claw-code-parity}/`
  path as a `path = "..."` dependency or workspace member.

A future ADR may revisit this if a specific, irreplaceable
dependency case emerges. None exists today and we should be
surprised if one does — the systems are not designed to be
embedded as libraries.

### 2. OpenClaw-derived type names must not appear in `cosmon-core`.

If, after reading the bibliography, we decide a pattern is worth
adopting, the adopted version is **re-named into cosmon's
vocabulary** before any line of code is written. Concretely, the
following names from the bibliography are **forbidden** in
`crates/cosmon-core/src/**` and in cosmon's public CLI surface
(`cs <verb>` and `--<flag>` enumerations):

- `Gateway`, `Sandbox`, `Blueprint`, `OpenShell` (capitalised
  type names from NemoClaw),
- `Session` used in the OpenClaw sense (a context container
  expiring at 4 AM) — `Molecule` is cosmon's word,
- `Plugin`, `PluginSDK`, `Extension` used in the OpenClaw
  plugin-loader sense — cosmon has `Formula`,
- `Channel` used in the OpenClaw messaging-surface sense —
  cosmon's `channels` are the six communication planes
  (neurion, DAG, FS, artifact chain, propulsion, whisper),
- `agent` used as a process/daemon name — cosmon's `Agent` is
  a typed cognition identity (see THESIS Part IV).

These names already mean specific things in cosmon's
ubiquitous-language glossary (THESIS Part IV–V). Re-using the
bibliography's homonyms inside cosmon would silently fork the
glossary, which is the cosmon-ward failure mode we cannot afford
(see CLAUDE.md *« Cosmon-ward feedback flow »* and architectural
invariants §8b).

When a borrowed pattern needs a fresh name, propose it in the
adopting PR; CODEOWNERS-side review enforces this section.

### 3. Patterns identified as adoption candidates carry an explicit rename in the lore notes.

The two lore companion notes already do this work:

| Bibliography pattern                 | Source     | Proposed cosmon name      | Decision phase |
|--------------------------------------|------------|---------------------------|----------------|
| `openclaw onboard`                   | OpenClaw   | `cs onboard` (different scope) | Future        |
| Scoped progressive-disclosure AGENTS.md | OpenClaw | Keep `CLAUDE.md`, recurse | Future         |
| Skills snapshot at session start     | OpenClaw   | *formula seal* (briefing-seal extension) | Future |
| Versioned digest-pinned blueprint    | NemoClaw   | *forme* (smithy-side)   | smithy Phase 3 |
| Gateway holds secrets, sandbox sees placeholder | NemoClaw | `smithy-keep` (smithy-side) | smithy Phase 2 |
| Out-of-process kernel policy (Landlock + netns) | NemoClaw | (no rename — Linux primitive name) | smithy Phase 2 |

A pattern not on this table cannot be adopted by drive-by — it
needs its own ADR or, at minimum, a citation back to this ADR
and a name proposed in the PR.

### 4. Patterns explicitly **refused** are inscribed so they cannot return by mimétisme.

| Refused pattern                       | Source     | Reason                              |
|---------------------------------------|------------|-------------------------------------|
| Persistent gateway daemon             | OpenClaw   | Kills the stateless-CLI wedge       |
| Plugin loader with manifest + npm install | OpenClaw | Forks the composability axis (formulas + molecules are the only extension points) |
| Multi-channel inbox (WhatsApp, Slack, …) | OpenClaw | Scope dilution toward personal-assistant category |
| Embedded k3s in deployment            | NemoClaw   | Control-plane footprint inappropriate for cosmon's single-laptop target |
| TUI-approval-per-egress               | NemoClaw   | Incompatible with autopilot doctrine |
| Mono-registry cloud distribution      | NemoClaw   | Violates CI9 (smithy) and is structurally fragile |

These refusals are **structural**, not aesthetic. A PR that
reintroduces any of them is a structural breach and must be
filed as an ADR-grade molecule, not merged as an enhancement.

### 5. The git-remote blocklist does **not** need extension.

The three upstream repos are not internalised substrates (no
snapshot-copy, no `path = "..."` dependency, no shared lineage).
The mechanism in CLAUDE.md *« Git remote allowlist »* (the
`almanac` precedent) does not apply here. **No entry is added to
`.cosmon/config.toml [git_remote_blocklist]`** as part of this
ADR. If a future PR ever introduces an internalised substrate
copy of one of these repos, that PR must (a) revisit this ADR,
(b) add the corresponding remote to the blocklist, (c) justify
the internalisation.

## Consequences

### Good

- The reading is captured in internal architecture notes where it can inform
  future PRs without contaminating `cosmon-core`.
- Pattern adoption has a clear gate (rename + ADR citation).
- Pattern refusal is named explicitly, so a future contributor
  cannot innocently re-propose a daemon-shaped gateway.
- The smithy-side note ties the same reading to a different
  galaxy's roadmap without forcing cosmon to inherit smithy's
  decisions.

### Acceptable cost

- A PR author who wants to adopt a bibliography pattern must
  re-read this ADR and propose a rename. That is a (small)
  recurrent tax. It is the cost of keeping the glossary clean.
- Some future readers will reach for the OpenClaw / NemoClaw
  vocabulary because it is familiar; this ADR is the place to
  point them.

### Acknowledged limit

- The ADR cannot prevent **idea contamination** at the design
  level — a contributor who has spent days inside OpenClaw will
  unconsciously bring its mental model. The mitigation is
  social, not structural: cite this ADR in design discussions,
  re-read the *« what cosmon should learn »* section of the
  companion notes, and reach for the cosmon glossary first.

## Verification

A reviewer checking this ADR's compliance on a PR should:

1. **Grep for forbidden names** in any new `cosmon-core/**/*.rs`:
   ```
   git diff main -- crates/cosmon-core/ | rg -i '\b(Gateway|Sandbox|Blueprint|OpenShell|PluginSDK|Extension)\b'
   ```
   Non-empty = breach unless the PR is this ADR itself.
2. **Grep for forbidden dependencies** in `Cargo.toml` or
   `package.json` files anywhere in the workspace:
   ```
   git grep -E '(openclaw|claw-code|nemoclaw|openshell)' -- '*.toml' '*.json'
   ```
3. **Confirm any adopted pattern carries a citation** back to
   this ADR and to the relevant lore note.

These are not CI-automated today; doing so would itself violate
*« propose mechanisms of verification, do not impose them »*
(architectural invariants §8b). The check is performed at
review time.

## Related

- Internal architecture notes — OpenClaw + NemoClaw reads, plus smithy's NemoClaw containerization lessons.
- [ADR-038](038-whisper-perturbation-port.md) — whisper port (the only push channel cosmon has, contrasted with NemoClaw's WS pushes).
- [ADR-082](082-architecture-baseline.md) — substrate-tier obligations cosmon must honour.
- [`CLAUDE.md` § Git remote allowlist](../../CLAUDE.md) — precedent for the `almanac` internalisation rule; explicitly *not* extended here.
- [THESIS.md Part IV–V](../../THESIS.md) — cosmon's ubiquitous-language glossary that the forbidden-names list protects.
