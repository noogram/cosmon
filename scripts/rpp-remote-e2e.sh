#!/usr/bin/env bash
# scripts/rpp-remote-e2e.sh — entry point for the container-level e2e
# suite of the §8j Remote Pilot Port. The orchestration itself lives in
# `tests/e2e/` (pytest); this file is the stable command name that CI,
# the how-to page and muscle memory already point at.
#
#   bash scripts/rpp-remote-e2e.sh                  # the whole scenario
#   bash scripts/rpp-remote-e2e.sh -k healthz       # one test, alone
#   bash scripts/rpp-remote-e2e.sh --pdb            # break in on failure
#
# Every argument is passed through to pytest, so `-k`, `-x`, `--lf`,
# `--junitxml=…` and `--pdb` work from here unchanged. Read
# `tests/e2e/conftest.py` for the fixtures, the artefact layout and the
# exit codes (0 green, 1 a test failed, 2 a prerequisite is missing —
# there is no skip).
set -euo pipefail
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PY="${PYTHON:-python3}"

# A missing runner is a prerequisite failure (exit 2), not a red test and
# certainly not a skip — the same contract the suite itself keeps.
if ! "$PY" -c 'import pytest' >/dev/null 2>&1; then
  echo "error: pytest is not importable from \`$PY\`." >&2
  echo "       python3 -m venv .venv && . .venv/bin/activate" >&2
  echo "       pip install -r $REPO_ROOT/tests/e2e/requirements.txt" >&2
  exit 2
fi

exec "$PY" -m pytest "$REPO_ROOT/tests/e2e" "$@"
