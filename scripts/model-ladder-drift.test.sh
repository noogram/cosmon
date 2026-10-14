#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-only
# Synthetic offline CLI contract; every audit asserts consumer byte identity.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
exec python3 "$here/tests/model-ladder-drift/test_drift.py" "$@"
