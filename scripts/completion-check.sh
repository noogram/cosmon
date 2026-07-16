#!/bin/bash
# completion-check.sh — Measure fleet production metrics.
#
# Usage: ./scripts/completion-check.sh <directory>
# Example: ./scripts/completion-check.sh ~/knowledge/cosmopedia-full/articles

DIR="${1:-.}"

if [ ! -d "$DIR" ]; then
  echo "Directory not found: $DIR" >&2
  exit 1
fi

echo "=== Completion Check: $DIR ==="
echo ""

# Count files
ARTICLES=$(find "$DIR" -name "*.md" -not -path "*/contributors/*" -not -name "_*" | wc -l | tr -d ' ')
REVIEWS=$(find "$DIR" -path "*/reviews/*.md" 2>/dev/null | wc -l | tr -d ' ')
TOTAL_FILES=$(find "$DIR" -name "*.md" | wc -l | tr -d ' ')

echo "Files: $TOTAL_FILES total ($ARTICLES articles, $REVIEWS reviews)"

# Lines and words
TOTAL_LINES=$(find "$DIR" -name "*.md" -exec cat {} + 2>/dev/null | wc -l | tr -d ' ')
TOTAL_WORDS=$(find "$DIR" -name "*.md" -exec cat {} + 2>/dev/null | wc -w | tr -d ' ')

echo "Volume: $TOTAL_LINES lines, $TOTAL_WORDS words"

# Efficiency
if [ "$ARTICLES" -gt 0 ]; then
  WORDS_PER_ARTICLE=$((TOTAL_WORDS / ARTICLES))
  echo "Efficiency: ~$WORDS_PER_ARTICLE words/article"
fi

# Timestamps
OLDEST=$(find "$DIR" -name "*.md" -exec stat -f "%m %N" {} + 2>/dev/null | sort -n | head -1 | cut -d' ' -f2-)
NEWEST=$(find "$DIR" -name "*.md" -exec stat -f "%m %N" {} + 2>/dev/null | sort -rn | head -1 | cut -d' ' -f2-)

echo ""
echo "First file: $(basename "$OLDEST" 2>/dev/null || echo "?")"
echo "Last file:  $(basename "$NEWEST" 2>/dev/null || echo "?")"

# Cross-references (count internal links)
LINKS=$(find "$DIR" -name "*.md" -exec grep -c "\[.*\](.*\.md)" {} + 2>/dev/null | awk -F: '{sum+=$2} END {print sum}')
echo ""
echo "Cross-references: ${LINKS:-0} internal links"

# Energy check
ENERGY_LOG="${HOME}/cosmon/state/log/energy.jsonl"
if [ -f "$ENERGY_LOG" ] && [ -s "$ENERGY_LOG" ]; then
  RECORDS=$(wc -l < "$ENERGY_LOG" | tr -d ' ')
  echo ""
  echo "Energy: $RECORDS consumption records logged"
else
  echo ""
  echo "Energy: no records (cosmon_energy_log not yet used by agents)"
fi
