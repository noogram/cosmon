#!/bin/bash
# session-tokens.sh — Estimate token usage from Claude Code session logs.
#
# Usage: ./scripts/session-tokens.sh [session_id]
#        ./scripts/session-tokens.sh                  # auto-detect current session
#        ./scripts/session-tokens.sh <path/to.jsonl>  # direct path
#
# Reads the session JSONL log and sums up input_tokens, output_tokens,
# cache_creation_input_tokens, and cache_read_input_tokens from usage blocks.

set -e

# Find the log file
if [ -n "$1" ] && [ -f "$1" ]; then
  LOG="$1"
elif [ -n "$1" ]; then
  # Try as session ID — search all projects
  LOG=$(find ~/.claude/projects -name "${1}.jsonl" 2>/dev/null | head -1)
  if [ -z "$LOG" ]; then
    echo "Session not found: $1" >&2
    exit 1
  fi
else
  # Auto-detect: find the largest recent .jsonl in the current project
  PROJECT_DIR=$(echo "$PWD" | sed 's|/|--|g' | sed 's|^-|/|' | sed 's|^|'"$HOME"'/.claude/projects/-|')
  # Simpler: just find the newest large jsonl
  LOG=$(find ~/.claude/projects -name "*.jsonl" -newer /tmp -size +100k 2>/dev/null | \
        xargs ls -t 2>/dev/null | head -1)
  if [ -z "$LOG" ]; then
    echo "No recent session log found" >&2
    exit 1
  fi
fi

echo "=== Session Token Usage ==="
echo "Log: $(basename "$LOG" .jsonl)"
echo "Size: $(du -h "$LOG" | cut -f1)"
echo "Lines: $(wc -l < "$LOG" | tr -d ' ')"
echo ""

# Extract all usage blocks and sum tokens
# The usage data is in JSON like: "usage":{"input_tokens":N,"output_tokens":N,...}
INPUT=$(grep -oE '"input_tokens":[0-9]+' "$LOG" | grep -oE '[0-9]+' | awk '{s+=$1} END {print s+0}')
OUTPUT=$(grep -oE '"output_tokens":[0-9]+' "$LOG" | grep -oE '[0-9]+' | awk '{s+=$1} END {print s+0}')
CACHE_CREATE=$(grep -oE '"cache_creation_input_tokens":[0-9]+' "$LOG" | grep -oE '[0-9]+' | awk '{s+=$1} END {print s+0}')
CACHE_READ=$(grep -oE '"cache_read_input_tokens":[0-9]+' "$LOG" | grep -oE '[0-9]+' | awk '{s+=$1} END {print s+0}')

TOTAL_INPUT=$((INPUT + CACHE_CREATE + CACHE_READ))
TOTAL=$((TOTAL_INPUT + OUTPUT))

# Count turns
TURNS=$(grep -c '"role":"assistant"' "$LOG" 2>/dev/null || echo 0)

echo "Tokens:"
echo "  Input:          $(printf "%'d" $INPUT)"
echo "  Cache create:   $(printf "%'d" $CACHE_CREATE)"
echo "  Cache read:     $(printf "%'d" $CACHE_READ)"
echo "  ─────────────────────"
echo "  Total input:    $(printf "%'d" $TOTAL_INPUT)"
echo "  Output:         $(printf "%'d" $OUTPUT)"
echo "  ═════════════════════"
echo "  TOTAL:          $(printf "%'d" $TOTAL)"
echo ""
echo "Turns: $TURNS assistant messages"

# Context estimate
if [ "$TURNS" -gt 0 ]; then
  AVG_INPUT=$((TOTAL_INPUT / TURNS))
  echo "Avg context/turn: $(printf "%'d" $AVG_INPUT) tokens"
fi

# Cost estimate (rough, Opus pricing)
# Opus: $15/Mtok input, $75/Mtok output (as of 2025)
if command -v python3 &>/dev/null; then
  COST=$(python3 -c "
input_cost = $TOTAL_INPUT * 15 / 1_000_000
output_cost = $OUTPUT * 75 / 1_000_000
cache_read_savings = $CACHE_READ * (15 - 1.5) / 1_000_000  # cache read is 90% cheaper
total = input_cost + output_cost - cache_read_savings
print(f'  Input cost:     \${input_cost:.2f}')
print(f'  Output cost:    \${output_cost:.2f}')
print(f'  Cache savings:  -\${cache_read_savings:.2f}')
print(f'  ═════════════════════')
print(f'  Estimated cost: \${total:.2f}')
")
  echo ""
  echo "Cost estimate (Opus pricing):"
  echo "$COST"
fi
