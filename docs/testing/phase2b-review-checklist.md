# Phase 2b Review Checklist

## Scope

Phase 2b captures the **full runtime lifecycle** of cosmon under strace:
nucleate → tackle → wait → done, with real tmux, real Claude CLI, and
real Anthropic API calls. This extends Phase 2a (filesystem-only, no
network, no subprocess chains).

## Red Lines (hard failures)

- [ ] **No API key leakage** — `sk-ant-` must not appear in any sanitized
  trace, log, or committed file. Grep sweep: `grep -rE 'sk-ant-'`
- [ ] **No auth token leakage** — `Bearer`, `Authorization`, `x-api-key`
  values must be `REDACTED` in sanitized output
- [ ] **No repo URL leakage** — no `github.com/projects` or similar in
  sanitized traces
- [ ] **No commit hashes** that could identify the repo (container-internal
  hashes are OK — they're ephemeral)
- [ ] **No cosmon/noogram/Noogram** tokens in sanitized output
- [ ] **No physics verbs** (nucleate, evolve, tackle, etc.) in sanitized output
- [ ] **No host paths** (`/Users/...`, `/home/...`) in any output

## Sanitization Invariants

| Source Pattern | Replacement | Rationale |
|---------------|-------------|-----------|
| `sk-ant-*` | `REDACTED_API_KEY` | API credential |
| `Bearer *` | `Bearer REDACTED` | Auth header |
| `x-api-key: *` | `x-api-key: REDACTED` | Auth header |
| `Authorization: *` | `Authorization: REDACTED` | Auth header |
| `api.anthropic.com` | `api.vendor.example` | Vendor identity |
| `anthropic` | `vendor` | Vendor identity |
| `cs` (binary) | `tool` | Product identity |
| `nucleate` | `create` | Physics verb |
| `tackle` | `start` | Physics verb |
| `evolve` | `step` | Physics verb |
| `done` (verb) | `finish` | Physics verb |
| `wait` (verb) | `await` | Physics verb |
| `observe` | `inspect` | Physics verb |
| `reconcile` | `sync` | Physics verb |
| `ensemble` | `list` | Physics verb |
| `collapse` | `abort` | Physics verb |
| `.cosmon/` | `.tool/` | Product directory |
| `cosmon` | `tool` | Product name |
| `noogram` | `tool` | Product name |
| `Noogram/noogram` | `Project/project` | Company name |
| `molecule` | `entity` | Domain term |

## Workflow

```
1. Run capture:        bash scripts/tenant-demo-strace-test-phase2b.sh
2. Review raw traces:  ls phase2b-traces-raw/
3. Sanitize:           bash scripts/sanitize-traces-phase2b.sh
4. Verify (automatic): sanitizer runs grep sweep, exits non-zero on leak
5. Manual review:      spot-check evidence/traces-phase2b/ for context leaks
6. Verdict:            write verdict in evidence/traces-phase2b/traces-review-phase2b.md
```

## Manual Review Checklist

After the automated grep sweep passes:

- [ ] Open each sanitized trace and scan for context that reveals product
  identity even without explicit tokens (e.g., unique directory structures,
  unusual flag combinations)
- [ ] Check that network connect() calls show `api.vendor.example`, not
  the real hostname
- [ ] Verify tmux-pane-capture.log (if present) contains no sensitive
  conversation content or API keys
- [ ] Check JSON stdout files for any leaked metadata

## What Phase 2b Adds Over Phase 2a

| Dimension | Phase 2a | Phase 2b |
|-----------|----------|----------|
| Commands | nucleate, observe, reconcile, tag, ensemble | nucleate, tackle, wait, done |
| Subprocess chains | None (single process) | tmux → claude → node → HTTPS |
| Network | None | api.anthropic.com (HTTPS) |
| API credentials | None | ANTHROPIC_API_KEY (runtime) |
| Git operations | None | branch, worktree, merge |
| Trace families | `file` only | `file` + `network` |
| Risk surface | Low (no secrets) | High (API key in network calls) |

## Delivery

Sanitized traces go to `evidence/traces-phase2b/`. The review markdown
(`traces-review-phase2b.md`) must contain a verdict: **SAFE TO SEND** or
**NEEDS FURTHER REDACTION** with specific findings.
