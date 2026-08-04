#!/usr/bin/env bash
# Workflow YAML gate: a workflow GitHub cannot parse creates zero jobs and
# reports as a bare path — every gate below it silently never runs. Measured
# 2026-08-04: an unquoted `prerequisite:` in a step name did exactly that.
set -euo pipefail
rc=0
for f in .github/workflows/*.yml .github/workflows/*.yaml; do
  [ -e "$f" ] || continue
  if ! python3 -c "import yaml,sys;yaml.safe_load(open(sys.argv[1]))" "$f" 2>/tmp/wf-lint.err; then
    echo "workflow-lint: FAIL $f"; sed 's/^/    /' /tmp/wf-lint.err; rc=1
  fi
done
[ "$rc" = 0 ] && echo "workflow-lint: OK — every workflow parses"
exit $rc
