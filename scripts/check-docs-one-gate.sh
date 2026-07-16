#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
# check-docs-one-gate.sh — the "one gate" docs-lint.
#
# Encodes the single structural rule from ADR-149 §4 (doc-site topology) and
# ADR-126 (the two-gate crate frontier), the mechanical projection of
# delib-20260711-8d00's load-bearing insight:
#
#     A public doc surface for a tool — a nav title, a section heading, OR a
#     `noogram.org/<tool>/install.sh` endpoint — exists IFF a stranger with zero
#     operator access can install and run it (public repo + resolving release
#     artifact).
#
# One rule; two constraints fall out of it at once:
#   • anti-vaporware — a premature tool (public repo not yet, no release) may be
#     mentioned in prose but MUST NOT carry a product/install surface;
#   • confidentiality — the private neurion product (it maps operator infra) has
#     no public binary, so it can NEVER acquire a section or install endpoint.
#     Only the name-scrubbed vendored organ crate `neurion-core` may appear, as
#     an internal crate in the API reference — never as a product surface.
#
# This is complementary to `confidentiality-banlist.sh`: that gate bans
# operator/FUND identity terms (a real person, a fund, a homeserver) anywhere on
# the publishable surface. THIS gate bans a confidential TOOL's *product surface*
# and a premature tool's *install endpoint*. Different banlist, same discipline:
# the rule lives in the script, falsifiable via --self-test, hard in CI.
#
# WHAT IT SCANS — `git grep` over the tracked publishable doc surface only
# (docs/book/src). What the gate sees is exactly what renders public.
#
# Usage:
#   ./scripts/check-docs-one-gate.sh            # hard gate over the doc surface
#   ./scripts/check-docs-one-gate.sh --self-test  # prove the patterns catch
# Exit: 0 clean · 1 violation(s) found · 2 invocation/environment error.
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail

# Off a TTY, detach stdin so no descendant can block the gate on a stray read
# (the gate scans FILES; it consumes nothing from stdin).
[ -t 0 ] || exec </dev/null

# ── the tool classification — SINGLE SOURCE. Extend here (and add a self-test
# canary) when a tool's stranger-installability changes. ─────────────────────
#
# INSTALLABLE — a public repo + a resolving per-platform release artifact exists.
# A tool here MAY carry a doc section and a /<tool>/install.sh endpoint. cosmon
# is the kernel: the whole book is its surface, and it ships the `cs` binary.
INSTALLABLE_TOOLS=(cosmon)
#
# CONFIDENTIAL — the tool's PRODUCT is private (maps operator infra). It must
# NEVER appear as a nav title, a section heading, or an install endpoint on the
# publishable surface. The name-scrubbed vendored `<tool>-core` organ crate is
# the ONLY permitted spelling, and only in prose/reference (an internal crate).
# A passing prose mention of the tool as an *integration point* cosmon talks to
# (e.g. "the neurion registry") is NOT a product surface and is allowed — the
# gate is scoped to headings + nav + install endpoints, not to prose.
CONFIDENTIAL_TOOLS=(neurion)

# ── locate the repo + the publishable doc surface ───────────────────────────
REPO_ROOT="$(git -C "$(dirname "${BASH_SOURCE[0]}")" rev-parse --show-toplevel 2>/dev/null)" || {
  echo "check-docs-one-gate: not inside a git repo" >&2; exit 2; }
cd "$REPO_ROOT" || exit 2

DOC_SRC="docs/book/src"
SUMMARY="$DOC_SRC/SUMMARY.md"

# ── helper: is <tool> on the installable allowlist? ──────────────────────────
is_installable() {
  local t="$1" x
  for x in "${INSTALLABLE_TOOLS[@]}"; do [ "$x" = "$t" ] && return 0; done
  return 1
}

# ── helper: strip the permitted `<tool>-core` organ spelling from a line, then
# report whether a bare confidential tool name still remains. Case-insensitive,
# portable (no GNU-only sed flags): lowercase the line, delete `<tool>-core`,
# then look for the bare tool. Tool names are lowercase in the tree. ─────────
line_has_bare_confidential_tool() { # <line> ; echoes matched tool names
  local line lower t
  line="$1"
  lower="$(printf '%s' "$line" | tr '[:upper:]' '[:lower:]')"
  for t in "${CONFIDENTIAL_TOOLS[@]}"; do
    # Remove the allowed organ spelling `<tool>-core` first.
    local scrubbed="${lower//${t}-core/}"
    case "$scrubbed" in
      *"$t"*) printf '%s\n' "$t" ;;
    esac
  done
}

# ── --self-test: falsifiability. A gate that cannot fail is not a gate. ───────
if [ "${1:-}" = "--self-test" ]; then
  fails=0
  # Gate A canary — a confidential tool as a heading/nav title must be caught,
  # while the name-scrubbed organ crate and a prose integration mention are not.
  checkA() { # <line> <expect: hit|clean>
    if [ -n "$(line_has_bare_confidential_tool "$1")" ]; then got=hit; else got=clean; fi
    if [ "$got" = "$2" ]; then echo "  ok   A[$2] $1"
    else echo "  FAIL A expected $2, got $got: $1"; fails=$((fails+1)); fi
  }
  # Gate B canary — an install endpoint for a non-installable tool must be
  # caught; one for an installable tool must pass.
  checkB() { # <tool> <expect: hit|clean>
    if is_installable "$1"; then got=clean; else got=hit; fi
    if [ "$got" = "$2" ]; then echo "  ok   B[$2] /$1/install.sh"
    else echo "  FAIL B expected $2, got $got: /$1/install.sh"; fails=$((fails+1)); fi
  }
  echo "check-docs-one-gate self-test:"
  checkA '# Neurion'                                          hit
  checkA '- [The neurion product](./neurion.md)'              hit
  checkA 'the neurion-core organ crate is vendored'           clean
  checkA 'MCP servers registered in the neurion registry'     hit  # a HEADING with this text would leak; prose is filtered by scope, not here
  checkA 'cosmon nucleates a molecule'                        clean
  checkB cosmon                                               clean
  checkB neurion                                              hit
  checkB topon                                                hit
  if [ "$fails" -eq 0 ]; then echo "self-test: PASS"; exit 0
  else echo "self-test: $fails FAILED"; exit 1; fi
fi

case "${1:-}" in
  ''|--) : ;;
  *) echo "check-docs-one-gate: unknown argument '$1'" >&2; exit 2 ;;
esac

if [ ! -d "$DOC_SRC" ]; then
  echo "check-docs-one-gate: $DOC_SRC not found (nothing to lint)" >&2
  exit 2
fi

violations=0

# ── GATE A — confidential tool as a PRODUCT SURFACE (nav title or section
# heading). Scope: SUMMARY.md nav-link lines + markdown headings in the book
# source. Prose mentions of an integration point are intentionally out of scope.
echo "check-docs-one-gate: GATE A — confidential tool as nav title / section heading"

# A1 — SUMMARY.md nav-link titles: lines like `- [Title](./path.md)`.
if [ -f "$SUMMARY" ]; then
  while IFS= read -r line; do
    [ -z "$line" ] && continue
    hit_tools="$(line_has_bare_confidential_tool "$line")"
    if [ -n "$hit_tools" ]; then
      echo "  VIOLATION (nav title): $SUMMARY" >&2
      echo "    $line" >&2
      echo "    → confidential tool(s) [$(echo "$hit_tools" | tr '\n' ' ')] as a nav entry." >&2
      violations=$((violations+1))
    fi
  done < <(grep -E '^\s*[-*]\s*\[' "$SUMMARY" 2>/dev/null || true)
fi

# A2 — markdown headings (`#`, `##`, …) anywhere in the book source.
while IFS= read -r match; do
  [ -z "$match" ] && continue
  file="${match%%:*}"
  text="${match#*:}"
  hit_tools="$(line_has_bare_confidential_tool "$text")"
  if [ -n "$hit_tools" ]; then
    echo "  VIOLATION (section heading): $file" >&2
    echo "    $text" >&2
    echo "    → confidential tool(s) [$(echo "$hit_tools" | tr '\n' ' ')] as a heading." >&2
    violations=$((violations+1))
  fi
done < <(git grep -nE '^#{1,6}[[:space:]]' -- "$DOC_SRC" 2>/dev/null | sed 's/^\([^:]*\):[0-9]*:/\1:/' || true)

# ── GATE B — install endpoint for a non-installable tool (vaporware / leak).
# Any `/<tool>/install.sh` on the publishable surface must name an INSTALLABLE
# tool. This catches both the premature tool (not-yet) and, redundantly, any
# confidential tool endpoint (never).
echo "check-docs-one-gate: GATE B — /<tool>/install.sh must be a stranger-installable tool"
while IFS= read -r match; do
  [ -z "$match" ] && continue
  # match: <file>:<lineno>:<content>
  content="${match#*:*:}"
  file="${match%%:*}"
  # Extract every `<tool>` from `/<tool>/install.sh` occurrences on the line.
  # Tool tokens are [a-z][a-z0-9-]* between slashes, before /install.sh.
  tools_on_line="$(printf '%s\n' "$content" \
    | grep -oE '/[a-z][a-z0-9-]*/install\.sh' \
    | sed -E 's#^/([a-z][a-z0-9-]*)/install\.sh$#\1#' | sort -u || true)"
  while IFS= read -r tool; do
    [ -z "$tool" ] && continue
    if ! is_installable "$tool"; then
      echo "  VIOLATION (install endpoint): $file" >&2
      echo "    /$tool/install.sh — '$tool' is not on the installable allowlist." >&2
      echo "    A /<tool>/install.sh endpoint requires a public repo + release artifact." >&2
      violations=$((violations+1))
    fi
  done <<< "$tools_on_line"
done < <(git grep -nE '/[a-z][a-z0-9-]*/install\.sh' -- "$DOC_SRC" 2>/dev/null || true)

if [ "$violations" -eq 0 ]; then
  echo "check-docs-one-gate: CLEAN — the doc surface honours the one gate."
  exit 0
fi

echo >&2
echo "check-docs-one-gate: ${violations} violation(s) — the one gate is breached." >&2
echo "A public doc surface (section/nav/install endpoint) exists IFF a stranger" >&2
echo "can install the tool (ADR-149 §4 / ADR-126). Remove the surface, or move a" >&2
echo "tool to INSTALLABLE_TOOLS only once it has a public repo + release artifact." >&2
exit 1
