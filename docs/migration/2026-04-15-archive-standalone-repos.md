# 2026-04-15 — Archive standalone repos after absorption

After foundry's absorption into cosmon (ADR 043) and wiki-genetic-algorithms'
absorption (ADR 044), and given that neurion and topon were already
absorbed earlier (`50111f41` — "particle convergence"), the four standalone
repos and their local checkouts should be archived.

**This procedure is pilot-driven.** The worker does not archive anything
automatically — the pilot runs the commands below, in order, after
verifying no one else has work pending on the legacy repos.

## Pre-archive checks (do these before anything else)

For each of `foundry`, `wiki-genetic-algorithms`, `neurion`, `topon`:

1. **No open issues:** `gh issue list --repo noogram-research/<repo> --state open`
2. **No open PRs:** `gh pr list --repo noogram-research/<repo> --state open`
3. **No external forks with recent commits:** inspect the forks list in
   the GitHub UI (`Insights → Forks`). External forks are unlikely but
   worth a 10-second glance.
4. **Local worktree clean:** `cd ~/dev/projects/<repo> && git status`

If any of the above surfaces activity, pause and notify the owners before
archiving.

## Foundry

Last commit prior to absorption: `24b88a22b8ae8256cf0bfa1bcb75e817a4e79dab`.

### GitHub side

1. Edit `github.com/noogram-research/foundry` README to add a banner at the
   top:

   ```markdown
   ## ⚠️ ARCHIVED — migrated to cosmon

   Foundry has been absorbed into cosmon as workspace crates `foundry-*`
   (cosmon ADR 043, 2026-04-15). Active development happens at
   https://github.com/noogram/cosmon.

   This repository is preserved read-only for historical reference.
   Last commit prior to absorption: `24b88a22`.
   Original license: `MIT OR Apache-2.0`.

   For new development, contributions, and issues: please use the cosmon
   repo.
   ```

2. Commit and push.
3. `Settings → General → Archive this repository`.

### Local side

```sh
mkdir -p ~/dev/projects/_archived
mv ~/dev/projects/foundry ~/dev/projects/_archived/foundry-2026-04-15
```

## Neurion

Verify the crates in `cosmon/crates/neurion-*` are current relative to
`~/dev/projects/neurion/`. If the standalone has diverged (commits that were
not re-absorbed), run a second absorption pass before archiving.

Then apply the same two-phase procedure as foundry (README banner →
GitHub archive → local move to `_archived/neurion-2026-04-15`).

## Topon

Same procedure as neurion. Verify `cosmon/crates/topon-*` is current before
archiving.

## wiki-genetic-algorithms (procedure C)

Last commit prior to absorption: `a3bac48` (merge of
`feat/task-20260414-e92b`). Upstream remote: `git@github.com:noogram/wiki-genetic-algorithms.git`.

### GitHub side

1. Edit `github.com/noogram/wiki-genetic-algorithms` README to add a banner:

   ```markdown
   ## ⚠️ ARCHIVED — migrated to cosmon

   wiki-genetic-algorithms has been absorbed into cosmon as workspace crates
   `ga-*` (ADR 044, 2026-04-15). Active development happens at
   https://github.com/noogram/cosmon.
   Identity artifacts (THESIS, MISSION, wiki, research, review) live at
   https://github.com/noogram/cosmon/tree/main/docs/genetic-algorithms.
   ```

2. `Settings → General → Danger Zone → Archive this repository`.

### Local side

```bash
mkdir -p ~/dev/projects/_archived
mv ~/dev/projects/wiki-genetic-algorithms ~/dev/projects/_archived/wiki-genetic-algorithms-2026-04-15
```

Before moving: commit or drop the uncommitted `.cosmon/state/` and
`.obsidian/workspace.json` changes in the standalone checkout — they are
ephemeral runtime artifacts and not needed in the archive.

## After all four are archived

- Update `neurion` registry (`~/dev/projects/neurion/` is becoming
  `_archived/`, so update the `repos` table to point at the cosmon crate
  paths).
- A short chronicle entry in an internal chronicle marking the moment
  when the three satellites completed convergence into the workspace.
