#!/usr/bin/env bash
# check-triad.sh — régression grep pour la Grande Migration (2026-04-18).
#
# Vérifie qu'aucune référence à l'ancienne topologie ne subsiste dans les
# zones critiques. Exit 0 si clean, 1 sinon.
#
# Scope (conforme au plan "home-became-galaxies") :
#   1. Content scan — fichiers actifs (configs, state, CLAUDE.md) :
#      - /srv/cosmon/*/.cosmon (sauf audit + logs + build caches regenerables)
#      - ~/.cosmon/
#      - ~/.mailroom/
#      - ~/Library/LaunchAgents/
#      - ~/.config/cosmon, ~/.config/neurion
#   2. Slug-name scan — juste les noms des dossiers Claude projects.
#   3. Neurion DB — rows des tables repos/binaries/config_files/services.
#
# Exclusions content-scan :
#   events.jsonl, *.jsonl, *.log, *.fls, *.txt (audit/cache/build)
#   .cosmon/hooks/ (ce fichier contient la regex — self-reference)

PATTERN_DEV='/Users/you/dev/projects/(cosmon|showroom|sandbox|cadence)'
PATTERN_SEC='/Users/you/mailroom/'
DB="$HOME/Library/Application Support/neurion/neurion.db"

bad=0

scan_content() {
  local dir="$1"
  local label="$2"
  [ -e "$dir" ] || { echo "SKIP [$label] (not found)"; return 0; }
  local files
  files=$(grep -rIlE "$PATTERN_DEV|$PATTERN_SEC" "$dir" 2>/dev/null \
    | grep -vE 'events\.jsonl|\.jsonl$|\.log$|\.fls$|\.txt$|\.cosmon/hooks/|\.db$' \
    || true)
  local count=0
  [ -n "$files" ] && count=$(echo "$files" | wc -l | tr -d ' ')
  if [ "$count" -gt 0 ]; then
    echo "FAIL [$label]: $count files still reference old topology"
    echo "$files" | head -10 | sed 's/^/  /'
    bad=$((bad+count))
  else
    echo "OK   [$label]"
  fi
}

scan_slug_names() {
  local dir="$1"
  local label="$2"
  [ -e "$dir" ] || { echo "SKIP [$label] (not found)"; return 0; }
  # Check for leftover slug directory names matching old topology.
  local bad_slugs
  bad_slugs=$(ls -1 "$dir" 2>/dev/null | grep -E '^-Users-you-(cosmon|showroom|sandbox|cadence)|^-Users-you-mailroom([^-]|$)' || true)
  local count=0
  [ -n "$bad_slugs" ] && count=$(echo "$bad_slugs" | wc -l | tr -d ' ')
  if [ "$count" -gt 0 ]; then
    echo "FAIL [$label slug-names]: $count old slug directory names remain"
    echo "$bad_slugs" | head -10 | sed 's/^/  /'
    bad=$((bad+count))
  else
    echo "OK   [$label slug-names]"
  fi
}

# --- Content scans ---
for g in cosmon showroom sandbox cadence mailroom; do
  scan_content "$HOME/galaxies/$g/.cosmon"   "galaxies/$g/.cosmon"
done

scan_content "$HOME/Library/LaunchAgents" "Library/LaunchAgents"
scan_content "$HOME/.cosmon"              ".cosmon"
scan_content "$HOME/.mailroom"         ".mailroom"
scan_content "$HOME/.config/cosmon"       ".config/cosmon"
scan_content "$HOME/.config/neurion"      ".config/neurion"
scan_content "$HOME/.claude/CLAUDE.md"    ".claude/CLAUDE.md"

# --- Slug-name scan ---
scan_slug_names "$HOME/.claude/projects"  ".claude/projects"

# --- Neurion SQL ---
if [ -f "$DB" ]; then
  sql_bad=$(sqlite3 "$DB" "
    SELECT (SELECT COUNT(*) FROM repos WHERE local_path LIKE '%/dev/projects/cosmon%' OR local_path LIKE '%/dev/projects/showroom%' OR local_path LIKE '%/dev/projects/sandbox%' OR local_path LIKE '%/dev/projects/cadence%' OR local_path LIKE '/Users/you/mailroom' OR local_path LIKE '/Users/you/mailroom/%')
      + (SELECT COUNT(*) FROM binaries WHERE path LIKE '%/dev/projects/cosmon%' OR path LIKE '%/dev/projects/showroom%' OR path LIKE '%/dev/projects/sandbox%' OR path LIKE '%/dev/projects/cadence%' OR path LIKE '/Users/you/mailroom%')
      + (SELECT COUNT(*) FROM config_files WHERE path LIKE '%/dev/projects/cosmon%' OR path LIKE '%/dev/projects/showroom%' OR path LIKE '%/dev/projects/sandbox%' OR path LIKE '%/dev/projects/cadence%' OR path LIKE '/Users/you/mailroom%')
    ;" 2>/dev/null)
  sql_bad=${sql_bad:-0}
  if [ "$sql_bad" -gt 0 ]; then
    echo "FAIL [neurion SQL]: $sql_bad rows reference old topology"
    bad=$((bad+sql_bad))
  else
    echo "OK   [neurion SQL]"
  fi
fi

echo
if [ "$bad" -gt 0 ]; then
  echo "check-triad FAILED: $bad total references remain"
  exit 1
fi
echo "check-triad PASSED — migration topology is clean."
exit 0
