# `/cosmon` skill — pilot cosmon by natural language

User-global Claude Code skill carrying the same orchestration pointer as
every project's `## Cosmon — Orchestration` `CLAUDE.md`/`AGENTS.md` section
(`cs init --upgrade` writes that one; this is the second rendering of the
same source text — see `cosmon_filestore::project_upgrade::
COSMON_ORCHESTRATION_BODY`, and the test that keeps the two in sync,
`orchestration_source_tests::claude_md_section_and_skill_render_the_same_body`).

Unlike the `CLAUDE.md` pointer, this skill needs no `cs init` in the target
repository first: install it once, at the user level, and Claude Code loads
it on demand in every repository.

## Install

```bash
./install.sh
```

Idempotent. Copies `SKILL.md` into `~/.claude/skills/cosmon/`.

To scope it to a single project instead, copy or symlink `SKILL.md` into that
project's `.claude/skills/cosmon/` directory.

## Use

In a Claude Code session, type `/cosmon`, or just ask it to pilot cosmon —
the skill's `description` is written to match on `cs`, nucleate, tackle,
molecule, whisper, or "cosmon project". Claude Code loads the skill body,
which points at `cs help` for the full surface and spells out the
nucleate → tackle → peek → whisper → done cycle.

## Regenerating `SKILL.md`

`SKILL.md` in this directory is generated, not hand-edited — same discipline
as `man/cs.1`. After changing `COSMON_ORCHESTRATION_BODY` in
`crates/cosmon-filestore/src/project_upgrade.rs`, regenerate with:

```sh
SKILL_UPDATE=1 cargo test -p cosmon-filestore skill_md
```
