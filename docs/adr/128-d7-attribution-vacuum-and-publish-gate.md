# ADR-128 — D7 attribution vacuum and the publish-content gate

**Status:** Accepted
**Date:** 2026-06-18
**Parent deliberation:** `delib-20260617-62ff`
**Panel that converged on the verdict:** kahneman · shannon · turing · torvalds · godin
**Source galaxy:** flow-models (application site reporting a substrate pathology, cosmon-ward)
**Companion children:** `task-work` molecules wired under the parent deliberation (V0 gate, attribution primitive, V1 widening)

## Context

Across application galaxies, the cosmon agent fleet recurrently stamps **the
operator's confidential affiliation** — a private organization name that must
never appear in any external artifact — as the
producer/author of EXTERNAL-facing artifacts: README headers, site footers
(`footer.html`), site `index.html`, GitHub repo descriptions. In one mission the
flow-models fleet leaked the confidential name **four times**, each at a structural
attribution point, each caught and scrubbed by the operator *after the fact*. This is a
recurring **D7 confidentiality breach** (standing rule: *never name the confidential affiliation externally*),
and per-molecule "do not name the affiliation" guards demonstrably fail to prevent it.

The operator's smoking-gun hypothesis: the global identity context
(`~/.claude/CLAUDE.md`) opens by naming the operator and their confidential
affiliation, which
structurally collides with the D7 embargo; at attribution time, Type-1 associative recall
reaches for the operator's stated affiliation and the negative guard loses. Reinforcers
that raise the activation of the confidential-affiliation token: a formula references
`~/dev/projects/agent-templates/`, a `/srv/cosmon/noogram/` design galaxy exists, a
`~/dev/projects/noogram/` runtime exists, and the operator's git identity is
`you@noogram.dev`.

The five-persona deep-think panel (`delib-20260617-62ff`: kahneman, shannon, turing,
torvalds, godin) ran the question. This ADR ratifies the verdict and the systemic guard.

## Decision — root-cause verdict

The recurring leak is a **VACUUM, not a COLLISION** (panel unanimous, kahneman lead). The
operator's hypothesis is *confirmed in substance but sharpened in mechanism*:

- External boilerplate carries an **obligatory attribution slot** ("built by ___", footer,
  copyright line, repo description) that the model is conventionally forced to fill.
- No **authorized public name** is supplied, so Type-1 associative retrieval returns the
  **highest-activation associate** of "the entity that built this" — deterministically
  the operator's confidential affiliation, given the verbatim identity header plus the reinforcer paths.
- The negative guard "don't say the affiliation" **never enters the competition**: suppression
  (Wegner ironic-process / white-bear) requires holding the forbidden token *active* to
  scan against it, and it needs System-2 monitoring at exactly the lowest-effort moment
  (stamping boilerplate) where System 2 has checked out. Negation is not a
  Type-1-executable operation.

**Diagnostic signature proving vacuum-over-collision:** the leaks are **slot-locked** —
4/4 at structural attribution points, 0 in prose, 0 in internal artifacts. A pure
collision would leak *promiscuously*, weighted by token activation. It does not.

**Corollary (the fix's shape):** because the failure is a vacuum, the fix must be
**positive supply** (give the model the right name) backed by **deterministic detection**
(a hard gate at the publish boundary), **never negative suppression** (which is exactly
what the failing per-molecule guards already attempt).

## Decision — the systemic guard (defense-in-depth)

Four layers, sequenced. All in cosmon's perimeter except the flagged operator gesture.

1. **DETECTION — `[confidential_blocklist]` publish gate in `cs done` (V0, load-bearing).**
   Clone the *existing, shipped* ban-list machinery —
   `collect_remote_blocklist_violations` / `check_git_remote_blocklist` in
   `crates/cosmon-cli/src/cmd/done.rs` (≈L595/L636, wired at L802; config block
   `[git_remote_blocklist]` in `.cosmon/config.toml`). The new gate is a near-exact clone:
   same pure substring core, same all-violations-at-once report, same hard `Err`→abort,
   fed `fs::read_to_string` of a **narrow** `publish_globs` set of the merged tree instead
   of `git remote -v` output. Fires in `cs done` *before* the post-merge push/deploy hook.
   - **Hard abort, identical blocking semantics across all three regimes**
     (Inert/Propelled/Autonomous). Autonomous mode has no operator to scrub after the fact;
     an advisory-in-Autonomous gate abandons exactly the regime that needs it. Never
     advisory, never regime-downgradable (turing).
   - **Mandatory residual-risk statement in the error message.** The gate is *syntactic*:
     it blocks the literal name, registered aliases, the operator domain, and the operator
     email. It does **not** detect paraphrase, implication, encoded, or composed disclosure
     (Rice-theorem-adjacent undecidability — turing). The error text must say so, to avoid
     manufacturing false confidence.
   - **Trust boundary:** the worker *is* the untrusted oracle (it leaked 4×); the gate must
     live in the deterministic transactional core the worker cannot edit or persuade — never
     in the worker's self-report.

2. **PREVENTION — `[attribution]` block + `cs tackle` injection (root-cause reducer).** A
   canonical *positive* public attribution in `.cosmon/config.toml`, folded into the worker
   bootstrap prompt by `tackle_env::build_claude_command` so every worker has the right name
   in hand *before* it reaches an attribution slot. The public maker name is **Noogram**
   (open agent infrastructure / AI tooling), with a contactable address (not a bare
   anonymous footer, which is its own vacuum the next worker "helpfully" enriches). Without
   this, the gate fires constantly and workers thrash; with it, the vacuum is filled at the
   source. **Both halves are required** — the gate is the floor, the attribution is the cure.

3. **OPERATOR GESTURE (out of cosmon's perimeter — surfaced, not performed).** Rewrite the
   `~/.claude/CLAUDE.md` identity line so the embargo and the Noogram default travel *in the
   same breath* (godin's verbatim rewrite is in the synthesis). A worker must **not** edit
   the operator's global system config from a worktree (system-state change). This is an
   atomic question for the operator: *leave the fund name in the global identity line
   (cosmon masks externally at tackle-time) — recommended — or strip it globally?*

4. **V1 FOLLOW-UP — widen detection to the publish closure + adopt whitelist-codebook.**
   shannon's confirmed residual: the `you@noogram.dev` git author/committer identity is
   stamped into every commit, invisible to a file-content grep — the highest-probability
   provable leak past V0. V1 widens the surface (git author/committer, commit messages,
   package author fields, OG/meta tags) and inverts the approach from open-vocabulary
   blacklist (recall < 1, unfixable) to **closed-codebook whitelist validation** of
   attribution slots (any string in a slot ≠ the canonical codeword is a violation by
   construction, recall → 1 on enumerated slots).

## Options Considered

### Option 1 — Per-project D7 prose guards only (rejected — status quo)
Keep injecting "do not name the fund" notes per molecule.
- **Pros:** Zero new code; already in place.
- **Cons:** This is the *failing* mechanism. It is negative suppression at the wrong
  cognitive layer; it keeps the token warm; it leaked 4×/mission. Whack-a-mole the operator
  explicitly rejected.
- **Why rejected:** It does not work, by construction (kahneman). The whole deliberation
  exists because this option failed.

### Option 2 — Pure pre-publish blacklist grep, no positive attribution (rejected)
Ship only the `[confidential_blocklist]` gate; do not supply a default attribution.
- **Pros:** Simple; catches the realized literal-string failure class with certainty.
- **Cons:** Leaves the vacuum unfilled, so the model keeps *trying* to emit the fund name and
  the gate fires constantly — workers thrash, the gate becomes noise, and any
  paraphrase/metadata channel still escapes (shannon, turing). Detection without prevention
  treats a vacuum as a collision.
- **Why rejected:** Necessary but insufficient. The gate is the floor, not the cure.

### Option 3 — Worker self-certification ("I confirm no leak") (rejected)
Have the worker assert at completion that it did not leak.
- **Pros:** No new core machinery.
- **Cons:** The worker is the untrusted oracle that leaked 4×; asking it to vouch for its own
  output is self-reference with no fixed point of trust (turing). Worthless as a gate.
- **Why rejected:** Violates the trust-boundary invariant — a gate must live where the worker
  has no vote.

### Option 4 — Cosmon edits `~/.claude/CLAUDE.md` automatically (rejected)
Have a worker/`cs` command rewrite the operator's global identity line.
- **Pros:** Highest-leverage single edit (grounds every session before any cosmon machinery
  runs).
- **Cons:** Editing the operator's global system config from a worktree is a system-state
  change outside cosmon's perimeter (kill-switch territory under auto-pilot). Cosmon must not
  own that mutation.
- **Why rejected:** Out of perimeter. The *intent* is recovered legally by Option chosen
  (Decision §2 + §3): inject at `cs tackle`, and surface the global-line rewrite as an
  operator atomic question.

### Chosen — Defense-in-depth: positive attribution supply + deterministic publish gate (accepted)
Decision §1–§4 above. Prevention fills the vacuum at the source; detection is the
deterministic floor cloning the proven `cs done` blocklist precedent; the global-config
rewrite is surfaced as an operator gesture; V1 widens the surface and adopts the codebook.

## Consequences

- **Positive.** The realized failure class (literal fund name in README/footer/index) is
  closed with certainty by V0. The root cause (the vacuum) is removed by the attribution
  primitive. The fix reuses a proven, coherence-clean precedent — no new command, no daemon,
  no new state store. The doctrine is substrate-level and inherited by every galaxy via
  `.cosmon/config.toml`, not re-implemented per project (answers the operator's "no
  whack-a-mole" requirement).
- **Residual risk (named, not hidden).** Paraphrase, implication, encoded, and composed
  disclosure remain undecidable and uncaught by V0 — human review and narrow oracle scope
  remain the backstop for the semantic failure class. The git-author identity channel
  (`you@noogram.dev`) is uncaught until V1. The GitHub-description (`gh repo edit`) and
  deployed-URL surfaces cross boundaries cosmon does not invoke; V0 cannot gate them and must
  not pretend to (torvalds) — they are a named gap.
- **Coherence checklist (V0 gate):** stateless ✓ · idempotent ✓ (pure check, no state) ·
  single perimeter ✓ (extends `cs done`'s existing guard family, does not duplicate a verb) ·
  symmetric undo ✓ (a check that aborts creates no state) · regime-aware ✓ (human-only
  `cs done`; a human clears a confidentiality hit, as with the remote blocklist).

## Surprising insight

The same vacuum that *causes* the leak is what *enables* the clean fix: you do not fight
Type-1 retrieval, you feed it (godin/kahneman). Deeper — "the confidential affiliation never belonged in the
byline *on the merits*, independent of secrecy": the public work is open AI tooling, whose
honest author is Noogram. The confidentiality leak and the *wrong-narrator* problem have the
same fix. The embargo is not a tax on the truth; the truth was already Noogram.
