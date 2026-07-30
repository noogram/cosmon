#!/usr/bin/env bash
# Exercise the three outcomes of scripts/install-hooks.sh.
#
# The rule under test is a REFUSAL, and a refusal nothing exercises is a
# refusal that cannot fire. The case that matters is the third: an earlier
# version of this step printed "replacing…" and overwrote anyway, on a path
# that `[hooks].post_merge` runs unattended after every harvest.
#
# Hermetic: temp directories only, no git hook is ever written outside them.

set -uo pipefail
cd "$(dirname "$0")/.."
SCRIPT="$PWD/scripts/install-hooks.sh"
SRC_ROOT="$PWD"

pass=0; fail=0
ok()  { printf '  \033[32m✓\033[0m %s\n' "$1"; pass=$((pass+1)); }
ko()  { printf '  \033[31m✗\033[0m %s — %s\n' "$1" "$2"; fail=$((fail+1)); }

tmp="$(mktemp -d -t install-hooks-test-XXXXXX)"
trap 'rm -rf "$tmp"' EXIT

run() {  # run <dest-dir>  -> sets RC and OUT
    OUT=$(COSMON_HOOKS_SRC_ROOT="$SRC_ROOT" COSMON_HOOKS_DEST="$1" bash "$SCRIPT" 2>&1)
    RC=$?
}

echo "── install-hooks.sh ──────────────────────────────────────────────────"

# 1. absent → installs, green
d="$tmp/absent"; mkdir -p "$d"
run "$d"
[ "$RC" -eq 0 ] && [ -f "$d/pre-push" ] && [ -f "$d/prepare-commit-msg" ] \
    && ok "absent destination is installed" \
    || ko "absent destination is installed" "rc=$RC / files missing"
cmp -s "hooks/pre-push" "$d/pre-push" \
    && ok "installed content matches the tracked source" \
    || ko "installed content matches the tracked source" "content differs"

# 2. identical → no-op, green, mtime untouched
before=$(stat -f %m "$d/pre-push" 2>/dev/null || stat -c %Y "$d/pre-push")
sleep 1
run "$d"
after=$(stat -f %m "$d/pre-push" 2>/dev/null || stat -c %Y "$d/pre-push")
[ "$RC" -eq 0 ] && ok "identical destination is accepted" \
                || ko "identical destination is accepted" "rc=$RC"
[ "$before" = "$after" ] \
    && ok "identical destination is not rewritten (mtime unchanged)" \
    || ko "identical destination is not rewritten" "mtime moved $before -> $after"

# 3. different → REFUSES, non-zero, and leaves the local file alone
d2="$tmp/different"; mkdir -p "$d2"
printf '#!/bin/sh\n# someone else installed this\nexit 0\n' > "$d2/pre-push"
mine="$(cat "$d2/pre-push")"
run "$d2"
[ "$RC" -ne 0 ] && ok "differing destination is refused (non-zero)" \
                || ko "differing destination is refused" "rc=$RC — it did not fail"
[ "$(cat "$d2/pre-push")" = "$mine" ] \
    && ok "the differing local hook is left untouched" \
    || ko "the differing local hook is left untouched" "IT WAS OVERWRITTEN"
printf '%s' "$OUT" | grep -q "$d2/pre-push" \
    && ok "the refusal names the local path" \
    || ko "the refusal names the local path" "path absent from output"

# 4. the refusal does not stop the other hook from being reported honestly
[ -f "$d2/prepare-commit-msg" ] \
    && ok "an unaffected hook is still installed alongside the refusal" \
    || ko "an unaffected hook is still installed" "it was skipped"

echo "──────────────────────────────────────────────────────────────────────"
if [ "$fail" -eq 0 ]; then
    echo "install-hooks.test: $pass passed, 0 failed."
    exit 0
fi
echo "install-hooks.test: $pass passed, $fail FAILED." >&2
exit 1
