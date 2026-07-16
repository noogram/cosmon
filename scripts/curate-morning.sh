#!/usr/bin/env bash
# curate-morning.sh — operator morning-review entry point for curate-patrol.
#
# Walks each galaxy declared in $COSMON_CURATE_CONFIG (default:
# ~/.config/cosmon/curate.toml), opens the latest curate-patrol
# `surfaced.md` for that galaxy in $EDITOR (or `obsidian-cli open` if
# OBSIDIAN_VAULT is set), waits for the operator to reply 1/2/3/later
# inline, then prompts "All replied?". Patrol applies verdicts on the
# next pass — this script NEVER auto-applies anything.
#
# Governing delib: delib-20260521-c3cd (synthesis §Operator-surface channel).
# Discipline:      global CLAUDE.md "One question, one decision".
# Sibling deliverables:
#   - ops/templates/curate-surfaced.md (template the patrol fills)
#   - .cosmon/formulas/curate-patrol.formula.toml (the patrol itself —
#     owned by sibling task-20260521-... C-CURATE-FORMULA)
#   - ~/.config/cosmon/curate.toml (allowlist — owned by sibling
#     task-20260521-... C-CURATE-LAUNCHAGENT)
#
# Exit codes:
#   0  — operator processed every galaxy's surfaced.md (or none existed)
#   1  — config file missing
#   2  — bad argument
#
# Usage:
#   curate-morning.sh                # walk all galaxies in curate.toml
#   curate-morning.sh --galaxy cosmon
#   curate-morning.sh --dry-run      # print what would open, do nothing
#   curate-morning.sh --help

set -u  # NOT -e: we want to walk through every galaxy even if one fails.

CFG="${COSMON_CURATE_CONFIG:-$HOME/.config/cosmon/curate.toml}"
EDITOR_CMD="${EDITOR:-vi}"
GALAXIES_ROOT="${COSMON_GALAXIES_ROOT:-$HOME/galaxies}"
DRY_RUN=0
ONLY_GALAXY=""

usage() {
    cat <<'HELP'
curate-morning.sh — open each galaxy's latest curate-patrol surfaced.md
in $EDITOR for operator review.

Usage:
  curate-morning.sh                # walk all galaxies in curate.toml
  curate-morning.sh --galaxy <G>   # only this galaxy
  curate-morning.sh --dry-run      # print what would open, do nothing
  curate-morning.sh --help

Environment:
  COSMON_CURATE_CONFIG  path to curate.toml (default ~/.config/cosmon/curate.toml)
  COSMON_GALAXIES_ROOT  galaxies parent dir (default ~/galaxies)
  EDITOR                opener (default vi)

The script NEVER applies operator verdicts itself. Patrol applies them
on the next pass when it reads surfaced.md back.
HELP
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        -h|--help) usage; exit 0 ;;
        --dry-run) DRY_RUN=1; shift ;;
        --galaxy)
            [[ $# -ge 2 ]] || { echo "error: --galaxy needs a name" >&2; exit 2; }
            ONLY_GALAXY="$2"; shift 2 ;;
        *) echo "error: unknown arg '$1'" >&2; usage >&2; exit 2 ;;
    esac
done

if [[ ! -f "$CFG" ]]; then
    cat >&2 <<EOF
error: curate config not found at $CFG

  Expected a TOML file with at minimum:

    [drain]
    galaxies = ["cosmon"]      # allowlist — v0 starts with cosmon only

  Owner: sibling task C-CURATE-LAUNCHAGENT. If that task hasn't landed yet,
  create the config by hand or set COSMON_CURATE_CONFIG to a test path.
EOF
    exit 1
fi

# Read the allowlist. Prefer `tomlq` (yq for TOML) ; fall back to a tolerant
# grep so this script keeps working in minimal environments. The grep
# fallback is intentionally simple: it matches lines like
#     galaxies = ["cosmon", "mailroom"]
# or one-per-line array form. Either way: extract bare names, strip quotes.
read_galaxies() {
    if command -v tomlq >/dev/null 2>&1; then
        tomlq -r '.drain.galaxies[]' "$CFG" 2>/dev/null && return 0
    fi
    # grep fallback — best-effort, single-line array.
    awk '
        /^[[:space:]]*galaxies[[:space:]]*=/ {
            inarr = 1
        }
        inarr {
            line = $0
            gsub(/.*=[[:space:]]*\[/, "", line)
            gsub(/].*/, "", line)
            n = split(line, parts, ",")
            for (i = 1; i <= n; i++) {
                gsub(/[[:space:]"'"'"']/, "", parts[i])
                if (length(parts[i]) > 0) print parts[i]
            }
            inarr = 0
        }
    ' "$CFG"
}

GALAXIES=$(read_galaxies)
if [[ -z "$GALAXIES" ]]; then
    echo "error: no galaxies found in $CFG (looking for [drain].galaxies)" >&2
    exit 1
fi

# Locate the most recent surfaced.md for a galaxy. Curate-patrol molecules
# live at:
#   $GALAXIES_ROOT/<G>/.cosmon/state/fleets/default/molecules/curate-*/surfaced.md
# Newest wins (ctime — close-enough proxy for "last patrol pass").
latest_surfaced_for() {
    local g="$1"
    local glob="$GALAXIES_ROOT/$g/.cosmon/state/fleets/default/molecules/curate-*/surfaced.md"
    # shellcheck disable=SC2086
    ls -1t $glob 2>/dev/null | head -1
}

# How many entries are unanswered in a surfaced.md? Counts blocks where the
# `**Reply:**` line is immediately followed by an empty line (no verdict
# written by the operator). This is informational only — the patrol does the
# authoritative parsing on next pass.
count_unanswered() {
    local f="$1"
    [[ -f "$f" ]] || { echo 0; return; }
    awk '
        /^\*\*Reply:\*\*[[:space:]]*$/ {
            getline next_line
            if (next_line ~ /^[[:space:]]*$/) unanswered++
        }
        END { print (unanswered ? unanswered : 0) }
    ' "$f"
}

# Main loop ----------------------------------------------------------------

processed=0
skipped=0
echo "[curate-morning] config: $CFG"
echo "[curate-morning] galaxies-root: $GALAXIES_ROOT"
echo "[curate-morning] editor: $EDITOR_CMD"
echo

for G in $GALAXIES; do
    if [[ -n "$ONLY_GALAXY" && "$G" != "$ONLY_GALAXY" ]]; then
        continue
    fi

    LATEST=$(latest_surfaced_for "$G")
    if [[ -z "$LATEST" || ! -f "$LATEST" ]]; then
        echo "  [skip] $G — no curate-patrol surfaced.md found"
        skipped=$((skipped + 1))
        continue
    fi

    PENDING=$(count_unanswered "$LATEST")
    REL="${LATEST#$HOME/}"
    echo "  [open] $G — ~/$REL"
    echo "         $PENDING unanswered entries (informational)"

    if [[ "$DRY_RUN" -eq 1 ]]; then
        echo "         (dry-run: not opening)"
        continue
    fi

    # Open the file. The operator replies inline 1/2/3/later, then saves
    # and quits ; we move on to the next galaxy.
    "$EDITOR_CMD" "$LATEST" </dev/tty >/dev/tty 2>&1 || {
        echo "         (editor exited non-zero — leaving file as-is)"
    }

    # Prompt — default Y so a tired operator pressing return keeps moving.
    printf "[curate-morning] %s surfaced.md fully replied? [Y/n] " "$G" >/dev/tty
    read -r yn </dev/tty || yn=""
    case "$yn" in
        n|N)
            echo "         → will re-surface next pass"
            ;;
        *)
            echo "         → patrol will apply on next pass"
            ;;
    esac
    processed=$((processed + 1))
done

echo
echo "[curate-morning] done — $processed processed, $skipped skipped"
echo "[curate-morning] (verdicts apply on next patrol pass — this script never auto-applies)"
exit 0
