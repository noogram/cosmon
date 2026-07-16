# ADR-055 — Cosmon Residence

**Status:** Proposed (2026-04-20)
**Scope:** vocabulary, type signature, and CLI verb for cosmon state
distribution. Defines `residence` (the internal architectural noun) and
`mode` (the operator-facing CLI verb) as two surfaces of one decision.
**Parent deliberation:**
`delib-20260420-0469`
— 9-persona panel (torvalds, feynman, shannon, jobs, niel, hawking,
wheeler, knuth, godin).
**Related deliberation:**
`delib-20260419-b9a5`
— the cosmon-saas distribution-levels panel, which converged on *one
binary, one `StateStore` trait, HTTP for remote*.
**Binds:**
- [ADR-016](016-autonomy-regimes-and-resident-runtime.md) — the three
  regimes (Inert / Propelled / Autonomous) are **orthogonal** to
  residence. Regime is a clock axis; residence is a location axis.
- [ADR-038](038-whisper-perturbation-port.md) — the six-channel
  inventory. `residence` must not collide with `channel`; it names
  *where the bytes live*, not *how the bits move*.
- [ADR-047](047-event-log-protocol-v0.md) — the `*.events.jsonl` moat
  applies unchanged at every residence: syntax is shared, semantics are
  galaxy-local.
- ADR-054 — Autonomous
  regime is tenant-owned; residence says *nothing* about whether a
  resident loop exists.
**Blocks:**
- `task-20260420-9d29`
  (`cs migrate` command — BLAKE3 manifest, stage-then-flip rollback).
- `task-20260420-5ad6`
  (Migrate cosmon-le-repo to team residence — execution).

## 1 · Context

An audit of the cosmon monorepo found **8 737 tracked files / 26 MB**
inside `.cosmon/state/`, growing linearly with every deliberation. Most
are cognitive artifacts (`prompt.md`, `frame.md`, `synthesis.md`,
persona responses) that belong with the code; a smaller subset (PIDs,
session names, worktree paths, lock files) is live state that loses
meaning across an OS-process boundary.

The panel (`delib-20260420-0469`) framed the question as *where does
cosmon memory live, and under what social contract?* Four candidate
modes emerged: **Solo** (local only), **Team** (shared git remote,
narration tracked), **Encrypted** (team + age-encrypted narration),
**Remote** (hosted by cosmon-saas over HTTP). Five personas (jobs, niel,
godin, shannon, feynman) pushed operator vocabulary; four (wheeler,
hawking, knuth, torvalds) pushed type-system rigor. The synthesis (R1)
observed these are *two surfaces of one decision*, not a conflict.

**Why `mode` alone is not enough.** `mode` is what the operator says at
a dinner party — one syllable, French and English, composes naturally
(*"I'm in solo mode"*). But it is already overloaded: ADR-016 uses
*regime* (Inert / Propelled / Autonomous) for the clock axis. Adding a
second `*_mode` field collides at every doc title, grep, and
onboarding conversation. Wheeler's objection stands: the canonical
type name must survive years of accreting ADRs.

**Why `residence`.** Wheeler's framing: *cosmon memory has to live
somewhere, under some social contract*. That is a **residence** — a
locus of habitation, not a mode of operation. A fundamental noun ("It
from Bit": the bits have to live somewhere) that travels cleanly to
ADR titles and does not collide with `regime` (clock) or `channel`
(wire). The split is the resolution: **`mode` at the CLI, `residence`
in the type system, one decision underneath**.

## 2 · Decision

### 2.1 Two names, one decision

- **Operator-facing CLI verb: `mode`.** The operator types `cs mode
  {solo|team|encrypted|remote}`. The `.cosmon/config.toml` field is
  spelled `mode = "team"`. Marketing, demos, and chronicles may say
  *"solo mode"* or *"team mode"* in prose.
- **Internal architectural noun: `residence`.** The Rust type is
  `Residence`. ADRs, type aliases, and `docs/handbook.md` refer to
  *the galaxy's residence*. Chronicles follow suit when the register
  is architectural rather than operator-facing.

The two words never disagree; they are different registers of the same
axis.

### 2.2 Default: `Residence::Solo`

`cs init` on a fresh directory produces a solo residence. No prompt, no
auto-detection of a git remote, no hint to upgrade. Sovereign,
revocable, zero distributed-systems failure modes. Auto-upgrading a
solo galaxy to team the moment a remote appears is explicitly
**rejected** (jobs, godin, feynman: quality leak / breach of permission
/ clever-and-therefore-wrong). The operator upgrades residence by
running `cs mode team` and nothing else.

### 2.3 Scope of this ADR

This ADR is a decision record: it fixes the vocabulary, the default,
and the type signature. Migration mechanics (`cs migrate`) are
deferred to `task-20260420-9d29`. Execution on cosmon-le-repo itself
is deferred to `task-20260420-5ad6`. The Rust enum in §3 is a **target
signature** — the implementation lands in a follow-up task.

## 3 · Type definition

The internal type lives in `cosmon-core` and exposes the nested
relation that hawking and feynman insisted on (*encrypted requires
team*; *remote is incompatible with an encapsulated-galaxy container*):

```rust
/// Where a galaxy's memory lives and under what social contract.
///
/// Residence is **orthogonal** to regime (ADR-016) and to channel
/// (ADR-038). It answers the question "where do the bits live?", not
/// "what clock drives the worker?" or "how does the signal travel?".
///
/// The variants form a nested hierarchy (hawking):
/// `Solo ⊂ Team ⊂ Encrypted ⊂ Remote` — each is a wider radius around
/// the same narration core. Impossibilities are encoded by the types
/// themselves: `Encrypted` carries the same `repo: GitRemote` field as
/// `Team`, making *encrypted-without-a-remote* unrepresentable;
/// `Remote` carries a `Url` instead of a git remote, making
/// *remote-inside-a-bind-mounted-container* a topological contradiction
/// we refuse to construct.
pub enum Residence {
    /// Single operator, local filesystem only.
    Solo,

    /// Shared git remote; narration tracked, live-state gitignored.
    Team { repo: GitRemote },

    /// Team + age-encrypted narration. Requires the same `repo` field
    /// as `Team` — encryption without a remote is meaningless.
    Encrypted {
        repo: GitRemote,
        recipients: Vec<AgePubkey>,
    },

    /// Narration + state hosted by cosmon-saas; accessed over HTTP.
    /// The client is cosmon itself (one binary, ADR delib-20260419-b9a5).
    Remote { url: Url },
}
```

The enum is a **presentation layer** over the two-axis decomposition
shannon and hawking sketched (`{local, shared} × {plain, age}` plus
`remote: Option<Url>`). The flat variants are what the operator sees;
the hidden invariant `Encrypted ⇒ has-repo` is what the type system
enforces. Both constituencies get what they need.

### 3.1 Residence semantics — what each variant promises

Amended 2026-04-20 after the noesis validation (task-20260420-fa82)
revealed that the first solo fix was *solo partial* — `.cosmon/state/`
was excluded, but nine structural files (`config.toml`,
`formulas/*.toml`, `surfaces.toml`, `.gitignore`) remained tracked. The
operator asked for a local galaxy and got half of one. This section
resolves the ambiguity:

- **`Solo` = TOTAL local invisibility.** After `cs migrate to solo`,
  **zero** files under `.cosmon/` are tracked by git. The pattern
  `.cosmon/` (the whole subtree) lives in `.git/info/exclude`, which
  is a local-only ignore file that never ships with a push. Rationale:
  a solo operator has no one to share with — `config.toml` is
  regenerable via `cs init`, formulas are bundled in the `cs` binary
  via `include_str!`, so there is no content worth preserving across
  git history. *No visible trace = the full intent honored.* Operators
  running a galaxy in a larger code repository (e.g. `~/noesis`) get a
  `.cosmon/` that is invisible to `git log`, `git push`, and every
  collaborator's clone.

- **`Team` = NARRATION-AWARE split.** The narration
  (`state/archive/…`, `state/fleets/*/molecules/*.md`) belongs on the
  orphan branch `cosmon/state`. Structural config (`config.toml`,
  `surfaces.toml`, `formulas/*.toml`, `.gitignore`) **can remain
  trackable on `main`** if the operator wants smooth onboarding for
  collaborators (clone the repo, get a working galaxy), or can be
  moved to the orphan branch if the operator prefers zero
  `main`-pollution. Team residence does not take a stance — it makes
  narration portable across clones via git fetch of
  `cosmon/state`.

- **`Encrypted` = Team + age wrap.** Same storage footprint as Team,
  but the orphan branch content is age-encrypted at rest. Recipients
  hold age keys; rotation is the subject of `task-20260420-8058`.

- **`Remote` = pointer only.** `.cosmon/config.toml` holds a remote
  endpoint (`cosmon-saas`); narration lives on that server. The local
  repo keeps no stale state files under `.cosmon/state/`.

**Not supported in v0.** `solo partial` (state-local, formulas
tracked, config tracked) is **not** a supported residence. If a
concrete need emerges — e.g. *"I want my config on main but my state
local"* — nucleate a follow-up deliberation with a name and a
rationale. Until then, `solo` means *total*.

**Detection and backwards-compat.** `cs migrate verify` knows each
residence's git-side invariant (see §6 for the table). A galaxy in
"solo partial" state is flagged with exit code 1 and a remediation
hint; it is **not** auto-migrated. The operator reads the diagnostic
and re-runs `cs migrate to solo` to converge.

## 4 · CLI surface

### 4.1 One verb, four values

```text
cs mode                       # prints current residence
cs mode solo                  # downgrade to local filesystem
cs mode team --remote <url>   # adopt a git remote as the team residence
cs mode encrypted \
    --remote <url> \
    --recipient age1…         # team + age encryption
cs mode remote --url <url>    # hand state off to cosmon-saas
```

That is the entire verb inventory for this axis. Each transition is a
single command. Transitioning between non-solo residences requires the
operator to pass through the `cs migrate` step (task-20260420-9d29);
`cs mode` is the *declaration* of intent, not the copier of bytes.

### 4.2 Explicit subtractions

The panel considered and **rejected** the following surfaces:

- `cs config set state.distribution=team` — hides the decision inside a
  generic config verb; removes the sovereignty language.
- `cs galaxy switch` — collides with `cs mode` on vocabulary and
  reintroduces the "galaxy as a slot I occupy" frame godin warned
  against.
- `cs init --mode team` — pressures the operator to decide at the
  wrong moment. `cs init` produces a solo galaxy; residence is
  upgraded later when the operator has reason to.
- `cs migrate` as an operator vocabulary word — the *migrate* verb is
  the mechanical bytes-mover (task-9d29), invoked by `cs mode` under
  the hood, not spoken by the operator.

Each subtraction keeps the demo to six lines.

## 5 · Topological constraints

### 5.1 Narration vs live-state (the OS-process boundary)

Hawking's theorem (from the synthesis): *the horizon of cosmon state
is the OS-process boundary.* Files dependent on the OS-process
namespace (PIDs, lock files, session names, `fleet.runtime.json`) are
**live-state**; files surviving a fresh shell with no process table
(`events.jsonl`, `prompt.md`, `briefing.md`, `synthesis.md`, seals) are
**narration**.

Residence decides *which bytes cross which horizon*:

| Residence | Narration (tracked)           | Live-state |
|-----------|-------------------------------|------------|
| Solo      | local disk only               | local only |
| Team      | pushed to git remote          | local only |
| Encrypted | age-encrypted in git remote   | local only |
| Remote    | HTTP-mirrored to cosmon-saas  | local only |

Live-state never crosses. It is re-derivable from narration
(`state.json` is a projection of `events.jsonl` via `cs reconcile` —
R4 in the synthesis).

### 5.2 Nested radius

Hawking's nesting is load-bearing: `Solo ⊂ Team ⊂ Encrypted ⊂ Remote`.
Each residence is a wider radius around the same narration core; `cs
mode solo` always down-migrates without data loss (narration stays on
disk).

### 5.3 Residence vs regime vs channel (orthogonality)

Three axes that must not be confused:

| Axis       | Values                                        | ADR |
|------------|-----------------------------------------------|-----|
| **Residence** | Solo / Team / Encrypted / Remote           | this ADR |
| **Regime**    | Inert / Propelled / Autonomous             | [ADR-016](016-autonomy-regimes-and-resident-runtime.md), ADR-054 |
| **Channel**   | neurion / DAG / filesystem / artifact / propulsion / whisper | [ADR-038](038-whisper-perturbation-port.md) |

A `Team` residence running in `Propelled` regime writes to the
`filesystem` channel exactly as a `Solo` residence does. Changing
residence does not change the regime; changing regime does not move
the bytes; channel names stay the same.

## 6 · Migration path

Residence transitions are executed by `cs migrate`
(task-20260420-9d29): a BLAKE3-manifested stage-then-flip operation
with pre/post invariant `A_pre ⊆ A_post` over `(molecule_id, path,
blake3(content))` triples, exit codes `0/1/2` mirroring `cs verify`.

**Migration is atomic across data and git.** A residence is not just a
filesystem layout; it is also a social contract with the git index. A
data-only migration that leaves `.cosmon/state/` tracked turns a
*solo* residence into a name that lies: the operator believed they
unplugged from sharing, but `git push` still carries the state. From
task-20260420-e906 onward `cs migrate to <residence>` therefore runs
four phases instead of three:

1. **seal** — BLAKE3 manifest + snapshot of git HEAD, `.gitignore`,
   and `.git/info/exclude` (the fields needed for a symmetric
   rollback).
2. **stage** — byte-for-byte copy into `<state>.next`.
3. **verify** — walk the staged tree, compare against the manifest.
4. **flip + git-side** — atomic rename pair, then apply the residence's
   git footprint:

   | Residence | git-side footprint                                                                                             |
   |-----------|----------------------------------------------------------------------------------------------------------------|
   | `solo`    | `git rm -r --cached .cosmon/` (**whole galaxy subtree**) + append `.cosmon/` to `.git/info/exclude` (local-only, not pushed). Enforces §3.1 *solo total*. |
   | `team`    | `git rm -r --cached .cosmon/state` + append `.cosmon/state/` to `.gitignore` + commit `chore(cosmon): migrate to team residence (git-side)`. Narration moves to orphan branch `cosmon/state`; structural files stay on `main`. |
   | `encrypted` | Same as `team`. Age-wrap happens at push time on the orphan narration branch, not here.                       |
   | `remote`  | Same as `team` (narration lives on the cosmon-saas server, local repo should not track it).                    |

   **Solo is widest — it targets the whole `.cosmon/` subtree, not
   just `.cosmon/state/` — because §3.1 says solo means *no trace*,
   and structural files like `config.toml` or `formulas/*.toml` are
   regenerable from `cs init` + `include_str!` bundled formulas.
   Team/Encrypted/Remote target `.cosmon/state/` only, because the
   structural files are shared with collaborators (they describe the
   galaxy, not its history).**

   The commit uses path-scoped pathspecs only — no bundling of
   unrelated staged changes. `cs migrate` refuses to commit when the
   index is not clean and asks the operator to commit or stash first.
   Opt out per-invocation with `--no-git` or `--no-commit`.

**Rollback is atomic too.** `cs migrate rollback` reads the git
pre-state from the pre-migration manifest and restores `.gitignore`,
`.git/info/exclude`, and the index (`git reset --mixed <head>`) byte
for byte. The galaxy's git footprint returns to exactly what it was
before `cs migrate to` was invoked.

**Verify is mode-aware.** After task-20260420-fa82, `cs migrate
verify` re-checks the BLAKE3 manifest **and** the residence's git-side
invariant:

| Residence   | Post-migration git invariant                                                                 |
|-------------|----------------------------------------------------------------------------------------------|
| `solo`      | `git ls-files .cosmon/` returns **0** paths.                                                 |
| `team`      | `git ls-files .cosmon/state/` returns **0** paths on the current branch.                     |
| `encrypted` | Same as `team` (age wrap is orthogonal to index state). *Deferred: age-content check.*        |
| `remote`    | `git ls-files .cosmon/state/` returns **0** paths (narration lives on the cosmon-saas server). |

A violation exits with code `1` and prints a remediation hint
(`git rm --cached …` + `cs migrate to <residence>`). This catches the
exact failure mode task-20260420-fa82 surfaced: a solo galaxy that
believed itself local but still had `config.toml`, `formulas/*.toml`,
`surfaces.toml`, and `.gitignore` under git's eye.

The cosmon monorepo itself (8 737 files / 26 MB) migrates to `team`
residence under task-20260420-5ad6: orphan branch seed
(`cosmon/state`), `git rm -r --cached`, `.gitignore` patch, `cs migrate
verify`, rebase of the five in-flight `feat/*` branches. That was the
first real-world application of this ADR — and the evidence that data
without git is only half a migration (chronicle
`2026-04-20-migration-git-side.md`). Neither task is in scope here —
this ADR names the axis; the migration tasks move the bytes.

Operators picking a residence on their own galaxy should read the
[per-project migration runbook](../guides/residence-migration-per-project.md):
a copy-pasteable walkthrough for `solo`, `team`, `encrypted`, and
`remote`, with rollback and FAQ.

## 7 · Consequences

**Gained**

- One axis, two surfaces: operators speak `mode`, types speak
  `Residence`, nothing collides.
- `cs init` stays silent — a new galaxy is solo by default and the
  operator is never asked a question they did not plan to answer.
- The type signature encodes the two structural impossibilities
  (`encrypted-without-remote`, `remote-inside-container`) as
  unconstructible states, not as runtime errors.
- Residence travels cleanly into doc titles, type names, and chronicle
  entries without contaminating the regime or channel vocabulary.
- A forward reference exists for the two execution tasks (9d29,
  5ad6): the decision is written down before the bytes move.

**Lost / constrained**

- Two names for one decision is a documentation tax: every
  cross-reference chooses its register. Worth the tax (no `*_mode`
  collision), but real.
- `cs mode` is declarative; byte-moving happens in `cs migrate`. The
  CLI must make clear `cs mode team` declares intent — `cs migrate`
  carries it out.
- Encrypted residence ships with an age dependency Solo and Team do
  not need. Packaging must keep age optional, not a hard dependency
  of `cosmon-core`.

**Open (deferred)**

- Exact shape of `GitRemote` and `AgePubkey` newtypes (cosmon-core).
- Residence grouping in `cs peek --all` (TUI refactor).
- Whether `Residence::Remote` constrains `StateStore` transport;
  `delib-20260419-b9a5` already says *HTTP+REST, sufficient, no CRDT*.

## 8 · References

- Synthesis:
  `delib-20260420-0469`
- Per-persona evidence:
  `.cosmon/state/fleets/default/molecules/delib-20260420-0469/responses/{wheeler,jobs,godin,hawking,feynman,shannon,torvalds,niel,knuth}.md`
- Cosmon-saas distribution deliberation:
  `delib-20260419-b9a5`
- Regime axis: [ADR-016](016-autonomy-regimes-and-resident-runtime.md),
  ADR-054
- Channel axis: [ADR-038](038-whisper-perturbation-port.md)
- Event-log substrate: [ADR-047](047-event-log-protocol-v0.md)
- Migration command: `task-20260420-9d29` (`cs migrate`)
- Repo execution: `task-20260420-5ad6` (cosmon-le-repo migration)

## The one-sentence residence

*A galaxy's residence is where its memory lives and under what social
contract — `mode` names it; `Residence` enforces it.*
