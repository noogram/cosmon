# ADR-069 — A cosmon project root is a `.cosmon/` **with** `config.toml`; a config-less `.cosmon/` is a user-level state host

**Status:** Accepted (2026-04-23)
**Authoring task:** `task-20260423-fdeb`
**Parent idea:** `idea-20260423-abbf` —
captured symptom, feasibility.md
§4 decision matrix (Option A dominates), plan.md
deliverable #1.

**Scope:**

- The project-discovery predicate used by `cs init` nest-refusal
  (`crates/cosmon-cli/src/cmd/init.rs`) and by the filestore walk-up
  (`crates/cosmon-filestore/src/resolve.rs`).
- The invariant *"new galaxies are easy to birth"* that drove the
  2026-04-18 migration of all code repos under `/srv/cosmon/`.
- The legitimate cohabitation of a user-level `~/.cosmon/` (scheduler,
  patrol supervisor, recovery logs) with any number of per-galaxy
  project roots.

**Binds:**

- **Refines** [ADR-031](031-cs-init-template.md) — `cs init` was the
  canonical creator of a project root; this ADR sharpens the
  *definition* of that root (it must carry `config.toml`) without
  changing what `cs init` writes.
- **Refines** [ADR-053](053-cosmon-daemon-supervisor.md) — the daemon
  supervisor's state lives under `$HOME/.cosmon/` by convention; this
  ADR ratifies that location as a **user-level state host**, not a
  project.
- **Unblocks** `task-20260423-810b` — the code change + tests +
  CHANGELOG entry that applies the predicate at the two call sites
  identified below.
- **Does not contradict** any existing §8-series architectural
  invariant. Adds no new invariant class; it *clarifies* the
  walk-up predicate that three existing commands already share.

---

## 1 · Context

### 1.1 · What forced the question

On 2026-04-23, while nucleating the first real Noogram client galaxy
(`/srv/cosmon/annex/`), `cs init` refused to create the project:

```
$ cs init
cs: refusing to nest cosmon projects: ancestor `.cosmon/` exists at /Users/you/.cosmon
target: /srv/cosmon/annex
Pick a path outside that galaxy, or `rm -rf /Users/you/.cosmon` first.
No `--force` escape — nested galaxies silently break walk-up discovery.
```

`~/.cosmon/` on the operator's machine holds:

- `scheduler.state.json` — cron-like patrol scheduler state.
- `daemon-supervisor.state.json` — supervisor state (ADR-053).
- `recover-*.log` — recovery logs.
- `formulas` → `~/cosmon/formulas` (symlink).
- `state/fleets/` — user-level fleet state.
- 17 cumulative firings of `mailroom-executor-pulse`
  (last one 2026-04-22 16:00).

It does **not** hold a `config.toml`. It was never created by
`cs init`; it grew organically as cosmon's daemon/scheduler surface
landed, using `$HOME/.cosmon/` as a convenient well-known path.

The tenant-demo galaxy was successfully initialised on 2026-04-21 20:51
while `~/.cosmon/` already existed. Between then and 2026-04-23, the
nest-refusal in `cs init` was hardened (commit `c6171fe7`,
*"feat(init): accept non-existent target + strict idempotency"*),
which introduced `find_nearest_ancestor_cosmon` — a walk-up that
treats any ancestor `.cosmon/` as a project root, with no
discrimination.

### 1.2 · The broken invariant

*"New galaxies are easy to birth."*

This is the principle that drove the operator's 2026-04-18 migration
of every code repo under `/srv/cosmon/`. Any operator with a
legitimate `~/.cosmon/` user-level state host — which every operator
running patrols or the daemon supervisor has — was silently locked
out of the galaxy-birthing primitive. The error message even
recommended `rm -rf ~/.cosmon/`, which would **destroy** the
user-level scheduler state: a destructive false friend.

Cosmon's own discipline forbids this posture:

> **Cosmon-ward feedback flow** — when an application-site galaxy
> discovers a cosmon-level pathology, surface it back to cosmon as
> a typed molecule. Do not silently patch around what should be
> redesigned in the core.
> — `CLAUDE.md` (global)

The `idea-20260423-abbf` chain (capture → feasibility → plan) is the
material form of that discipline. This ADR ratifies the
semantic fix chosen in the feasibility study.

### 1.3 · Why this gets its own ADR

The fix itself is small — roughly thirty lines across two files.
Backdooring it through the implementation PR would, however, hide a
**definitional** change:

1. Before: *a cosmon project is any ancestor directory containing
   `.cosmon/`*.
2. After: *a cosmon project is any ancestor directory containing
   `.cosmon/config.toml`.*

That is a semver-flavoured promise to anyone who writes tooling
against the walk-up predicate — scripts, notary hooks, pilot apps,
future galaxies. Writing the change as code alone would not tell a
future maintainer *why* a config-less `.cosmon/` is now invisible to
walk-up. The ADR makes the predicate explicit, citable, and
testable — the same posture ADR-065 took for the MSRV bump.

It also gives the sibling implementation task
(`task-20260423-810b`) a stable citation point for its commit
message, CHANGELOG entry, and the three regression tests that will
encode the predicate.

---

## 2 · Decision

A `.cosmon/` directory is treated as a **cosmon project root** by the
walk-up predicate **if and only if it contains a regular file named
`config.toml` at its root**.

Formally:

```
is_project_root(dir) ≡  dir.join(".cosmon").is_dir()
                      ∧ dir.join(".cosmon/config.toml").is_file()
```

A `.cosmon/` directory **without** `config.toml` is a **user-level
state host**: the home of schedulers, patrol supervisors, recovery
logs, and any cross-galaxy operator state that does not belong to a
specific project. User-level hosts do not participate in project
discovery and do not trigger nest-refusal.

### 2.1 · Two call sites affected

| Call site | File | Current behaviour | After this ADR |
|---|---|---|---|
| Init nest-refusal | `crates/cosmon-cli/src/cmd/init.rs` — `find_nearest_ancestor_cosmon` | Walks up parents, returns the first directory containing `.cosmon/`. | Walks up parents, returns the first directory containing `.cosmon/config.toml`. Skips config-less ancestors. |
| Filestore project discovery | `crates/cosmon-filestore/src/resolve.rs` — `walk_up_find_cosmon_dir_from` | Same walk, same lack of discrimination. | Same `config.toml` discriminator. Preserves the git-worktree redirect for real projects. |

Both sites share the predicate. The sibling implementation task
(`task-20260423-810b`) applies them together; splitting the fix
would let one site keep the old semantics while the other moved
forward, reintroducing the very drift this ADR forbids.

### 2.2 · What the refusal message keeps doing

For ancestors that **do** carry `config.toml`, the existing refusal
message is unchanged. `cs init` still refuses to create a project
inside another project — the original intent of the nest-refusal
(from `delib-20260418-770e`) is preserved; only the overshoot on
config-less hosts is corrected.

### 2.3 · What migrates

**Nothing.** Every project created by `cs init` — since day one —
writes `config.toml` unconditionally. There is no population of
legitimate projects that would now fail to be discovered. The only
directories the predicate newly excludes are user-level hosts, which
were never meant to be discovered as projects.

### 2.4 · What this ADR does not do

- Does **not** introduce a new path (no `UserRoot`, no XDG
  abstraction, no `$XDG_STATE_HOME/cosmon/` move). That was the
  feasibility study's Option B, filed as `temp:warm` follow-up if a
  second force appears.
- Does **not** add a `cs doctor` / `cs diagnose nesting` command.
  Option A removes the main use case; see §5 of the feasibility
  study. Re-file as `temp:cold` idea if a future refusal confuses
  an operator.
- Does **not** touch `cs init --upgrade`, `--soft`, or any other
  init subcommand. Walk-up semantics only.
- Does **not** change what `cs init` writes into a new `.cosmon/`.
  The template (ADR-031) is untouched.
- Does **not** define what a user-level state host's layout
  *should* be. That is the concern of ADR-053 (daemon supervisor)
  and the scheduler ADRs, not this one. This ADR only says: a
  `.cosmon/` without `config.toml` is one.

---

## 3 · Consequences

### 3.1 · For the operator

The galaxy-birthing primitive is restored. `cs init` in any
subdirectory of a config-less `~/.cosmon/` succeeds. The operator
keeps their legitimate user-level host; `/srv/cosmon/annex/`,
`/srv/cosmon/annex/<anywhere>/`, and every future galaxy under
`~/` can be born without touching `rm -rf`.

### 3.2 · For tooling that reads cosmon state

Any tool that walks up looking for `.cosmon/` should now check for
`.cosmon/config.toml` if it wants to match cosmon's own predicate.
Today this concerns:

- `cs-api` (pilot apps backend) — uses the same filestore helper.
  Unchanged once the filestore fix lands.
- `mcp-cosmon` (deprecated, 2026-04-11 grace window) — unaffected;
  workers use the CLI now.
- Third-party scripts — none known in the operator's machine
  inventory. The ADR is the forward-looking citation.

### 3.3 · For latent state pollution

The previous walk-up would silently resolve state directories under
`~/.cosmon/state/` when any `cs` command ran from a directory
without its own `.cosmon/`. This was a latent bug, not a feature —
it meant commands nucleated or read fleet state from a user-level
host that was never meant to be a project store. The fix closes
that hole as a side-effect, not just the init refusal.

### 3.4 · For existing projects (non-regression)

| Galaxy | Has `.cosmon/config.toml`? | Behaviour |
|---|---|---|
| `/srv/cosmon/cosmon/` | Yes | Unchanged. |
| `/srv/cosmon/tenant-demo/` | Yes (2026-04-21 init). | Unchanged. |
| `/srv/cosmon/annex/` | No (blocked today). | `cs init` now succeeds; walk-up from inside no longer pollutes `~/.cosmon/`. |
| Any future galaxy | Yes (after `cs init`). | Standard. |

### 3.5 · Reversibility

**High.** The predicate change is one conditional per call site. A
revert is a single commit that removes the `.is_file()` check and
restores the prior behaviour. The operator's fallback is to keep a
config-less `.cosmon/` below `$HOME` until the revert is merged; no
state is destroyed by either direction.

### 3.6 · Risk surface

| Risk | Likelihood | Mitigation |
|---|---|---|
| A future feature writes a `.cosmon/` without `config.toml` and expects walk-up to find it. | Low | This ADR names the predicate; a test in `cosmon-filestore` encodes it. Any such future feature must either write `config.toml` or file a successor ADR redefining the predicate. |
| Third-party tooling walks up and now misses a config-less `.cosmon/` it used to find. | Very low | No such tooling is known; cosmon's public scripting surface is the CLI and MCP (both updated). |
| Operator confusion between *project root* and *user-level host*. | Low | The error message for real nested-project refusal is unchanged. The feasibility study and this ADR define the terms; `CLAUDE.md` picks up the one-liner in the next doc-sync pass. |

---

## 4 · Verification

The sibling implementation task (`task-20260423-810b`) carries the
test fixtures. Three tests encode the predicate:

1. **Regression test.** Create a config-less ancestor `.cosmon/` and
   a nested target. `cs init <target>` must **succeed**, writing a
   fresh `.cosmon/config.toml` inside the target.
2. **Preserved refusal.** Create an ancestor `.cosmon/` **with**
   `config.toml`. `cs init <target>` must **refuse** with the
   existing message (preserves the original intent).
3. **Walk-up parity.** `walk_up_find_cosmon_dir_from(<subdir>)` must
   skip a config-less `.cosmon/` and return `None` (or the next
   config-bearing ancestor, if any).

All four gates (`cargo check`, `test`, `clippy -D warnings`, `fmt
--check`) must pass in the sibling task's PR. This ADR itself
changes no code; its own landing only adds a markdown file.

---

## 5 · Alternatives considered

### A. Soften walk-up on `config.toml` presence *(chosen)*

The predicate change this ADR ratifies. Minimal, universal, tested.
Strictly dominates on effort × blast radius × reversibility per
the feasibility study §4 decision matrix.

### B. Formalise `~/.cosmon/` as a user-level root (XDG-like)

Introduce an explicit `UserRoot` concept with `config` / `data` /
`state` under `~/.cosmon/` (or `$XDG_STATE_HOME/cosmon/`), and
exclude `$HOME` from walk-up by convention. **Rejected for now**:
larger surface area, introduces a new concept that does not pay
rent immediately, and only helps when the user-level host is under
`$HOME` specifically. Filed as `temp:warm` follow-up if a second
force appears (XDG migration, multi-user host, or a second kind of
non-project `.cosmon/`).

### C. Keep the refusal, add `cs doctor nesting`

Leave the predicate untouched and ship a diagnostic command that
explains why `cs init` refused and proposes a non-destructive
remedy. **Rejected**: the destructive suggestion (`rm -rf`) is in
the error message itself, which a `cs doctor` command cannot
un-print. The right fix is the predicate; the diagnostic is
infrastructure without a user once the predicate is right.
Feasibility §5 verdict.

### D. `--force` escape on `cs init`

Add `cs init --force` to bypass the nest-refusal. **Rejected**:
`--force` is a *permission* flag, not a *correctness* flag. The
nest-refusal is correct when the ancestor is a real project; a
flag to disable it would normalize breaking the original intent.
The right answer is to make the refusal fire only when it should,
not to add an escape.

---

## 6 · References

- **Parent idea chain.**
  - `.cosmon/molecules/idea-20260423-abbf/idea.md`
    — capture (symptom, regression evidence, broken invariant).
  - `.cosmon/molecules/idea-20260423-abbf/feasibility.md`
    — decision matrix §4; Option A recommended.
  - `.cosmon/molecules/idea-20260423-abbf/plan.md`
    — deliverables; this ADR is deliverable #1.
- **Sibling implementation task.** `task-20260423-810b` — applies
  the predicate at the two call sites and ships the three tests.
- **Architectural discipline.**
  - [ADR-031](031-cs-init-template.md) — what `cs init` writes
    (the template this ADR defines the *retrieval* predicate for).
  - [ADR-053](053-cosmon-daemon-supervisor.md) — the daemon
    supervisor whose state lives under the user-level host.
  - CLAUDE.md (global) §"Cosmon-ward
    feedback flow" — the discipline that produced the idea chain
    rather than a silent patch in `addl`.
- **Commit that introduced the overshoot.** `c6171fe7` (2026-04-19),
  *"feat(init): accept non-existent target + strict idempotency"*.
