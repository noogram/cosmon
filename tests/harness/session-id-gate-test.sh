#!/usr/bin/env bash
# session-id-gate-test.sh — self-test for scripts/check-no-session-ids.sh.
#
# The gate refuses agent-harness session identifiers in the surfaces no tree
# scan can see: commit messages, and a pull-request / issue body handed over as
# a file. A gate is only worth its line in CI if both directions are proven —
# it must RED on the thing it names and GREEN on the thing it does not — so
# every scenario below asserts an exit code, not an absence of noise.
#
# Scenarios:
#   1. Claude's shape in a commit message              → exit 1
#   2. The same commit without the trailer             → exit 0
#   3. A NON-Claude vendor shape (`Codex-Thread:`)     → exit 1  (rule is by CLASS)
#   4. A bare `Session-Id:` trailer                    → exit 1
#   5. A console deep link in the message body         → exit 1
#   6. The identifier in a PR body file                → exit 1
#   7. Trailers OUTSIDE the range are not judged       → exit 0  (history stays)
#   8. A commit that merely mentions the word session  → exit 0  (no false red)
#
# Scenario 7 is the one that keeps the 84 historical trailers on public `main`
# from reddening every future build: the gate judges a RANGE, never a walk of
# history.
#
# Exit codes: 0 every scenario passed · 1 a scenario failed · 2 setup error.

set -u

HERE="$(cd "$(dirname "$0")" && pwd)"
REPO="$(cd "$HERE/../.." && pwd)"
GATE="$REPO/scripts/check-no-session-ids.sh"
[ -x "$GATE" ] || { echo "harness error: $GATE not executable" >&2; exit 2; }

TMP="$(mktemp -d -t cosmon-session-id-XXXXXX)" || exit 2
trap 'rm -rf "$TMP"' EXIT
cd "$TMP" || exit 2

git init -q -b main
git config user.email "harness@cosmon.test"
git config user.name "cosmon harness"
echo seed > seed.txt && git add . && git commit -q -m "init"
BASE_MAIN="$(git rev-parse HEAD)"

passed=0; failed=0
# $1 label · $2 expected exit code · rest: the command
expect() {
  local label="$1" want="$2"; shift 2
  local rc=0
  "$@" >"$TMP/out.log" 2>&1 || rc=$?
  if [ "$rc" -eq "$want" ]; then
    echo "PASS  $label (exit $rc)"; passed=$((passed + 1))
  else
    echo "FAIL  $label (exit $rc, expected $want)"; sed 's/^/      /' "$TMP/out.log"
    failed=$((failed + 1))
  fi
}

# A commit on a scratch branch off main carrying $1 as its message; echoes the
# branch tip. Each scenario gets its own branch so ranges never interfere.
commit_with() {
  local name="$1" msg="$2"
  git checkout -q -B "$name" "$BASE_MAIN"
  echo "$name" > "$name.txt"; git add "$name.txt"
  git commit -q -m "$msg"
  git rev-parse HEAD
}

# 1 / 2 — the shape that motivated the rule, present then absent.
tip="$(commit_with dirty "$(printf 'feat: a change\n\nClaude-Session: https://claude.ai/code/session_0123456789abcdef\n')")"
expect "1. Claude-Session trailer is refused" 1 bash "$GATE" "$BASE_MAIN" "$tip"
tip="$(commit_with clean "$(printf 'feat: a change\n\nMerge provenance lives in the molecule id.\n')")"
expect "2. the same commit without the trailer passes" 0 bash "$GATE" "$BASE_MAIN" "$tip"

# 3 — by CLASS, not by vendor. A rule that only knows Claude's spelling is
#     defeated the first time a codex-piloted session invents its own.
tip="$(commit_with codex "$(printf 'feat: a change\n\nCodex-Thread: https://chatgpt.com/codex/threads/abc\n')")"
expect "3. a non-Claude vendor shape is refused" 1 bash "$GATE" "$BASE_MAIN" "$tip"

# 4 — the vendorless spelling.
tip="$(commit_with bare "$(printf 'feat: a change\n\nSession-Id: 01JYC5FTFWTGOGUHKOGJDM4X\n')")"
expect "4. a bare Session-Id trailer is refused" 1 bash "$GATE" "$BASE_MAIN" "$tip"

# 5 — not a trailer at all: a deep link in ordinary prose.
tip="$(commit_with prose "$(printf 'docs: note the discussion\n\nDecided in https://claude.ai/code/session_abcdef012345 after review.\n')")"
expect "5. a console deep link in the body is refused" 1 bash "$GATE" "$BASE_MAIN" "$tip"

# 6 — the PR/issue body surface, handed over as a file.
tip="$(commit_with bodyonly "$(printf 'feat: a change\n')")"
printf 'Closes #1.\n\nhttps://claude.ai/code/session_fedcba9876543210\n' > "$TMP/body.md"
expect "6. the identifier in a PR body is refused" 1 \
  env COSMON_SESSION_SCAN_BODY_FILE="$TMP/body.md" bash "$GATE" "$BASE_MAIN" "$tip"

# 7 — THE grandfathering property. A trailer that is already history is not in
#     scope, so the 84 on public main cannot red a build that adds nothing.
git checkout -q -B history "$BASE_MAIN"
echo old > old.txt && git add old.txt
git commit -q -m "$(printf 'feat: historical\n\nClaude-Session: https://claude.ai/code/session_deadbeefdeadbeef\n')"
HIST_TIP="$(git rev-parse HEAD)"
echo new > new.txt && git add new.txt
git commit -q -m "feat: a clean change on top"
expect "7. a trailer below the range base is not judged" 0 \
  bash "$GATE" "$HIST_TIP" "$(git rev-parse HEAD)"

# 8 — no false red. The word is not the identifier; a rule that fires on prose
#     teaches its operator to ignore it.
tip="$(commit_with wordy "$(printf 'fix(session): drop the stale session cache\n\nSee docs/adr/052 and https://github.com/noogram/cosmon/issues/48.\n')")"
expect "8. ordinary prose about sessions passes" 0 bash "$GATE" "$BASE_MAIN" "$tip"

echo
echo "session-id-gate-test: passed=$passed failed=$failed"
[ "$failed" -eq 0 ] || exit 1
