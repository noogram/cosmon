#!/usr/bin/env bash
# latex-audit.test.sh — demonstrate + regression-guard the latex-audit oracle.
#
# No TeX toolchain is required: latex-audit.sh is a pure parser, so we feed
# it captured .log fixtures and .tex sources and assert the verdict. This
# IS the DoD demonstration ("demonstrate on a sample with overfull boxes")
# made executable — the BEFORE fixture carries the exact pathology the
# mission named (~30 overfull hboxes, every figure in the appendix); the
# AFTER fixture is the converged paper.
#
# Usage: ./scripts/latex-audit.test.sh
# Exit: 0 on pass, non-zero on failure.

set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
AUDIT="$HERE/latex-audit.sh"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

pass() { echo "PASS: $*"; }
fail() { echo "FAIL: $*" >&2; exit 1; }

# ---------------------------------------------------------------------------
# Fixture BEFORE — the pathology the fleet keeps shipping.
#   * 3 Overfull \hbox lines (stand-ins for the reported ~30)
#   * 1 Overfull \vbox
#   * an undefined reference
#   * a paper whose every figure lives after \appendix, with rigid [H]
# ---------------------------------------------------------------------------
cat > "$WORK/before.tex" <<'TEX'
\documentclass{article}
\usepackage{graphicx}
\begin{document}
\section{Results}
We discuss the spectrum in Figure~\ref{fig:spectrum} and the beam in
Figure~\ref{fig:beam}. See also Figure~\ref{fig:ghost} which is undefined.

\appendix
\section{Figures}
\begin{figure}[H]\includegraphics{spectrum}\caption{Spectrum.}\label{fig:spectrum}\end{figure}
\begin{figure}[H]\includegraphics{beam}\caption{Beam.}\label{fig:beam}\end{figure}
\end{document}
TEX

cat > "$WORK/before.log" <<'LOG'
This is LuaHBTeX, Version 1.18.0
(./before.tex
Overfull \hbox (15.28pt too wide) in paragraph at lines 5--6
Overfull \hbox (8.91pt too wide) in paragraph at lines 5--6
Overfull \hbox (22.40pt too wide) in paragraph at lines 5--6
Overfull \vbox (12.00pt too high) detected at line 11
Underfull \hbox (badness 10000) in paragraph at lines 4--4
LaTeX Warning: Reference `fig:ghost' on page 1 undefined on input line 6.

LaTeX Warning: There were undefined references.

Output written on before.pdf (1 page, 12345 bytes).
LOG

# Text mode: must FAIL (exit 2) and name the headline defects.
set +e
out="$("$AUDIT" --tex "$WORK/before.tex" --log "$WORK/before.log")"
rc=$?
set -e
[ "$rc" -eq 2 ] || fail "before fixture should exit 2 (FAIL), got $rc"
echo "$out" | grep -q "FAIL  overfull-hbox" || fail "before: overfull-hbox not flagged FAIL"
echo "$out" | grep -q "FAIL  overfull-vbox" || fail "before: overfull-vbox not flagged FAIL"
echo "$out" | grep -q "FAIL  undefined-ref" || fail "before: undefined-ref not flagged FAIL"
echo "$out" | grep -q "FAIL  figures-all-late" || fail "before: figures-all-late not flagged FAIL"
echo "$out" | grep -q "WARN  figures-rigid" || fail "before: rigid [H] placement not flagged WARN"
pass "before fixture FAILs with all four FAIL-class defects + rigid-float WARN"
printf '%s\n' "$out" | sed 's/^/    /'

# JSON mode: verdict + metric counts correct.
set +e
js="$("$AUDIT" --json --tex "$WORK/before.tex" --log "$WORK/before.log")"
set -e
echo "$js" | grep -q '"verdict":"FAIL"' || fail "before JSON: verdict not FAIL"
echo "$js" | grep -q '"overfull_hbox":3' || fail "before JSON: overfull_hbox != 3"
echo "$js" | grep -q '"figures_after_appendix":2' || fail "before JSON: figures_after_appendix != 2"
pass "before fixture JSON carries correct metrics"

# ---------------------------------------------------------------------------
# Fixture AFTER — the converged paper. microtype loaded, figures floated
# into the body with [tbp], zero overfull, refs resolved.
# ---------------------------------------------------------------------------
cat > "$WORK/after.tex" <<'TEX'
\documentclass{article}
\usepackage{microtype}
\usepackage{graphicx}
\begin{document}
\section{Results}
\begin{figure}[tbp]\includegraphics{spectrum}\caption{Spectrum.}\label{fig:spectrum}\end{figure}
\begin{figure}[tbp]\includegraphics{beam}\caption{Beam.}\label{fig:beam}\end{figure}
We discuss the spectrum in Figure~\ref{fig:spectrum} and the beam in
Figure~\ref{fig:beam}.
\end{document}
TEX

cat > "$WORK/after.log" <<'LOG'
This is LuaHBTeX, Version 1.18.0
(./after.tex
Output written on after.pdf (1 page, 13579 bytes).
LOG

set +e
out2="$("$AUDIT" --tex "$WORK/after.tex" --log "$WORK/after.log")"
rc2=$?
set -e
[ "$rc2" -eq 0 ] || fail "after fixture should exit 0 (PASS), got $rc2: $out2"
echo "$out2" | grep -q "VERDICT: PASS" || fail "after: verdict not PASS"
echo "$out2" | grep -q "FAIL " && fail "after: unexpected FAIL row present"
pass "after fixture PASSes cleanly"
printf '%s\n' "$out2" | sed 's/^/    /'

# ---------------------------------------------------------------------------
# Tolerance knob: --max-overfull lets a paper pass with a bounded budget.
# ---------------------------------------------------------------------------
set +e
"$AUDIT" --max-overfull 5 --tex "$WORK/before.tex" --log "$WORK/before.log" >/dev/null
rc3=$?
set -e
# Still FAILs (vbox + undefined-ref + figures), but hbox is now within budget.
out3="$("$AUDIT" --max-overfull 5 --tex "$WORK/before.tex" --log "$WORK/before.log" || true)"
echo "$out3" | grep -q "PASS  overfull-hbox" || fail "tolerance: hbox should pass under --max-overfull 5"
pass "--max-overfull tolerance knob works"

# ---------------------------------------------------------------------------
# Directory auto-discovery + log-only (no .tex) graceful degradation.
# ---------------------------------------------------------------------------
mkdir -p "$WORK/proj"
cp "$WORK/after.tex" "$WORK/proj/main.tex"
cp "$WORK/after.log" "$WORK/proj/main.log"
set +e
"$AUDIT" "$WORK/proj" >/dev/null
rc4=$?
set -e
[ "$rc4" -eq 0 ] || fail "dir auto-discovery should PASS the after-project, got $rc4"
pass "directory auto-discovery resolves main.log + main.tex"

mkdir -p "$WORK/logonly"
cp "$WORK/before.log" "$WORK/logonly/orphan.log"   # no sibling .tex
out5="$("$AUDIT" --log "$WORK/logonly/orphan.log" || true)"
echo "$out5" | grep -q "WARN  figures-placement .*no .tex" || fail "log-only should warn that figures are unchecked"
pass "log-only run degrades gracefully (figure check skipped, not crashed)"

echo
echo "ALL TESTS PASSED"
