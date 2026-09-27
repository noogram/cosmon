<!-- SPDX-License-Identifier: AGPL-3.0-only -->

# ADR-181 — A codex worker writes its settings to its own profile

**Status:** Accepted  
**Date:** 2026-09-27  
**Issue:** noogram/cosmon#84  
**Implementation:** `task-20260927-9b99`  
**Extends:** ADR-177 (harness settings are carried verbatim)

## Context

An operator attached to a running codex worker and changed its reasoning level
in the codex TUI. codex persisted the change into `~/.codex/config.toml`, and
every later codex worker on the machine inherited it. ADR-177's per-dispatch
`-c key=value` overrides do not help here: they set what a worker starts with,
not where codex writes a change made during the run.

codex writes such a change to the *active user config layer*. Without a
profile, that layer is `$CODEX_HOME/config.toml`. Since profile-v2,
`codex -p <name>` layers `$CODEX_HOME/<name>.config.toml` on top of the base
file and makes the overlay the active user layer. Measured against
codex-cli 0.157.1 with a scratch `CODEX_HOME`: the same `/model` → High →
"enter default" gesture rewrote `model_reasoning_effort` in `config.toml`
without `-p`, and wrote it only to the overlay with `-p w1`, leaving the base
file byte-identical. A missing overlay file reads as empty.

## Decision

An interactive codex worker launches with `-p cosmon-worker-<session>`, where
`<session>` is its tmux session name mapped onto codex's profile-name alphabet
(`[A-Za-z0-9_-]`). The flag is structural, like `--add-dir`: it survives an
`[adapters.codex].extra_args` override. It is omitted when the operator's
`extra_args` or harness settings already select a profile, because codex
accepts one profile and the explicit choice wins. `codex exec` does not get it;
it has no TUI to persist from.

The overlay is keyed by session name, which is per molecule, so a re-tackle of
the same molecule keeps its adjustment. cosmon never creates or deletes the
file; codex creates it on the first write. Deleting it resets the molecule to
the machine defaults.

## Alternatives rejected

- **A private `CODEX_HOME` per worker.** It stops the write-back too, but the
  `ChatGPT` login (`auth.json`) and the session rollouts the realized-model
  observer reads live in that directory. Isolation would mean copying
  credentials or linking them. A copy splits the login: a token refresh in
  one home rotates the refresh token the others still hold. A link depends on
  how codex rewrites `auth.json`, which was not established here.
- **Drift detection only** (hash the global config at dispatch and harvest).
  It makes the leak visible after the fact but does not prevent it. The issue
  names it as the fallback if isolation is not possible, and the profile
  overlay makes isolation possible.

## Consequences

- The global `~/.codex/config.toml` stays the default that workers read.
  cosmon's own write to it, the exact-path project pre-trust, is unchanged.
- Adjustments made inside workers accumulate as small
  `~/.codex/cosmon-worker-*.config.toml` files, one per molecule where someone
  changed a setting. They are an honest record of those changes and can be
  removed at any time.
- This depends on codex's profile-v2 `-p` semantics. A codex version where
  `-p` names a legacy `[profiles.<name>]` table in `config.toml` would refuse
  to start with an unknown profile. The worker then fails the readiness probe
  loudly rather than leaking silently.

**Falsifier.** A codex release in which a TUI change made under `-p <name>`
still reaches `$CODEX_HOME/config.toml`.
