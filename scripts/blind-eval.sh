#!/bin/bash
# blind-eval.sh — Blind pairwise evaluation of two article sets.
#
# Usage: ./scripts/blind-eval.sh <dir_A> <dir_B>
# Example: ./scripts/blind-eval.sh ~/solo-articles ~/fleet-articles
#
# For each topic that exists in BOTH directories, presents the articles
# to a judge (Claude via `claude -p`) without revealing which is which.
# Outputs: win rate per system.

DIR_A="${1:?Usage: blind-eval.sh <dir_A> <dir_B>}"
DIR_B="${2:?Usage: blind-eval.sh <dir_A> <dir_B>}"

if [ ! -d "$DIR_A" ] || [ ! -d "$DIR_B" ]; then
  echo "Both directories must exist" >&2
  exit 1
fi

echo "=== Blind Pairwise Evaluation ==="
echo "System A: $DIR_A"
echo "System B: $DIR_B"
echo ""

A_WINS=0
B_WINS=0
TIES=0
TOTAL=0

# Find common topics
for article_a in "$DIR_A"/*.md; do
  topic=$(basename "$article_a")
  article_b="$DIR_B/$topic"

  if [ ! -f "$article_b" ]; then
    continue
  fi

  TOTAL=$((TOTAL + 1))

  # Randomize which is shown first
  if [ $((RANDOM % 2)) -eq 0 ]; then
    FIRST="$article_a"
    SECOND="$article_b"
    FIRST_IS="A"
  else
    FIRST="$article_b"
    SECOND="$article_a"
    FIRST_IS="B"
  fi

  CONTENT_1=$(cat "$FIRST")
  CONTENT_2=$(cat "$SECOND")

  # Judge prompt
  VERDICT=$(claude -p "You are a blind article quality judge. Compare these two articles on the SAME topic. Do NOT guess which system produced them.

--- ARTICLE X ---
$CONTENT_1

--- ARTICLE Y ---
$CONTENT_2

Which article is better in terms of: accuracy, completeness, structure, clarity, and sourcing?
Answer EXACTLY one of: X, Y, or TIE. Then explain in 2-3 sentences.
Your answer (first word must be X, Y, or TIE):" 2>/dev/null | head -1)

  WINNER=$(echo "$VERDICT" | grep -oE "^(X|Y|TIE)" | head -1)

  # Map back to A/B
  if [ "$WINNER" = "X" ] && [ "$FIRST_IS" = "A" ]; then
    A_WINS=$((A_WINS + 1))
    echo "  $topic: A wins"
  elif [ "$WINNER" = "X" ] && [ "$FIRST_IS" = "B" ]; then
    B_WINS=$((B_WINS + 1))
    echo "  $topic: B wins"
  elif [ "$WINNER" = "Y" ] && [ "$FIRST_IS" = "A" ]; then
    B_WINS=$((B_WINS + 1))
    echo "  $topic: B wins"
  elif [ "$WINNER" = "Y" ] && [ "$FIRST_IS" = "B" ]; then
    A_WINS=$((A_WINS + 1))
    echo "  $topic: A wins"
  else
    TIES=$((TIES + 1))
    echo "  $topic: TIE"
  fi
done

echo ""
echo "=== Results ==="
echo "Total articles compared: $TOTAL"
echo "System A wins: $A_WINS"
echo "System B wins: $B_WINS"
echo "Ties: $TIES"

if [ "$TOTAL" -gt 0 ]; then
  A_RATE=$((A_WINS * 100 / TOTAL))
  B_RATE=$((B_WINS * 100 / TOTAL))
  echo ""
  echo "Win rate: A=${A_RATE}% B=${B_RATE}%"
fi
