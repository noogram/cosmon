# ADR-170 — The galaxy declares its repository

**Status:** Accepted (2026-08-04).
**Date:** 2026-08-04.
**Decider:** Noogram.
**Authoring task:** `task-20260728-7d49`.

**Entry artefact.** A report from an applicative galaxy whose deliverable
belongs to a third party: cosmon has no vocabulary for "this galaxy works on a
repository that is not itself".

**Related ADRs:**
[ADR-069](069-cosmon-project-vs-user-root.md) (project root vs user root — the
walk-up this one pairs with),
[ADR-133](133-one-repo-artifact-map-membrane.md) (audience, the axis this one
is *not*),
[ADR-055](055-cosmon-residence.md) (Solo/Team, the axis this one exposes as
too narrow).

---

## Context

### Two resolvers that agree by luck

A galaxy's *state* and a galaxy's *repository* are found by two entirely
independent walks, both starting from the process's current directory:

| Question | Resolver | Wins |
|---|---|---|
| Where is the state? | `cosmon_filestore::resolve_state_dir` | nearest ancestor `.cosmon/` |
| Where is the repository? | `git rev-parse --show-toplevel` | nearest ancestor `.git` |

Nothing in the codebase, the config, or the docs said these two must answer
the same tree. In the flat case — a galaxy that *is* a repository — they do,
and cosmon has always been written as if that were a law.

It is not. Verified empirically: with a target repository nested inside an
orchestration galaxy, running from inside the nested repository resolves the
state to the *galaxy* and the repository to the *target*. The nested topology
already worked, correctly, by accident, with nothing anywhere declaring it.

### The pathology is silence, not breakage

An implicit-by-cwd binding fails the way a mute guard rail fails. `cs tackle`
fired from the wrong directory branches the wrong repository: no error, no
warning, no prompt. The work lands somewhere else and is discovered later, by
a human noticing. Cosmon has retired this failure shape repeatedly
(ADR-162's earned readiness, ADR-163's answerable question); the repository
binding was the last large instance of it still standing.

### The missing axis

Residence (ADR-055) has two variants, `Solo` and `Team`, and `Team` names *the*
remote, singular. The artifact map (ADR-133) answers who may **read** a file —
audience. Neither answers who **owns** it: read, write, lifecycle, and the
right to walk away with it. `partner:<name>` never denoted another repository;
it denoted another branch of the same one. A galaxy whose deliverable belongs
to a third party therefore has no word for its own situation.

## Decision

**A galaxy may declare the repository its work lands in, and cosmon honours
the declaration over the coincidence.**

The optional `[project] target_repo` key in `.cosmon/config.toml` names it.
Resolution lives in one module, `cosmon_cli::target_repo`, which every
repository-needing command now calls instead of re-implementing
`git rev-parse --show-toplevel` locally.

1. **Absent — nothing changes, byte for byte.** The repository is the one
   containing the current directory, resolved exactly as before. This is not a
   deprecated mode; it is the default and remains correct.
2. **Present — the declaration wins.** The repository is probed at that path.
   Relative values resolve against the **galaxy root** (the directory holding
   `.cosmon/`), never the cwd, so the declaration denotes the same tree from
   wherever `cs` was fired. `"."` writes the merged case explicitly.
3. **A declared non-repository is refused, loudly, naming the key and the
   resolved path.** It does *not* fall back to the cwd.
4. **A leading `~` is refused** rather than probed literally, because a shell
   expands it and a config file does not.

### Why clause 3 is load-bearing

Falling back to the cwd when the declared path is not a repository would be
"helpful" and would restore the precise failure this ADR exists to remove: the
operator declared a target, the declaration silently did not take, and work
landed in whatever tree the shell happened to be sitting in. A declaration that
can be silently ignored is not a declaration. Refuse, name the key, name the
path, branch nothing.

### The staging, and why v2 is not decided here

- **v0** — today's behaviour. Unchanged.
- **v1 — this ADR.** Opt-in. One codepath, not a fork: the undeclared case is
  the `None` arm of the same function, not a second implementation.
- **v2 — deliberately not decided here.** Making the key *recommended*, having
  `cs init` write it live rather than commented, and re-describing the flat
  case as `target_repo = "."` — a special case of the general one rather than a
  separate mode — is a normalisation with ecosystem cost across every existing
  galaxy. It needs the architects' deliberation this molecule's mission asked
  for, and it needs the ownership axis below decided first.

## Consequences

### What this closes

The binding between a galaxy and its repository is now *sayable*. An operator
can read one line of config and know which tree `cs tackle` will branch, rather
than reconstructing it from where they happen to be standing.

### What this does not close — and is not pretending to

- **Ownership remains unmodelled.** `target_repo` says *where the work lands*.
  It does not say *who owns what lands there*. Residence still offers two
  variants and the artifact map still answers only audience. Whether ownership
  joins the artifact map as a third axis, or multi-repository becomes a
  first-class topology, is exactly the open question — this ADR narrows it by
  making the multi-repository case declarable, and does not answer it.
- **Worktrees still live in the target repository.** `create_worktree` does
  `repo_root.join(".worktrees")`, and molecule branches (`feat/mol-*`) are cut
  in the target. When the target belongs to a third party, cosmon's scaffolding
  is workshop furniture in someone else's storefront. `cs done` merges and
  deletes the branch, but an in-flight molecule is visible. Declaring
  `target_repo` makes this **legible**; a separate `worktrees_root` is what
  would make it **movable**. That is a follow-up, not a v1 omission — moving
  worktrees out of the repository touches the harvest path, which is the one
  part of cosmon where a mistake loses work.

### Verification

The refusal, the anchoring, the tilde, and the byte-identical absent case each
carry a test in `crates/cosmon-cli/src/target_repo.rs`. The config-knob reader
gate (`crates/cosmon-core/tests/config_knobs_have_readers.rs`) covers the field
automatically: a `target_repo` that parsed and governed nothing would fail CI,
which is the defect class this key would otherwise have been a fresh instance
of.
