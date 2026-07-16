#!/usr/bin/env bash
# latex-audit.sh — objective oracle for the LaTeX quality-convergence gate.
#
# Contract version: 1
# Source: cosmon/scripts/latex-audit.sh
#
# This is the deterministic FLOOR of the `latex-convergence` gate
# (.cosmon/formulas/latex-convergence.formula.toml, ADR-129). It is the
# paper-pipeline analogue of `lumen visual-audit` for the visual-qa gate:
# a pure text parser over the LaTeX build artifacts (`.log`) and the source
# (`.tex`) that reports — without rendering, without a TeX install — the
# defect classes a worker keeps shipping:
#
#   * ~30 Overfull \hbox  (text bleeding past the right margin)
#   * Overfull \vbox      (content past the bottom margin)
#   * undefined \ref / \cite (broken cross-references)
#   * figures dumped in the appendix / at the end instead of placed
#     near their first discussion and early in the body
#   * rigid float placement ([H]/[h]) that prevents LaTeX from floating
#     figures up near where they are referenced
#   * missing microtype (the single biggest typographic lever)
#
# It does NOT compile (no rasteriser, no toolchain dependency in cosmon —
# same discipline as ADR-120 keeping the rasteriser out of the core). It
# reads what the compiler already wrote. The SEMANTIC ceiling — "is the
# figure actually next to the sentence that discusses it, does the page
# read well" — is the worker's reading job (the formula), exactly as the
# vision checklist is the ceiling above lumen's pixel floor.
#
# Usage:
#   bash scripts/latex-audit.sh DIR            # auto-discover *.log + *.tex
#   bash scripts/latex-audit.sh main.log       # explicit log; .tex inferred
#   bash scripts/latex-audit.sh --tex main.tex --log main.log
#   bash scripts/latex-audit.sh --json DIR     # machine-readable verdict
#   bash scripts/latex-audit.sh --max-overfull 2 DIR   # hbox tolerance
#
# Exit: 0 = PASS (no FAIL-class defect), 2 = FAIL (>=1 FAIL-class defect),
#       1 = usage / missing-input error. Exit 2 mirrors `lumen visual-audit`.
# WARN-class findings (underfull, mixed-appendix figures, rigid floats,
# missing microtype) are reported but do NOT fail the gate on their own.

set -uo pipefail

# ---------------------------------------------------------------------------
# Arguments
# ---------------------------------------------------------------------------
JSON=0
MAX_OVERFULL=0
TEX_FILE=""
LOG_FILE=""
TARGET=""

usage() {
  sed -n '2,40p' "$0" | sed 's/^# \{0,1\}//'
  exit 1
}

while [ $# -gt 0 ]; do
  case "$1" in
    --json) JSON=1; shift ;;
    --max-overfull) MAX_OVERFULL="${2:-0}"; shift 2 ;;
    --tex) TEX_FILE="${2:-}"; shift 2 ;;
    --log) LOG_FILE="${2:-}"; shift 2 ;;
    -h|--help) usage ;;
    -*) echo "latex-audit: unknown flag $1" >&2; usage ;;
    *) TARGET="$1"; shift ;;
  esac
done

# ---------------------------------------------------------------------------
# Resolve the .log and .tex inputs
# ---------------------------------------------------------------------------
if [ -n "$TARGET" ]; then
  if [ -d "$TARGET" ]; then
    [ -z "$LOG_FILE" ] && LOG_FILE="$(ls "$TARGET"/*.log 2>/dev/null | head -1)"
    [ -z "$TEX_FILE" ] && TEX_FILE="$(ls "$TARGET"/*.tex 2>/dev/null | head -1)"
  elif [ -f "$TARGET" ]; then
    case "$TARGET" in
      *.log) [ -z "$LOG_FILE" ] && LOG_FILE="$TARGET" ;;
      *.tex) [ -z "$TEX_FILE" ] && TEX_FILE="$TARGET" ;;
      *) [ -z "$LOG_FILE" ] && LOG_FILE="$TARGET" ;;
    esac
  fi
fi

# Infer the sibling .tex from the .log basename when only the log was given.
if [ -n "$LOG_FILE" ] && [ -z "$TEX_FILE" ]; then
  cand="${LOG_FILE%.log}.tex"
  [ -f "$cand" ] && TEX_FILE="$cand"
fi

if [ -z "$LOG_FILE" ] || [ ! -f "$LOG_FILE" ]; then
  echo "latex-audit: no .log file found (give a DIR, a *.log, or --log FILE)" >&2
  exit 1
fi

# ---------------------------------------------------------------------------
# Counting helpers (bash 3.2 portable — no associative arrays, no mapfile)
# grep -c exits 1 on zero matches; `|| true` keeps the assignment a clean 0.
# ---------------------------------------------------------------------------
count_in() {  # count_in PATTERN FILE  -> number of matching lines
  local pat="$1" file="$2"
  [ -f "$file" ] || { echo 0; return; }
  grep -cE "$pat" "$file" 2>/dev/null || true
}

# ---------------------------------------------------------------------------
# LOG-derived findings
# ---------------------------------------------------------------------------
# A successful run prints "Output written on <file>.pdf (N pages, ...)".
BUILD_OK=0
if grep -qE '^Output written on .*\.pdf \(' "$LOG_FILE" 2>/dev/null; then
  BUILD_OK=1
fi

# Overfull boxes always begin a log line, so ^-anchored matching is robust
# even though LaTeX wraps the *rest* of each log line at ~79 columns.
OVERFULL_HBOX=$(count_in '^Overfull \\hbox' "$LOG_FILE")
OVERFULL_VBOX=$(count_in '^Overfull \\vbox' "$LOG_FILE")
UNDERFULL=$(count_in '^Underfull \\[hv]box' "$LOG_FILE")
UNDEF_REF=$(count_in 'LaTeX Warning: Reference .* undefined' "$LOG_FILE")
UNDEF_CITE=$(count_in 'LaTeX Warning: Citation .* undefined' "$LOG_FILE")
MISSING_CHAR=$(count_in 'Missing character:' "$LOG_FILE")

# ---------------------------------------------------------------------------
# TeX-derived findings (figure placement + typography)
# ---------------------------------------------------------------------------
FIG_TOTAL=0
FIG_AFTER_APPENDIX=0
FIG_RIGID=0
MICROTYPE=1          # 1 = present / not-checkable, 0 = absent
APPENDIX_LINE=""
TEX_CHECKED=0

if [ -n "$TEX_FILE" ] && [ -f "$TEX_FILE" ]; then
  TEX_CHECKED=1
  FIG_TOTAL=$(count_in '\\begin\{figure' "$TEX_FILE")
  # Rigid placement specifiers nail a float where it is written; if floats
  # are written at the end, [H]/[h]-only keeps them at the end.
  FIG_RIGID=$(count_in '\\begin\{figure\*?\}\[[Hh!]+\]' "$TEX_FILE")

  # Locate the appendix boundary (\appendix, or the bibliography as a
  # de-facto "everything after this is back-matter" marker).
  APPENDIX_LINE="$(grep -nE '^[^%]*\\appendix( |$|\{|\\)' "$TEX_FILE" 2>/dev/null | head -1 | cut -d: -f1)"
  if [ -z "$APPENDIX_LINE" ]; then
    APPENDIX_LINE="$(grep -nE '^[^%]*\\(printbibliography|bibliography)\b' "$TEX_FILE" 2>/dev/null | head -1 | cut -d: -f1)"
  fi
  if [ -n "$APPENDIX_LINE" ]; then
    FIG_AFTER_APPENDIX=$(awk -v L="$APPENDIX_LINE" 'NR>L && /\\begin\{figure/ {n++} END{print n+0}' "$TEX_FILE")
  fi

  if ! grep -qE '\\usepackage(\[[^]]*\])?\{[^}]*microtype[^}]*\}' "$TEX_FILE" 2>/dev/null; then
    MICROTYPE=0
  fi
fi

# ---------------------------------------------------------------------------
# Verdict assembly  (FAIL classes vs WARN classes)
# ---------------------------------------------------------------------------
FAILS=0
WARNS=0
ROWS=""      # newline-joined "STATUS|check|detail" for text rendering

row() {  # row STATUS CHECK DETAIL
  ROWS="${ROWS}$1|$2|$3
"
  case "$1" in
    FAIL) FAILS=$((FAILS+1)) ;;
    WARN) WARNS=$((WARNS+1)) ;;
  esac
}

# Build
if [ "$BUILD_OK" -eq 1 ]; then
  row PASS build "PDF produced (Output written on … found in log)"
else
  row FAIL build "no 'Output written on …pdf' in log — the compile did not finish"
fi

# Overfull hbox (the headline defect)
if [ "$OVERFULL_HBOX" -gt "$MAX_OVERFULL" ]; then
  row FAIL overfull-hbox "$OVERFULL_HBOX Overfull \\hbox (margin overflow); tolerance is $MAX_OVERFULL"
else
  row PASS overfull-hbox "$OVERFULL_HBOX Overfull \\hbox (<= tolerance $MAX_OVERFULL)"
fi

# Overfull vbox (always a defect — content past the bottom margin)
if [ "$OVERFULL_VBOX" -gt 0 ]; then
  row FAIL overfull-vbox "$OVERFULL_VBOX Overfull \\vbox (content past bottom margin)"
else
  row PASS overfull-vbox "0 Overfull \\vbox"
fi

# Undefined references / citations
if [ "$UNDEF_REF" -gt 0 ]; then
  row FAIL undefined-ref "$UNDEF_REF undefined \\ref (broken cross-reference)"
else
  row PASS undefined-ref "0 undefined \\ref"
fi
if [ "$UNDEF_CITE" -gt 0 ]; then
  row FAIL undefined-cite "$UNDEF_CITE undefined \\cite (run biber/bibtex + re-compile)"
else
  row PASS undefined-cite "0 undefined \\cite"
fi

# Figure placement
if [ "$TEX_CHECKED" -eq 1 ] && [ "$FIG_TOTAL" -gt 0 ]; then
  if [ -n "$APPENDIX_LINE" ] && [ "$FIG_AFTER_APPENDIX" -eq "$FIG_TOTAL" ]; then
    row FAIL figures-all-late "all $FIG_TOTAL figures are after the appendix/bibliography (line $APPENDIX_LINE) — none placed near their discussion"
  elif [ "$FIG_AFTER_APPENDIX" -gt 0 ]; then
    row WARN figures-some-late "$FIG_AFTER_APPENDIX of $FIG_TOTAL figures sit after the appendix/bibliography (line $APPENDIX_LINE) — confirm they belong there, not in the body"
  else
    row PASS figures-placement "all $FIG_TOTAL figures are in the body, ahead of the back-matter"
  fi
  if [ "$FIG_RIGID" -gt 0 ]; then
    row WARN figures-rigid "$FIG_RIGID figure(s) use rigid [H]/[h] placement — prefer [tbp] so LaTeX floats them up near the first \\ref"
  fi
elif [ "$TEX_CHECKED" -eq 1 ]; then
  row PASS figures-placement "no figures in source"
else
  row WARN figures-placement "no .tex available — figure placement not checked"
fi

# Underfull (cosmetic — never fails the gate alone)
if [ "$UNDERFULL" -gt 0 ]; then
  row WARN underfull "$UNDERFULL Underfull box(es) — loose spacing, cosmetic"
fi

# Missing glyphs
if [ "$MISSING_CHAR" -gt 0 ]; then
  row WARN missing-char "$MISSING_CHAR 'Missing character' warning(s) — a glyph is absent from the active font"
fi

# microtype
if [ "$TEX_CHECKED" -eq 1 ] && [ "$MICROTYPE" -eq 0 ]; then
  row WARN microtype "microtype not loaded — the single biggest lever against overfull boxes; add \\usepackage{microtype}"
fi

VERDICT="PASS"
[ "$FAILS" -gt 0 ] && VERDICT="FAIL"

# ---------------------------------------------------------------------------
# Render
# ---------------------------------------------------------------------------
if [ "$JSON" -eq 1 ]; then
  # Hand-assembled JSON (no jq dependency) — the findings array first.
  printf '{'
  printf '"verdict":"%s",' "$VERDICT"
  printf '"log":"%s",' "$LOG_FILE"
  printf '"tex":"%s",' "${TEX_FILE:-}"
  printf '"fails":%d,"warns":%d,' "$FAILS" "$WARNS"
  printf '"metrics":{"overfull_hbox":%d,"overfull_vbox":%d,"underfull":%d,"undefined_ref":%d,"undefined_cite":%d,"missing_char":%d,"figures_total":%d,"figures_after_appendix":%d,"figures_rigid":%d,"microtype":%d,"build_ok":%d},' \
    "$OVERFULL_HBOX" "$OVERFULL_VBOX" "$UNDERFULL" "$UNDEF_REF" "$UNDEF_CITE" "$MISSING_CHAR" "$FIG_TOTAL" "$FIG_AFTER_APPENDIX" "$FIG_RIGID" "$MICROTYPE" "$BUILD_OK"
  printf '"findings":['
  first=1
  printf '%s' "$ROWS" | while IFS='|' read -r status check detail; do
    [ -z "$status" ] && continue
    [ "$first" -eq 0 ] && printf ','
    # escape backslashes and quotes for JSON
    esc_detail=$(printf '%s' "$detail" | sed 's/\\/\\\\/g; s/"/\\"/g')
    printf '{"status":"%s","check":"%s","detail":"%s"}' "$status" "$check" "$esc_detail"
    first=0
  done
  printf ']}'
  printf '\n'
else
  echo "latex-audit — $LOG_FILE${TEX_FILE:+ + $TEX_FILE}"
  printf '%s' "$ROWS" | while IFS='|' read -r status check detail; do
    [ -z "$status" ] && continue
    printf '  %-5s %-20s %s\n' "$status" "$check" "$detail"
  done
  echo "  ----"
  echo "  VERDICT: $VERDICT  ($FAILS fail, $WARNS warn)"
fi

[ "$VERDICT" = "PASS" ] && exit 0 || exit 2
