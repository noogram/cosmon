#!/usr/bin/env bash
# Workflow YAML gate: a workflow GitHub cannot parse creates zero jobs and
# reports as a bare path — every gate below it silently never runs. Measured
# 2026-08-04: an unquoted `prerequisite:` in a step name did exactly that.
#
# Second rule, same shape of damage: no `github.event.*` expression inside the
# body of a `run:` block. Actions substitutes expressions TEXTUALLY into the
# script before any shell parses it, and it does not know what a comment is —
# so an expression sitting in a `#` line is injected exactly like one sitting
# in code. A PR body is attacker-controlled, multi-line, and may hold quotes:
# everything after its first line leaves the comment and becomes code, and an
# apostrophe ends the job at parse time. Measured 2026-08-11, PR #47, run
# 31498866585: `line 76: unexpected EOF while looking for matching quote`,
# exit 2, on a branch that already carried the 2026-08-05 code-side fix — the
# only surviving occurrence was the explanatory comment. Event data must enter
# a step through its `env:` mapping, where it is a value and never source.
set -euo pipefail

dir="${COSMON_WORKFLOW_DIR:-.github/workflows}"

rc=0
for f in "$dir"/*.yml "$dir"/*.yaml; do
  [ -e "$f" ] || continue
  if ! python3 - "$f" <<'PY'
import re
import sys

import yaml

path = sys.argv[1]
raw = open(path, encoding="utf-8").read()

try:
    doc = yaml.safe_load(raw)
except Exception as exc:  # noqa: BLE001 — any parse failure is the finding
    print(f"workflow-lint: FAIL {path}")
    print("    " + str(exc).replace("\n", "\n    "))
    sys.exit(1)

# `github.event.…` written anywhere inside a run body — code or comment. The
# `env:` mapping of a step is a sibling key, so it is out of reach by
# construction and needs no exemption.
EXPR = re.compile(r"\$\{\{[^}]*\bgithub\.event\b[^}]*\}\}")

lines = raw.splitlines()


def line_of(needle, start=0):
    """1-based file line holding `needle`, or 0 when it cannot be located."""
    for i in range(start, len(lines)):
        if needle in lines[i]:
            return i + 1
    return 0


findings = []
for job_name, job in (doc.get("jobs") or {}).items():
    if not isinstance(job, dict):
        continue
    for step in job.get("steps") or []:
        if not isinstance(step, dict):
            continue
        script = step.get("run")
        if not isinstance(script, str):
            continue
        label = step.get("name") or step.get("uses") or "<unnamed step>"
        for script_line in script.splitlines():
            for match in EXPR.finditer(script_line):
                findings.append((job_name, label, line_of(script_line), match.group(0)))

if findings:
    print(f"workflow-lint: FAIL {path}")
    for job_name, label, lineno, expr in findings:
        where = f"{path}:{lineno}" if lineno else path
        print(f"    {where}  job '{job_name}', step '{label}'")
        print(f"        event expression in a run: body — {expr}")
    print("    Actions substitutes expressions textually before the shell")
    print("    parses the script, comments included. Pass the value through")
    print("    the step's env: mapping and read it as a variable; rewrite any")
    print("    explanation so it does not spell the expression out.")
    sys.exit(1)
PY
  then
    rc=1
  fi
done
[ "$rc" = 0 ] && echo "workflow-lint: OK — every workflow parses, no event expression in a run: body"
exit $rc
