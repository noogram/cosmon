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

Both launch paths render the tokens from one builder,
`cosmon_core::worker_argv::codex_worker_profile_args`: `cs tackle` quotes them
into the tmux command string (`build_codex_command`), and the in-process
`LibraryExecutor` used by `cs run --resident` and the RPP executor hands them
to the transport as argv. The first cut applied the overlay only on the
`cs tackle` path, and an independent review found the in-process path still
launching codex bare.

The overlay is keyed by session name, which is per molecule, so a re-tackle of
the same molecule keeps its adjustment. cosmon never creates the file; codex
creates it on its first config write. That write is not only an operator's
change: codex also records startup acknowledgements there (a model-migration
notice, the screen-reader check, new-model announcement counts) when the base
file does not already hold them. Deleting the file resets the molecule to the
machine defaults.

### Harvest and collapse leave the overlay in place

`cs done`, `cs collapse` and the resident harvest do not remove
`cosmon-worker-<session>.config.toml`. Four reasons:

1. **It is the record of the drift.** The issue's minimum acceptable
   behaviour was to make a worker-side change visible. The overlay is exactly
   that record, keyed by molecule; deleting it at harvest would erase the
   evidence at the moment the work is judged.
2. **It has no effect once the molecule is terminal.** The session name
   carries the molecule id, so no later worker selects that profile. A
   leftover overlay changes no behaviour.
3. **cosmon does not own `CODEX_HOME`, and the harvester cannot name it
   reliably.** Harvest runs in `cs done`, the patrol, or the resident loop,
   each resolving `CODEX_HOME` from its own environment, which is not
   guaranteed to be the one the worker's pane was launched with (the tmux
   server freezes its environment at first start). An automatic delete could
   miss the file or remove a same-named file in another home. This follows
   ADR-178's stance that no automatic path deletes what it cannot prove it
   owns.
4. **The cost is small and the files are easy to clear.** Each overlay is a
   few hundred bytes with a fixed prefix; `rm ~/.codex/cosmon-worker-*.config.toml`
   clears them all, and removing one for a running worker is safe (codex
   recreates it on its next write).

Revisit this if overlays are found to affect a later worker, or if cosmon
comes to own a per-dispatch `CODEX_HOME` value it can pass to the harvester.

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
- Small `~/.codex/cosmon-worker-*.config.toml` files accumulate, at most one
  per codex molecule. A file appears only when codex writes config during
  that worker's run, whether because an operator changed a setting or because
  codex recorded a startup acknowledgement.
- An acknowledgement a worker records (for example, dismissing a new-model
  notice) stays in its overlay, so the next worker can show the same notice
  until the operator acknowledges it in their own codex session, which
  writes the base file.
- This depends on codex's profile-v2 `-p` semantics. A codex version where
  `-p` names a legacy `[profiles.<name>]` table in `config.toml` would refuse
  to start with an unknown profile. The worker then fails the readiness probe
  loudly rather than leaking silently.

**Falsifier.** A codex release in which a TUI change made under `-p <name>`
still reaches `$CODEX_HOME/config.toml`.
