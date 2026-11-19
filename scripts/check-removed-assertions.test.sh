#!/usr/bin/env bash
# check-removed-assertions.test.sh — tests for scripts/check-removed-assertions.sh
# (the CI Assert Guard decision). Builds throwaway git repositories; identities
# are set locally inside them, never globally.
set -uo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
GUARD="$HERE/check-removed-assertions.sh"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

failures=0
ok()  { echo "PASS: $*"; }
bad() { echo "FAIL: $*" >&2; failures=$((failures+1)); }

# new_repo <dir>: repo with main holding a.rs with two assertions.
new_repo() {
  mkdir -p "$1" && cd "$1" || exit 2
  git init -q -b main
  git config user.email t@example.invalid
  git config user.name T
  git config commit.gpgsign false
  printf '#[test]\nfn t() {\n    assert_eq!(1, 1);\n    assert!(true);\n}\n' > a.rs
  git add a.rs && git commit -qm base
}

# --- 1. base gains assertions after the fork; branch removes none -> pass ----
new_repo "$WORK/r1"
git checkout -q -b feature
echo 'fn unrelated() {}' > b.rs && git add b.rs && git commit -qm feature
git checkout -q main
printf '#[test]\nfn u() {\n    assert_eq!(2, 2);\n    assert_ne!(1, 2);\n}\n' > c.rs
git add c.rs && git commit -qm "other PR merged"
out=$(PR_BODY="" bash "$GUARD" main feature 2>&1); rc=$?
if [ $rc -eq 0 ]; then ok "assertions added on base after the fork are not counted"
else bad "false positive on base-side additions (rc=$rc): $out"; fi

# --- 2. branch removes an assertion -> fail, line listed ---------------------
new_repo "$WORK/r2"
git checkout -q -b feature
sed -i.bak '/assert!(true)/d' a.rs && rm a.rs.bak
git commit -qam "drop assertion"
out=$(PR_BODY="" bash "$GUARD" main feature 2>&1); rc=$?
if [ $rc -eq 1 ] && printf '%s' "$out" | grep -qF -- '-    assert!(true);'
then ok "removed assertion fails and is listed"
else bad "expected rc=1 with the line listed (rc=$rc): $out"; fi

# --- 3. same, with the override marker -> pass -------------------------------
out=$(PR_BODY="redundant.
assert-guard: reviewed" bash "$GUARD" main feature 2>&1); rc=$?
if [ $rc -eq 0 ] && printf '%s' "$out" | grep -q 'Override marker found'
then ok "override marker accepted"
else bad "expected rc=0 with marker (rc=$rc): $out"; fi

# --- 4. hostile body is data -------------------------------------------------
sentinel="$WORK/pwned"
body="\`touch $sentinel\` \$(touch $sentinel) 'q' \"dq\"
assert-guard: reviewed"
out=$(PR_BODY="$body" bash "$GUARD" main feature 2>&1); rc=$?
if [ $rc -eq 0 ] && [ ! -e "$sentinel" ]; then ok "body with backticks, quotes, newlines is data"
else bad "hostile body mishandled (rc=$rc, sentinel exists: $([ -e "$sentinel" ] && echo yes || echo no))"; fi
body="\`touch $sentinel\` \$(touch $sentinel)"
PR_BODY="$body" bash "$GUARD" main feature >/dev/null 2>&1; rc=$?
if [ $rc -eq 1 ] && [ ! -e "$sentinel" ]; then ok "hostile body without marker still fails, nothing runs"
else bad "hostile body without marker (rc=$rc)"; fi

# Remove one line at a time so a different match cannot mask the result.
case_number=0
for case_line in 'debug_assert!(true);' 'custom_assert!(true);' '#[should_panic]' 'assert_eq !(1, 1);'; do
  case_number=$((case_number + 1))
  new_repo "$WORK/macro-$case_number"
  printf '%s\n' "$case_line" > extra.rs
  git add extra.rs && git commit -qm "add candidate"
  git checkout -qb feature
  git rm -q extra.rs && git commit -qm "remove candidate"
  out=$(PR_BODY="" bash "$GUARD" main feature 2>&1); rc=$?
  if [ "$case_line" = 'custom_assert!(true);' ]; then
    [ "$rc" -eq 0 ] && ok "identifier ending in assert is ignored" || bad "suffix matched (rc=$rc): $out"
  elif [ "$rc" -eq 1 ] && printf '%s\n' "$out" | grep -qFx -- "-$case_line"; then
    ok "removed $case_line is listed"
  else
    bad "expected rc=1 with $case_line listed (rc=$rc): $out"
  fi
done

bash "$GUARD" main >/dev/null 2>&1; rc=$?
[ "$rc" -eq 2 ] && ok "missing revision exits 2" || bad "missing revision exited $rc"
bash "$GUARD" main no-such-revision >/dev/null 2>&1; rc=$?
[ "$rc" -eq 2 ] && ok "unresolvable revision exits 2" || bad "unresolvable revision exited $rc"

[ "$failures" -eq 0 ] && { echo "check-removed-assertions.test: OK"; exit 0; }
echo "check-removed-assertions.test: $failures failure(s)" >&2
exit 1
