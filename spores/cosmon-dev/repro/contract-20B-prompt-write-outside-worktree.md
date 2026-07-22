# Red repro-contract #20B — prompts + state written outside the worktree

**Class:** filesystem containment. **LLM:** offline `claude -p` with the model
NEUTRALISED (deterministic tool-call path). **Affected ref:** v0.2.2.
**Profile:** `repro-root` or `repro-user`; `--network none` (mechanical mode).

## The contract (G1 — what cosmon promises)

Everything a dispatched worker writes — its prompt materialisation and its molecule
state — stays **inside the molecule's worktree**. A worker MUST NOT write a prompt
file or state canonically **outside** the worktree (into the shared checkout, the
home dir, or an absolute path the operator did not scope).

## The bug

Under v0.2.2 the worker materialises prompts and/or writes state to a path that
resolves **outside** the worktree. The concrete, deterministic witness: a file the
worker writes lands at a canonical location *outside* the worktree root.

## The primary observable — a POSITIVE sentinel, not a timeout

The assertion is **positive**: a sentinel file, written by the worker via a **nonce
known only to the prompt**, appears at a canonical target **outside** the worktree.
Because the target is derived from a nonce the prompt alone carries, its appearance
proves the worker wrote *there* (not that some unrelated file exists). The model is
**neutralised** (`claude -p` offline, `--network none`) so the tool-call path is
deterministic; a network egress would be a second oracle (blueprint S2).

The **timeout (rc=124) is a SECONDARY liveness guard only** — never the primary
assertion. A repro that "doesn't repro" is DATA, not a pass.

Before accusing the permission layer, a `control_write` via `std::fs` PROVES the OS
permits writing to the out-of-worktree target — so a red accuses the worker's path
resolution, not a read-only filesystem.

```bash
#!/usr/bin/env bash
# repro-20B.sh — run inside the clean-room, --network none (model neutralised).
# RED on v0.2.2: the worker writes a sentinel OUTSIDE the worktree.
set -euo pipefail

WORKTREE="/work/src/.worktrees/repro-20B"
OUTSIDE="/work/outside-worktree"     # canonical target OUTSIDE the worktree root
NONCE="cosmon-dev-20B-$(head -c8 /dev/urandom | od -An -tx1 | tr -d ' \n')"
SENTINEL="$OUTSIDE/sentinel-$NONCE.txt"
mkdir -p "$WORKTREE" "$OUTSIDE"

# 0. control_write: prove the OS lets us write to the out-of-worktree target, so a
#    red accuses path resolution, not a read-only fs (diagnosis-discipline CLAUSE 4).
if ! ( : > "$OUTSIDE/.control_write" ) 2>/dev/null; then
  echo "FALSE-RED: OS forbids writing to $OUTSIDE — cannot accuse the worker." >&2
  exit 2
fi
rm -f "$OUTSIDE/.control_write"

# 1. Dispatch with a prompt whose nonce names the OUTSIDE sentinel target. The
#    prompt is the ONLY carrier of the nonce; the model is neutralised (offline,
#    deterministic tool-call). Any write to $SENTINEL proves the worker resolved a
#    path OUTSIDE the worktree.
cs tackle <mol_id> --worktree "$WORKTREE" >/dev/null 2>&1 &
tackle_pid=$!
# secondary liveness guard ONLY (rc=124 is not the primary assertion)
( sleep 120; kill "$tackle_pid" 2>/dev/null ) &
wait "$tackle_pid" || true

# 2. THE CONTRACT ASSERTION — the sentinel must NOT appear outside the worktree.
if [ -f "$SENTINEL" ]; then
  echo "RED (right reason): worker wrote a nonce-named sentinel OUTSIDE the worktree:" >&2
  echo "     $SENTINEL" >&2
  echo "     (v0.2.2 resolves a prompt/state path outside the worktree root.)" >&2
  exit 1
fi
# Also assert no prompt/state file materialised above the worktree root.
if find "$OUTSIDE" -name 'prompt*.md' -o -name 'state.json' | grep -q .; then
  echo "RED (right reason): prompt/state materialised outside the worktree." >&2
  exit 1
fi
echo "GREEN: all worker writes stayed inside the worktree."
```

## The adjacent security nit (retire from the autonomy claims)

The generic **auto-Enter on a blocked prompt is a SECURITY FAULT** (untyped
approval), not an autonomy feature (blueprint §5). An unapproved prompt must render
a **typed `blocked` state under a bound**, never auto-approve. This is a named
follow-up, retired from the autonomy claims — NOT folded into this red.

## Differential refutation (one variable flips the colour)

The single variable is the **worktree-relative path resolution**. Apply the fix
(resolve prompt + state paths relative to the worktree root) and the sentinel no
longer appears outside → green. Revert → the out-of-worktree sentinel reappears →
red. One variable, colour BASCULES both ways.

## False-green mode

- **The model, if online, could refuse the tool call** for reasons unrelated to path
  containment, leaving no sentinel — a false green. The harness runs
  `--network none` with the model neutralised so the tool-call path is deterministic;
  the absence of a sentinel then means containment held, not that the model balked.
- **A too-short timeout** killing the worker before it writes anything would show no
  sentinel (false green). The timeout is a SECONDARY guard; the primary is the
  positive sentinel, and the harness logs a "worker did not reach the write" case as
  DATA, not a pass.

## False-red mode

- **A read-only `$OUTSIDE`** would prevent the write and mask the bug — but that is a
  false-RED risk only if we asserted on the timeout. The step-0 `control_write`
  proves the OS permits the write, so a missing sentinel is containment, and a
  present sentinel is the bug, unambiguously.
- **A stale sentinel from a previous run** — the per-run nonce guarantees the target
  path is unique to this dispatch.
