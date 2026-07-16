#!/usr/bin/env bash
# check-galaxy-complete.sh — verify a directory looks like a complete cosmon galaxy.
#
# Usage: scripts/check-galaxy-complete.sh <galaxy_path> [--galaxy-name NAME]
#
# Exit 0  — every required check passed.
# Exit 1  — at least one required check failed.
# Exit 2  — usage error.
#
# Required checks (warnings only for THESIS.md / ROADMAP.md):
#   - <galaxy_path>/CLAUDE.md exists
#   - <galaxy_path>/.cosmon/config.toml exists
#   - <galaxy_path>/.git/ exists with at least one commit (rev-parse HEAD succeeds)
#   - <galaxy_path>/docs/lore/CHRONICLES.md exists with at least one dated entry
#   - neurion `repos` table contains a row whose local_path matches <galaxy_path>
#     (skipped with a warning if neurion is unreachable — neurion is a separate
#     service, this script must work offline)
#
# Origin: task-20260428-ff86 (anti-recidive of "Le trou doctrinal du brief de
# bootstrap", 2026-04-29). The script is the executable form of
# galaxy-onboarding's invariants — useful for post-bootstrap audit, CI checks,
# or a `cs nucleate galaxy-audit` formula in the future.

set -u

if [[ $# -lt 1 || $# -gt 3 ]]; then
  echo "usage: $0 <galaxy_path> [--galaxy-name NAME]" >&2
  exit 2
fi

GALAXY_PATH="$1"
shift
GALAXY_NAME=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --galaxy-name)
      GALAXY_NAME="${2:-}"
      shift 2
      ;;
    *)
      echo "unknown argument: $1" >&2
      exit 2
      ;;
  esac
done

if [[ ! -d "$GALAXY_PATH" ]]; then
  echo "FAIL: $GALAXY_PATH does not exist or is not a directory" >&2
  exit 1
fi

# Resolve to absolute path for neurion comparison.
GALAXY_PATH_ABS="$(cd "$GALAXY_PATH" && pwd)"

fails=0
warns=0

check() {
  local label="$1"
  local cond_msg="$2"
  if [[ "$3" == "ok" ]]; then
    printf "  ✓ %s\n" "$label"
  else
    printf "  ✗ %s — %s\n" "$label" "$cond_msg" >&2
    fails=$((fails + 1))
  fi
}

warn() {
  local label="$1"
  local cond_msg="$2"
  printf "  ! %s — %s (warning)\n" "$label" "$cond_msg" >&2
  warns=$((warns + 1))
}

echo "Galaxy completeness check — $GALAXY_PATH_ABS"

# 1. CLAUDE.md
if [[ -f "$GALAXY_PATH_ABS/CLAUDE.md" ]]; then
  check "CLAUDE.md present" "" ok
else
  check "CLAUDE.md present" "missing — formula 'galaxy-onboarding' would create it" miss
fi

# 2. .cosmon/config.toml
if [[ -f "$GALAXY_PATH_ABS/.cosmon/config.toml" ]]; then
  check ".cosmon/ bootstrapped" "" ok
else
  check ".cosmon/ bootstrapped" "missing — run 'cs init $GALAXY_PATH_ABS'" miss
fi

# 3. git initialized with at least one commit
if [[ -d "$GALAXY_PATH_ABS/.git" ]] && git -C "$GALAXY_PATH_ABS" rev-parse HEAD >/dev/null 2>&1; then
  check "git repo with at least one commit" "" ok
else
  check "git repo with at least one commit" "missing — run 'git init && git commit' in $GALAXY_PATH_ABS" miss
fi

# 4. docs/lore/CHRONICLES.md
chronicles_path="$GALAXY_PATH_ABS/docs/lore/CHRONICLES.md"
if [[ -f "$chronicles_path" ]]; then
  if grep -qE '^## [0-9]{4}-[0-9]{2}-[0-9]{2}' "$chronicles_path"; then
    check "docs/lore/CHRONICLES.md with dated entry" "" ok
  else
    check "docs/lore/CHRONICLES.md with dated entry" "file present but no '## YYYY-MM-DD' entry" miss
  fi
else
  check "docs/lore/CHRONICLES.md with dated entry" "missing" miss
fi

# 5. neurion registration — best-effort, warn if neurion DB not found.
# Neurion is an MCP-stdio server; for a shell-script audit we read its
# SQLite DB directly. Standard location is
# ~/Library/Application Support/neurion/neurion.db on macOS, with a
# fallback to ~/.config/neurion/neurion.db (linux-style).
if [[ -z "$GALAXY_NAME" ]]; then
  GALAXY_NAME="$(basename "$GALAXY_PATH_ABS")"
fi

NEURION_DB=""
for candidate in \
    "$HOME/Library/Application Support/neurion/neurion.db" \
    "$HOME/.config/neurion/neurion.db" \
    "$HOME/.local/share/neurion/neurion.db"; do
  if [[ -f "$candidate" ]]; then
    NEURION_DB="$candidate"
    break
  fi
done

if [[ -n "$NEURION_DB" ]] && command -v sqlite3 >/dev/null 2>&1; then
  neurion_row=$(sqlite3 "$NEURION_DB" \
    "SELECT local_path FROM repos WHERE name = '$GALAXY_NAME'" 2>/dev/null || true)
  if [[ "$neurion_row" == "$GALAXY_PATH_ABS" ]]; then
    check "neurion repos table has row for $GALAXY_NAME" "" ok
  elif [[ -n "$neurion_row" ]]; then
    check "neurion repos table has row for $GALAXY_NAME" \
      "row exists but local_path '$neurion_row' != '$GALAXY_PATH_ABS'" miss
  else
    check "neurion repos table has row for $GALAXY_NAME" \
      "no row — register via 'neurion.upsert_entry' MCP tool" miss
  fi
else
  warn "neurion DB or sqlite3 not found" \
    "skipping neurion registration check (use the MCP tool to verify manually)"
fi

echo
if [[ $fails -eq 0 ]]; then
  echo "OK — galaxy looks complete (warnings: $warns)"
  exit 0
else
  echo "FAIL — $fails required check(s) failed (warnings: $warns)"
  echo "Hint: run 'cs nucleate galaxy-onboarding --var galaxy_path=$GALAXY_PATH_ABS --var galaxy_name=$GALAXY_NAME --var one_line=\"...\"' to repair."
  exit 1
fi
