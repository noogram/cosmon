# Personas for deliberation panels

The `deep-think` and `deep-think-inline` formulas run a **panel** of expert
personas. Each persona is just a named agent the worker can invoke.

## Where personas come from

Personas are resolved at run time from the worker's own **Claude Code
subagents** — the agent definitions the host exposes under `.claude/agents/`
(project-scoped) or the user-scoped agents directory. The short-name in
`--var panel=feynman,jobs,wheeler` (or the auto-selected names when
`panel=auto`) matches an available subagent by name.

Because resolution goes through whatever subagents the install exposes, the
formulas carry **no absolute persona path** and are portable across galaxies
and machines. A fresh install with no custom agents still works: the panel is
drawn from the built-in subagents the host ships.

## Optional: a shared persona library

Maintaining a curated, shared library of persona definitions is **optional
operator infrastructure**, not a requirement of the formula. If you keep one,
wire each definition into the host as a subagent (for Claude Code, a file or
symlink under `.claude/agents/<name>.md`) so its short-name resolves the same
way the built-ins do. The formula never reads such a library directly — it only
ever sees the subagents the host advertises.
