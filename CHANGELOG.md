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

### Added

- **`cs spore install` — the verb that gets a shared bundle into a project.**
  The spore family could validate, germinate and export a bundle, but every one
  of those verbs started from a bundle already on disk, and getting it there had
  no verb: clone or copy by hand, pick a directory, and copy the recipes into
  `.cosmon/formulas/`. Skipping that last step is silent and expensive — a
  molecule stores its formula by **id** and `cs tackle` resolves that id against
  the mission project's registry, so an uninstalled bundle germinates fine and
  then runs with every per-step `adapter`/`model` pin inert (the 23-node run of
  task-20260725-eb3b). `cs spore install <source>` is both steps: it fetches
  from a local path, `github:owner/repo[/subdir][@ref]`, a GitHub tree/blob URL
  (paste the browser's), or any other git remote (`--git-ref` / `--subdir`);
  places the bundle under `<project>/spores/<spore-name>/` or `--dest`; and
  registers each recipe under the name the recipe **declares**, which is the
  name dispatch looks up — registering under the bundle's file name would leave
  the pins exactly as unreachable. It is called `install` and not `add` (which
  would name a dependency manifest cosmon does not have for spores) or `import`
  (which would claim to invert `export`, an in-place hash-and-RO-Crate emit).
  Fail-closed before anything is written: an `--expect-hash` mismatch against
  the same content-addressed id `cs spore export` prints, a bundle missing a
  file its manifest declares, a **symlink** in the fetched tree (refused, never
  followed), a path that escapes the destination, a non-empty destination, or a
  registry recipe of the same name with different content — the last two
  overridable with `--force`. A byte-identical recipe is a no-op, so
  re-installing is idempotent instead of training the operator to pass `--force`
  past the one conflict that mattered. `--dry-run` prints the plan;
  `--no-formulas` places the bundle and still reports every pin it is leaving
  unreachable. Provenance (`source`, resolved commit, bundle hash) is recorded
  in `.spore-install.toml` in the destination, deliberately outside the coverage
  set the bundle hash binds. See [`docs/cs-spore.md`](docs/cs-spore.md).

- **A formula can declare what it needs of its worker, and `cs tackle`
  refuses a dispatch that cannot supply it** — the open half of
  noogram/cosmon #4, clause 2, and the reporter's own suggestion: *gate
  formulas on adapter capabilities*. The `local` adapter is an in-process
  chat loop with no shell, no VCS and no `cs` command; the earlier fix
  stopped *telling* it to run cargo and commit (adapter-aware briefing,
  `d81b58a`), but a formula's step text still describes shell work, and no
  wording lends a chat loop a shell. Formulas now say so directly —
  `requires_capabilities = ["shell", "vcs"]` — and `cs tackle` refuses the
  pairing with exit code **17**, naming the missing faculties and the
  adapter that has them. The refusal lands before the worktree, the pane
  and the model preflight, so the molecule stays pending and re-tacklable
  with nothing to clean up, and it fires under `--dry-run` too. Declared on
  `producer-work` (its `smoke-dispatch` gate *executes* a script) and
  `merge-conflict` (VCS surgery); opt-in per formula, so every formula that
  declares nothing — including everything `cs demo` routes to on the local
  floor — is untouched. `COSMON_SKIP_CAPABILITY_GATE=1` dispatches anyway
  for deliberate experiments. Vocabulary: `shell`, `vcs`, `cs-cli`
  (`cosmon_core::adapter_capability`); an unknown token fails the formula
  load rather than being silently dropped, since a dropped requirement is a
  formula claiming a gate it does not enforce. The resident runtime treats
  the refusal as **permanent** — like the briefless guard, an identical
  retry reproduces it exactly — and parks the molecule instead of
  re-dispatching it every tick; the `cs run` summary counter is accordingly
  `permanently_parked` (was `briefless_parked`, which the new member would
  have made a lie).

### Documentation

- **The external-contributor architecture path now starts with ten concrete
  checks instead of a 3,700-line prerequisite.** The full invariant catalogue
  remains authoritative, but its new front page routes a typical PR only to
  the sections it needs. The PR template expands its compressed coherence
  jargon into one self-verifiable test per checkbox, with an explicit reason
  required for every non-applicable line.

- **The ten-minute quickstart now says to `ollama pull qwen3:8b`, not just
  `ollama serve`** — the remaining docs item of noogram/cosmon #4. A daemon
  with nothing pulled answers, so the setup looks healthy and the dispatch
  dies seconds in. The prerequisite block now names the pull, the
  structured-`tool_calls` requirement that makes `qwen3:8b` the default, and
  the concrete failure of a model that lacks it (`qwen2.5-coder:7b` pastes
  its tool call into the message text as raw JSON). The capability gate
  above is documented in `docs/guides/local-model-selection.md`, beside the
  model choice it is easily mistaken for.

### Fixed

- **A committee seat's `contract-hash` is now verified against the contract
  body, instead of being taken on the convener's word.** The hash was compared
  against the roster's own copy of itself and never against the prose it claims
  to address, so a digest-shaped string that digests nothing passed every
  check. The omission was licensed by a written justification — that live
  rosters carry opaque labels, so verifying "would refuse every committee
  convened to date — an outage, not a control." Measured across the 29 live
  contracts under the normalisation the parser itself computes, that
  justification was false in both claims: none of the 29 is an opaque label
  (all are digest-shaped), and 20 already verify. The 9 that do not are
  fabrications the old check could not see — one identical `blake3:7bf51880…`
  appears under three *different* contract bodies, and one names `sha256:` at
  32 hex characters, half a sha256's width. Verification is algorithm-agnostic
  by declared prefix (`blake3`, `sha256`, `blake2b-256`), because 19 of the 20
  honest hashes are not blake3 and a blake3-only check would have been the real
  outage — and each of the three is load-bearing: the corpus's 20 honest
  digests are 18 sha256 (7 of them bare hex), 1 blake3 and 1 blake2b-256, so
  dropping any one algorithm refuses a contract whose author computed it
  correctly. Re-measured over the same 29 after the change: exactly the 9
  fabrications are refused and all 20 honest digests verify.
  Conveners compute the hash with `shasum -a 256 body.md` or
  `cosmon_core::committee::committee_contract_hash`; a stable label is no
  longer accepted.

## [0.5.0] — 2026-07-31

### Fixed

- **`cs tackle` can no longer forge a briefing-delivery receipt from an empty
  pane** (part of noogram/cosmon#26). A `capture-pane` that succeeded but
  returned a blank frame was classified as "the briefing left the composer", and
  two such readings in a row signed the delivery receipt. A blank frame now
  reads `Unobservable`, which signs nothing. In the same family, the receipt's
  needle scan is now wrap-invariant: a briefing whose final line is wider than
  the pane is no longer declared absent because the terminal wrapped it.
- **Every dispatch stopped paying a flat ~90 s briefing-confirmation tax**
  (noogram/cosmon#26). Claude Code 2.1.220 no longer exposes a reliable
  `Working` state, so the old exit condition never fired and every dispatch ran
  out the whole window. Delivery is now proven by the briefing leaving the
  composer. Measured: 105–107 s dispatches down to 24–25 s.
- **The briefing submit-retry now survives the process that started it**
  (noogram/cosmon#26). Past the short in-band window, a detached
  `cs briefing-backstop` in its own process group keeps pressing on a
  twenty-minute budget and removes the pending record only on a delivery
  receipt.
- **`cs` no longer discards its own warnings** (reported by @jdthaler on
  noogram/cosmon#26). The CLI installs a tracing subscriber on stderr — warnings
  and above by default, `RUST_LOG` honoured — so recovery instructions such as
  the still-pending briefing message actually reach an operator.
- **`briefing_backstop_survival` no longer hangs 60 s on every Linux run**
  (noogram/cosmon#31). Its liveness check shelled out to `kill -0 -<pgid>`,
  which procps `kill` parses as an option and answers 0 unconditionally — a
  tautology that panicked the test on Linux and took the CI `Test` job from
  326 s to five consecutive lost runners. The test now proves its claim
  behaviourally: a live bystander in the killed group must die of SIGKILL, and
  the detached backstop's marker must still appear.
- **`cosmon-remote` presents the OIDC `id_token` as bearer and requests
  `openid`** (noogram/cosmon#27, contributed against by @jdthaler's report).
  Bearer selection requires usable identity claims, the login report names the
  identity the token carries, and the rpp-adapter pins the audience on every
  enforced binding projection.
- **The Telegram notify hook no longer mangles angle brackets** on bash ≥ 5.2,
  where an unescaped `&` in a replacement names the matched text; `kind: info`
  events render as prose instead of a raw JSON envelope.
- **`event-listener` 5.4.1 → 5.4.2**, closing RUSTSEC-2026-0221 (unsound
  `Send`/`Sync` on `StackSlot`).

### Added

- **A dispatch-latency profile** on `RUST_LOG=cosmon::dispatch=info`: one
  monotonic clock from `cs tackle` entry through preflight, model resolution,
  spawn, readiness, paste and delivery receipt. Measured on real dispatches, it
  attributes the remaining seconds — the dominant term is the per-dispatch
  model probe, not the harness boot.
- **Spore verdicts are two immutable rounds** — initial then confirmation —
  replacing the N-round convergence loop; a PASS with no paired counter-verdict
  is inadmissible, and the diff decides the lane.
- **The per-molecule journal is a projection of the ledger** — one writer, no
  second source of truth.
- **`cs config` fails the build on a knob no reader consumes.**
- **The release crossing exists as a primitive** (`scripts/release/crossing.sh`
  + `sign-and-push.sh`), with an end-to-end test that runs the whole crossing
  under a real signature.

### Changed

- `just quick` / `just gates` split the verification contract: every gate but
  the test suite in ~90 s, the whole contract before merging.
- `verify_deploy` compares build trees, never SHAs.
- `[project] trunk_branch` governs the merge destination.

## [0.4.1] — 2026-07-29

### Fixed

- **`cs tackle` running as root no longer leaves root-owned residue behind its
  own refusal** (noogram/cosmon#20, reported against v0.4.0). The root-spawn
  refusal preceded the worker session and the cognitive probe, but not the
  filesystem provisioning: it lived inside the claude spawn path, so one
  `sudo cs tackle` exited 1, spawned nothing, and still created root-owned
  `.claude.json`, `settings.json`, `.worktrees/`, `.git/config`,
  `.git/packed-refs`, `fleet.json` and `fleet.runtime.json`. After that single
  mistake the *documented* non-root dispatch died with `mkdir: Permission
  denied` on `.worktrees/` and the molecule timed out `pending`. The decision is
  now taken at the entry of `cs tackle`, above every write — a strictly stronger
  guarantee, with the typed token `root-spawn-refused:*` and the remedy text
  unchanged. Operators already bitten: `chown <uid>:<uid> <galaxy>/.worktrees`
  and remove the two config files (recipe in the container guide).
- **The typed refusal is recorded append-only.** A refused dispatch no longer
  creates an `events.jsonl` it found missing, since that would make the refusal
  the very thing it refuses.

### Changed

- ADR-166's "a refused dispatch leaves no trace on the filesystem" is now
  carried by a test that measures it. The old assertion checked the worktree's
  *owner*, which a refused dispatch never creates; the residue was the
  `.worktrees` **parent**, which nothing looked at. The new end-to-end test
  snapshots every path under the galaxy root and the Claude config home — owner,
  group and mode — and asserts the sets are identical.
- The container guide states the scope of its `unshare`/seccomp claim (which
  engine, which profile, which discriminating arm), names what the shared uid
  costs — any worker can write any sibling worker's worktree — carries a
  recovery recipe, and warns that the missing-prerequisite gate fires *before*
  the root-spawn refusal, so a reproduction without the adapter on `PATH`
  measures the wrong gate.
- The clause describing what the append-only rule changed is corrected. A
  molecule has **no event journal**: the `events.jsonl` that sometimes sits in a
  molecule directory is a diagnostic side-file (152 of the 164 present carry
  nothing but `adapter_pane_signature_checked`). Before this release the refusal
  record created that file, so a refused root dispatch manufactured the very
  artefact that made the refusal look recorded. The fleet ledger is the one
  journal, for refusals and for successful runs alike.

## [0.4.0] — 2026-07-29

**A release about controls that measure the property next to the one that
matters.** Three independent review rounds on noogram/cosmon#20 kept finding the
same shape at every layer — a check that measures *speaking* rather than
*being*, *presence* rather than *content*, the *label* rather than the
*property*. The committee integrity witnesses had zero production callers, so a
roster could claim two providers and be contradicted by nothing; the release
gate `CLAUDE.md` had ordered for months did not exist as a file; the credential
canary proved a hardcoded pattern matched a hardcoded string and never that the
tracked tree had been scanned. All three are now enforced by a tool at a
boundary, each pinned by a falsifier proven red on revert *and* a counterweight
proving the gate can still pass — because a gate proven only to fail is
indistinguishable from an outage.

Minor rather than patch: a supported invocation was **removed**. `cs` running as
root refuses to spawn a worker at all, with or without a demote target. Under
SemVer 0.x a patch is for backwards-compatible fixes, and a user upgrading
0.3.0 → 0.3.1 does not expect their dispatch to start being refused.

### Removed

- **The root → uid demote path.** `cs` running as root with `COSMON_WORKER_UID`
  set no longer demotes a worker; it declines before it writes anything. There
  is no flag to restore the old behaviour, because what made it work was handing
  the worker the repository's shared object store — see the Security entry
  below, and [ADR-166](docs/adr/166-the-root-to-uid-demote-path-is-refused.md).
  The nominal pilot (`cs` running as the same non-root uid its workers run as)
  is unaffected and needs no hand-over at all.

### Security

- **A root dispatcher no longer demotes a worker to another uid — it
  refuses.** Making a demotion work means handing the worker the repository's
  *shared* git object store and *shared* `refs/heads`, because a linked
  worktree commits through both and git offers no way to delegate one branch
  or one object. Any grant large enough to let a worker commit is therefore
  large enough to let it rewrite a sibling molecule's branch and delete an
  object that molecule's history depends on — reproduced at uid 10001 in a
  Linux container and again at uid 501 on macOS. After three narrowings of the
  hand-over in two days, the fourth move is to close the path: `cs` running as
  root with `COSMON_WORKER_UID` set now declines before it writes anything —
  no consent pre-grant, no `chown`, no process — with a typed refusal
  (`root-spawn-refused:demote-shares-repository-storage`) naming the uid to run
  as and pointing at the container guide. The nominal pilot, `cs` running as
  the same non-root uid its workers run as, is unaffected and is what the
  refusal points at; it needs no hand-over at all. The bounded design that
  would make demotion safe — per-worker refs and objects over a read-only
  shared store, with `cs done` fetching rather than merging in place — is a
  different worktree lifecycle and is named, not half-built. See
  [ADR-166](docs/adr/166-the-root-to-uid-demote-path-is-refused.md), which
  supersedes ADR-165 §2.
- **`cs resurrect` rolls back its dispatch ledger when the promotion to
  `Running` fails.** Between `commit_dispatch` (which registers an Active
  worker and emits `WorkerSpawned`) and the spawn itself, two fallible steps
  returned early without undoing those writes — a worker the fleet believed
  live and no process anywhere. `cs tackle` rolled this back at both of its
  exits; this door was missed.

- **`cs-api` now enforces its own bind rule instead of documenting it, and
  no longer grants every browser origin.** The daemon has no
  authentication and one of its routes, `POST /molecules/{id}/tackle`,
  spawns a worker — so the listening address is the only access-control
  boundary the process has. It was a plain pass-through: the "run behind
  Tailscale" caveat lived in a Rust doc comment while `docs/guides/ios-pilot.md`,
  `apps/ios-pilot/README.md`, the in-app settings footer and the LaunchAgent
  installer all instructed an all-interfaces bind. A reader of the guide
  never met the caveat.

  `0.0.0.0` / `::` is now refused outright, with no flag to override it: it
  does not name a network, it names every interface the host has now or
  acquires later, so the exposure cannot be determined and we fail closed.
  Any other non-loopback address requires the explicit
  `--i-know-this-exposes-an-unauthenticated-api`, whose help text states
  what it opens; the admitted address is a value only the check can
  construct, so an unvalidated bind is not a reachable state. CORS defaults
  to emitting no headers at all — the Mac and iOS pilots are native clients
  that never send an `Origin` — and `--allow-web-origin <ORIGIN>` names
  origins explicitly (exact match, repeatable, `*` refused by name). Every
  document that taught the old gesture now teaches the safe one, beside the
  refusal messages the reader will actually see. Ratified as
  `docs/architectural-invariants.md` §8z; the authentication gap that
  remains is stated, with its decided shape, in `crates/cosmon-api/README.md`.

- **The credential canary now proves every rule, through the engine that
  actually scans.** It asserted a *second hardcoded copy* of the github-token
  regex against a loose file with plain `grep` — so breaking the real rule,
  deleting it, or breaking `git grep` still printed PASS. Every rule is now
  driven through the same `git grep -nIE` path as the scan, against a
  shape-only synthetic must-hit keyed by rule name, with coverage checked in
  both directions. It found a live defect on its first run: a pattern beginning
  with `-` is parsed by git as an *option*, so **both PEM rules had never run**
  — git errored, stderr went to `/dev/null`, and the rules reported clean on a
  tree they never searched. Fixed with `-e` at all three call sites.

- **A `.publish-allow` sidecar is a waiver again, not a whole-file exclusion in
  costume.** Its only conditions were "non-empty" and "tracked", and it was
  consulted before the rule was known — so one byte in `x.pem.publish-allow`
  waived *every* credential rule on `x.pem` forever. Four conditions now:
  comment-less formats only (everything else takes the inline marker), a reason
  matching the opt-out shape, the two key-shaped rules only, and a required
  `publish-allow-blob: <sha>` pinning the waived content, so a waiver written
  for a synthetic test key does not inherit whatever bytes land at that path
  next. The canary's `CRED_EXCLUDE` list is pinned to a reviewed literal, so a
  new blind spot costs two edits in one diff.

- **The worker-prompt attribution directive no longer discloses the shape of
  what it protects.** It ended by declaring that a particular operator
  affiliation was private and would never appear in an artifact — a public
  statement that a specific fact is withheld discloses that the fact exists, and
  it leaked twice over, since the string is published source *and* resident in
  every worker's context. Replaced by the positive rule that was doing the real
  work: use the configured public name and no other. The test now asserts the
  property (no withheld-category vocabulary, closure intact) rather than the
  removed sentence verbatim, so a rewording that reintroduces the leak fails in
  CI rather than in review.

### Added

- **`scripts/publish.sh --check` — the release gate `CLAUDE.md` had always
  ordered, and which had never existed.** `git log --all -- scripts/publish.sh`
  was empty: the file was not deleted, it was never written, so the line
  guarding the public projection named a property nobody measured.
  `docs/RELEASE-CHECKLIST.md` step 2 ordered the same absent command. On a tree
  where every existing gate reported PASS it found four violations of "machine
  paths must never be tracked" on main — including a tracked **symlink** to an
  absolute machine path, invisible to every content scan in the tree because
  `git grep` does not open symlink blobs. It is the structural subset decidable
  from a bare clone with git and `python3` and nothing else, which is what lets
  it fail the build where `release-checklist.sh` and `confidentiality-lint.sh`
  honestly report PEND. Per-line waivers only, via the inline marker
  `publish: allow — <reason>`.

- **`cs peek` has a door to its own glyph vocabulary.** The TUI renders a
  lifecycle pastille, a whisper bubble, a temperature, a three-signal step cell,
  a trust bar and an energy bar, and nowhere could an operator look up what any
  of them meant — the `?` overlay documented keybindings only, and `man cs` and
  the handbook said nothing. Page 2 of the `?` overlay is now a glyph legend,
  reached with Tab, scrollable with `j`/`k` and PgUp/PgDn; both pages scroll
  rather than truncate, because hiding half an answer is not a fix for
  illegibility. Every entry is **derived** — it calls the same renderer the
  table calls and iterates the same exhaustive `ALL` lists — and a test fails
  when a glyph exists in a renderer and not in the legend. A stale legend is
  worse than none, because it is believed.

- **`cs reconcile --check` enforces the committee integrity witnesses, which
  until now had zero production callers.** Witness (1) — provider diversity —
  and witness (2) — the seat's durable adversarial contract — were enforced only
  by a worker reading a recipe, so a roster that skipped the check was
  contradicted by nothing. The gate is now a tool at the CLI boundary, and every
  axis it measures is *derived* rather than declared: a seat's provider family
  is re-resolved from the `[adapters.<name>]` section it sits on (a declaration
  that does not survive resolution is refused by name, and a seat naming no
  adapter is refused as unresolvable rather than skipped); the `injected` flag
  is read off the seat's own directory rather than hand-set; the seat's posture
  file is **read**, not merely counted, and checked against the contract version
  and hash its roster entry declares. Committee-hood itself is resolved from the
  molecule's recorded `formula_id`, so a convener who writes no artefact at all
  is still inspected. Terminal molecules are printed in full as HISTORICAL and
  do not fail the gate — a permanently red gate over a committee that can no
  longer write a roster is an outage wearing a control's clothes.

- **`cs tackle` refuses an incoherent `(adapter, model)` pair before it spends
  anything.** Measured 2026-07-28: `cs tackle <seat> --adapter codex` with
  `ANTHROPIC_MODEL=claude-opus-5` in the dispatching shell resolved, dispatched,
  was rejected by codex at launch with an HTTP 400, and left a floor-bearing
  seat sitting mute at a prompt — indistinguishable from a provider outage.
  Earlier seats' `model-selection.json` recorded `"outcome":"available"` for
  that pair: the probe had measured that an id *resolves*, not that the pair is
  *legal*, and reported a positive for what it never checked. The verdict is now
  derived from the same resolution the provider-diversity floor already uses
  (base_url → adapter lineage vs. model-id prefix), so a new `gpt-*` or
  `claude-*` needs no edit and there is no allowlist to rot. Anything not
  resolvable to a named vendor — a local endpoint, an undeclared adapter, an
  unrecognised id — returns `NotChecked` and is **not** refused, because
  refusing on the unknown would break every self-hosted endpoint. The claude
  probe's trail now stamps a `probe_scope` naming what it did not check.

- **A verdict's `confirmed → CLEAN` mapping is read through a required
  `mechanism_polarity`.** Three formula files stated the mapping
  unconditionally, twenty-five lines under a definition whose own example is the
  opposite row — so a seat that *reproduced* a defect would have been filed
  CLEAN. `confirmed` means the stated mechanism holds; whether that is good news
  depends on what the mechanism claimed, which the reader cannot infer. The
  polarity is now load-bearing rather than declarative: `cs reconcile --check`
  refuses a missing polarity and an off-table triple, and the falsifier is
  site-granular — it requires the condition in the same paragraph a reader
  consumes, not merely somewhere in the flattened file.

- **A live-worker container bench (`arm A`) that drives a real mission past the
  credential gate,** with the verdict made honest: it refuses to build from a
  dirty tree, and re-samples HEAD and the porcelain status *after* `docker
  build` so that an edit or a commit landing mid-build is caught. The engine is
  resolved in one place (`scripts/lib/bench-engine.sh`) on a dedicated
  `cosmon-bench` colima profile; an unreachable engine is INCONCLUSIVE (exit 2)
  with the exact `colima start` line, never a silent fallback to another
  context. New: `scripts/container-engine-posture.sh`,
  `scripts/lib/source-provenance.sh` and its test.

### Changed

- **The worker briefing is a protocol, not a jailbreak.** The closing blocks of
  the dispatch prompt were written in the grammar of a prompt injection — a
  `NON-NEGOTIABLE` banner, "This is physics, not politeness", a `DO NOT — These
  are violations` list, and the claim that `cs complete` was the *only* valid
  way to end. Two observed costs, neither hypothetical: the operator read a live
  worker pane and asked whether prompts had been injected into a running
  molecule — they were reading our own briefing, and a control the system's
  owner mistakes for an attack on their own machine spends trust on every
  inspection. And a task refused the ordered exit, correctly: it finished its
  deliverable, found the real state did not support the transition, and declined
  to fabricate one — right on the substance, and left `running` with the work
  done, because the prompt put a good judgement in conflict with a blanket order
  and offered no third door. The anti-stall property is load-bearing and is
  kept, carried now by explanation and by a named third door (`cs note` +
  `cs collapse` with a reason kind) instead of by coercion.

- **`cs tackle` reads the built-in dispatch registry instead of re-typing it.**
  It composed its registry from a literal `vec![…]` of the same ten names
  `spawn_seam::built_in_adapter_names` already held. A second inventory is a
  first inventory that will one day disagree — and the committee roster gate
  measures a seat's adapter against the canonical list, so a name in the literal
  only would dispatch and be unrosterable. Behaviour is unchanged; there is now
  one list to add to.

- **A seat is rostered when its adapter *resolves*, not when it has a TOML
  section.** The gate refused any seat with no `[adapters.<name>]` section —
  the property next to the one that matters, since `codex`, `claude`, `aider`
  and `opencode` all dispatch with no section at all. In a galaxy whose only
  non-generator family is reached through codex, the diversity gate refused the
  sole provider that would have supplied the diversity, and no jury could be
  seated. Worse, the remedy the old message prescribed was a fiction: codex has
  no `base_url` and no `api_key_env`, so any section written to satisfy the gate
  is unverifiable against the real dispatch path. Adapters are now measured on
  two separate questions with two different sentences — *dispatchable* (in the
  canonical registry or the TOML inventory; a name in neither is a ghost) and
  *resolvable* (the section declares an endpoint, or the name carries a vendor
  lineage cosmon knows). A registry-only adapter's family comes from what cosmon
  knows about the binary it spawns, never from the seat's label.

### Fixed

- **A demoted worker can now commit, because cosmon hands over the git
  plumbing its worktree writes through — and the resource set is derived
  in one place instead of remembered at two.** A linked worktree keeps
  almost nothing inside itself: its HEAD, index and reflog live in
  `<repo>/.git/worktrees/<name>`, and the objects and refs a commit
  creates live in the repository's common dir. cosmon chowned the
  worktree and stopped there, so a worker demoted to a non-root uid could
  edit every file and record none of them — measured by an external
  tester, two dispatches, both artefacts written, neither committed, both
  molecules left short of a terminal state. Git additionally refused the
  repository as *dubious ownership*, because it resolves the gitdir to a
  directory owned by somebody else.

  Both git roots are now read out of git's own on-disk pointers (the
  worktree's `.git` file names the gitdir; the gitdir's `commondir` names
  the common dir) rather than assembled from a path template, then
  transferred and judged like every other resource. Ownership is the fix,
  so `safe.directory` is deliberately **not** configured for the worker:
  the *dubious ownership* message was a true report of a real defect, and
  granting the exemption there would have silenced the diagnosis and left
  the `EACCES` underneath it. It is configured for the **dispatcher**
  instead, scoped to the paths just transferred, because handing the
  plumbing to the worker is what makes root the foreign uid.

  The third leaf of the same class, so the class is what was closed. The
  repair list and the judge list were already required to be one list;
  what kept failing was the *list*. Every path is now derived from
  primitives a caller genuinely knows, by a single constructor both demote
  call sites must use — the struct is `#[non_exhaustive]`, so no other
  crate can build one by hand and under-declare — and the transfer is
  recursive over roots rather than exhaustive over leaves, which is what
  makes the enumeration's incompleteness survivable. Walking it also
  surfaced a resource nobody had named: the adapter binary itself, which a
  demoted worker cannot exec when the installer put it under a `0700`
  home. It is now judged (and, alone among them, never repaired — the
  binary belongs to whoever installed it) so the operator is told what to
  `chmod` instead of watching a silent pane. What remains unknowable is
  stated where the next reader meets it, in the port's module docs.

- **`cs tackle` now records a dispatch before it spawns one, so a worker
  cosmon started can no longer be invisible to cosmon.** A tmux worker is
  committed to the operating system the instant it is spawned and outlives
  the process that started it — but the molecule's `Running` flip, its
  worker binding and its `WorkerSpawned` event were written only *after*
  the whole readiness pipeline: the model preflight probe, the 30s liveness
  wait, the briefing paste, the submit-confirmation window. On a healthy
  dispatch that is ~98 seconds during which a live, paid worker exists and
  nothing on disk says so.

  Anything that ended the dispatcher inside that window — a `^C` on a
  dispatch that looked hung, a closed terminal, a host suspend — left a
  worker with no ledger entry at all: `cs observe` read `pending`,
  `cs patrol` could not see it (its orphan scan looks for the opposite
  shape), and the worker's own `cs evolve` was refused, so a molecule could
  finish real committed work with an empty step list. Six of this fleet's
  240 completed molecules carried that signature.

  A filesystem ledger and a `fork`/`exec` cannot commit atomically, so the
  record moves to the near side of the spawn and is rolled back if the spawn
  fails: a molecule marked `Running` with no worker is the shape the orphan
  scan already heals, while a worker with no molecule is visible to nothing.
  The ordering is enforced by the compiler — the commit returns a token that
  the spawn requires and checks against its own `(molecule, worker)` pair,
  so spawn-then-record no longer compiles. `cs resurrect` had the identical
  shape and is fixed with it. Both `cs patrol` and `cs tackle` now name a
  live session whose molecule does not admit being dispatched, telling the
  operator where the worker's commits are. Ratified as
  `docs/architectural-invariants.md` §8ab.

- **`cs tackle` no longer asks a question, and cosmon's first-run consent
  prompt no longer hangs a captured dispatch.** In a container, a dispatch
  with a valid credential ran its full 240s timeout and spawned nothing: `cs
  tackle` had printed the French `opt-in-share` prompt into a stdout the
  orchestrator was capturing, on a stdin that was still the terminal
  inherited from `docker exec -it`. No keystroke could arrive and no output
  could warn. The guard tested `stdin().is_terminal()` — "is a terminal
  attached?" — instead of "can a human see this and answer it?".

  Two repairs, because the predicate and the placement were both wrong. A
  first-run question is now asked only when **stdin and stdout are both
  terminals**; a captured stdout auto-declines down the identical path a
  missing TTY already took, and says so on stderr with the explicit remedy.
  And the question left the dispatch path entirely — it now fires from `cs
  init` (suppressed under `--json`) and from `cs opt-in-share` invoked alone.
  Nothing on the dispatch path may block on a human.

  The regression test allocates a real pty and asserts the process
  *terminates*, not what it wrote: the broken build records the same
  decline, just after somebody types into a terminal nobody is watching.
  [ADR-163](docs/adr/163-a-question-may-only-be-asked-where-an-answer-can-arrive.md),
  architectural invariant §8w. Fifth door of noogram/cosmon#20.

- **The `claude` adapter no longer stalls on Claude Code 2.1.220's first-run
  wizard.** The installer moved from 2.1.218 to 2.1.220 under us; the new build
  opens a syntax-theme wizard on any config directory that has never completed
  onboarding, and cosmon refused the dispatch — correctly and loudly, but the
  work still did not happen. `cosmon-transport`'s consent pre-grant now writes
  `hasCompletedOnboarding` alongside folder trust, so the wizard is never
  rendered. Reported by `@jdthaler` on noogram/cosmon#20.

  The pre-grant is **re-asserted before every spawn**, and that is the load-
  bearing part rather than an implementation detail: Claude Code rewrites
  `.claude.json` wholesale from its own in-memory state when a session ends and
  drops keys the running build does not recognise, so a grant written once can
  be erased by the very worker it allowed to launch. There is no durable place
  to put it — `settings.json` survives but is not honoured for this key, and
  `claude config set -g` is gone. Measured rather than inferred:
  `docs/benches/claude-2.1.220-consent-durability.md`.

  The acceptance criterion is two *consecutive* dispatches on a pristine config
  directory with no human in between — a single green dispatch cannot tell a
  re-asserted grant from a run-once one. Pinned by
  `cargo test -p cosmon-transport --test claude_consent_live -- --ignored`
  against the installed binary, and by arm F of the container bench.

  The dispatch boundary (§8v, ADR-162) is unchanged: no marker was added, the
  classifier still refuses any screen it cannot certify, and cosmon does not
  answer onboarding — it declines to summon it.

- **Every tmux worker pane gets a UTF-8 locale floor, so its TUI is legible.**
  cosmon drives its worker as a TUI under tmux and never ensured the locale that
  makes that TUI readable. In a container with no locale configured — `LANG`
  unset, `LC_CTYPE=POSIX`, the ordinary default of a slim Debian base — tmux
  draws every non-ASCII glyph as `_` and corrupts text with it. Nothing on the
  screen points at the locale, so an external tester reports it as a cosmon
  rendering bug. Measured in `debian:bookworm-slim` + tmux 3.3a: `capture-pane`
  returns the bytes **intact**, so the application writes correct UTF-8 and tmux
  stores it correctly — the substitution happens when tmux draws to a *client*
  whose locale does not declare UTF-8, and a server started under `C.UTF-8`
  still renders `_` to a POSIX client. Both halves are addressed for what they
  are: the pane cosmon spawns is prefixed with `LC_ALL=<utf8 locale>` at the one
  choke point covering all four tmux-backed adapters (and only when neither
  `LC_ALL`, `LC_CTYPE` nor `LANG` already declares UTF-8, so a UTF-8 host keeps
  a byte-identical command), and the attach line `cs tackle` and `cs resurrect`
  print carries it too, because that attach is one cosmon does not spawn.

- **The realized-model observer honours `CLAUDE_CONFIG_DIR`, and says so when
  the seam is broken.** It resolved the Claude session-log root as
  `$HOME/.claude/projects` unconditionally — while cosmon itself exports
  `CLAUDE_CONFIG_DIR` onto every worker it spawns, for multi-account routing and
  for every container deployment its own guide instructs. Measured 2026-07-27 in
  Colima: `$HOME/.claude` absent, the worker's log under
  `/home/cosmon-worker/.claude-fz/projects/…`, and a detached watcher ticking
  every second for 7m34s emitting nothing. `cs peek` rendered
  `realized: … (pending)` for the whole life of every worker, so the
  pin-versus-realized comparison that exists to close the `/model`-hack leak
  could never fire. It worked on the operator's Mac only because an interactive
  install had created `~/.claude` by accident. Fixed at the resolver rather than
  patched at the watcher, and `cs tackle` now carries the config dir forward,
  because a detached watcher cannot re-derive a routed account directory from
  its own environment.

- **A test that skips is no longer a test that passes.** `claude_consent_live`
  printed `SKIP: not runnable` and returned when tmux or `claude` was absent,
  reporting `0 passed; 0 failed; 1 ignored` and a green line — for the only
  automated check of the startup-consent pre-grant. Reaching that code means
  somebody passed `--ignored` and asked for the measurement, so it now fails
  loudly with what is missing. In the same pass: the confidentiality banlist
  states the rule it actually enforces rather than a wider one it does not, and
  a recorded measurement that a later reader had inherited as a falsifier now
  says in its own text that it contains zero assertions.

- **The doc gate is unbroken, and an empty journal now says it is empty.** A
  public doc comment linked to a private const — it compiled, linted and tested
  green and failed only `RUSTDOCFLAGS='-D warnings' cargo doc`, which is why
  that gate is not redundant with the others. Journals now carry a header
  stating how many events they hold and what an event would look like:
  previously an empty journal was pruned by the zero-byte rule and its
  emptiness — the whole measurement — vanished.

- **A jury that records its own compromise may not certify, and a diversity
  floor with no slack is a single point of failure at any floor height.** The
  floor was measured on the roster as planned rather than on the jury that
  actually sat, and an endpoint nobody observed was counted as an endpoint that
  matched. Both are re-checked against what was delivered.

### Documentation

- **Do not put a cosmon galaxy on a `-v` bind mount from macOS.** Those mounts
  are virtiofs, where `chown` is a silent no-op: cosmon chowns the worktree, the
  filesystem ignores it, and the ownership preflight refuses the dispatch with
  no sign that anything failed. Put the project on container-local storage.
  Contributed by `@jdthaler`, who lost an afternoon to it —
  `docs/guides/claude-worker-in-a-container.md`. (Corrected 2026-07-27: this
  entry said "Under Docker Desktop". Measured on both engines, Docker Desktop
  is the one where the mount *honours* `chown` and the whole failure mode is
  invisible; the silent no-op reproduces on colima/virtiofs.)

- **The container benches moved to a dedicated colima profile, and the fidelity
  claim they carried was measured instead of asserted.** The three benches
  pinned `--context desktop-linux` under a header stating that this was the
  external tester's engine and that "a colima context runs an Ubuntu kernel with
  a DIFFERENT user-namespace posture and is NOT faithful". On 2026-07-27 the
  tester corrected his own earlier description, unprompted: his bed is Colima
  (Lima-based), Ubuntu 24.04.4 LTS, aarch64. The benches had been pinned away
  from his real engine by a comment written to keep them faithful to it.

  Both engines were then probed rather than re-guessed. Under the default
  seccomp profile, `unshare` as a non-root uid is **blocked** on colima and
  **succeeds** on Docker Desktop; a bind-mounted `chown` is **silently ignored**
  on colima and **honoured** on Docker Desktop. So the old default reproduced
  neither of the tester's two standing findings — it was not merely mislabelled,
  it was the engine that cannot see them. The block is attributed to the default
  seccomp profile by flipping `--security-opt seccomp=unconfined` and nothing
  else, not by reading a sysctl: on colima both userns sysctls are permissive
  and `unshare` is refused anyway.

  The engine is now resolved in one place (`scripts/lib/bench-engine.sh`) on the
  profile `cosmon-bench`, which belongs to the benches and to nothing else. An
  unreachable engine is **INCONCLUSIVE (exit 2)** with the exact `colima start`
  line — never a silent fallback to another context. New:
  `scripts/container-engine-posture.sh` and
  `docs/benches/engine-fidelity-2026-07-27.md`. Captures taken on the old engine
  are kept as produced and carry a dated note naming the engine they were
  actually taken on.

- **ADR-153 records that "pure and unit-tested" was never enforcement.** Its
  Consequences claimed the dual witness was closed because the logic was pure
  and unit-tested. It was — and it enforced nothing, because the kernel had no
  production callers, so every predicate passed while a witness-failing roster
  was contradicted by no tool. The amendment names the gap, points at the
  boundary that now refuses, and keeps the original rejection of a `cs
  committee` verb intact: no surface was added. A second amendment records the
  identical shape found one layer down, on what seats *emit* rather than on who
  sits, so the next occurrence is recognised rather than rediscovered.

- **The convener owes each seat a distinct entry point, named in the brief.**
  Distinct provider families stop two judges making the same mistake about what
  they read; they do not stop them reading the same place, and the artefact
  hands every seat the same itinerary. Deliberately a briefing rule and not a
  scoring one — folding entry-point distinctness into the diversity floor would
  add a second self-attested axis, which is the defect class the round existed
  to close.

- **Release-bound guidance no longer teaches a mechanism that does not exist and
  is also the named anti-pattern.** `docs/architecture-baseline.md` told
  vendoring galaxies to extend "the path allowlist via `.publish-allowlist.txt`"
  — a file with zero hits in `git ls-files` and zero readers in `scripts/`,
  describing a pathspec exclusion that `publish.sh` forbids by name two hundred
  lines away. This was the worst of the three to leave standing, because
  vendoring guidance is copied into other repositories as a recommendation. A
  guard now asserts on *this* repository, so the prohibition cannot quietly
  become a recommendation again.

- **The two demote integration suites carry a banner saying they exercise a
  dormant path,** now that the root → uid demote path is refused, and the CLI
  pre-flight tests point at the refusal the funnel actually reaches.

- **`cs peek`'s how-to points at the new glyph legend,** and the
  `converge-clean-room` formula is available at galaxy level rather than only
  inside the galaxy that first wrote it.

## [0.3.0] — 2026-07-24

**Root-container safety, hardened by an adversarial double-model review.** The
`claude` adapter now runs unattended in a root Linux container end-to-end — and
it does so by *never running the cognitive agent as root* rather than by
bypassing the guard. The whole #20/#21 surface (reported by @jdthaler) went
through three rounds of adversarial clean-room review (Claude + codex,
context-zero, refutation mandate); every defect the reviewers found — including
two they reproduced as live exploits — is closed and pinned by a test that
replays the exact attack. A three-pass end-to-end gate in the faithful Claude
Code 2.1.218 container confirmed the briefing-submit path before release.

Naming convention: issues are cited by their project number and reporters by
GitHub handle (**@jdthaler** for #20/#21). The reporter behind #22–#26 asked to
remain unnamed; those are cited by issue number only.

### Security

- **Non-root worker demotion (#20, contract-20A).** On a root dispatcher, cosmon
  demotes the cognitive worker to a non-root uid before `exec`, or refuses
  *before any live worker exists* — it never spawns a live agent with root's
  blast radius. The demotion is composed at the binary token (immune to hostile
  `CLAUDE_CONFIG_DIR` / `ANTHROPIC_MODEL` / state-path values that could
  otherwise divert a string-splice), runs its model preflight *as the demoted
  identity* (never as root), and refuses cleanly with a typed error if the
  target uid cannot use its config / worktree / state (instead of a silent
  mid-run `EACCES`).
- **Out-of-worktree write grant (#20, facet B).** The molecule state a worker
  must write on `cs evolve` / `cs complete` lives outside its worktree; under
  `acceptEdits` that write used to prompt and hang an unattended worker. The
  adapter now declares that directory writable (`--add-dir`) and grants
  `Bash`/`Edit`/`Write`, on the `cs tackle`, `cs thaw`, and patrol-respawn paths
  alike.
- **Spore output containment (ADR-161 + hardening).** Germinated spore nodes
  write *only* under a canonicalized, symlink-safe run home; parent-component
  symlinks, regular files planted at a node home, `..` / absolute / `a/b` node
  ids, and case-collisions are all refused. Containment is real
  (canonicalization), not lexical.
- **Spore validation fail-closes** when an emergent loop parameter (e.g.
  `max_rounds`) exceeds its own sealed `[bounds]` ceiling — an emergent value
  can no longer overrun its foaming ceiling.
- **Seal-cache honesty.** An unreadable-but-declared spore config no longer
  collides with a module-only verification-cache entry and reports `verified`;
  it fails closed.
- **Turn-scoped local publishing.** The local-adapter output publisher commits
  *only* what the current turn produced — never pre-existing untracked files,
  branch diffs, or an operator's own uncommitted hunks in a file the worker also
  touched (surfaced-not-committed on that collision).

### Added

- **`cosmon-dev` spore** — the repository's first development spore: red-first
  container reproduction (a failing test as the validation gate), clean-room LLM
  judging, and a provider-diverse cross-model committee. TLA+-sealed
  (`spore.tla` / `spore.cfg`, TLC-verified).
- **Run-scoped output home for germinated nodes (ADR-161).** Germination now
  computes a gitignored, germination-id-namespaced home under the state store —
  `.cosmon/state/spore-runs/<germination-id>/<node>/` — and hands it to every
  node, exactly as molecule state already lives under `.cosmon/state/`. Gate
  records get a defined home instead of polluting the reusable spore definition
  or the repo root.
- **Opt-in `cs run --adapter <name>`.** The resident scheduler can be explicitly
  directed to dispatch pin-less molecules (static and dynamically-nucleated
  children) to a named adapter — without weakening the deliberate
  anti-silent-spend floor (with no flag, pin-less stays `local`).
- **Real TLC seal-gate verification.** `cs spore run` now *verifies* a spore seal
  when a TLC toolchain is available (no `--allow-unchecked-seal` needed), and
  stays fail-closed when it is not.
- **Selectable local model.** The local (Ollama) adapter's model is now
  discoverable and selectable rather than hard-pinned to `qwen3:8b` (#23).
- **Always-on polymer trace sidecar** (+ script and test).
- **codex adapter self-close** — the out-of-worktree cosmon state dir is
  declared writable so a codex worker can persist its audit and complete.

### Fixed

- **Adapter resolution under the resident (#21).** The resident used to silently
  floor every pin-less molecule to the local adapter, making a documented
  `[adapters.default]` workaround a no-op. It now delegates to the same canonical
  resolution chain as `cs tackle` and honours `COSMON_DEFAULT_ADAPTER`; the two
  paths' agreement is documented.
- **First-run on a freshly `git init`'d repo (#22).** A newcomer following the
  getting-started path — `cs init` → `git init` → `cs demo` — hit
  `cs: git branch feat/<mol> failed: fatal: not a valid object name: 'main'`: a
  fresh `git init` leaves an *unborn HEAD*, so cutting the feature branch
  aborted, and the only workaround was an undocumented manual
  `git commit --allow-empty`. `cs tackle` / `cs demo` now detect the unborn-HEAD
  case and seed a single `cosmon: initial commit` before branching, so the
  documented path succeeds with no hidden step. A repository that already has
  history is left byte-for-byte untouched — cosmon never fabricates a commit over
  existing work. `cs init`'s "Next steps" now states the step accurately.
- **Local-adapter output honesty (#24).** The local adapter's `synthesis.md` used
  to report a fabricated `Code written to .../.cosmon/state/.../main.rs` path
  while the file was actually in the worktree; it now reports the real location.
- **Local-adapter first-run friction (#25).** On the documented demo, the local
  worker left its output uncommitted so `cs done` refused without `--force`, and
  the result was buried in `.worktrees/`; the worker now commits its own output
  and the location is surfaced, so the demo completes end-to-end with no
  `--force`.
- **Briefing submit reliability (#26).** On Claude Code v2.1.218 the briefing's
  submit keystroke did not reliably fire, leaving a worker idle with the briefing
  queued (periodic nudges had been masking it). The submit is now a raw carriage
  return with a bounded in-band retry; the guarantee is one window wide. A
  *durable* cross-process backstop beyond that window is tracked as **#26 (open)**
  for a future release; the honest limit is documented in the code.
- **`cs nucleate --adapter` test gate** — allowlisted for the thin-cli parity
  check; the harness's git-identity handling is now environment-independent.

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

### Fixed: the local/Ollama adapter booked no-op-with-chatter missions as completed (Jesse #4)

- The first fix guarded only the *empty* case: a model that produced literally
  nothing failed loudly. A clean-room re-audit reproduced the deeper hole on
  the installed `cs 0.2.2` — the completion criterion was "non-empty synthesis
  OR a worktree deliverable", and **any chat model emits some text**. A
  task-work brief of "reply with the single word hello, create no files" reached
  `completed` with energy untouched, an empty branch, and a synthesis body of
  just "hello." A second run dumped a fabricated `<tool_result>` transcript as
  raw text into the synthesis — a plausible-looking success with zero work
  behind it. A synthesis body is a *thin proxy*, not proof of work.
- Completion now requires a real **work product**, not chatter. On the local /
  ollama floor a turn that leaves an empty branch (no file created or edited) is
  refused: the molecule lands not-completed — recoverable and re-tacklable —
  instead of a silent green checkmark. A genuine *text* deliverable is not
  broken: it satisfies the floor by writing its answer to a file (the formula's
  RESULT CONTRACT, e.g. `result.md`), which the local loop lands in the worktree
  and the guard then counts. A mission that refuses to produce any file is
  chatter, not a task-work deliverable.
- The acceptance-artifact contract is now a first-class primitive. Formulas
  declare machine-checkable `acceptance_artifacts` per step, and the runtime
  refuses completion when a declared artifact is **absent, empty, outside the
  molecule directory, or older than the step start** — enforced not only on the
  per-step `cs evolve` gate but also on the in-process / detached-local
  completion path, which previously never checked it. Enforcement fires only
  where declared, so text-only formulas are unaffected.
- Also corrected a misleading log line: the `local` adapter runs as a *detached*
  worker, not the in-process ADR-100 Direct-API model its completion message
  claimed.
- The worker's **execution protocol is now adapter-aware** — partly addressing
  the issue's second clause ("worker briefing assumes a full coding agent"). A
  `local` / `ollama` / `llama-cpp` / `llama` adapter is a small model running on
  the operator's own hardware with no shell, no git and no `cs` command; it used
  to receive the identical full-coding-agent contract as a Claude coding agent
  ("run all gates", "commit your changes", `cs evolve`, `cs complete`). Its
  protocol wrapper is now matched to what it can do — produce the declared
  deliverable, with cosmon driving the lifecycle transitions on its behalf; the
  Claude / external-CLI coding-agent briefing is unchanged. **This is not the
  whole fix, and we say so honestly:** a clean-room re-audit showed the *formula
  step text* (e.g. `task-work`'s "run all gates: build, test, ...") is itself
  coding-agent-shaped and still renders into the local prompt, contradicting the
  new protocol. Fully closing clause 2 needs adapter-capability gating of
  formulas (a cargo-gates mission should not dispatch to a chat-only model at
  all) - that, and the issue's docs-suggestion prong, are tracked follow-ups,
  not shipped here. The acceptance-artifact guard above still keeps a local
  *failure* honest regardless.

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
- **Completed for the local/Ollama adapter (the reporter's exact conditions).**
  A clean-room re-audit found the first pass only *partially* closed #3 for a
  detached local worker, with three residual defects — all now fixed:
  - The PID witness never survived in `state.json` for a local worker: it was
    written before the worker's process record was bound and then overwritten
    with `pid: none`, so the liveness check's PID axis was inert. The PID +
    start-time is now stamped on the record `cs tackle` actually persists.
  - `cs run` false-flagged and reset its own *live* (or `SIGSTOP`'d) local
    workers every recheck: a local worker has no tmux pane, yet the scan
    OR-combined the always-dead tmux axis. The liveness axis is now chosen by
    worker kind — a PID witness is authoritative and short-circuits; the tmux
    axis is never applied to a paneless local worker.
  - A re-dispatch re-resolved the model from the ambient environment instead
    of the molecule's original pin, so with `ANTHROPIC_MODEL` set the local
    adapter resolved to a non-served model and every re-dispatch was refused —
    the molecule stalled `pending` to the timeout (exit 124). The re-dispatch
    now carries the molecule's original adapter + model and strips ambient
    model env, so the floor stays the floor. New regression tests reproduce
    the live-worker false reset and the stuck-`pending` re-dispatch.

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

### Security: a spore bundle's advertised hash now covers its crew constitution (task-20260721-f939)

- `cs spore export` computes a content-addressed BLAKE3 bundle id, but the id
  covered `spore.toml` + formulas + seal and **not** the `fleet.toml` crew
  constitution shipped in the same bundle — so the crew could be altered
  without moving the advertised hash (an integrity gap: two bundles differing
  only in `fleet.toml` shared one id). The coverage set now includes
  `fleet.toml` and its `file:` includes, and `cs spore export` explicitly
  lists every covered file (human, `--json`, and ASTRA `spore:bundleFiles`) so
  an integrity audit can see what the id binds. Regression test closes the
  falsifier. This moves previously-advertised bundle ids by design.

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
