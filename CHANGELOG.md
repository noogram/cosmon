# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

The pinned contract is **every user-facing binary a release ships** — `cs`,
`cosmon-remote`, `cosmon-rpp-adapter` and `cs-oidc-mock` all print the version
sealed by the matching git tag (`vX.Y.Z`) and the section heading below. The
canonical list is [`packaging/shipped-binaries.txt`](packaging/shipped-binaries.txt).
Library crates inside the workspace (`cosmon-core`, `cosmon-state`, …) are
internal and versioned independently; they carry no public API guarantee at
this stage.

> The commit-by-commit development history before `0.1.0` is preserved in the
> git log and in [`docs/lore/CHRONICLES.md`](docs/lore/CHRONICLES.md). This
> file starts its curated, public-facing record at the first tagged release.

## [Unreleased]

## [0.2.2] — 2026-07-21

**External-tester hardening.** Every issue Jesse Thaler (MIT) raised against
0.2.1 is fixed and converged clean: the flagship `cs verify` tamper check that
tripped on cosmon's own honest output, two adapter faults that left workers
alive but doing nothing, a hard-coded path leaking into worker prompts, and the
missing Linux build prerequisites. A containerised regression bench and a
null-context judge validated the fixes independently, and a math-attack v2
spore ran fourteen nodes to terminal through the local adapter with the LLM
firewall honored — end-to-end proof the sovereign local path carries real work.

### Fixed: a local dispatch no longer collapses a molecule when the backend cannot serve the model

- Dispatching to `--adapter local` against a reachable-but-empty Ollama spawned
  a worker that died within ~30 s. The patrol then auto-collapsed the molecule.
  Collapse is *terminal*, so the brief was lost and had to be re-nucleated by
  hand under a new id — an infrastructure failure destroying work that had
  nothing wrong with it. Observed twice on 2026-07-19.
- `cs tackle` now preflights the `local` / `ollama` adapter before committing a
  molecule to it: one `GET /v1/models` against the same base URL the worker
  will dial, asserting the resolved model is actually served. A backend that is
  down, or that does not serve the model, refuses the dispatch instead of
  spawning a doomed worker. The refusal is recoverable where the collapse was
  not — the molecule stays `pending` and re-tacklable, because the check runs
  before the worktree is created and before the status flips to running.
- The two failures are reported distinctly, because they need different repairs:
  a dead backend says `ollama serve`, an unpulled model says `ollama pull <id>`.
  Both state that the molecule survived.
- Bypass with `COSMON_SKIP_ADAPTER_PREFLIGHT=1`. It skips the check; it never
  weakens it.
- **Not** guarded: "no model was selected". The model chain's floor is `None`
  by design, meaning *"let the adapter use its own default"*, and it is tested
  as such. Refusing on `None` would reject every healthy bare `--adapter local`
  dispatch while still missing the real fault — an explicitly pinned but
  unpulled model dies identically. The serveable-model check catches both.

### Fixed: every shipped binary now reports the version you downloaded

- A fresh install of `0.2.1` gave you `cs 0.2.1` and `cosmon-remote 0.3.0` —
  from an asset named `cosmon-remote-0.2.1-…`. The service tarball was worse
  still: `cosmon-rpp-adapter 2.5.0` and `cs-oidc-mock 0.1.0`. Downloading one
  version and being answered with three others reads as a broken install.
- The four crates that ship a user-facing binary now inherit the release
  version. Library crates keep their independent semver, unchanged.
- **One-time version discontinuity.** `cosmon-remote` moves `0.3.0 → 0.2.1`,
  `cosmon-rpp-adapter` moves `2.5.0 → 0.2.1`, and `cosmon-oidc-testkit` moves
  `0.1.0 → 0.2.1`. The two downward moves are recorded here deliberately. They
  are legal because none of these crates has ever been published to crates.io
  (all are `publish = false`), so no registry ordering and no `cargo add`
  consumer is affected; the numbers were internal counters that no user could
  observe, while the numbers users *could* observe were wrong. From this
  release on there is one version, and it is the one on the tarball.
- `cosmon-rpp-adapter`'s `/healthz` and `/v1/auth/me` report that same version,
  so a self-hoster now reads one number from the download, from `--version`,
  and from the running service instead of three.
- The alignment is enforced, not remembered: `scripts/release-version-conformance.sh`
  runs every shipped binary at release time and fails the release on any
  mismatch, and a workspace test fails on the branch if a shipped-binary crate
  pins its own version or if the release workflow's binary list drifts from
  the canon. Both prior packaging defects (gnu-vs-musl, the missing connector)
  shipped precisely because nothing checked.

### Fixed: a worker waiting on the operator is no longer nudged toward acting

- A worker that finishes its work and holds atomic questions for a human looks
  dead on every clock cosmon owns — no progress events, a silent terminal — so
  all three nudge channels read the deliberate pause as a stall and told it to
  "continue execution immediately", over and over. That is not merely noise: a
  sentence repeated indefinitely at a gated worker is slow pressure toward
  taking the very action the gate exists to withhold.
- `cs patrol --propel`, `cs patrol --nudge`, and the `--heal` re-engagement
  remedy now pass through **one** admission judge instead of three copies of
  "does this look idle?" — so a repair lands everywhere at once, which the
  previous fix (thinking-worker spam, 0.2.1) did not. The operator gate
  outranks every clock in it, and is recognised from either the
  `temp:awaiting-op` tag or the durable `blocked_on.json`. A molecule that is
  not `Running` is likewise never nudged; a `Starved` one especially, where a
  re-prompt can compound the throttle. `--propel` now reports gated workers in
  their own line: the one decline the operator must act on, because the
  molecule is waiting on *them*.

### Added: merge history now reads like the mission that produced it

- A `cs done` completion merge now carries scheduler-derived lineage trailers —
  `Mol-Id`, `Mission-Id`, and `Depends-On` — so the shape of a mission is
  recoverable from the git log alone, not just from the ledger. Base-sync merges
  carry an explicit `Base-Sync` trailer, which replaces the old merge-direction
  heuristic that guessed a merge's purpose from which way it pointed and got it
  wrong on any non-trivial topology.
- A new read-only `cs mission graph <root>` renders the mission DAG by joining
  the ledger's dependency edges to the merge commits that realized them, so you
  can see the whole tree — what depended on what, and where each branch landed —
  without reconstructing it by hand.
- The ordinary single-molecule path is byte-identical to before save for the new
  trailer lines; nothing about a solo `cs done` changes shape. (Phase 1 of
  delib-20260720-cff4.)

### Fixed: `cs verify` read the wrong event schema and failed on every real molecule (Jesse #1)

- The event-chain walker read the legacy kind-tagged envelope, but real
  molecules write EventV2 records (`type` / `emitter_kind`). So `cs verify`
  failed with `missing field kind` on every molecule that had ever run — the
  flagship tamper-evidence claim tripping on cosmon's own honest output, which
  is the worst possible place for it to break.
- The walker now reads the EventV2 `seq` chain, and it requires that sequence to
  be *contiguous*. That second half matters: a dropped middle record (say `0, 2`
  with `1` excised) is now caught as tampering rather than silently accepted,
  closing the hole a schema-only fix would have left open.

### Fixed: `cs verify` seals tripped on honest rewrites (Jesse #1)

- `briefing.md` is rewritten by cosmon at each step of a molecule, and the
  bootstrap seal walked the operator's *ambient* `CLAUDE.md` / `AGENTS.md`. So
  both seals FAILed on any multi-step molecule — again, an honest rewrite read
  as tampering.
- Seals now snapshot the per-step content and verify against that snapshot. A
  legacy seal with no snapshot degrades to an honest `SKIP`-inconclusive instead
  of a false alarm, while a genuine content swap *inside* a snapshot is still
  caught — the check gets quieter about honest change without going blind to
  dishonest change.

### Fixed: the local/Ollama adapter booked no-op missions as completed (Jesse #4)

- A weak local model that produced nothing at all was still marked done, so a
  mission could report success having done no work. Completion now requires a
  real-work guard, and an unresolvable ollama model fails loudly instead of
  silently no-opping its way to a green checkmark.

### Fixed: the Claude adapter failed against Claude Code v2.x (Jesse #6)

- Two failures stacked on top of each other. Claude Code v2.x's root permission
  guard refused `bypassPermissions`, and — even past that — the briefing was
  pasted into the TUI but never submitted, so workers sat healthy and idle at
  zero tokens, looking alive while doing nothing.
- The adapter now survives the root guard (`IS_SANDBOX`) and confirms the
  briefing was actually submitted: it re-nudges Enter until the worker is
  observed Working, inside a bounded 90-second window, so a swallowed keystroke
  no longer strands a worker.

### Fixed: a hard-coded `/srv/cosmon` leaked into worker prompts (Jesse #5)

- Worker-prompt and persona paths carried a hard-coded `/srv/cosmon`, which
  surfaced verbatim in prompts on any machine that wasn't laid out that way.
  They now resolve to the project/galaxy root, so the path a worker is told
  about is the path it actually runs in.

### Docs: the Linux build prerequisites are now stated (Jesse #2)

- A from-source build on Linux/glibc needs `pkg-config` and `libdbus-1-dev` —
  the keyring pulls in secret-service, which pulls in libdbus — and nothing said
  so, so the build failed with a cryptic linker error. Getting Started now names
  the two packages up front.

### Fixed: a crashed worker no longer wedges a whole DAG (Jesse #3)

- When a detached worker process died mid-mission, a restarted `cs run` trusted
  the `running` state unconditionally and waited on the orphaned molecule until
  `--timeout` (exit 124), never re-dispatching it. (An initial investigation
  wrongly reported this as "already handled": orphan-detection code existed but
  was gated off by default, so the default `cs run` never reclaimed the orphan.)
- The worker now records a PID + start-time witness, and `cs run` consults
  liveness before treating `running` as in-flight: a molecule whose recorded
  worker is provably dead is reset to `pending` and re-dispatched into its
  existing worktree/branch/event-chain — on by default. The witness is
  conservative (PID reuse cannot reclaim a healthy worker), and completed work
  is still never re-run. Regression test reproduces the kill+restart recovery.

### Fixed: the headless Claude spawn crashed and could hang (Jesse #6, residual)

- The main `cs tackle --adapter claude` path was already interactive-TUI, but
  the headless spawn in `cosmon-transport` (patrol/thaw respawn paths) still
  built the removed `--prompt` flag (dies on Claude Code v2.x) and passed a
  multi-KB briefing inline through `bash -c` where the escaping left Claude
  waiting on stdin. It now uses `-p` with the briefing on the child's **stdin**,
  and exports `IS_SANDBOX=1` for root + `bypassPermissions`.

### Security: the Claude briefing is no longer exposed in shared /tmp

- The stdin-delivery fix above first wrote the briefing — which routinely
  carries private operator context — to a predictable `/tmp` path that was
  never removed (a TOCTOU / arbitrary-overwrite and confidentiality-leak
  surface, caught by an adversarial review of our own fix). The file is now
  created atomically with an unpredictable name and mode `0600`, and unlinked
  before Claude starts (reaped on spawn failure), so it never persists in
  shared temp storage.

## [0.2.1] — 2026-07-19

### Fixed: the Homebrew formula declared the wrong licence

- The rendered tap formula claimed `MIT` while the `cs` binary ships
  AGPL-3.0-only. The renderer now reads the licence from the workspace
  `Cargo.toml` and the formula tests lock the two together, so a future
  re-licence moves both in one edit.

### Added: the served `install.sh` derives from source, with a drift detector

- `install.sh` is now published as a cosign-signed release asset built from
  `infra/install/install.sh`, and a `served-drift` CI job fetches the live
  public installer and fails loudly when it diverges from source. The served
  copy had drifted silently twice (a gnu-to-musl fix, then a v0.2.0 installer
  that discarded the `cosmon-remote` connector without a word); the detector's
  own red path replays that real incident in CI so it can never quietly stop
  detecting. The operator publish step is documented in
  `infra/install/RUNBOOK.md`.

### Fixed: `cs patrol --propel` no longer spams workers that are thinking

- The idle classifier consulted only cosmon events, so a worker in a long
  reasoning stretch was nudged every ~70 s with identical PROPULSION messages
  — polluting the very context it meant to revive. The classifier now checks
  real pane activity before concluding idle, and re-nudges back off
  exponentially with a cap that escalates to a patrol anomaly instead of
  repeating forever.

### Fixed: a trust grant no longer self-revokes on ordinary repo edits (task-20260719-a850)

- **`cs trust` now holds.** The trust gate's delegated-target scan read the
  *entire text* of `.cosmon/config.toml` and every formula looking for paths —
  including prose. A formula step whose `description` merely mentioned
  `README.md` enlisted the repository's real `README.md` into the hashed shell
  surface. On an active repo that pulled in dozens of ordinary tracked files
  (`README.md`, `Cargo.toml`, `crates/**/*.rs`, `docs/**`), so any normal edit
  to any of them revoked every grant. In the field this read as `cs trust`
  reporting success and the very next `cs done` refusing with
  `repository trust is stale` — reproducibly, with nothing edited in between,
  driving operators to `COSMON_ASSUME_TRUSTED=1`.
- The scan now parses each surface file as TOML and follows only values that
  can reach `sh -c`. Prose keys and TOML comments are excluded; neither can
  inject shell, so no coverage is lost. `config.toml` and the formulas are
  still hashed byte-for-byte, so editing a comment still revokes the grant —
  only the *transitive* expansion narrows (26 → 4 targets on this repository).
- The exclusion is a **denylist**: an unrecognized key counts as shell-bearing,
  so a future executor field carrying a command is covered the day it lands
  rather than silently reopening the RCE-by-clone hole. A surface file that
  does not parse as TOML falls back to the previous full-text scan.

### Added: integration test for the real `cs realized-watch` re-exec path

- The detached realized-model watcher armed by `cs tackle` was covered only by
  a simulated spawn; the known reserve from the round-4 adversarial audit. A
  new integration test exercises the actual binary re-exec: watcher starts,
  ticks, emits `ModelObserved` from a synthetic session log, dedups atomically,
  and respects its lifetime bound.

### Fixed: the three CI reds from the 0.2.0 cut

- A rustdoc intra-doc link broke the Documentation job; the help goldens still
  carried the pre-bump version string; and the README-quickstart e2e lost a
  teardown race (`rm -rf` vs a still-alive tmux worker — kill, wait for death,
  then remove, with a bounded retry).

### Fixed: the confidentiality lint's structural check never ran

- `confidentiality-lint.sh` invoked a `scripts/publish.sh` that never shipped
  in this tree, so the gate failed as a tooling error the first time an
  external docs build ran it. It now delegates to the release checklist's
  command-backed GATE items, and the matches it surfaced once it actually ran
  (an operator name in test fixtures, a non-public galaxy name in a formula
  example, an internal French pattern note) were genericized or removed.

### Docs: a front door, one install story, and the cross-examine claim made liable

- The mdBook gains a **Getting Started** ramp (Install cosmon, Ten minutes to
  cosmon) at the head of the sidebar; the release notes, README, book, and
  landing now tell one install story (native script, Homebrew tap, cargo — the
  same signed bytes); and the introduction's *"cross-examine each other's
  findings"* — its most differentiating claim — now links to a real
  adversarial-review section grounded in the deep-think panels and pre-mortem
  rounds. The introduction itself went through a five-profile reading
  pre-mortem plus an independent cross-model proofread; the surviving text
  restores the qualifiers the repository's own README and SECURITY.md already
  carried.

## [0.2.0] — 2026-07-19

**Highlights.** This release hardens the trust perimeter and makes execution
attribution honest, across 62 detailed entries below.

- **Security — trust & egress.** The sovereignty gate is now deny-by-default,
  repo-supplied shell and delegated script targets are hash-pinned behind the
  trust gate (closing an RCE-by-clone class), and exposed multi-tenant egress
  fails closed on non-Linux hosts.
- **Attribution & honesty.** `cs peek` reports the model that actually ran
  (via the new `ModelObserved` event) alongside the one that was pinned;
  merges carry native `Co-Authored-By` trailers with real-adapter folding;
  model selection, adapter, and worker energy are surfaced end to end.
- **Fleet robustness & patrol.** Briefless molecules are parked instead of
  busy-looped, `cs patrol` gains `--heal` and `--dialogue-scan`, `cs done`
  gains a merge-perimeter scope-guard and a blocking `pre_done` gate, and the
  `archived ⇒ terminal` invariant is detected and healed.
- **Release engineering & public projection.** Public releases are produced
  from isolated, scrubbed projections behind a deny-by-default membrane and a
  publish-identity gate; `install.sh` ships a non-destructive pilot-pack and
  the contribution path is open.
- **Remote, OIDC & RPP.** `cosmon-remote` gains real OAuth2-PKCE login with
  silent refresh, the `run`/`do`/`converse` avatar surface, and a unified
  tenant CLI.
- **Adapters & reference.** The `codex` adapter dispatches with energy
  accounting, OpenAI calls are rate-limit paced, and the mdBook now carries a
  generated, CI-enforced command Reference.

### Added: realized-model attribution — intention vs realization (delib-20260718-c70e)

- `cs peek` now distinguishes the model you **asked for** (the pin, resolved
  through the cli → formula → env → config → global → default ladder) from the
  model that **actually ran**. The realized value folds from a dedicated
  `ModelObserved` event and never reads the pin — silence is expressed by not
  emitting the event, so "never fabricate a record of execution" holds by
  construction.
- The realized slot is a faithful **tri-state**: `?` worker died before any
  observation, `-` ran and never reported its model, `X→Y` the observed
  trajectory (a real quota fallback renders as the trajectory it was, not a
  single model that never happened). Agreement with the pin renders **no**
  glyph — drift is the signal, agreement is silence (`claude/opus~>sonnet`).
- Capture rides the runtime seams: a detached watcher armed at `cs tackle`
  emits on the **first** model-bearing assistant turn and re-emits only on
  change; `cs wait` probes per poll; `cs done`/`cs complete` capture
  post-mortem with atomic dedup. `cs peek` is a strict reader — it never
  emits. Hardened over three adversarial pre-mortem rounds before GO
  (per-attempt/worker scoping, typed per-adapter parsers, mandatory worker
  scope, explicit capability declaration).

### Added: automatic `Co-Authored-By` trailers with real-adapter fold (delib-20260717-194b)

- When `[attribution]` is configured, `cs done` stamps the `--no-ff` merge
  commit with a `Co-Authored-By: <name> (<adapter>)` trailer, where the
  adapter is **folded from the molecule's event journal** — the trailer names
  the adapter that actually worked, not the one that was requested. Worker
  commits are never rewritten; the merge commit is the sole trailer carrier.
- Fixed: the append-only `events.jsonl` conflict-resolution merge path
  finalized with `git commit --no-edit`, silently dropping the trailer under
  concurrent fleet activity. The trailer now survives that path too.

### Added: codex worker energy accounting in `cs ensemble` / `cs peek`

- New codex session-log token parser and price table in `cosmon-core`, and an
  adapter-aware energy probe: codex workers now report tokens and cost next to
  their claude siblings instead of rendering as dashes.

### Added: `cs --version` carries build identity

- Dev and release builds print `cs <version> (<short-sha>[+dirty], built <date>)`,
  so "which binary is actually installed?" is answerable from the binary
  itself — the deploy-gap class of confusion (HEAD moved, binary didn't) is
  now diagnosable in one command.

### Fixed: fleet robustness — boot-stall nudge, codex self-update, whisper gate

- `cs patrol` now nudges boot-stalled molecules whose briefing was pasted but
  never submitted (observed 13× in the field) instead of letting them sit
  inert forever.
- Codex workers no longer die mid-task to the CLI's startup self-update
  ("Please restart Codex" killed the pane); the self-update is suppressed on
  every codex worker spawn.
- `cs whisper` accepts env-prefixed pane commands in its signature gate —
  codex workers spawn under a git-identity env prefix and were wrongly
  refused.
- Removed an env-var data race in the runtime backlog-guard tests.

### Release plumbing

- The client tarball ships the `cosmon-remote` connector.
- The brew formula gains a Linux ARM stanza and a real render pipeline
  (checksums computed from actual assets, not placeholders).

### Fixed: native attribution closes alternate merge and Codex startup gaps

- `cs done --strategy ff-only` now refuses configured native attribution
  instead of successfully fast-forwarding without a trailer carrier. The
  operator-identity backstop validates both names and emails, and a missing
  adapter witness emits an explicit warning rather than implying full
  provenance.
- Interactive Codex workers pre-trust their exact canonical worktree path with
  a locked, atomic, formatting-preserving config edit, preventing fresh
  repositories from stalling at the first-run trust screen.
- Shipped Noogram maker/byline slots consistently use `noogram.org`; historical
  and defensive-DNS references to `noogram.dev` remain distinguishable.

### Security: public releases are isolated, scrubbed projections

- Release checks now fail closed on tracked runtime state, credential-shaped
  filenames, operator paths, private infrastructure names, internal IDs, and
  unreviewed binary assets. The gate also renders and scans the mdBook output
  and search assets, and carries a canary for every audited leak class.
- The isolated release clone rewrites author, committer, message, and retained
  blob text to the public Noogram attribution. Development history is never
  rewritten in place.
- Runtime artifacts and private screenshots are removed from the index and
  purged from publishable history. `CLAUDE.md` now resolves to the public
  `AGENTS.md` contributor surface.
### Security: trust gate now hashes delegated script targets, fail-closed and mixed-language (inc-2 fix-2, task-20260715-6200)

- **The repo-supplied-shell trust gate (`cs trust`) now covers *delegated
  script targets*, not just the pointer.** The B5 gate hashed only
  `.cosmon/config.toml` + `.cosmon/formulas/*.toml`. A shell surface that
  *delegates* — `post_merge = "bash scripts/deploy.sh"`, a gate
  `build_command = "python ci/build.py"`, a formula `command = "./gate.sh"` —
  left the actual code that runs *outside* the hash. An attacker could ship a
  benign pointer, get `cs trust` granted, then rewrite the pointed-at script
  (via `git pull`) with the grant still reading `Trusted` — a full RCE-by-clone
  bypass. The surface hash now folds in every path token in the surface that
  resolves to a regular file **inside the repo root** (its repo-relative path
  *and* bytes), so editing a delegated script revokes the grant.
- **Mixed-language coverage.** Delegated-target extraction is language-agnostic:
  a `.sh`, `.py`, `.js`, `.rb`, `Makefile`, or any other referenced file is
  hashed the same way, closing the mixed-language `build_command` gap. Bare
  build-tool invocations that read an implicit default (`make` → `Makefile`,
  `just` → `justfile`) also pin that default.
- **Unconditional, fail-closed hashing.** Every surface file is folded
  unconditionally; a file that exists but cannot be read now contributes a
  distinct `READ-ERROR` sentinel instead of the old `unwrap_or_default()`
  silently-empty bytes, so a readability-toggle cannot make a hostile target
  hash like a benign empty one. Delegated-target resolution is *jailed* to the
  repository root — a token canonicalizing outside the repo (absolute `/tmp/…`,
  an escaping symlink) is never hashed (a different, local-attacker threat).
  Scope stays one hop deep by design (a script that `source`s a third file is a
  documented residual).

### Fixed: the durable merge-result event is now singular and post-gate (PR-B, task-20260714-aa2e)

- **`cs done` writes exactly one `MergeCompleted` per successful merge, and it
  is written *after* the post-merge compile gate — not before.** The old flow
  emitted a `MergeResult::Ok` the instant the branch landed, *before* the gate
  ran. That pre-gate `Ok` lied twice: a merge the gate then rolled back left a
  permanent `Ok` in `events.jsonl` alongside the later `Error`, and a merge the
  gate could only mark **Unverified** was still recorded as a clean `Ok`. The
  event is now keyed on the gate's `GateOutcome`:
  - `Verified` / `NothingToVerify` → `ok` (or `ok:escalated(n)` after `n`
    escalation retries);
  - `Unverified` → the durable witness `ok:unverified` (or
    `ok:escalated(n):unverified`) — never a bare `Ok`.
  The gate-error path still emits its own terminal `error:<detail>` and returns
  before the success event, so that path is likewise a single event. Wire
  strings stay legacy-parseable (`MergeResult::from` maps anything unknown to
  `Other`), so old logs and downstream readers are unaffected.
- **Round-2 hardening (task-20260715-e0a6):** the durable `ok:unverified`
  witness now has an **end-to-end falsifier test** — an `Unverified` gate lands
  the merge (branch torn down, worker content on main) yet persists an
  `ok:unverified` `merge_completed` line, and reverting the fold to a bare `ok`
  reddens it. And the post-gate witness append is no longer swallowed by a bare
  `let _ = emit_one(...)`: an `events.jsonl` write failure now surfaces a
  **loud `CRITICAL` advisory** on stderr and in the warning stream (the merge
  already landed, so teardown still proceeds — but a lost honesty witness is
  never inferred from a silently missing line).

### Security: repo-supplied shell is trust-gated — RCE-by-clone (B5, task-20260714-9602)

- **Cosmon now refuses to run a repository's own shell strings until you
  vouch for the repository once.** A formula's `command` / `verification`
  steps and the `post_merge` / `pre_done` hooks in `.cosmon/config.toml`
  execute via `sh -c` on strings the *repo* supplies. A cloned hostile repo
  could therefore run arbitrary code the moment you `cs tackle` / `cs done` it.
  The fix follows the `direnv allow` model: a one-bit, per-repository trust
  grant recorded **outside** the repo (`~/.cosmon/trust/`), so a clone cannot
  ship its own grant. Detecting a malicious formula is undecidable (Rice), so
  the gate refuses untrusted shell rather than trying to classify it.
- **`cs trust`** — grant (default), `--status`, or `--revoke` trust for the
  current repository. Editing the shell surface (`.cosmon/config.toml` or a
  formula) marks the grant `stale` and requires a re-`cs trust`, exactly as
  editing `.envrc` revokes a `direnv allow`.
- **Gated sinks:** `cs evolve` verification + auto-gate, `cs tackle` gate
  command, `cs done` `pre_done` (hard-refuse) and `post_merge` (advisory
  skip-with-warning, since it runs after the merge lands), and `cs verify`
  shell-gate replay.
- **CI / automation:** `COSMON_ASSUME_TRUSTED=1` bypasses the gate for a repo
  vetted out-of-band; `COSMON_TRUST_DIR` relocates the trust store.
- **Deployment note:** after this lands, your own cosmon checkout needs one
  `cs trust` before the next worker's gates or `post_merge` hook will run.
  Documented in SECURITY.md's threat model and in-scope list.

### Security: exposed multi-tenant egress is fail-closed on non-Linux hosts (task-20260713-8acc)

- **A `deny-external` (strict-local) worker on an exposed multi-tenant host
  that cannot kernel-enforce egress is now refused, not degraded to advisory.**
  On macOS (and any non-Linux host) the egress jail is `Advisory` — the policy
  is recorded but the subprocess runs unjailed and *can* reach the network.
  That is a benign convenience on a single-operator dev host, but a security
  hole on the hosted RPP endpoint: a tenant's unjailed worker could reach a
  remote oracle. `EgressJail::preflight` gains an `exposed_multi_tenant` axis;
  `cs tackle` reads it from the new `COSMON_EGRESS_EXPOSED` var **or** the RPP
  `COSMON_API_REQUEST` marker and refuses the dispatch fail-closed, regardless
  of `COSMON_EGRESS_REQUIRE_NETNS`.
- **Known limitation documented as an invariant.** architectural-invariants.md
  §8u records that egress is kernel-real only on Linux; hosting an exposed
  multi-tenant cosmon endpoint on macOS with strict-local tenants is blocked
  until native enforcement lands.
- **Native macOS enforcement designed.** [ADR-155](docs/adr/155-macos-egress-enforcement-seatbelt.md)
  designs `EnforcementMode::Seatbelt` (a `sandbox-exec` network-deny profile,
  ship-first) with a Network Extension content filter as the robust follow-on.

### Added: `cs done` scope-guard — merge-perimeter gate (P3 of task-20260712-3819)

- **A molecule can now declare its allowed change-perimeter** with
  `cs nucleate --var scope_allow="docs/book/src/**,README.md"` (comma- or
  newline-separated globset patterns). At `cs done`, the files the merge would
  introduce (`git diff --name-only <base>...<branch>`) are partitioned against
  that perimeter and any **out-of-scope** file is surfaced. Closes the P3
  pathology where a docs-only brief silently rewrote 40 crate-source files,
  which would have broken the golden man-page test and changed `cs --help`.
- **Advisory by default, strict opt-in.** An out-of-scope merge prints a
  structured warning and proceeds (invariants §8b — *propose mechanisms of
  verification, do not impose them*; an out-of-scope change is a quality signal,
  not a confidentiality breach). Set `[scope_guard] strict = true` in
  `.cosmon/config.toml` to escalate to a hard `cs done` abort. A molecule that
  declares no `scope_allow` perimeter is unaffected — the guard is inert with no
  perimeter, so this is a zero-cost default for every predating project.
- New pure core primitive `cosmon_core::scope_guard` (I/O-free; injected glob
  matcher seam) and `ScopeGuardConfig` on `ProjectConfig`. Sibling gate to
  `[git_remote_blocklist]` / `[confidential_blocklist]` / `[publish_identity]`.

### Removed: `cs mcp` legacy stdio server — retired; `cosmon-mcp` reclassified as remote-MCP transport (decision C14, task-20260712-74a1)

- **`cs mcp` is gone.** The embedded stdio MCP server (`cs mcp`), the
  standalone `cosmon-mcp` binary, and the `cosmon_mcp::serve_stdio()` library
  entry point are removed. Local worker/pilot operation is the `cs` CLI's job
  (CLI-first invariant) — this surface had no real consumer and was 3 months
  past its 2026-04-11 deprecation window.
- **`cosmon-mcp` is NOT deleted.** An audit (C14) found the deprecation premise
  had inverted: since 2026-04-11 the crate became the transport substrate for
  `cosmon-rpp-adapter`'s remote-tenant Streamable-HTTP MCP endpoint
  (`streamable_http_service()`). It is reclassified from "deprecated, awaiting
  deletion" to "active transport-only library, one consumer." The
  `cosmon-cli → cosmon-mcp` path dependency (used only by `cs mcp`) is dropped;
  the crate is now pulled in transitively by `cosmon-rpp-adapter`.
### Added: `cs peek` TUI — per-molecule ADAPTER column with honest, persisted dispatch attribution (task-20260712-6609)

- **New `ADAPTER` column in the `cs peek` fleet table.** Every molecule row
  now shows the adapter that *actually* dispatched it, folded from the durable
  `events.jsonl` record (`AdapterSelected` / `ModelSelected`), not the current
  config. Compact shape `adapter/model [source]` — e.g.
  `claude/claude-opus-4-8 [cli]` — where `source` is the honest origin of the
  choice (`cli`, `formula`, `env`, `config`, `global`, `default`). The
  attribution also appears as an `adapter` field in the expanded-row detail.
- **Honesty rule — reasoning/thinking effort is never inferred.** The column
  surfaces a reasoning effort *only* when a past event honestly recorded it.
  Cosmon persists no effort on any spawn-time event today, so the marker is
  silent — it is **never** back-filled from the live `.cosmon/config.toml` or a
  current `ModelSpec`, which would attribute today's setting to yesterday's run.
- **New shared, zero-I/O projection `cosmon_core::adapter_attribution`** —
  `AdapterAttribution::fold` (events → attribution) plus `compact_cell` /
  `detail_line` renderers are the single source of truth both `cs peek` and any
  future HTTP surface render through, so the two cannot drift. The canonical
  120-column `cs peek --snapshot` byte raster and its anti-drift tests are
  untouched.

### Added: P3 per-provider judgment-quality calibration probe — seed-corpus + P1–P4 grid + `calibration-probe` formula (delib-20260711-f62a C5/D-3, task-20260711-83bd)

- **New labelled seed-corpus `evidence/calibration-corpus/`** — the first
  *versioned ground-truth DATA artifact* in cosmon (formulas and code had a
  home; a labelled dataset did not — feynman, D-3). Each entry is a known-root
  debugging bug: `{bug_input, known_root, known_minimal_fix,
  known_tautological_trap, clean_verdict, pathology_traps[P1–P4]}`, contracted
  by `schema.json`. Row 1 is `pack-4` (the `pack(4)` case); a second entry
  `singular-cov` seeds a distinct domain.
- **New pure-core executable spec `cosmon_core::calibration`** — the P1–P4
  `JudgmentPathology` grid (anchoring / overconfidence / confirmation /
  sycophancy, each cited to an L0-audited arXiv source), the `Corpus` /
  `CorpusEntry` Rust mirror with `validate()`, per-adapter scoring, and a
  baseline `regressions()` diff. The turing point is enforced at the **type
  level**: `LivenessBit` and `JudgmentScore` are inconvertible newtypes, so an
  oracle-canary liveness bit can never be used as a judgment score. Snapshots
  carry a mandatory Rice-flavored disclaimer (lower bound per model-version, not
  a certificate).
- **New `calibration-probe` formula** — replays one corpus entry under every
  wired adapter at a byte-identical system-prompt, classifies each verdict
  against the grid, and diffs against a stable baseline
  (`.cosmon/state/calibration/last-snapshot.json`). Reuses the `oracle-canary`
  loop and the `cross-provider-committee` Path-A adapter pin; **measures
  judgment quality, never liveness**; a regression is a finding, never a merge
  veto (§8b). This probe is the only empirical police on the S-3
  stake-self-classification residual the add-only committee schema cannot close.

### Added: `[provider_bias]` add-only committee baseline + `cs reconcile --check` diversity lint (ADR-147 tier a, task-20260711-e542)

- **New `[provider_bias]` config section** — the exogenous, add-only baseline
  for cross-provider reading committees (ADR-147 / C3). It declares
  `additional_readers`, `additional_falsifiers`, and a floor
  `min_distinct_provider_endpoints`, plus named `[provider_bias.profiles.*]`.
  The **effective** requirement-set is the monotone union
  `baseline ∪ ⋃ profiles`: reader/falsifier ids are set-unioned, the floor is
  joined by `max`. There is **no** subtract/override field, so a *downgrade is
  inexpressible in the type* — the same "cap-négatif-absent" trick that makes
  `[model_budget]` unable to configure extra credit burn. Absent (the default)
  is byte-identical to a galaxy that predates the knob. This is the schema that
  makes buterin's S-1 hold: a diversity constraint is collusion-resistant only
  when it lives where the audited worker cannot lower it.
- **New `cs reconcile --check` lint — `check_no_profile_requirement_downgrade`.**
  Sibling of the Ghost-A `check_no_strong_config_default` lint, same
  `Vec<String>` shape and same fail-closed-under-`--check` contract (`exit 1`).
  It resolves each committee seat to its `(provider, base_url, model-family)`
  endpoint tuple and reddens when two seats collapse onto the same tuple (an
  echo, not an independent reader) or when the distinct-endpoint count falls
  below the floor. Distinctness is measured on the **resolved endpoint, never
  the adapter name** (ADR-147): an `[adapters.openai]` seat whose `base_url`
  fronts Claude is unmasked, not blessed by its label.
- **Correction to the cosmon-mechanisms survey (feynman).** The survey's claim
  that add-only *"maps exactly onto model_budget"* is **false**:
  `config_default_is_strong` is a fail-**open** value predicate over one field;
  the add-only guarantee is a subset/monotonicity relation between two
  *requirement-sets* — an object that did not exist in cosmon until this change
  introduced [`ProviderRequirementSet`].
- **§8b ceiling, explicit.** The lint is a CI dry-run, bypassable by
  `--no-verify`; the `model-family` label is *derived from config, not
  attested*. It makes a mono-family committee **loud and attributable, not
  impossible** — the attested tier (b) `SameFamilyRefusal` is the ADR-grade
  follow-on. Any endpoint-diversity floor values are low-confidence hypotheses
  measured A/B on our own workload, never from a leaderboard.

### Fixed: the resident runtime parks a briefless molecule instead of busy-looping its dispatch (task-20260711-4310)

- **`cs run` no longer re-attempts a briefless molecule every tick.** The
  sibling guard (task-20260711-919a) made `cs tackle` *refuse* a briefless
  molecule with a distinct exit code, but the resident runtime treated every
  non-zero `cs tackle` exit as **transient** — retracting its optimistic
  dispatch mark and re-emitting the dispatch next tick. A briefless molecule
  can never satisfy the guard, so this was an infinite busy-loop: `cs tackle`
  spawned each poll interval, the trace flooded, and — because every tick then
  "produced decisions" — the phantom-running stall gate perpetually reset,
  starving the reap sweep. The runtime now classifies the briefless exit code
  as a **permanent** refusal and *parks* the molecule (attempts it exactly
  once, records the refusal on the decision trace, then leaves it alone). The
  well-formed rest of the DAG drains normally.
- **The briefless-dispatch exit code is now a shared cross-crate contract**
  (`cosmon_core::dispatch_refusal::BRIEFLESS_DISPATCH`), aliased by the CLI
  guard that emits it and read by the runtime that parks on it — single source
  of truth, pinned by a test so the emitter and reader cannot drift.
- **`cs run` reports parked briefless molecules.** New `briefless_parked`
  count in the `--json` output and the human summary (shown only when
  non-zero). A non-zero value means the operator has molecules that need a
  brief restored (from `prompt.md` frontmatter) or a collapse.

### Fixed: a briefless molecule can no longer be nucleated or dispatched (task-20260711-919a)

- **`cs nucleate` rejects a required variable supplied blank.** A `--var
  topic=""` (or whitespace-only) on a formula that declares `topic` as a
  required, default-free variable now fails fast instead of birthing a
  molecule with no operator intent. New typed error `empty-variable` (exit
  path mirrors `missing-variable`; HTTP 400 on the RPP nucleate route).
- **`cs tackle` refuses to dispatch a briefless molecule** — one whose
  formula declares required, default-free variables that are now missing or
  blank. This is the load-bearing half for the observed pathology:
  empty-topic `task-work` molecules the runtime dispatched **after** a
  `cs reconcile` cleared `state.json` variables, spawning workers with an
  empty Mission. New refusal `GuardError::BrieflessDispatch` (exit code 16).
  Corollary of the frontier stuck-frozen fix (task-20260711-9b86): a DAG
  frontier reporting "ready" is necessary, not sufficient, for dispatch.
- **`task-work` now declares `topic` as a required, default-free variable**,
  so the guard fires for the formula where the pathology was observed.
  Formulas with no required-and-default-free variable (e.g. `temp-review`)
  are unaffected. Recover a lost brief from the molecule's `prompt.md`
  frontmatter and restore the variable, or collapse the molecule.
### Added: generated command Reference in the mdBook, CI-enforced against the clap tree (task-20260711-47e5, doc-modernization B1′ P2)

- **New `Reference` section in the docs book** (`docs/book/src/reference/*.md`):
  a CLI overview plus one page per command group — Molecule lifecycle, Fleet
  management, Execution, Project, **Observability**, **Integrity & audit**, and
  Tools — each generated from the live clap tree, plus hand-written
  `exit-codes.md` and `formulas.md`. Three renderers now share one source of
  truth: `cs --help`, `man/cs.1`, and the book Reference.
- **~19 internal/experimental verbs hidden** from `cs --help` and the book
  (`events, ask, mur, motion, resurrect, security, sensorium, tokens, note,
  stitch, heartbeat, replay, test, presence, inspect, artifacts, cluster,
  apps, vllm-mlx`). They still parse and run — visibility is a documentation
  decision, not removal. `cs help` now groups commands into 7 role-based
  sections (the old catch-all "Tools" split into Observability and
  Integrity & audit).
- **Anti-drift CI**: the generated pages are golden-checked against the clap
  signature surface (`REFERENCE_UPDATE=1` to refresh); a command-name grep and
  an internal-link check cover the hand-written and prose surfaces.
- **Confidentiality**: operator-identity tokens that leaked into `--help` /
  `man/cs.1` example text were scrubbed at source.

### Fixed: silent refresh no longer resurrects a spent refresh token on a rotating provider (task-20260710-128e, review a6ae F6)

- **`cosmon-remote` silent refresh against Forgejo (`InvalidateRefreshTokens=true`)
  no longer forces a spurious re-login.** When a rotating provider's refresh
  grant omits the new `refresh_token`, the presented token has already been
  invalidated by that very grant; the old fallback reused it, so the *next*
  refresh failed `invalid_grant`. `RefreshConfig` now carries a
  `RefreshRotation` policy (`Rotating`, the safe default, / `Static`): an omitted
  refresh token is reused only on a `Static` provider and surfaces a clean
  `RefreshExpired` (→ re-login) on a `Rotating` one. Internal library change to
  `cosmon-remote`; no `cs` CLI surface change.

### Decided: operational-class RPP routes stay unthrottled at the app layer — edge-delegated (task-20260710-4364, review df19 F3)

- **The unauthenticated operational class** (`/healthz`, `/`, `/install.sh`,
  `/dist/*`, `/metrics`, `/diagnostics`, `/.well-known/cosmon-oauth-clients`,
  `/mcp` discovery) **carries no application-layer rate limit, by recorded
  decision.** §8j clause (c)'s per-`sub` leaky bucket is scoped to the
  JWT-authenticated admission boundary; DoS control for the read-only
  operational class is delegated to the network edge (reverse proxy /
  tailnet ACL), the only layer that sees the real peer behind the
  `127.0.0.1` TLS terminator. An app-layer per-IP bucket self-DoSes via IP
  rotation and a global one starves the allocation-free `/healthz` probe.
  Documentation-only change (invariants §8j rider + inline route/router
  docs); no runtime behaviour changed. See
  `docs/architectural-invariants.md` §8j.
### Added: `cosmon-remote login` — real OAuth2-PKCE against Forgejo + silent refresh (delib-20260710-33b7 C2/C7, task-20260710-2565)

- **New `cosmon-remote login` / `cosmon-remote logout` commands.** `login` runs
  a real OAuth 2.0 authorization-code + PKCE (S256) browser flow against the
  deployment's Forgejo identity provider, captures the code on a loopback
  redirect (`http://127.0.0.1:7777/callback`), exchanges it, and persists the
  `{access, refresh}` pair via the credential-store (OS keyring, or a 0600 file
  on a headless box). This is **distinct** from `auth login`, which remains the
  Claude/Anthropic device flow — the two use separate modules and error types.
- **Silent refresh on every command.** For a profile that has completed a real
  login, `client_for` now reads the persisted credential and refreshes the
  15-minute access token silently (zero network when valid), so the operator
  re-authenticates only when the ~monthly refresh token lapses. The refresh is
  single-writer per credential key (advisory lock + compare-and-swap +
  adopt-winner + persist-before-use), so two parallel invocations never
  invalidate each other's rotated token.
- **New `oidc` module** in `cosmon-remote`: `discovery` (OIDC metadata + a
  cosmon-namespaced `client_id` reverse-discovery document), `pkce_s256`,
  `loopback`, `exchange`, and the `flow` orchestration
  (`login`/`ensure_token`/`refresh_credential`/`force_refresh`/`logout`).
  `OidcError` is an own `#[non_exhaustive]` enum folded into `Error`.
- **`Profile` gains additive optional `issuer` + `client_id` fields** recorded
  by `login`, so subsequent commands rebuild the credential key offline. Mock
  deployments (no real login) keep the legacy `oidc_url/issue` mint behaviour
  unchanged.

### Deprecated: mode-C string-match tool-parse recovery demoted to a non-streaming fallback (delib-20260707-df9b M4, task-20260708-f068)

- **The string-match tool-call-parse recovery is now a deprecated fallback,
  not the primary path.** With M2's own-side streaming extraction landed
  (`stream:true` is always requested), ollama performs no server-side
  tool-call parse and can no longer emit the mode-C HTTP 500. The recovery
  arm — `is_tool_parse_error_signal`, `tool_parse_correction_message`, the
  spliced `user` turn in `OpenAIProvider::one_turn`, and the
  `OpenAiError::ToolCallParse` variant — survives only for other `/v1` shims
  that ignore `stream:true` and parse server-side.
- **Deprecated-in-comment with a scheduled removal**, per tolnay's staged
  retreat: all four sites are marked for deletion **one release after M2
  ships**, once the shim inventory is confirmed. Removing the
  `#[non_exhaustive]` `ToolCallParse` variant is a semver-MAJOR event, so it
  is deleted deliberately on that schedule rather than smuggled into a patch.
- **No behaviour change this release** — the fallback still fires for a
  server-side-parsing shim that 500s. Divergences (c) *user-turn-not-
  tool_result* and (d) *whole-body-not-streaming* were one gap: the `user`
  turn was the shadow of server-side parsing.

### Changed: sovereignty-gate resolver — round-13 closes the two heuristic accept-doors (delib-20260707-3b7e, task-20260708-b669)

- **The round-12 DENY-BY-DEFAULT inversion held, but had left two pre-existing
  heuristic `ok` short-circuits upstream of the `:457` deny terminal.** Round-13
  closes both, so **every resolver `ok` is now backed by a positive, exactly-
  enumerated referent; the only unmatched fall-through is `:457 = deny`.**
  - **Door A (non-path-char → prose exemption) is deleted.** A token carrying a
    char outside `[A-Za-z0-9._/-]` was waved through as "prose" — but
    `~/tenant-demo-secrets/cap-table` is an ordinary home path (only non-path char `~`)
    with a private tail, and any exotic-char dressing (`~ % # &`) escaped the same
    way. Once a token has a `/` it is a path and must resolve positively or DENY.
    The three genuine sed/regex/query fragments the bundle's own scripts embed
    (`s/^`, `\1/p`, `api/v1/users/search?limit`) are enumerated EXACTLY in
    `resolves_path_allow` (spec §R10.allow).
  - **Door B (dotted-hostname → public-URL exemption) is narrowed to a host
    whitelist.** Any label-with-a-dot used to resolve as a public URL, so
    `vault.tenant-demo-internal/master-key`, `internal.corp/tenant-secrets`,
    `whispers.backup/inbox`, `noyau-vault.io/dump` passed as if reachable. A
    host-shaped token now resolves **only if** its host segment ∈ the documented
    public-host set `RESOLV_PUBLIC_HOST` (`codeberg.org`,
    `registry.vendor.tenant-demo.io`; spec §R10.host). A bare dot is never enough.
- **Verified:** 0 over-denial on all 438 path-like/citation tokens the 19-file
  bundle ships; 37/37 falsification canaries hold (Door A+B exact turing/karpathy
  forms + a generational probe: private forms invented without an `http(s)://`
  scheme and without a referent all DENY); self-test non-vacuous (a mutated canary
  flips the gate to exit 2); gate clean (exit 0) in 0.60 s. 16 round-13 canaries
  wired into the pre-scan self-test so closure is proven every run.

### Changed: sovereignty-gate resolver — DENY-BY-DEFAULT polarity inversion (delib-20260707-8eca, task-20260707-ecd6)

- **`scripts/sovereignty-gate.sh` resolver is inverted from allow-by-default to
  deny-by-default.** The two trusted-lead whitelists (`RESOLV_ABS_ROOTS`,
  `RESOLV_REL_ROOTS`) that short-circuited a path to `ok` on its *lead segment*
  before ever consulting the tail are **deleted**. A path with a directory
  component now resolves `ok` **only if** it points positively at an authorised
  referent — a declared mount, a bundle file, a public URL, or an **exact** entry
  in the enumerated positive whole-path ALLOW (`resolves_path_allow`,
  externalised to `sovereignty-spec.md` §R10). Everything else DENYs.
- **This closes the CLASS, not one more shape.** Rounds 3–11 each added denials
  to an open lead-whitelist and each was beaten by the next unknown private name
  under a trusted lead. With deny-by-default, an invented private form
  (`/opt/client-financials`, `state/tenant-secrets`, `noogram/client-roster`,
  `/opt/keys/master.age`, `usr/local/bin/exfil-tenant-db`) resolves positively
  nowhere → denied by construction, without being enumerated. The private-motif /
  secret-extension / secret-basename lists are demoted to commented
  defense-in-depth (redundant with the positive predicate, never the closure).
- **Verified:** 0 over-denial on all 435 path-like tokens the 19-file bundle
  ships; all historical + generative canaries DENY; the generational probe finds
  nothing; gate stays clean (exit 0) in <0.9 s. 25 round-12 canaries wired into
  the pre-scan self-test so closure is proven every run.

### Added: `cs run --affinity` — model-affinity ordering of the frontier drain (ADR-145, task-20260707-9833)

- **`cs run --affinity`** reorders each dispatch batch so molecules bound to
  the same model run contiguously, and the model already resident in the
  oracle's VRAM drains first. On a single-GPU local oracle (`ollama-g5`: one
  ~120 B model resident, a second forces a ~40 GB disk swap) this turns an
  alternating frontier's reload-every-turn into one load per model.
- **`cs run --resident-model <id>`** seeds the model already warm at runtime
  start, so its bucket drains with no reload.
- Off by default: without `--affinity`, dispatch order is byte-identical to
  before (pure critical-path). The reorder is a permutation — the set of
  molecules dispatched and the DAG semantics are unchanged; only the order
  within a ready batch differs. The per-molecule model is pre-resolved from
  each molecule's formula-step `model =` pin (the ADR-142 Incarnation model).
- Wires the previously-uncalled `cosmon_graph::affinity_order` +
  `model_switch_count` primitives (merged `task-20260705-c843`) into the
  runtime, restoring the *merged primitive = wired primitive* invariant.
  `keep_alive` stays off the dispatch path (floor runs the OpenAI-compat `/v1`
  endpoint, not the native Ollama adapter); interim mitigation is daemon-side
  `OLLAMA_KEEP_ALIVE=-1` + a one-model-per-fleet pin. See
  [ADR-145](docs/adr/145-model-affinity-frontier-drain.md).

### Changed: avatar-tenant-demo round-9 — whole-path sovereignty resolver + disclosure strips + witnessed deploy (task-20260706-b286)

- **`scripts/sovereignty-gate.sh` resolves the WHOLE path, never the head.**
  The `RESOLV_RUNTIME_DIRS` accept-list is deleted; a path with directories
  resolves only as bundle self-reference or when the entire path (no `..`
  segments) sits under a mount the bundle's own `volumes-*.csv` declares
  (tmpfs excluded). Kills the round-8 falsification where a one-segment
  `/tmp/` prefix smuggled any private tail past the gate (spec §R9,
  delib-20260706-2042 B1). New canaries: runtime-prefixed private tail,
  prefix-dressed S1, `..`-traversal. Gate shrinks 521 → 515 lines while
  getting stronger; DENY-class 3 now also catches bare `tailscale|tailnet`.
- **The disclosure review (task-20260705-059e) is APPLIED**, verdict by
  verdict: the bundle no longer advertises its own leak-scan, names Claude /
  `.claude.json` (supply-chain), internal binary rosters, retired components,
  parc incidents, distribution repo paths, tailscale exposure tech, or
  doctrine labels. `oidc-identity.toml.example` neutralized (Phase taxonomy +
  drain DAG vocabulary → plain-language values). Travel allowlist re-frozen:
  −58 tokens / +17. Forgejo scripts now install under their shipped basenames.
- **The deploy is witnessed, not asserted**: handoff archive → `cp
  .env.example .env` → staged `up --wait` (forgejo Healthy → cosmon-server
  Healthy) → `/api/healthz` → `{"ok":true}` HTTP 200, trust bootstrap
  converged from the handoff, auth fail-closed 401. Found in the process:
  the vendor registry carries no v3.0 tags (its `latest` is a pre-v3.0
  image) — the round-10 push is named in the molecule report.

- **New `scripts/lpthe/` bundle** ships a fully-static
  `x86_64-unknown-linux-musl` `cs` to an invited-guest host with an unknown,
  unmodifiable glibc and no container runtime (delib-20260705-7288 C2, the
  "container-less avatar" of ADR-141's boot contract minus crypto/containers):
  `build-cs-musl.sh` (cross-compile + pin BLAKE3/toolchain into `MANIFEST.txt`),
  `ship-lpthe.sh` (versioned tarball via `scp -J tycho`, with a far-side seal
  check), the idempotent `provision.sh` (local-state symlink, formula wiring,
  ollama health-check, `cs init`, NFS cold-copy mirror, smoke-test), and
  `cosmon-state-backup.sh` (the rsync cold-copy pass).
- **Workspace TLS backend is now rustls (ring), not native-tls/openssl.** The
  root `reqwest` dependency dropped `default-tls` for `rustls-tls`, removing
  `openssl-sys` from the entire tree so the musl cross-build links no C OpenSSL.
  No code used any native-tls-specific API; behaviour is unchanged bar the TLS
  provider. This aligns the workspace default with the per-crate `rustls-tls`
  overrides that `cosmon-cli`/`cosmon-remote`/`cosmon-provider` already carried.

### Fixed: cost-aware model fallback — silence never escalates to strong (task-20260705-ba98)

- **The silent-fallback leak is closed.** The probe-fallback chain
  (`task-20260614-3116`) was cost-inverted: the first fallback from the cheap
  floor `claude-fable-5` was the *strongest, most expensive* model
  `claude-opus-4-8`, so a transient fable outage silently escalated a worker to
  strong, expensive credits **with no positive operator act** — violating the
  unanimous `delib-20260704-b476` invariant #3, *"strong is never inherited;
  silence resolves to the weakest safe model."* (Diagnosed by
  `task-20260705-1ad9`.)
- **`DEFAULT_MODEL_CHAIN` is now cost-ascending** (`claude-fable-5` →
  `claude-sonnet-4-6` → `claude-opus-4-8`), so the first fallback is the
  next-cheapest model, never the strongest.
- **`build_chain` / `decide_worker_model` exclude strong models from a cheap
  pin's fallback tail.** A strong default joins the tail **only** when the pin
  itself is strong (a positive per-molecule act already honoured by the C4
  `strong_gate`). A cheap pin falls through only to cheaper-or-equal models; if
  none answer, `cs tackle` **fails closed** (`NoModelAvailable`, refuse to
  spawn) rather than silently spending on strong. The strong cost class is
  cosmon's intrinsic `DEFAULT_STRONG_MODELS` union the operator's per-adapter
  `[adapters.<name>].strong` set (b476 T1), threaded from `cs tackle` into the
  probe layer via the new `extra_strong` argument.
- **Behaviour-flip test.** `cosmon-core::model_chain`'s reproduction test
  (`silent_fallback_reproduces_…`) is now the guard
  `silent_fallback_guards_against_cheap_pin_escalating_to_strong_opus`: a cheap
  pin whose model is down resolves to the mid model and never probes opus.

### Added: `cs observe` / `cs ensemble` surface the resolved model + its source (delib-20260704-b476 C3)

- **Model observability** — `cs observe <id>` now prints a **Model** block and
  `cs ensemble` appends a `⟦model · source⟧` badge to each worker's molecule
  cell, so an operator sees *which model is running where, and why* at a
  glance. Both fold the typed `ModelSelected` event (C2) off `events.jsonl`
  into a per-molecule attribution — the resolved model id (or `default` at the
  von-neumann floor) plus its selection source (`--model` flag / formula-pin /
  env / config / global / floor). "Latest wins": a re-tackled molecule shows
  its most recent selection.
- **`--json` fields** — `cs observe --json` gains `model`, `model_source`, and
  `model_adapter`; `cs ensemble --json` worker rows gain `model` and
  `model_source`. A floor selection carries `model_source: "default"` with a
  null/absent `model`; a molecule with no recorded selection omits the fields
  entirely — the two are distinguishable.
- **`cosmon_state::ops::model_attribution`** — new read-side projection
  (`model_selections` batch fold + `latest_model_selection` single-molecule)
  over the events log, with a byte-substring pre-filter so the scan skips
  non-`ModelSelected` lines before any parse. Advisory read (trace-not-lock):
  a missing or unreadable log yields no attribution rather than an error.

### Added: typed `ModelSelected` event — model attribution on the wire (delib-20260704-b476 C2)

- **`EventV2::ModelSelected`** — `cs tackle` now co-mints a typed event with
  every spawn recording *which* model was pinned (`None` at the floor) and
  *where* the choice came from (`ModelSelectionSource`: flag / formula-pin /
  env / config / global-config / the `None` floor). The model sibling of
  `AdapterSelected`, emitted ex-ante (before the availability probe) so the
  attribution is deterministic. This promotes the old `model-selection.json`
  sidecar onto `events.jsonl`: the ceiling guard (C4) can fold strong-dispatch
  counts over the log rather than a mutable counter file, and `cs ensemble` /
  `cs observe` can surface model + source without parsing a sidecar. Emitted by
  the new `emit_model_selected` helper in `cosmon-state`.

### Added: auto-provisioning images — the seed init-container is retired (ADR-141)

- **`cosmon-rpp-adapter` trust bootstrap** — at boot, before arming the JWKS
  fetch, the server now converges `security/trusted-issuers.toml` **itself**
  from three declaration sources: `IdP` handoff files (`[trust_bootstrap]
  handoff_dir` in `rpp.toml`), the `TRUSTED_ISS`/`TRUSTED_JWKS_URI`/
  `TRUSTED_AUDIENCES` env trio, and static `[[trust_bootstrap.issuer]]`
  entries. Merge-preserving (foreign `[[issuer]]` blocks on the volume survive
  verbatim), fail-closed parse-back (a degenerate result refuses the boot),
  `TRUSTED_FORCE=1` full-rewrite reset, bounded first-boot wait for the
  handoff. Handoff-declared nucleon bindings are rendered through the same
  audited `nucleon_map` renderer as the operator path. New operator one-shot:
  `cosmon-rpp-adapter trust converge`.
- **`cosmon-forgejo` self-provisioning IdP image**
  (`dist/avatar-tenant-demo/forgejo/`) — wraps the upstream rootless Forgejo
  entrypoint; at boot it creates the admin via the internal CLI (the reserved
  username `admin` is refused loudly — the 2026-07-02 parc incident class),
  creates the OAuth2 app, and publishes issuer + `client_id` + binding `sub`
  as a handoff file. *Healthy = provisioned.* Proven against a real virgin
  Forgejo by `forgejo/test-provision-local.sh`, including the reserved-`admin`
  negative test.
- **v3.0 recipe** — the `seed` init-container (`cosmon-seed-trusted-issuers`),
  `seed/init-seed.sh`, and `volumes-seed.csv` are deleted (absorbed); the
  compose gains the `forgejo` service and the `provision-handoff` volume
  (rw forgejo / ro cosmon-server — the `client_id` never crosses containers
  via env). `TRUSTED_ISSUERS_FILE` is retired in favour of
  `[[trust_bootstrap.issuer]]` + `TRUSTED_FORCE=1`.
### Added: `cs patrol --dialogue-scan` — blocking-dialogue detection, money-safe

- **New `cs patrol --dialogue-scan` sweep** — captures each running worker's
  pane and classifies any blocking dialogue sitting in it. Motivating incident:
  ten showroom workers blocked ~30h on the Claude Code spend-limit dialog with
  no human to press Enter, propelled by hand. The sweep separates two worlds:
  a **tool-permission prompt** (cheap keystroke, no stake) from a
  **money-stake dialog** (spend limit, usage credit, plan upgrade).
  - `money_stake` and unrecognised blocks **always page the operator** via
    `cs notify` and are **never** auto-confirmed — that refusal is encoded in
    the pure classifier (`cosmon-core::dialogue`), not in a flag.
  - A safe `permission` prompt is auto-confirmed (default-accept Enter) **only**
    when `--auto-confirm-safe` is also passed; the default is surface-to-human.
  - A molecule still blocked past `--dialogue-blocked-after` (default 900s)
    escalates to a **canary-RED** operator page — the heartbeat half of the ask.
- **New flags** on `cs patrol`: `--dialogue-scan`, `--auto-confirm-safe`,
  `--dialogue-lines` (default 40), `--dialogue-blocked-after` (default 900).
  `--json` gains a `dialogue_scan` block (per-finding class / action /
  blocked_seconds / evidence).
- **New event** `EventV2::BlockingDialogueDetected` — append-only audit record
  of every detection and the action taken (`alerted` / `auto_confirmed` /
  `reported` / `canary_red`).
- **Discipline (be1e / ADR-137 §2).** Pane text is an adversarial channel read
  only to *surface* a finding; the sole autonomous keystroke is the opt-in
  default-accept on a `permission`-class prompt. Money stakes are refused in
  pure code. See [`docs/guides/worker-propulsion-patrol.md`](docs/guides/worker-propulsion-patrol.md)
  for the declarative per-galaxy patrol template (`patrols.toml`).

### Added: blocking `[hooks] pre_done` gate — `cs done` can now refuse a DONE

- **New `[hooks] pre_done` config field** — a shell command run *before*
  `cs done` merges a worker branch. Invoked as `sh -c '<pre_done>' -- <mol-id>`
  (the molecule id is `$1`). **A non-zero exit ABORTS the whole teardown**:
  no merge, no `merged_at` stamp, no worktree removal, no branch delete, no
  tmux kill — and `cs done` returns a hard error carrying the script's stderr
  as the reason (`pre_done_refused` in `--json`). This closes the structural
  hole surfaced cosmon-ward from showroom (`delib-20260701-bfdf`, torvalds
  D1): `post_merge` runs *after* the irreversible merge and can only warn, so
  until now nothing in the molecule cycle could refuse a DONE — a falsifiable
  Definition-of-Done could only live in GitHub branch-protection, outside the
  molecule cycle. The gate runs before the trunk lock, so a refused DONE
  touches nothing and the operator (or worker) fixes the gap and reruns.
- **Operator kill-switch** — `cs done --skip-pre-done-hook`, or the
  `COSMON_SKIP_PRE_DONE_HOOK` environment variable (any non-empty value).
  For a deliverable the operator knows is good but the script cannot see.
- **Ships absent by default** — every existing project is unaffected;
  `post_merge` semantics are unchanged (still advisory).

### Fixed: `cs spore export` always emits the RO-Crate (ADR-140 D6, N7)

- **`cs spore export` no longer no-ops on a spore without a `[spore.astra]`
  stanza.** Emission was gated on an opt-in `emit` flag, so an explicit
  `cs spore export` could silently write nothing: an export verb that does
  not export. Export is the *share-time* emit (ADR-140 D6): it now always
  writes `ro-crate-metadata.json` to the `--out` dir, with the
  `[spore.astra].output` path only customizing the location. The seal is
  still marked present-but-unverified, never "verified".
- **End-to-end spore fixture.** Wired the public workshop
  `grace-business-analysis` bundle as a citation-only e2e test
  (`spore_e2e_fixture.rs`): `cs spore validate` / `run` / `export` against
  the live bundle assert the node set germinates, the seal gate fails closed
  without `--allow-unchecked-seal`, and the ASTRA crate is emitted. The
  fixture is referenced where it lives (not copied); the test skips honestly
  when the workshop galaxy is not checked out.

### Changed — renamed internal layer `almanac` → `almanac` (typo fix, 2026-06-27)

- **Typo correction.** The internal Zotero/MCP substrate layer was accidentally
  named `almanac` (missing the `l`) instead of `almanac`. Renamed consistently
  across all docs, ADRs, code comments, crate references, and lore files.
  The git-remote-blocklist entries (`github.com:noogram/almanac`,
  `github.com/noogram/almanac`) were already correct and are unchanged.
  Files renamed: `docs/adr/076-almanac-internalisation.md` →
  `076-almanac-internalisation.md`; `086`/`087`/`089`/`090`/`091-almanac-*` →
  `086`/`087`/`089`/`090`/`091-almanac-*`;
  `docs/lore/2026-04-26-almanac-internalisation.md` →
  `2026-04-26-almanac-internalisation.md`. Running-service rename
  (`~/.config/almanac/`) handled separately by the operator.

### Added — `cs patrol --heal`: the Deacon for the safe reversible anomaly classes (ADR-137 P3, `task-20260626-53f3`)

- **New `cs patrol --heal` mode** — the L2 *remediate* layer of the
  molecule-health primitive (ADR-137 §11 P3). Runs one stateless
  detect → §5-guard → remediate pass that mutates **only the low-risk,
  reversible anomaly classes**, each behind the P2 no-interference guard:
  **A1** unsent-paste (delegates to the transport's robust submit-retry —
  a bare Enter, never a pane re-grep), **A4/A8** idle-after-complete /
  completed-unharvested (`cs done` harvest from the orchestrator — a worker
  never self-`cs done`s), **A5** idle-no-progress (nudge referencing
  `briefing.md`), **A6** overloaded (a backoff hold, never a collapse).
  The collapse / integrity classes (A3/A7/A9) are *reported* but never
  auto-collapsed here (deferred to P4).
- **`cs patrol --heal --dry-run`** — zero-mutation preview of the health report
  + the guarded actions the Deacon would take. The safe default for earning
  operator trust before a scheduled heal pass.
- **Control-plane-keyed throughout** — detection and guarding read only typed
  state (molecule status, liveness lease, presence registry, whisper log, tags,
  kill-switches), **never a pane glyph**. The seven `delib-20260625-be1e`
  defects are structurally foreclosed (the SEV-1 `grep 'cs done'` use/mention
  bug cannot recur; no collapse-on-kill orphan; suffix-not-title mapping).
- **Idempotent + guarded + logged** — a per-molecule backoff ledger
  (`heal-state.json`, disposable sediment) enforces per-class cooldowns and the
  three-strikes stop; applied actions append to `heal-actions.jsonl`.
- **Retired** `scripts/drainage-tick.sh` lines 94–135 — the bespoke pane-grep
  health-pass the be1e panel flagged DO-NOT-SHIP. The drainage script keeps only
  its dispatch half (the separate Autonomous-regime concern).

### Added — Harbor auto-upgrade discipline (P8) doctrine + reference scripts (`task-20260625-b365`)

- **New doctrine [`docs/release/harbor-auto-upgrade-discipline.md`](docs/release/harbor-auto-upgrade-discipline.md).**
  The federation-wide rule for how a *running* cosmon guest instance (Dave's,
  Tenant-Demo's) pulls a new `cosmon-server` image from Harbor. Three rules: (1) pin
  to a STABLE channel, **never `:latest`** — the instance reads a tag the
  operator moves on purpose, with `pull_policy: never`; (2) **deliberate
  post-smoke promotion** — `edge → smoke → stable` is a human gesture (re-tag of
  the exact smoke-passed digest, never a rebuild, no `--auto`); (3)
  **drain-aware restart / live-session freeze** — never restart while live
  worker sessions exist. Migrated from `chancery:task-20260605-bf7a`.
- **`scripts/release/harbor-promote.sh`** — the deliberate promotion gesture.
  Re-tags `cosmon-server:edge` (or a named `--digest`) onto the `:stable`
  channel, gated behind a mandatory `--smoke-passed` signature. Promotion moves
  a registry pointer to the already-pushed digest — no pull, no rebuild.
- **`scripts/release/harbor-drain-aware-restart.sh`** — the only sanctioned
  restart path. Reads `cs status --json` `.sessions.active[]` (the existing
  worker-liveness surface, ADR-116) as the drain primitive; **freezes with exit
  75 (`EX_TEMPFAIL`)** when live sessions exist so a supervising scheduler
  retries until the instance idles. `--force` evicts, but only with an
  attributable `--force-reason`. Adds no new state, daemon, or `cs` verb —
  doctrine + two shell scripts reading a surface cosmon already projects.
### Added — `cosmon-remote run`: `do` + attributed cost delta (GATE Q1, `task-20260625-ba34`)

- **New top-level verb `cosmon-remote run`.** A thin client-side wrapper over
  the existing `do` flow (nucleate → credit guard → tackle → follow) that
  brackets the work with two `GET /v1/quota` reads and reports the **quota
  delta this run charged against the caller's bucket** — the "delta de coût
  attribué" the GATE Q1 onboarding test measures. The only cost plane the
  frozen v1 API exposes is the leaky-bucket rate-limit snapshot, so `run`
  attributes *that*, honestly: bucket level before → after, head-room consumed,
  and a one-line caveat that the bucket leaks continuously (a long follow can
  net-drain, so the delta is a net figure, not a request count). **Zero new
  routes** — same composition discipline as `do` (§5.1 untouched, §8p surface
  unchanged). The quota bracket is best-effort: a failed snapshot (older
  adapter / transient error) degrades the cost line to `unavailable` but never
  fails the run. `--json` emits the delta under `cost_delta`; `--yes` + `--json`
  give a reproducible, archivable test run. `do` stays available for callers
  who don't want the price.
- **Recipe.** [`docs/guides/cosmon-remote-recette-dave.md`](docs/guides/cosmon-remote-recette-dave.md)
  — a non-expert, chronometered, end-to-end recipe (install → doctor → auth
  login → run → result) with a fill-in timing sheet, written for the
  dave-noyau onboarding test.
- **Goldens re-blessed (conscious choice).** `run` is a 5th additive root verb;
  `tests/goldens/run.help.txt` is new, `root.help.txt` and `man/cosmon-remote.1`
  re-rendered from the live clap tree. The `fusion_diff` catalogue test now
  blesses five additive verbs (avatar, do, doctor, converse, run). Additive ⇒
  minor; no existing verb's surface changed.

### Added — `cs doctor supervision`: detect double-supervised binaries (`task-20260623-a2db`)

- **New read-only probe `cs doctor supervision`.** Cross-references the cosmon
  supervision roster (`~/.config/cosmon/patrols.toml` + `daemons.toml` — the
  single source of truth) against installed macOS LaunchAgents
  (`~/Library/LaunchAgents/com.you.*.plist` and `/Library/LaunchAgents`),
  and flags as a blocking error any binary supervised twice. This is the
  cosmon-side, DRY guard against the "retired-but-resurrected plist": a binary
  migrated to a patrol/daemon must not also carry a LaunchAgent. Shared
  interpreters (`bash`, `python`, …) are excluded from binary matching to avoid
  false positives; the `com.you.<name>` label still counts. Folded into the
  `cs doctor security` umbrella so the security patrol/CI catches the drift
  automatically. Forensic origin: `mailroom-sync` survived its 2026-04-19
  patrol migration (the live plist was never `launchctl unload`ed) and went
  unnoticed for two months — nothing cross-referenced the two supervisors. The
  probe found a second live instance (`mailroom-mural-build`) on its first
  run. Full causal report in the molecule directory.
- **Workspace build unblock (stop-gap).** `cosmon-transport/Cargo.toml` now
  enables `cosmon-core`'s `test-harness` feature, which holds the *production*
  `CommandRunner`/`Clock` ports it implements. Commit `494999bd4` had gated the
  whole `harness` module behind that feature, breaking `cargo check --workspace`
  fleet-wide. Mirrors the existing `cosmon-runtime` pattern; the proper fix
  (splitting production ports out of the test-gated module) belongs to the
  `task-20260622-da94` seam-lifting.
### Added — federation gitleaks baseline unblocks `cs done` harvests (`task-20260623-e9f0`)

- **`cs init` now scaffolds a repo-root `.gitleaks.toml`** from the canonical
  shared baseline (`assets/gitleaks/cosmon-baseline.gitleaks.toml`), and
  `cs init --upgrade` backfills it into galaxies already in flight. Both are
  customization-preserving (never overwrite an existing config). This closes a
  cross-galaxy invariant breach surfaced as a signal from mailroom: gitleaks'
  entropy-based `generic-api-key` rule structurally false-positives on the
  free-text `reason` prose in `.cosmon/state/events.jsonl` (e.g.
  `artefact=knowledge`), so every `cs done` that flushed state failed the
  pre-commit hook and **aborted the merge** until a human intervened. The
  baseline silences *only* that rule on *only* state-journal paths while keeping
  every high-confidence rule — plus a dedicated AWS `AKIA…` rule that gitleaks'
  default set lacks — scanning those journals, so a real secret pasted into a
  `reason` (cf. the Wasabi incident) is still caught at commit time. Rejected
  alternatives (sanitising the hash-sealed source-of-truth journal at write
  time; gitignoring it) are documented in
  [`docs/guides/gitleaks-state-journals.md`](docs/guides/gitleaks-state-journals.md).
  Complements the native `cs doctor leaks --corpus` scanner (same
  high-confidence posture, no entropy FP).

### Changed — hexagonal hardening of the CLI: state-store port + publish hygiene (`task-20260622-7072`, delib-20260622-187a)

- **CLI handlers route through the `StateStore` port.** The hexagonal story
  ("core holds `dyn StateStore`, adapters swappable") was previously honored
  in exactly one command (`cs run`); every other command imported the concrete
  `cosmon_filestore::FileStore`. Persistence now flows through a single seam:
  `Context::store()` / `Context::store_at()` build the `Box<dyn StateStore>`,
  and the high-traffic commands (`cs collapse`, `cs purge`, `cs review`,
  `cs witness`, `cs ensemble`, `cs observe`, `cs harvest`, `cs reconcile`) call
  the port — so swapping the JSON backend for the planned SQLite/Dolt adapter
  means changing one method, not ~30 call sites. `molecule_dir` is promoted to
  the `StateStore` trait. The lock-/path-coupled long tail
  (`cs evolve`/`complete`/`done`/`tackle`/…) is tracked in `task-20260623-5621`.
- **Long-tail commands routed through the port; `project_root` promoted**
  (`task-20260623-5621`, [ADR-131](docs/adr/131-statestore-port-locking-paths.md)).
  `project_root` joins `molecule_dir` on the `StateStore` trait — "where is the
  store rooted" is a storage concern every backend answers, not a filesystem
  detail — unwelding `cs thaw` and `cs patrol`. Every remaining command that
  constructed `FileStore` but called *only* port methods now builds through the
  seam (`cs status`, `freeze`, `stuck`, `resume`, `teardown`, `quench`, `note`,
  `interaction`, `migrate`, `notarize`, `deps`, `verify`, `verify-graph`,
  `await-operator`, plus the cross-galaxy/`diverge` foreign-store reads via a new
  Context-free `open_store` helper). The `cosmon_filestore::FileStore` name now
  survives in production only in the single construction seam (`cmd/mod.rs`) and
  the deferred lock-coupled core. ADR-131 specifies the object-safe RAII-guard
  **locking port** that closes that last gap and defers its ~23-call-site
  conversion to a dedicated PR (the crash-recovery core stays un-churned here).
- **`cargo publish` default-deny is now audited.** The workspace already sets
  `publish = false` for every library crate (only the reserved `cosmon`
  name-holder publishes); `scripts/architecture-audit.sh` gains a seventh
  invariant **INV-PUBLISH-DEFAULT-DENY** that enumerates workspace members via
  `cargo metadata` and FAILs if more than one crate is registry-publishable, so
  a stray `cargo publish` can never push an internal lib to crates.io from the
  public repo. Audit contract version bumped 1 → 2.
- **Reconcile idempotency is now proven at the CLI level.** A new end-to-end
  integration test (`tests/reconcile_idempotent_cli.rs`) runs `cs reconcile`
  twice against a multi-surface fixture (`STATUS.md` + `ISSUES.md`) and asserts
  every declared surface file is byte-identical on the second pass — closing the
  gap behind CLAUDE.md's "enforced by tests" claim, which previously held only
  at the renderer level (`cosmon-surface`).
### Removed — pre-publication repo scope trim: non-product crates git-rm'd (`task-20260622-eeb9`, delib-20260622-187a B1)

- **The published `cosmon` repo is now trimmed to the actual product.** 28
  non-product crate directories (≈1180 tracked files) were `git rm`'d so the
  on-disk `crates/` set equals cargo's resolved product closure (verified by
  `git ls-files crates/ | cut -d/ -f2 | sort -u` matching `cargo metadata`).
  Previously these were only dropped from `[workspace] members` — they stayed
  git-tracked and would have published verbatim in the AGPL release. Removed:
  the Zotero/reference stack and **Sci-Hub DOI index** (`almanac-*`), the
  operator voice stack (`mailroom-voice-*`), the Lean prover (`foundry-*`),
  the genetic-algorithm proof search (`ga-*`), sibling-galaxy MCP servers
  (`neurion-mcp`, `topon-mcp`), `schedulerd`, `noogram-mycelial-monitor`,
  `cosmon-bridge-gastown`, the operator-feature crates (`cosmon-saas`,
  `cosmon-matrix-tick`, `cosmon-voice-bridge`), and the vendored llama.cpp
  chain (`cosmon-llama`, `cosmon-llama-sys` + `vendor/matrix-sdk`).
- **`cs tackle --adapter llama-cpp` (in-process llama.cpp loop) was removed.**
  The `llama` / `mock-ffi` cargo features and the `cosmon-provider::llama`
  adapter are gone; the `llama-cpp` adapter row stays registered and now fails
  loudly with a typed `FeatureNotCompiled` error rather than dispatching. The
  `ProviderId::LlamaCpp` enum variant is kept so existing `state.json` files
  still deserialize. A Rust-native local-model path for local-first autonomy
  will be reconsidered separately.

### Added — `archived ⇒ status.is_terminal()` invariant: detect + heal (`task-20260618-35f2`, idea-20260618-1b10)

- **`cs verify --invariants`** enforces the structural invariant
  `archived ⇒ status.is_terminal()`: an archived molecule must carry a
  terminal status (`completed`/`collapsed`). A row with
  `{archived: true, status: running}` is a *ghost* — torn down out-of-band
  (e.g. `cs done --force` on a never-completed molecule) without terminalizing
  its status, so it keeps rendering as live work. Fleet-wide when no molecule
  id is given (the galaxy-wide audit), per-molecule when one is. Detect-only:
  the check never mutates state and exits non-zero on any violation. Composes
  with `--federation` in the fleet-wide audit.
- **`cs reconcile --heal-invariants`** opts into a one-shot on-disk migration:
  every archived-but-alive ghost is rewritten to `status = collapsed` (reason
  `archived-but-alive heal`) with a durable `MoleculeStatusChanged` +
  `MoleculeCollapsed` event pair so the heal survives a cache rebuild from
  `events.jsonl`. Idempotent; default `cs reconcile` stays a pure projection
  and never mutates molecule state. After a heal pass, the galaxy-wide
  `cs verify --invariants` reports zero violations.
### Fixed — `archived ⇒ status.is_terminal()` invariant in `cs done --force` (`task-20260618-abb7`, idea-20260618-1b10)

- **`cs done --force` on a molecule that never reached a terminal state (worker
  died before any `cs evolve`, or never tackled) now terminalizes its status**
  instead of leaving `{archived: true, status: Running}` on disk. Such a row was
  a permanent `👻 unnamed-merge` ghost: archived physically, yet `Running` to
  every `status`-keyed reader (`cs observe`, `detect_ghost`), and un-killable —
  a repeat `cs done` short-circuited on `archived` and `cs reconcile` re-derived
  it. The `--force` teardown now stamps `status = Collapsed`
  (cause `Manual`, reason `forced-teardown`) in the same save that writes
  `merged_at` / `archived`. The terminus reuses the existing `Collapsed` variant
  (no new status, no ADR) — semantically honest since no work completed, and the
  guard makes it a no-op on the normal path where the molecule is already
  `Completed`.
- **Defense-in-depth at the readers.** `detect_ghost` now returns `None` for any
  archived molecule (archived ⇒ off the shelf ⇒ never a live ghost), and the
  default `cs observe` list view drops archived rows (`--all` / `--status` still
  surface them). This heals every legacy on-disk ghost of this shape (e.g.
  `task-20260418-d0c4` in `sandbox`) with zero state migration —
  the row stays as written but no longer renders. Reported cosmon-ward from the
  `sandbox` galaxy (the reactor learns from what it burns).

### Added — D7 publish-identity gate in `cs done` (`task-20260617-4bce`, ADR-128 §V1)

- **`cs done` now scans the git author/committer identity of the commits a
  merge would publish** (`<base>..<branch>`) and aborts before merging if a
  confidential identity rides them. This widens the D7 publish-content guard
  beyond file content to the git-identity channel — the operator email is
  stamped into every commit and is invisible to any content grep. Configured
  per-project in `.cosmon/config.toml` under a new `[publish_identity]` block
  with two layers: `allowed_emails` (closed-codebook whitelist — any
  author/committer email outside the codebook is a violation by construction,
  recall → 1 on the git-identity slot) and `forbidden_substrings`
  (defense-in-depth blacklist over names and commit messages). **Ships empty
  by default** — backward-compatible for every project (cosmon itself is
  internal, where the operator identity is legitimate). The abort message
  carries a mandatory residual-risk statement: the gate is syntactic and does
  not detect paraphrase, implication, or composed disclosure (undecidable).

### Changed — release membrane flipped to deny-by-default allowlist (`task-20260617-4847`, ADR-127)

- **`cs release-audit` is now a deny-by-default allowlist, not a frozen
  denylist.** The old gate asked "does this match a known-bad pattern?" — a
  monotone-decreasing filter on a monotone-increasing set of confidential
  tokens, so a brand-new client name / domain / subsystem shipped **silently**
  (the 2026-06-10 failure class). The new primary verdict is: *is every
  shipping path positively cleared?* Every tracked, non-purged path must carry
  a per-path permit in `.cosmon/release-allowlist.toml` (never a glob) or it is
  a `path-not-permitted` regression. New confidential file → no permit →
  RED **by construction**. This generalises Gate G's binary deny-by-default
  polarity to the whole text tree.
- **Content-bound permits (`seal = "blake3:…"`)** go `permit-stale` on any
  edit — *cleanliness-now*, not freshness-at-t0. Path-level permits (no seal)
  clear the path and survive ordinary edits; the legacy token/structural
  detectors demote to a **content backstop** on permitted files.
- **Migration is incremental (ADR-127 §7).** The membrane is **armed by the
  presence of the allowlist file**. Absent it, the audit behaves as before but
  emits a **loud warning** (`membrane: legacy-denylist`) — an absent control
  can never masquerade as a clean tree. Bootstrap the allowlist with the new
  `scripts/release/bless-allowlist.sh` (a **separate** tool; the audit stays
  read-only — write-read asymmetry preserved).
- **Bucket-3: the detector stopped being its own leak.** The confidential
  denylist literals (client tokens, private domains, private-infra crate names,
  the purge lists) moved OUT of `crates/cosmon-cli/src/cmd/release_audit.rs` — a source
  file that ships in the public binary — INTO the private, purged-from-release
  `.cosmon/release-rules.toml`, loaded at runtime. The shipped source now
  carries **zero** client names (tests use synthetic tokens). A foreign clone
  with no rules file runs with an inert backstop and says so.
- New report fields under `--json`: `membrane_mode` (`allowlist` |
  `legacy-denylist`) and `warnings`. New detectors: `path-not-permitted`,
  `permit-stale`, `permit-orphan`.

### Fixed — README CLI Reference table no longer lies (`task-20260616-8f4a`)

- **The README `## CLI Reference` table advertised four phantom verbs**
  (`cs spawn`, `cs stop`, `cs mail`, `cs nudge` — none exist in the clap
  tree) and omitted the real flagship verbs (`tackle`, `done`, `peek`,
  `wait`, `demo`, `doctor`, `init`). A reader who copy-pasted from the table
  got "no such subcommand". The table is now regenerated from the actual
  subcommand surface and covers the real pilot cycle + monitoring portal +
  first-contact verbs.
- **`crates/cosmon-cli/tests/readme_cli_table.rs` is a phantom-verb gate**
  (karpathy): it reads the live subcommand list from `cs __help-tree` and
  asserts (1) every `cs <verb>` named in the README table is a real
  subcommand, and (2) the load-bearing flagship verbs are never silently
  dropped again. The table can no longer rot away from the binary.
- **Project-structure block** now marks `cosmon-mcp` as DEPRECATED /
  out-of-default-workspace, matching the CLI-first invariant (it had been
  listed as a live "MCP server for agent orchestration").
### Added — the contribution path is now open (`task-20260616-0e75`)

- **Root [`CONTRIBUTING.md`](CONTRIBUTING.md)** is the discoverable front door
  every host (GitHub, editors) looks for. It links the full
  [`docs/CONTRIBUTING.md`](docs/CONTRIBUTING.md) guide, states the four-gate
  Definition of Done, and points contributors at the real backlog surface
  (`ISSUES.md`) instead of a private issue tracker.
- **README "Contributing" section** now links `CONTRIBUTING.md` and replaces
  the dead "Check open issues" step (which sent contributors to a private,
  404-ing GitHub issues page) with a pointer to the local `ISSUES.md` surface.

### Changed — `just install` is public-safe; private federation split out (`task-20260616-0e75`)

- **`just install` no longer installs the private federation tooling.** It
  now builds and installs only the public cosmon product binaries (`cs`,
  `cs-api`, `cosmon-remote`, `cosmon-daemon-supervisor`) plus the `cs` man
  page. Because this recipe is the `cs done` post-merge hook, a contributor's
  `cs done` is now decoupled from the federation — it never installs
  `neurion`, `topon-mcp`, or `almanac`.
- **New `just install-federation`** installs the private federation tooling
  (`neurion`, `topon-mcp`, `almanac`) — operator-workstation only.
- **New `just install-all`** = `install` + `install-federation`, the
  historical full-install behaviour for a federated workstation.

### Added — `install.sh` drops a non-destructive pilot-pack (`task-20260615-6310`)

- **`curl <host>/install.sh | sh` now makes the avatar pilotable by any
  harness out of the box.** After fetching the `cosmon-remote` binary and
  persisting the profile, the installer drops a **pilot-pack**: a managed,
  idempotent, never-clobbering block of NL piloting instructions so codex,
  opencode, gemini-cli, and Claude Code on the box all learn to drive
  `cosmon-remote` with zero per-project setup. Implements the ADR-125
  (Valence/Aperture) pilot-pack design from the `pilot-portability` and
  `piloting-cosmon-from-any-harness` guides.
- **Three artifacts, all idempotent and non-destructive:** (1)
  `~/.config/cosmon/pilot.AGENTS.md` — the canonical content cosmon owns;
  (2) a fenced *managed block* (`# >>> cosmon pilot-pack >>>` … `<<<`)
  inside `~/AGENTS.md`, the AAIF/Linux-Foundation standard — replaced only
  between its markers, the rest of the user's file byte-preserved (conda/rbenv
  pattern); (3) `~/CLAUDE.md` and `~/GEMINI.md` symlinked to `AGENTS.md` (one
  file, every harness) — never clobbering a real file.
- **Speaks the REMOTE surface** (`do`/`result`/`events`/`converse`, no `done`)
  because an avatar box usually carries only `cosmon-remote`.
- **Announced + opt-out + standalone refresh.** The drop prints what it did;
  skip it with `--no-pilot-pack` or `COSMON_SKIP_PILOT_PACK=1`; refresh it later
  with `sh install.sh --pilot-pack` (no binary fetch, no host needed — the same
  function the install-time path calls, so the two never drift).

### Added — `codex` adapter now dispatches (Gap#5, `task-20260615-df30`)

- **`cs tackle --adapter codex <id>` now spawns a real worker.** codex was
  already advertised (`cs --help`, `man cs`), exit-classified, preflight-probed,
  and tmux-supervised, but was missing from the two places that matter — the
  dispatch registry (`declared_names`) and the `spawn_and_prompt` match — so it
  died at `validate_adapter_name` with *"not declared."* Both gaps are now
  closed: codex joins `claude`/`aider` as the third external-CLI subprocess
  adapter, invoked as `codex exec '<prompt>'` in a tmux pane.
- **New `CodexProbe`** (`cosmon-transport::readiness`) asserts liveness from
  codex's `exec` preamble — the same anti-surface-lie `LiveProbe` contract the
  claude/aider paths use — so an `[exited]` carcass pane (binary missing on
  PATH, crash on launch) is caught instead of the prompt firing into a dead
  pane.
- `spawn_codex_session` now invokes `codex exec` (codex's non-interactive
  automation subcommand, matching the exit-classifier's existing assumption)
  rather than the never-reached `codex --workdir` form; the pane's cwd is
  supplied by tmux (`new-session -c <worktree>`).

### Added — OpenAI adapter: client-side rate-limit pacing (`task-20260615-b9ce`)

- **The `openai` adapter now paces transient HTTP 429s instead of aborting.**
  `OpenAIProvider::one_turn` retries a transient `RateLimited` response with a
  bounded, `Retry-After`-aware back-off (new `RetryPolicy`, default 4 retries
  honouring the server header, capped at 60 s, exponential fallback). Quota
  breaches (`QuotaExceeded`), transport, and decode failures still surface on
  the first response — only the transient tier-throttle is paced. The retry
  count and per-wait cap keep `one_turn` finite, preserving the harness
  spine's `O(K)` termination proof. Tune or disable per-provider via
  `OpenAIProvider::with_retry_policy` (`RetryPolicy::DISABLED` delegates pacing
  to an external scheduler). Motivated by the measured Mistral Large
  4-requests-per-minute billing tier
  (`docs/measurements/parity-cliff-mistral-leg-2026-06-15.md`, §C): the model
  is Claude-class on quality, and this removes the one operational wall to a
  fast multi-turn agentic loop without an account upgrade.

### Changed — cosmon-remote surface: one language, one name (P3+P4, `task-20260614-d482`)

- **Tenant CLI surface is now English throughout** (operator verdict
  2026-06-14: the French was internal culture that had leaked). Translated
  every user-facing string in `doctor`, the `--help` golden-path epilogue
  (`root_help`), runtime command output (`config`, `molecule nucleate`
  truth, `auth me` worker line), the actionable error hints (`hints`), and
  the phone-home one-liner. No i18n layer added — a single target language
  needs no channel capacity it will not use; the co-located source strings
  are the future i18n branch-point, not opened by anticipation.
- **The displayed binary name follows `argv[0]`, never hand-pinned** (P4).
  The copy-paste remediation lines and the `--help`/usage epilogue now
  render under the *invoked* name (`cosmon` alias vs `cosmon-remote`) via
  the already-existing `invoked_name()` source — the 6 literals shannon
  flagged (`main.rs` config-init / no-profiles / nucleate-tackle) plus the
  `auth me` worker line, and the root epilogue now built dynamically at
  runtime. The man page and committed goldens keep the canonical
  `cosmon-remote` (the man-page filename `man cosmon-remote` is a real
  artifact name, name-independent). Re-blessing of `tests/goldens/root.help.txt`
  and `man/cosmon-remote.1` is the conscious gesture for the epilogue
  translation — the only golden bytes that moved. `cargo test -p
  cosmon-remote` 94 green; clippy clean (no new warnings).
- **Scoped out (deliberate, see `result.md`):** the canonical
  `~/.config/cosmon-remote/` directory (data, not argv); the cross-crate
  effect markers `[coûteux]/[irréversible]` (owned by `cosmon_surface_canon`,
  ripple into smithy's generated API-ref — separate molecule); and the
  in-library `doctor`/`hints` remediation commands' name token (lib layer
  has no argv; `hints` name-routing is C4's passage).

### Added — cosmon-remote 0.3.0 (avatar-surface B2: run + do)

- **`molecule run <root>`** — dials the new `POST /v1/molecules/{id}/run`
  (B2 bounded drain, ADR-124, `task-20260610-56c4`): the client REQUESTS
  a drain of the DAG rooted at `<root>`; the resident `cs run` loop in
  the tenant container DECIDES what to tackle, under the binding-sealed
  B1/B2/B3 bounds (read them via `quota`; never client-writable). 202 on
  spawn; `drain.started` / `drain.terminated` (named reason tokens
  mirroring `cs run` exits 0/90/91/92/124) on the events stream. `cs run`
  exits the ADR-080 §5.1 operator-only list via the §5.2 successor path
  (ADR-124) — the operator semantics (unbounded, local flags) stay
  unexposed.
- **`do "<topic>"`** — one gesture: nucleate + credit guard + tackle +
  follow (observe poll + best-effort events tail). PURELY client-side
  composition of existing routes — zero new routes, doctrine §5.1
  untouched; `molecule nucleate` stays available as the advanced path.
  The golden first hour becomes `login → do → result` (4 gestures).
  The **credit guard** (« this LAUNCHES AN AGENT and BURNS CREDIT —
  continue? ») displays before the FIRST spend (the tackle; nucleate is
  free), once: a confirmed interactive yes persists
  `credit_guard_acknowledged = true` in `config.toml`; `--yes` skips for
  one run WITHOUT persisting (a script's yes is not the operator's).
  Declining leaves the molecule pending and names the manual gesture.
  Gates pinned by `tests/do_flow.rs`: a `do` produces a recoverable
  `result` end-to-end; a declined guard hits the tackle route zero times.
- `config.toml` grows the optional `credit_guard_acknowledged` key —
  additive, omitted when unset: existing files round-trip byte-identical
  (fixture-pinned).
- Help surface: root gains the `do` line, `molecule` gains the `run`
  line — both additive, pinned exactly by `tests/fusion_diff.rs`
  alongside the 0.2.0 blessings. Minor, non-breaking.
### Added — cosmon-remote (avatar-surface A4: top-level `converse`)

- **Top-level `converse` verb** (`POST /v1/avatar/converse`, canal (b))
  — send a typed message (`request`/`announce`) to a bound
  avatar-tiers. Deliberately the LAST command in `--help` (off the
  golden path) and never an `avatar` subcommand: « avatar est un mot de
  doctrine, jamais un nom d'API » (tenant guide §12.2). The route's
  gating is unchanged server-side: on-by-binding,
  refused `503 no_binding` without an explicit operator binding. The
  canon line's exposure flipped adapter-only → tenant-verb; the verb
  joined both bijection gates (14/14). Additive ⇒ minor.
- **L3 anti-cycle bound (server-side).** Synchronous `request`
  conversations carry a `hop` relay counter (additive body field,
  default 0); chains at or beyond the binding's bound are refused with
  the stable code `409 max_hops_exceeded`. The bound is read from the
  binding (`max_hops` key, default 8) — readable, never writable by the
  client. `announce` (fire-and-forget) is exempt: no mutual wait, no
  cycle (godel L3 — the runtime analogue of the TLA+ circular-wait
  finding).
### Added — cosmon-remote 0.3.0 (avatar-surface A3: man + doc-gen parity)

- **`man cosmon-remote`.** The proven `cs __man-page` pattern is
  transposed: a hidden `__man-page` subcommand renders the man page from
  the live clap tree via `clap_mangen`; the committed
  `man/cosmon-remote.1` is golden-checked byte-for-byte
  (`tests/help_goldens.rs::man_page_matches_committed`, `MAN_UPDATE=1`
  to refresh). One snapshot family: the man is a deterministic
  projection of the same tree the help goldens pin — never written
  beside it (shannon G3').
- **Pedagogical blocks live in the clap tree.** The hand-written
  cs-thin `help.rs` blocks (TYPICAL WORKFLOW, AUTHENTICATION, EXIT
  CODES) are drained into `after_long_help` on the root, `auth login`
  (the three-step PKCE flow) and `molecule nucleate` (nucleate → tackle
  → result) — rendered in `--help` AND the man page from one source
  (tolnay §4.2). Conscious golden re-bless: the affected pages flip to
  clap's long-form rendering (options reflow; catalogue lines intact,
  pinned by `fusion_diff.rs`).
- **`[coûteux]` / `[irréversible]` markers derived from scope.** Every
  route-backed `about` appends `CanonRoute::effect_suffix()`, which
  delegates to the ONE map `cosmon_surface_canon::effect_annotation`
  (godel C5: `cosmon:worker:spawn` ⇒ `[coûteux]`; the reserved
  `cosmon:worker:terminate` ⇒ `[irréversible]`). Today exactly
  `molecule tackle` renders a marker — derived from the canon's scope
  column, never hand prose.
- **Formula semantics stay OUT of the binary** (godel C2/L4, tolnay
  §4.3): help and man state explicitly that `--formula` is opaque and
  that a formula's meaning is deployment content, not frozen surface.
  A discovery route/catalogue endpoint is a named follow-up — it would
  grow the §8p canon and needs its own molecule.
- **`xtask gen-api-ref`.** New workspace tool projecting the §8p route
  tables (catalogue + bijection summary) from `surface_events.txt` into
  marker-delimited blocks of smithy's
  `docs/specs/cosmon-rpp-api-reference.md` (`--check` mode for CI/gates;
  golden-checked in `xtask/tests/`). Fixes drift M3: the generated doc
  carries the full canon — `GET /v1/molecules/{id}/result` included —
  with computed counts, no hand-bumped literals.
### Added

- `cs patrol --abandon` — patrouille-abandon (avatar-surface C3):
  folds traces an instance has already emitted
  (audit envelopes, phone-home reports, PKCE sessions, instance
  ledgers) into five named abandonment motifs per tenant;
  `decroissance-de-signalement` carries gravity HIGH.
  Read-only; `--abandon-root` and `--abandon-quiet-hours` knobs.
- `cosmon-remote`: passive opt-out remontée — on an
  abandonment-predicting failure (503, 502, write-4xx burst) one line
  offers to send `request_id + error code` (never artifact content,
  never the raw sub) on the next successful request via
  `X-Cosmon-Phone-Home`; disable with `config set phone-home off`.
  The adapter materialises reports under `<inbox>/phone-home/` via a
  middleware (no new route, §8p untouched).

### Changed — cosmon-remote 0.2.0 (avatar-surface A2: the tenant-CLI fusion)

- **One tenant binary.** `cs-thin` (the second, operator-built tenant CLI)
  is deleted; `cosmon-remote` is the single delivered surface and now
  carries the engine discipline under its hood (shannon's M2_code=1
  verdict: the binary tenants install was
  covered by no bijection test). NOT breaking for installed tenants: the
  binary name, profiles (`config.toml` byte round-trip pinned by test),
  JSON shapes (`MoleculeKindWire::Unknown(raw)` skew tolerance intact)
  and exit codes are unchanged. 28 of 35 pre-fusion `--help` pages are
  byte-identical (golden-checked); the 7 conscious text diffs are pinned
  exactly by `tests/fusion_diff.rs` and argued below.
- **Routes are projections, not prose.** Every `/v1/` route the binary
  dials is a build-time const folded from the §8p surface canon
  (`surface_events.txt`); the clap `about` strings embed the same consts.
  No clap struct re-declares a route; removing a canon line is a compile
  error in the tenant binary. `routes_and_verbs_are_bijective` now runs
  delivered-binary-side, closing the canon ↔ `#[verb]` registry ↔
  installed-binary triangle.
- Artifact help placeholders aligned with the canon (`{mol_id}`→`{id}`,
  `{name}`→`{token}`) — description text only, args unchanged.

### Added — cosmon-remote 0.2.0

- **`cosmon` alias.** `install.sh` poses a `cosmon` symlink next to
  `cosmon-remote` (additive; never clobbers a foreign `cosmon`); help and
  usage render under the invoked name (delib T1: the long name is the
  contract, the short one is the product face).
- **`avatar status|incarnate|grant|audit|mould-info`** drained from
  cs-thin — the delivered binary now covers all 13 §8p tenant verbs.
  Scopes are minted per route from the canon's scope column.

### Fixed — cosmon-remote 0.2.0

- **`molecule freeze` worked on no adapter since v1.0.0-rc** — it posted
  `{reason}` without the mandatory `state` discriminator (400). It now
  sends `{state: "frozen", reason}` per the fused-route contract.
- **`molecule thaw` dialled the removed `/thaw` route (410 Gone).** It
  now rides `POST /v1/molecules/{id}/freeze` with `{state: "active"}`;
  its help text says so (conscious golden re-bless — the old text
  advertised a dead route).
- **`molecule tackle` under-minted its token** (`molecule:write` only,
  where the adapter's authorise grid demands `write+worker:spawn`) —
  the composed scope now comes from the canon line.

### Fixed

## [0.1.0] — 2026-06-10

The first tagged release of Cosmon: a **stateless CLI that gives AI coding
agents a persistent identity, a typed lifecycle, and crash-recovery** — so you
can run several Claude (or other adapter) sessions in parallel on one codebase
without losing track of who is doing what.

This is the inaugural public version. There is no prior release; everything
Cosmon does today ships in `0.1.0`. The section below describes what the
release **is**, not how it was built.

### Added

- **The pilot cycle — `nucleate → tackle → wait → done`.** The core loop:
  `cs nucleate <formula>` creates a typed unit of work (a *molecule*),
  `cs tackle <id>` spawns a worker for it in an isolated git worktree + tmux
  session, `cs wait <id>` blocks (backgroundable) until the worker reaches a
  terminal state, and `cs done <id>` merges the branch and tears the session
  down. One decision per invocation, git-composable, no orchestrator process
  required.

- **Stateless by design — no daemon, no database server, no scheduler.** JSON
  files under `.cosmon/state/` are the single source of truth. Every command
  is a one-shot, idempotent invocation; the system is composable with any
  external scheduler and survives crashes because nothing lives only in RAM.

- **Crash-recovery and lifecycle management for agents.** Molecules carry a
  compile-time-checked typestate lifecycle (pending → active → completed, with
  collapse / freeze / thaw / decay transitions). A worker that dies mid-flight
  leaves its state on disk; `cs reconcile` rebuilds every projected surface as
  a pure function of that state, and a molecule in motion can be resumed rather
  than restarted from zero.

- **`cs demo` — one-command first contact.** A self-contained walkthrough that
  runs the full `nucleate → tackle → wait → render` cycle on a fresh temp
  directory with no pre-seeded state, so a newcomer can see the pilot cycle
  work end-to-end before reading a line of doctrine. A clean-machine preflight
  in `cs tackle` checks git / tmux / adapter on `PATH` *before any side effect*
  and fails fast with one actionable line per missing prerequisite (run
  `cs doctor` for a fuller check).

- **Molecules + formulas as the only extension point.** Everything Cosmon
  tracks is a molecule (six kinds: 💡 idea, 🔧 task, 📐 decision, 🐛 issue,
  ⚡ signal, 🧠 deliberation); every workflow is a declarative TOML *formula*
  over molecules. You extend the system by writing a formula, not by adding a
  command, a daemon, or a plugin interface. Per-step git commits and
  BLAKE3-sealed `prompt.md` / `briefing.md` artifacts give every molecule a
  durable proof-of-work trail.

- **DAG orchestration — `cs run`.** Typed links (`Blocks`, `Refines`,
  `Entangled`, `DecayProduct`, …) form a dependency graph; `cs run <root>`
  walks it, dispatching ready molecules and merging each predecessor before its
  dependents are tackled (merge-before-dispatch). The DAG carries ordering;
  content flows through the filesystem and git branch lineage, never through
  mailboxes or a message broker.

- **The monitoring portal — `cs peek`.** A recursive TUI that descends from a
  fleet overview down to a single molecule's tmux pane, briefing, log, events,
  and artifacts — one keystroke per fractal descent. `cs peek --all` aggregates
  across every galaxy on the machine. `cs ensemble` gives an actionable backlog
  snapshot; `cs observe` dumps a single molecule's state for scripts.

- **Energy accounting.** `EnergyBudget` and `Temperature` track token
  consumption and cost per molecule — a secondary lens on the fleet, not the
  reason to adopt it.

- **Surface sync.** Internal state is projected onto plain files that
  non-participants can read (`STATUS.md`, `ISSUES.md`, `docs/adr/INDEX.md`, and
  optionally GitHub Issues) via `cs reconcile`, with a CI `--check` gate that
  flags drift.

- **Agent-first interface.** Every command supports `--json` (NDJSON) output.
  Workers interact with the state store through the same `cs` CLI a human uses
  (walk-up discovery from the worktree), mirroring the git model.

- **Rust workspace foundation.** A zero-I/O domain core (`cosmon-core`:
  typestate molecules, newtype IDs, physics vocabulary, an exhaustive
  `thiserror` hierarchy) with all I/O behind traits in separate crates
  (`cosmon-state`, `cosmon-filestore`, `cosmon-transport`, `cosmon-graph`,
  `cosmon-surface`, …). `#![forbid(unsafe_code)]` across the workspace,
  `#![deny(missing_docs)]` on the core, and CI gates on build, test, clippy,
  and fmt.

[Unreleased]: https://github.com/noogram/cosmon/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/noogram/cosmon/releases/tag/v0.1.0
