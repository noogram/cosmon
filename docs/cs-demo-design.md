# `cs demo` — first-contact design

*Premier test utilisateur mature. Compresse le gap "time to first wow" de
~30 min (lire THESIS + setup tmux + nucleate + tackle + wait + done) à
**≤60 secondes** via une commande unique.*

Issued from **delib-20260415-6b9d / IDÉE-5** — pilot arbitrage 2026-04-15.

---

## 1. User flow

```text
$ cs demo
▶ cs demo — formula deep-think
❯ Is GPL contamination a viable mechanism for cognitive governance?

  ⏳ nucleated delib-20260415-xyz
  ⏳ tackled delib-20260415-xyz (worker dispatched)
  ⏳ waiting on delib-20260415-xyz (timeout 600s, poll 5s)
  ✅ delib-20260415-xyz reached completed in 132.4s (27 polls, 1 transitions)

📂 synthesis.md — .cosmon/state/fleets/default/molecules/delib-.../synthesis.md
────────────────────────────────────────────────────────────────────────
# Synthesis
…rendered markdown body…
────────────────────────────────────────────────────────────────────────
  ⏳ torn down delib-20260415-xyz (branch merged)

✨ demo finished in 151.8s — molecule delib-20260415-xyz preserved at
   .cosmon/state/**/delib-20260415-xyz/
$
```

Flags:

- `--prompt <TEXT>` — skip the interactive TTY read (for scripts, CI, headless).
- `--formula <NAME>` — override auto-classification; must be an existing
  `.cosmon/formulas/<name>.formula.toml`.
- `--no-teardown` — leave worktree / tmux / fleet worker intact.
- `--timeout <SECONDS>` — bounded wait (default 600s, mirrors `cs wait`).
- `--json` — NDJSON event stream for scripting.

## 2. Vs. the four-verb cycle

| Aspect | Manual cycle | `cs demo` |
|-------:|:-------------|:----------|
| Invocations | 4 (`nucleate`, `tackle`, `wait`, `done`) | 1 |
| Prompt ergonomics | `--var topic=...` / `--var question=...` | positional conversational input |
| Formula choice | explicit | auto-classified (override via flag) |
| Progress feedback | separate commands | inlined |
| Synthesis render | `cat .cosmon/state/.../synthesis.md` | printed in terminal |
| Teardown | manual | automatic |

`cs demo` is a **thin orchestrator**, not a new surface. Underneath, the
exact same `nucleate → tackle → wait → done` cycle runs, emits the same
events, produces the same on-disk artefacts.

## 3. Invariants respected

- **Stateless CLI** — no daemon, no persistent thread: foreground process
  walks the existing verbs, then exits (ADR-016 L1).
- **Zero new state** — `.cosmon/state/**/<mol-id>/` receives the same
  files (`prompt.md`, `briefing.md`, `synthesis.md`, `events.jsonl`,
  per-step git commits) as any other molecule.
- **Formula-driven** — no hidden demo-only codepath. Auto-classification
  selects one of the existing formulas (`deep-think`, `task-work`,
  `idea-to-plan`). Operators disagree by passing `--formula`.
- **Composability principle** — zero new extension points (not a new
  molecule kind, not a new step type, not a new tag namespace).
- **CLI over MCP for workers** — orchestration is done by re-invoking
  `cs` as a subprocess via `current_exe()`; the same binary enforces the
  same invariants end-to-end.
- **Merge-before-dispatch** respected: `cs done` still runs post-wait.
- **Zero I/O in core** — `cs demo` lives strictly in `cosmon-cli`.

## 4. Architectural decisions

- **Location**: new module `crates/cosmon-cli/src/cmd/demo.rs`. No core
  changes, no trait additions.
- **Orchestration**: subprocess invocation of `cs <verb>` via
  `std::process::Command::new(current_exe())`. This avoids re-entering
  complex `clap::Args` shapes programmatically, keeps each verb
  independently testable, and preserves fault isolation (a `tackle`
  failure fails the demo cleanly without poisoning the parent state).
- **Progress UI**: stdout/stderr inherit. `cs tackle` and `cs wait` already
  print human-readable status lines; `cs demo` adds a thin chatbot banner
  and per-step progress markers. No `indicatif`, no `termimad` — zero
  new dependencies.
- **Synthesis render**: best-effort resolution of
  `.cosmon/state/fleets/<fleet>/molecules/<mol-id>/{synthesis.md ↓
  briefing.md ↓ prompt.md}` with simple section headers. If the molecule
  produces no synthesis (e.g. a task-work), we still print the briefing
  so the user sees *something*.
- **Formula classification** (heuristic, not ML):
  - question mark / `what|why|how|should|is|are|est-ce|pourquoi|comment …` → `deep-think`
  - imperative `implement|add|fix|refactor|build|write|create|update|remove|delete|rename` → `task-work`
  - otherwise → `idea-to-plan`
- **Variable key mapping**: `deep-think` → `question`, everything else → `topic`.
  Mirrors the existing formula conventions; extracted as
  `formula_variable_key` so tests can assert parity without TOML parsing.

## 5. Scope boundaries

Out of scope (explicit non-goals):

- No GUI, no TUI chatbot, no web UI. `cs demo` stays CLI.
- No multi-turn conversation — that is `cs nucleate` cycling, not demo.
- No persistent demo history — the molecule *is* the history.
- No new molecule kind or formula type — demo consumes what exists.
- No classifier upgrades (ML, LLM round-trip). Keep the heuristic crude
  and document the `--formula` override.

## 6. Progression

Phase 0 (this change) — minimal wedge: 1-line prompt, three formulas,
raw markdown render, subprocess orchestration, integration test.

Phase 1 (follow-up) — optional `termimad` render with a feature flag,
richer progress stream (poll `events.jsonl` tail), `--list-formulas`
helper.

Phase 2 (much later) — demo-local formula shortcuts (`cs demo ask`, `cs
demo build`) if usage patterns warrant. Not before.

## 7. Testing strategy

- **Unit tests** — `classify_prompt`, `formula_variable_key`, empty-prompt
  rejection. Colocated in `crates/cosmon-cli/src/cmd/demo.rs`.
- **Integration test** — `crates/cosmon-cli/tests/demo.rs` runs
  `cs demo --prompt "…" --formula task-work --no-teardown` in a temp
  `.cosmon/` scaffold with a stub task formula, asserts the molecule is
  nucleated and marked complete, and that the artefact files exist.
  Full nucleate→tackle→wait→done round-trip is exercised by the
  Gas Town smoke-test formula in the normal CI path.
