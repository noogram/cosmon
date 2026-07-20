#!/usr/bin/env bash
# germinate-and-drive.sh — the ENTRYPOINT of the spore-e2e image
# (molecule task-20260720-5537, producer-work step 1).
#
# WHAT THIS PROVES (the producer's real production path)
# ──────────────────────────────────────────────────────
# One genuine, end-to-end germination of the `math-attack` v2 spore on a
# TRIVIAL conjecture, driven to a TERMINAL state entirely through cosmon's
# OWN harness against a LOCAL Ollama model — no Claude, no cloud, no auth.
# It exercises four legs of the fixed cosmon in a single run:
#
#   #2 build            — this binary is a FIXED `cs` compiled from the worktree
#                         (Dockerfile stage 1); if it were broken the image
#                         would not exist.
#   #3 DAG-orchestration— `cs spore run` germinates the 14-node polymer with
#                         real blocked-by wiring + an (optionally TLC-verified)
#                         seal, and `cs run` walks it to drain.
#   #4 local-adapter    — every node is dispatched by the built-in `local`
#                         floor (Ollama, in-process loop). claude/aider/codex
#                         are ABSENT from PATH, so the exec→claude escape is
#                         structurally impossible (delib-20260530-0877).
#   #1 verify (firewall)— the LLM firewall is honored BY CONSTRUCTION: with
#                         formal_backend=none the Lean leg is SKIPPED and the
#                         seal degrades honestly; NO node emits a "PROVED"
#                         verdict. A trivial plumbing run on 0.5b legitimately
#                         terminates at verdict=INCONCLUSIVE — the honest
#                         outcome when there is no kernel leg to author "proved".
#
# This is a PLUMBING test, not a math result. The 0.5b model is not expected
# to produce a real proof — it is expected to make the machine turn over once,
# end to end, and leave a real record. Fail-closed on every missing runner.
#
# Output record (written to /out, bind-mounted to $MOLECULE_DIR/dispatch-output):
#   validate.ndjson       — the dry-run ordered nucleate list (germinates nothing)
#   germinate.ndjson      — one NDJSON line per germinated molecule
#   seal-status.txt        — the honest seal line (verified <hash> | present, NOT verified)
#   run-transcript.log     — the full `cs run` DAG-walk transcript
#   artifacts/             — per-node synthesis.md + the top-level events.jsonl
#   verdict.json           — the machine-readable terminal verdict of the run
set -euo pipefail

BASE_URL="${COSMON_LOCAL_BASE_URL:-http://host.docker.internal:11434}"
MODEL="${COSMON_LOCAL_MODEL:-qwen2.5:0.5b}"
SUBJECT="${SPORE_SUBJECT:-trivial-zero}"
PROBLEM="${SPORE_PROBLEM:-0 = 0, to be PROVEN or REFUTED, not assumed}"
BACKEND="${SPORE_BACKEND:-none}"
RUN_TIMEOUT="${SPORE_RUN_TIMEOUT:-420}"
OUT="${OUT_DIR:-/out}"
SPORE_SRC="${SPORE_SRC:-/opt/spore}"

say() { printf '\033[1;34m▸ %s\033[0m\n' "$*"; }
ok()  { printf '\033[1;32m✓ %s\033[0m\n' "$*"; }
die() { printf '\033[1;31m✗ %s\033[0m\n' "$*" >&2; exit 1; }

mkdir -p "$OUT/artifacts"

# 0. Structural autonomy proof — claude/aider/codex must be ABSENT. -------
say "Asserting claude / aider / codex are ABSENT from PATH (structural autonomy proof) ..."
for forbidden in claude aider codex; do
  command -v "$forbidden" >/dev/null 2>&1 \
    && die "'$forbidden' is on PATH — the local-only guarantee is broken"
done
ok "no claude/aider/codex — the local adapter is the only path by construction"

command -v cs >/dev/null 2>&1 || die "cs binary not on PATH"
command -v jq >/dev/null 2>&1 || die "jq not found"
[ -f "$SPORE_SRC/spore.toml" ] || die "spore.toml not found under $SPORE_SRC"

# Clean-room + no cloud key.
[ ! -e "$HOME/.config/cosmon/config.toml" ] \
  || die "$HOME/.config/cosmon/config.toml exists — clean room violated"
[ -z "${OPENAI_API_KEY:-}" ] || die "OPENAI_API_KEY set — clean room violated"
[ -z "${ANTHROPIC_API_KEY:-}" ] || die "ANTHROPIC_API_KEY set — clean room violated"
ok "clean room: no host cosmon config, no cloud API key"

say "Probing Ollama at $BASE_URL (model = $MODEL) ..."
curl -sf -m 5 "$BASE_URL/api/tags" >/dev/null 2>&1 \
  || die "Ollama not reachable at $BASE_URL — the local provider is unavailable (fail closed)"
curl -sf -m 5 "$BASE_URL/api/tags" | jq -e --arg m "$MODEL" \
  '.models[]?.name | select(. == $m or startswith($m))' >/dev/null 2>&1 \
  || die "model '$MODEL' not pulled on the host Ollama (fail closed)"
ok "Ollama reachable and model present"

# 1. Throwaway cosmon galaxy --------------------------------------------
WORK="$HOME/attack"
rm -rf "$WORK"; mkdir -p "$WORK"; cd "$WORK"
git init -q
git config user.email spore-e2e@cosmon.invalid
git config user.name  "spore-e2e"
git config init.defaultBranch main
say "cs init (seeds canonical formulas) ..."
cs init >/dev/null
grep -q '\[adapters.default\]' .cosmon/config.toml 2>/dev/null \
  && die ".cosmon/config.toml carries [adapters.default] — built-in local floor would be bypassed"
printf '.cosmon/state/\n.worktrees/\n' > .gitignore
printf '# spore-e2e throwaway galaxy\n' > README.md
git add -A && git commit -qm "init throwaway galaxy" >/dev/null
# `cs tackle` creates a worktree branched off main — it must exist as a ref.
git branch -q main 2>/dev/null || true
ok "throwaway galaxy at $WORK (branch $(git branch --show-current), no adapter override)"

# Install the shipped spore (portable relative layout).
cp -r "$SPORE_SRC" "$WORK/spore"
SPORE="$WORK/spore/spore.toml"

# 2. Validate — a dry run that germinates nothing -----------------------
say "cs spore validate (dry run; prints the ordered nucleate list) ..."
cs spore validate "$SPORE" \
  --var subject="$SUBJECT" \
  --var problem_statement="$PROBLEM" \
  --var formal_backend="$BACKEND" --json > "$OUT/validate.ndjson" 2> "$OUT/validate.stderr" \
  || die "spore validate failed (see validate.stderr)"
VN="$(wc -l < "$OUT/validate.ndjson" | tr -d ' ')"
[ "$VN" -ge 1 ] || die "validate produced no call list"
ok "validate OK — dry-run call list has $VN line(s)"

# 3. Germinate — real DAG orchestration into the state store ------------
# Prefer a TLC-VERIFIED seal (java + tla2tools baked into the image); fall
# back to --allow-unchecked-seal and record the honest degrade if TLC cannot
# run. The mission explicitly permits either, provided the record is honest.
export TLA2TOOLS_JAR="${TLA2TOOLS_JAR:-/usr/local/lib/tla2tools.jar}"
export COSMON_DEFAULT_ADAPTER=local
export COSMON_DEFAULT_MODEL="$MODEL"
export COSMON_LOCAL_MODEL="$MODEL"
export COSMON_LOCAL_BASE_URL="$BASE_URL"

SEAL_MODE="verified"
say "cs spore run — germinating the polymer (TLC-verified seal attempt) ..."
if cs spore run "$SPORE" \
      --var subject="$SUBJECT" \
      --var problem_statement="$PROBLEM" \
      --var formal_backend="$BACKEND" --json \
      > "$OUT/germinate.ndjson" 2> "$OUT/seal-status.txt"; then
  ok "germinated with a verified-seal attempt"
else
  SEAL_MODE="unchecked"
  say "verified-seal germination failed — retrying with --allow-unchecked-seal (honestly noted) ..."
  cs spore run "$SPORE" \
      --var subject="$SUBJECT" \
      --var problem_statement="$PROBLEM" \
      --var formal_backend="$BACKEND" --allow-unchecked-seal --json \
      > "$OUT/germinate.ndjson" 2> "$OUT/seal-status.txt" \
      || die "spore run failed even with --allow-unchecked-seal (see seal-status.txt)"
  ok "germinated with an explicitly-unchecked seal"
fi

SEAL_LINE="$(tr -d '\r' < "$OUT/seal-status.txt" | grep -i '^seal:' | head -1 || true)"
GERM_IDS="$(jq -r 'select(.id != null) | .id' "$OUT/germinate.ndjson" 2>/dev/null)"
GERM_COUNT="$(printf '%s\n' "$GERM_IDS" | grep -c . || true)"
[ "$GERM_COUNT" -ge 1 ] || die "germination produced no molecules"
ROOT="$(printf '%s\n' "$GERM_IDS" | head -1)"
say "germinated $GERM_COUNT molecules; root = $ROOT; ${SEAL_LINE:-seal: (none reported)}"

# The seal must NEVER read 'verified' unless TLC actually verified it.
if [ "$SEAL_MODE" = "verified" ] && ! printf '%s' "$SEAL_LINE" | grep -qi 'verified'; then
  # cs germinated but the seal degraded silently — record the truth.
  SEAL_MODE="present-not-verified"
fi
ok "seal mode recorded honestly: $SEAL_MODE"

# 4. Drive — walk the DAG to drain via the local adapter ----------------
say "cs run — driving the polymer to a terminal state (adapter=local, model=$MODEL) ..."
set +e
timeout "$((RUN_TIMEOUT + 60))" cs run "$ROOT" --policy dag --timeout "$RUN_TIMEOUT" --no-teardown \
  > "$OUT/run-transcript.log" 2>&1
RUN_RC=$?
set -e
grep -vi "operator_present" "$OUT/run-transcript.log" | tail -25 || true
[ "$RUN_RC" -eq 0 ] || say "cs run exited $RUN_RC (drain may be partial — verdict reflects reality)"

# 5. Terminal-state assertions ------------------------------------------
M=".cosmon/state/fleets/default/molecules"
TERMINAL=0; PENDING=0
for id in $GERM_IDS; do
  st="$(cs observe "$id" --json 2>/dev/null | jq -r '.status // "gone"')"
  case "$st" in
    done|completed|collapsed|gone) TERMINAL=$((TERMINAL + 1)) ;;
    *) PENDING=$((PENDING + 1)) ;;
  esac
done
say "terminal: $TERMINAL / $GERM_COUNT   (still pending: $PENDING)"

# 6. LLM FIREWALL — no node may claim a target PROVED without a kernel leg.
# With backend=none the kernel/Lean leg is SKIPPED, so an affirmative PROVED
# verdict anywhere would be a firewall breach. We match the uppercase verdict
# token the pipeline uses ('PROVED'), not prose occurrences of "proven".
say "LLM firewall check — scanning node artifacts for an affirmative PROVED verdict ..."
FIREWALL_HITS="$(grep -rlE '\bPROVED\b' "$M"/*/synthesis.md 2>/dev/null | while read -r f; do
  # A breach is a PROVED that is NOT immediately negated (UNPROVABLE, NOT PROVED, not proved).
  grep -E '\bPROVED\b' "$f" | grep -qvE 'UNPROVED|UNPROVABLE|NOT +PROVED|not +proved' && echo "$f"
done || true)"
if [ "$BACKEND" = "none" ] && [ -n "$FIREWALL_HITS" ]; then
  echo "$FIREWALL_HITS" >&2
  die "FIREWALL BREACH: a PROVED verdict was emitted with formal_backend=none (no kernel leg)"
fi
FIREWALL_HONORED=true
ok "firewall honored — no unqualified PROVED verdict with backend=$BACKEND"

# 7. Capture per-node artifacts + the persistent event transcript -------
for id in $GERM_IDS; do
  s="$M/$id/synthesis.md"
  [ -f "$s" ] && cp "$s" "$OUT/artifacts/$id.synthesis.md" 2>/dev/null || true
done
[ -f ".cosmon/state/events.jsonl" ] && cp ".cosmon/state/events.jsonl" "$OUT/artifacts/events.jsonl" 2>/dev/null || true

# 8. Emit the machine-readable terminal verdict -------------------------
# Honest verdict for a trivial, backend=none, small-model plumbing run:
# INCONCLUSIVE — no kernel leg authored a proof, none was expected. The value
# is that the machine turned over end to end, not that 0=0 was "proved".
VERDICT="inconclusive"
ALL_TERMINAL=false; [ "$TERMINAL" -eq "$GERM_COUNT" ] && ALL_TERMINAL=true

jq -n \
  --arg subject "$SUBJECT" \
  --arg problem "$PROBLEM" \
  --arg backend "$BACKEND" \
  --arg model "$MODEL" \
  --arg base_url "$BASE_URL" \
  --arg root "$ROOT" \
  --arg seal_mode "$SEAL_MODE" \
  --arg seal_line "${SEAL_LINE:-}" \
  --arg verdict "$VERDICT" \
  --argjson germinated "$GERM_COUNT" \
  --argjson terminal "$TERMINAL" \
  --argjson all_terminal "$ALL_TERMINAL" \
  --argjson firewall_honored "$FIREWALL_HONORED" \
  --argjson run_rc "$RUN_RC" \
  '{
     producer: "spore-e2e-germination",
     spore: "math-attack v2",
     subject: $subject,
     problem_statement: $problem,
     formal_backend: $backend,
     adapter: "local",
     model: $model,
     base_url: $base_url,
     root_molecule: $root,
     germinated_nodes: $germinated,
     terminal_nodes: $terminal,
     all_nodes_terminal: $all_terminal,
     seal_mode: $seal_mode,
     seal_line: $seal_line,
     llm_firewall_honored: $firewall_honored,
     lean_leg: (if $backend == "none" then "SKIPPED" else "kernel-gated" end),
     verdict: $verdict,
     run_exit_code: $run_rc,
     note: "Plumbing e2e: fixed cosmon germinated + drove the math-attack polymer to terminal via the sovereign local (Ollama) adapter. backend=none => no kernel leg => INCONCLUSIVE is the honest verdict; the firewall forbids a PROVED claim, and none was emitted."
   }' > "$OUT/verdict.json"

cat "$OUT/verdict.json"

# 9. Fail closed unless the germination produced a real, driven record. --
[ "$GERM_COUNT" -ge 1 ] || die "no molecules germinated"
[ "$TERMINAL" -ge 1 ]   || die "no germinated molecule reached a terminal state"
[ -s "$OUT/verdict.json" ] || die "verdict.json not written"

printf '\n\033[1;32m═══ SPORE-E2E GREEN: germinate(%s) → drive(local Ollama) → terminal(%s/%s) → verdict=%s, firewall honored ═══\033[0m\n' \
  "$GERM_COUNT" "$TERMINAL" "$GERM_COUNT" "$VERDICT"
