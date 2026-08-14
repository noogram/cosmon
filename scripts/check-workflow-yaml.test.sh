#!/usr/bin/env bash
# Self-test for scripts/check-workflow-yaml.sh and for the step it protects.
#
# Three cases, all offline:
#   1. negative control — an event expression in a run: COMMENT must FAIL the
#      guard. Without it the guard would be an assertion nobody ever sees fire,
#      and the comment occurrence is precisely the one that survived the
#      2026-08-05 fix and killed run 31498866585 six days later.
#   2. negative control — the same expression in run: CODE must FAIL.
#   3. the real ci.yml assert-guard step, rendered the way Actions renders it
#      (textual substitution of every expression) with a PR body that carries
#      an apostrophe AND a newline, must run to a clean exit 0.
# Case 3 fails on the pre-fix workflow with `unexpected EOF while looking for
# matching quote`, which is the exact message ph-lean's PR #47 got.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
repo="$(cd "$here/.." && pwd)"
guard="$here/check-workflow-yaml.sh"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

fail() { echo "check-workflow-yaml.test: FAIL — $*" >&2; exit 1; }

# ---------------------------------------------------------------------------
# 1 + 2. Negative controls: the guard must reject a planted expression.
# The bait is assembled at runtime so this test file never spells the
# expression out either — writing it is what triggers it.
# ---------------------------------------------------------------------------
open='${'; bait="${open}{ github.event.pull_request.body }}"

for placement in comment code; do
  mkdir -p "$tmp/$placement"
  case "$placement" in
    comment) body="          # explanation naming $bait, which is the trap"$'\n'"          echo hello" ;;
    code)    body="          echo \"$bait\"" ;;
  esac
  cat > "$tmp/$placement/bait.yml" <<YAML
name: bait
on: [pull_request]
jobs:
  bait:
    runs-on: ubuntu-latest
    steps:
      - name: planted in $placement
        run: |
$body
YAML
  if COSMON_WORKFLOW_DIR="$tmp/$placement" "$guard" >"$tmp/$placement.out" 2>&1; then
    cat "$tmp/$placement.out" >&2
    fail "guard accepted an event expression planted in a run: $placement"
  fi
  grep -q 'event expression in a run: body' "$tmp/$placement.out" \
    || fail "guard failed on the $placement bait without naming the rule"
done

# ---------------------------------------------------------------------------
# 3. The assert-guard step itself, on a body with an apostrophe and a newline.
# ---------------------------------------------------------------------------
work="$tmp/repo"
mkdir -p "$work"
git -C "$work" init -q
git -C "$work" config user.email ci@example.invalid
git -C "$work" config user.name CI
printf '#[test]\nfn t() { assert_eq!(1, 1); }\n' > "$work/a.rs"
git -C "$work" add a.rs
git -C "$work" -c commit.gpgsign=false commit -qm base
base="$(git -C "$work" rev-parse HEAD)"
printf '#[test]\nfn t() {}\n' > "$work/a.rs"
git -C "$work" -c commit.gpgsign=false commit -qam "drop the assertion"
head="$(git -C "$work" rev-parse HEAD)"

# A body a human would plausibly write: two lines, an apostrophe in each.
pr_body="Ce n'est pas une régression: l'assertion était redondante.
assert-guard: reviewed"

script="$tmp/step.sh"
BASE_SHA="$base" HEAD_SHA="$head" PR_BODY="$pr_body" \
python3 - "$repo/.github/workflows/ci.yml" "$script" <<'PY'
import os
import re
import sys

import yaml

workflow, out = sys.argv[1], sys.argv[2]
doc = yaml.safe_load(open(workflow, encoding="utf-8"))

step = next(
    s
    for s in doc["jobs"]["assert-guard"]["steps"]
    if s.get("name") == "Check for removed test assertions"
)

# The fake Actions context. Values are what the runner would substitute.
context = {
    "github.event.pull_request.body": os.environ["PR_BODY"],
    "github.event.pull_request.base.sha": os.environ["BASE_SHA"],
    "github.event.pull_request.head.sha": os.environ["HEAD_SHA"],
}


def render(text):
    """Substitute expressions the way Actions does: textually, no escaping."""

    def sub(match):
        key = match.group(1).strip()
        if key not in context:
            raise SystemExit(f"unmodelled expression in the step: {key}")
        return context[key]

    return re.sub(r"\$\{\{(.*?)\}\}", sub, text)


with open(out, "w", encoding="utf-8") as fh:
    fh.write("#!/usr/bin/env bash\n")
    for name, value in (step.get("env") or {}).items():
        # env: values are passed as values — quoting is the runner's job here,
        # and a single-quoted heredoc keeps this harness from re-injecting.
        fh.write(f"read -r -d '' {name} <<'COSMON_ENV_EOF' || true\n")
        fh.write(render(value) + "\nCOSMON_ENV_EOF\n")
        fh.write(f"export {name}\n")
    fh.write(render(step["run"]))
PY

chmod +x "$script"

# The 2026-08-11 failure was a SYNTAX error — the job died before evaluating
# anything — so parse the rendered script separately from running it. This
# assertion holds on any host.
if ! bash -n "$script" 2>"$tmp/parse.err"; then
  cat "$tmp/parse.err" >&2
  fail "rendered assert-guard step does not parse with an apostrophe in the body"
fi

if ! (cd "$work" && bash "$script" >"$tmp/step.out" 2>&1); then
  cat "$tmp/step.out" >&2
  fail "assert-guard step did not accept a marker in a body with an apostrophe and a newline"
fi

# The step detects removed assertions with `grep -P`, which BSD grep (macOS)
# does not have. On the ubuntu runner — the only host this step ever executes
# on — it does, and then the override path is reachable and asserted here.
if printf 'x' | grep -qP 'x' 2>/dev/null; then
  grep -q 'Override marker found' "$tmp/step.out" \
    || fail "assert-guard step exited 0 without reaching the override path"
  detail="override path exercised"
else
  detail="parse+exit only: this host's grep has no -P, so the step found no removals"
fi

echo "check-workflow-yaml.test: OK — bait rejected in comment and code; assert-guard survives an apostrophe ($detail)"
