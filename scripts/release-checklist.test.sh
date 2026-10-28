#!/usr/bin/env bash
# Regression test for post-flip required status check count normalization.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
mkdir -p "$TMP/bin"

cat >"$TMP/bin/gh" <<'GH'
#!/usr/bin/env bash
set -u
case "${2:-}" in
  repos/noogram/cosmon/branches/main/protection)
    exit 0
    ;;
  repos/noogram/cosmon/branches/main/protection/required_status_checks)
    case "${GH_CHECKS_CASE:-404}" in
      404) printf 'partial response\n'; exit 1 ;;
      empty) exit 0 ;;
      malformed) printf 'not-a-number\n'; exit 0 ;;
    esac
    ;;
esac
exit 1
GH
chmod +x "$TMP/bin/gh"

# Run the exact gate 11 block from the production script with only its output
# helpers stubbed, keeping unrelated release checks out of this fast test.
{
  cat <<'HEAD'
#!/usr/bin/env bash
set -uo pipefail
REPO_SLUG=noogram/cosmon
POST_FLIP=1
FAILS=0
PENDS=0
gate_pass() { printf '[GATE PASS] %s\n' "$1"; }
gate_fail() { printf '[GATE FAIL] %s\n' "$1"; FAILS=$((FAILS+1)); }
gate_pend() { printf '[GATE PEND] %s\n' "$1"; PENDS=$((PENDS+1)); }
HEAD
  sed -n '/^# ── 11\./,/^# ── 12\./{ /^# ── 12\./d; p; }' \
    "$ROOT/scripts/release-checklist.sh"
  printf 'exit "$FAILS"\n'
} >"$TMP/gate-11.sh"

for response in 404 empty malformed; do
  stderr="$TMP/stderr"
  stdout="$TMP/stdout"
  if PATH="$TMP/bin:$PATH" GH_CHECKS_CASE="$response" \
      bash "$TMP/gate-11.sh" >"$stdout" 2>"$stderr"; then
    status=0
  else
    status=$?
  fi
  if grep -q 'integer expression expected' "$stderr"; then
    echo "FAIL: $response response emitted an integer-expression diagnostic" >&2
    exit 1
  fi
  if ! grep -q '\[GATE FAIL\] 11\. branch protection exists but lists 0 required checks' "$stdout"; then
    echo "FAIL: $response response did not fail gate 11 with zero required checks" >&2
    exit 1
  fi
  if [ "$status" -eq 0 ]; then
    echo "FAIL: $response response unexpectedly passed gate 11" >&2
    exit 1
  fi
done

echo "PASS: post-flip check count is normalized for 404, empty, and malformed responses"
