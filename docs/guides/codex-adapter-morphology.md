<!-- SPDX-License-Identifier: AGPL-3.0-only -->

# Codex adapter morphology — reverse-engineered from source

The cosmon codex adapter must serve as a reliable **cross-model second
opinion** (the anti-over-confidence lever the `cosmon-dev` spore's committee
depends on, seat `codex-sol`). Three deterministic blockers stood in the way.
This guide records the *real morphology* of the OpenAI Codex CLI
(`github.com/openai/codex`, Rust + TS, Apache-2.0), read from source at
`@HEAD` against local `codex-cli 0.144.6`, so the adapter **épouse la forme de
l'outil** rather than fighting it with empirical workarounds. Every fact below
is cited to a source path or a `--help` output; nothing is guessed.

Principle: *an adapter fits the shape of its host tool; it does not coerce it.*
Reverse-engineering from source makes the adaptation drift-proof — the source
is the truth, not our degraded empirical observations.

---

## Blocage 2 (fixed) — out-of-worktree state lock denied by the sandbox

### Symptom

A codex worker did the work (audit rendered in the pane) but died without
closing: `cs evolve` could not write the Cosmon state lock outside the worktree
(`Operation not permitted`), so it could neither record the audit nor run
`cs complete`. The molecule wedged `running` with a dead pane and the ephemeral
pane work was lost (observed 2× — `eb9f`, `e559`).

### Root cause (morphology)

Codex's `workspace-write` sandbox computes its writable roots in
`SandboxPolicy::get_writable_roots_with_cwd`
(`codex-rs/protocol/src/protocol.rs:1176-1264`). The **only** default writable
roots are:

1. explicitly-configured `writable_roots`,
2. **cwd** (the process working dir),
3. **`/tmp`** (unless `exclude_slash_tmp`),
4. **`$TMPDIR`** (unless `exclude_tmpdir_env_var`).

There is **no** "walk up to the git toplevel" logic; each root grants its own
subtree only (`can_write_path_with_cwd`,
`codex-rs/protocol/src/permissions.rs:719-726`).

A cosmon worker's cwd is its isolated worktree
(`<main-repo>/.worktrees/<mol>/`), but the fleet state it must write — the
`fleet.lock` / `trunk.lock` advisory locks, molecule state, and `events.jsonl`
— lives in the **main** repo's `.cosmon/state/` (walk-up discovery redirects a
worktree's state host to the main checkout, see
`cosmon_filestore::walk_up_find_cosmon_dir_from`). That path is a
*sibling-of-an-ancestor* of the worktree — outside the cwd subtree, not under
`/tmp` or `$TMPDIR`, and not in `writable_roots`. So the write is **correctly
denied**. This is expected sandbox behaviour, not a codex bug.

Note: `--dangerously-bypass-approvals-and-sandbox` *fully* disables the FS
sandbox (`cli/src/main.rs:1955-1964` sets `sandbox_mode = DangerFullAccess`,
which returns empty writable roots and `has_full_disk_write_access() == true`,
`protocol.rs:1155-1178`). The interactive default carries that flag, so the
denial reproduces in the paths that *don't* — `codex exec` (no bypass) and any
operator who overrides `extra_args` to a hardened `--sandbox workspace-write`
posture.

### Fix (morphological)

Codex exposes a **first-class** `--add-dir <DIR>` flag — "Additional
directories that should be writable alongside the primary workspace" (in both
`codex` and `codex exec --help`; the flag form of
`sandbox_workspace_write.writable_roots`). The adapter now declares the
main-repo `.cosmon/` directory (which contains `state/`) writable via
`--add-dir`, resolved from the worker's worktree with the same
`walk_up_find_cosmon_dir_from` redirect `cs evolve` uses to find the state
store — so the two agree by construction.

The flag is **structural**, like the self-update kill
(`NO_STARTUP_UPDATE_OVERRIDE`): emitted in both launch modes and *not* part of
the `DEFAULT_INTERACTIVE_ARGS` set an `extra_args` row replaces. An operator
who hardens the sandbox must never thereby re-break the worker's ability to
write its own completion lock. Empty `writable_roots` (a bare CI checkout with
no `.cosmon/` ancestor) emits no `--add-dir` and leaves the command
byte-identical to the pre-fix shape.

- Struct field: `CodexSessionConfig::writable_roots`
  (`crates/cosmon-transport/src/codex.rs`).
- Emission: `push_writable_roots` in `build_codex_command`.
- Resolution/wiring: `spawn_codex_and_prompt`
  (`crates/cosmon-cli/src/cmd/tackle.rs`).
- Deterministic repro (fails for the right reason pre-fix): tests
  `writable_root_declares_out_of_worktree_state_dir_writable`,
  `writable_root_survives_extra_args_override` (codex.rs) and
  `writable_root_is_declared_in_both_modes_and_survives_override`
  (`tests/codex_interactive_command.rs`).

---

## Blocage 1 (documented) — `codex-sol` / `codex-terra` invalid under a ChatGPT account

### Symptom

`cs tackle --adapter codex --model codex-sol` dies at model-select:
`ERROR 400 invalid_request_error: "The 'codex-sol' model is not supported when
using Codex with a ChatGPT account."` (deterministic). Workaround that works:
`--adapter codex` **without** `--model` (the account's default GPT model).

### Root cause (morphology) — the restriction is **server-side**, not client

The Codex client does **not** hardcode a per-auth model allowlist, and does
**not** validate `--model` locally:

- `--model <slug>` → `ConfigOverrides.model` → wins over config
  (`core/src/config/mod.rs:3696`) → passed to the provider verbatim.
  `requested_model_is_available` only gates when `allow_provider_model_fallback`
  is set (`models-manager/src/manager.rs:570-579`); otherwise an invalid slug
  is **not** rejected locally — it goes to the backend and returns the 400.
- The valid set under a ChatGPT account is fetched at runtime and
  **visibility-filtered by the server**: when the user has a ChatGPT account and
  the remote list carries at least one `ModelVisibility::List` model, that
  remote list becomes the sole source of truth (`apply_remote_models`,
  `models-manager/src/manager.rs:422-437`). Under API-key auth the bundled
  catalog is used instead.
- The exact error string appears only in a backend-mock test
  (`core/tests/suite/compact.rs:2338-2340`); the client's role is to *react* to
  the upstream 400, not emit it.
- Auth mode is legible locally: `~/.codex/auth.json` carries
  `"auth_mode": "chatgpt"` (vs `"apikey"`) and `OPENAI_API_KEY`. `AuthMode`
  enum + `has_chatgpt_account()` / `uses_codex_backend()` at
  `codex-rs/protocol/src/auth.rs:9-55`.

**Conclusion:** `codex-sol`/`codex-terra` are API-key-only slugs server-side; a
ChatGPT account rejects them. cosmon **cannot** hardcode a valid list without
drifting from the server. A client-side allowlist is exactly the coercion this
guide forbids.

### Recommended resolution (named follow-up: `codex-chatgpt-model-preflight`)

Two honest, drift-proof options — do **not** invent a client model allowlist:

1. **Document + default (this section).** Under a ChatGPT account, only the
   account's default model works reliably; pin nothing (omit `--model`) for the
   `codex-sol` committee seat, or
2. **API-key auth.** `codex login` with an API key (`auth_mode: "apikey"`) makes
   the full bundled catalog — including the `codex-*` / sol/terra/luna family —
   valid, at which point a model pin resolves.

A future preflight may read `~/.codex/auth.json` (codex's own drift-proof
source) and, when `auth_mode == "chatgpt"` **and** an explicit `--model` is
pinned, surface a clear dispatch-time warning ("non-default model may be
rejected by the Codex backend under a ChatGPT account; omit `--model` or use
API-key auth") *instead of* letting the worker die mid-run. It must stay a
warning, never a hardcoded block, because the valid set is server-owned.

---

## Blocage 3 (documented) — lifecycle / completion detection

### Morphology

- **`codex exec`** runs one turn then exits: **0** on success, non-zero (1) on a
  fatal error (`error_seen` tracked "for automation-friendly signaling",
  `exec/src/lib.rs:957-959`). Per-command failures surface their own
  `exit_code` in the event stream; `codex exec --json` emits parseable
  completion.
- **Interactive TUI** is an event loop: after a turn it fires an
  `agent-turn-complete` notification and **stays open** awaiting input
  (`tui/src/chatwidget/turn_runtime.rs:215`,
  `chatwidget/notifications.rs:70`). It does not exit per-turn — so "pane died"
  is *not* a completion signal for an interactive codex worker.
- **`notify` hook** — config `notify: Option<Vec<String>>`
  (`core/src/config/mod.rs:737`). Invoked as
  `notify-send Codex '{"type":"agent-turn-complete",…}'` with the JSON as the
  final argv argument (`hooks/src/legacy_notify.rs:13-41`); spawned detached.
  Payload is kebab-case: `type`, `thread-id`, `turn-id`, `cwd`, `client`,
  `input-messages`, `last-assistant-message`.

### Recommended resolution (named follow-up: `codex-notify-completion-hook`)

Wire a cosmon-side `notify` command into the codex worker's config (`-c
'notify=[...]'` or `~/.codex/config.toml`) so the adapter detects
`agent-turn-complete` deterministically and, when the worker finished its audit
but could not self-complete, **recovers the pane artifact** instead of leaving
the molecule wedged `running`. This replaces the fragile "pane died" heuristic
for interactive codex workers. Deferred here to keep this change scoped to the
work-losing Blocage 2; the `notify` payload above is the stable contract.

---

## Version caveat

Read against `openai/codex@HEAD`; local binary is `codex-cli 0.144.6`. The model
*slugs* were renamed to the sol/terra/luna family after 0.144.6 (hence the
`codex-sol` slug observed), but the three load-bearing mechanisms —
remote-catalog + auth-gated model resolution, `get_writable_roots_with_cwd`
semantics, and the `notify` array → JSON-argv on turn complete — are stable
across that window.
