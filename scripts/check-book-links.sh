#!/usr/bin/env bash
# Fail closed on local mdBook links; HTTP(S) observations remain advisory.
set -euo pipefail

repo_root="$(git -C "$(dirname "${BASH_SOURCE[0]}")" rev-parse --show-toplevel)"
exec python3 "$repo_root/scripts/check-book-links.py" "$@"
