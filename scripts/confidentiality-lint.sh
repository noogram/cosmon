#!/usr/bin/env bash
# confidentiality-lint.sh — durable guardrail against leaking operator identity,
# family, clients, or private galaxy names into the public cosmon tree.
#
# OWN-GOAL-FREE BY DESIGN: this script contains NO real names. The denylist
# lives OUTSIDE the repo, operator-local, at:
#   $COSMON_DENYLIST_DIR (default: ~/.cosmon)
#     confidential-strict.txt      — distinctive tokens, matched anywhere
#     confidential-contextual.txt  — common-word galaxies, matched only in
#                                    an explicit galaxy-reference context
# Regenerate those from the operator's local authoritative sources (galaxy
# roots + people index). A clone without the denylist runs in contributor
# mode (no-op + notice) — a contributor cannot leak names they do not have.
#
# Usage:  bash scripts/confidentiality-lint.sh           # scan tracked tree
# Exit:   0 clean · 1 confidential match found · 2 tooling error
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"

# Structural checks are autonomous and fail closed even when the optional
# operator-local name denylist is absent. (Was `scripts/publish.sh --check`,
# a pre-publication reference that never shipped in this tree — the
# command-backed successor is the release checklist's GATE items.)
bash scripts/release-checklist.sh --check

DIR="${COSMON_DENYLIST_DIR:-$HOME/.cosmon}"
STRICT="$DIR/confidential-strict.txt"
CTX="$DIR/confidential-contextual.txt"
# Positive allowlist of PUBLIC galaxy names (see check (1b) below). Unlike the
# denylist, this file is optional — the built-in public trio always applies.
ALLOWGAL="$DIR/galaxy-allowlist.txt"

if [[ ! -f "$STRICT" && ! -f "$CTX" ]]; then
  echo "confidentiality-lint: structural gate passed; no optional local denylist at $DIR." >&2
  exit 0
fi

# Never scan the lint machinery, vendored deps, or the denylist itself.
#
# The `:(exclude)scripts/...` entries below are the SCRUB MACHINERY: files that
# carry banned terms as DATA, because forbidding a term requires naming it once.
# Same intentional-keep rationale as confidentiality-banlist.sh (which excludes
# itself and its self-test). Keep this list minimal — every entry is a blind
# spot. A term may appear here only as a pattern/mapping, never as prose.
PATHSPEC=(
  ':(exclude)scripts/confidentiality-lint.sh'
  ':(exclude)vendor/**'
  ':(exclude)scripts/confidentiality-banlist.sh'
  ':(exclude)scripts/confidentiality-banlist.test.sh'
  ':(exclude)scripts/publish.sh'
  ':(exclude)scripts/sovereignty-gate.sh'
  ':(exclude)scripts/sovereignty-spec.md'
  # The WHOLE release membrane, not one file of it (task-20260716-c4eb). This
  # entry used to name `scripts/release/tree-replacements.txt` alone, but every
  # file in that directory is the same kind of thing: purge-history.sh must name
  # the paths it removes, message-replacements.txt must name the strings it
  # replaces, cosmon-release-audit.sh must name the tokens it forbids. A map
  # cannot rewrite a term it is not allowed to write down.
  #
  # This is not a new blind spot. `scripts/release/` never ships — purge-history
  # B5 removes it from the projection — and release-checklist.sh GATE 4, the
  # referee that actually gates the flip, already excludes `scripts/release/*`
  # wholesale. This aligns the two scanners on one rule instead of leaving the
  # narrower one to red on data the broader one deliberately ignores.
  #
  # `scripts/release-checklist.sh` is NOT under this path and stays scanned — it
  # ships, so it must stay clean.
  ':(exclude)scripts/release/**'
)

# ── LEADING WORD BOUNDARY — portable, and canaried. ──────────────────────────
# The content scans anchor each term on a leading word boundary. They used to
# spell that `\b`, which is NOT a POSIX ERE escape: `git grep -E '\bfoo'`
# silently matches NOTHING wherever git's ERE engine lacks the GNU extension
# (observed here: macOS, git 2.53 — `git grep -E '\bnucleate'` returns 0 hits
# against 2814 real ones). The scan found nothing and the gate reported CLEAN
# while scanning nothing: fail-OPEN, and green — the worst kind.
#
# `LB` is the same assertion in pure POSIX ERE — "not preceded by a word char"
# — so it needs no PCRE build and behaves identically on BSD and GNU.
LB='(^|[^A-Za-z0-9_])'

# CANARY — a gate that cannot fail is not a gate. Prove on every run that the
# boundary matcher still matches a term that MUST hit and rejects one that MUST
# NOT, so a future regex regression reds the build instead of going quiet.
if ! printf 'x com.you.example.plist\n' | grep -qiE "${LB}example"; then
  echo "❌ confidentiality-lint: boundary-matcher canary FAILED (must-hit missed)." >&2
  echo "   Refusing to report a clean tree we did not actually scan." >&2
  exit 2
fi
if printf 'xxexample\n' | grep -qiE "${LB}example"; then
  echo "❌ confidentiality-lint: boundary-matcher canary FAILED (must-miss matched)." >&2
  exit 2
fi

hits=0
tmp="$(mktemp)"; trap 'rm -f "$tmp"' EXIT

# (0) FILE PATHS — a banned token in a filename ships regardless of content.
if [[ -f "$STRICT" ]]; then
  while IFS= read -r term; do
    [[ -z "$term" || "${term:0:1}" == "#" || ${#term} -lt 4 ]] && continue
    if git ls-files | grep -iE "${LB}${term}([^A-Za-z0-9_]|$)" >>"$tmp" 2>/dev/null; then hits=1; fi
  done < "$STRICT"
fi

# (1) CONTENTS — leading-boundary, case-insensitive match so constant names
# (MATTEO_MIN, GRADIUM_KEY) and All-Caps variants are caught, not just \bword\b.
# Boundary is `$LB`, not `\b` — see the canary above.
if [[ -f "$STRICT" ]]; then
  while IFS= read -r term; do
    [[ -z "$term" || "${term:0:1}" == "#" || ${#term} -lt 4 ]] && continue
    if git grep -inIIE -e "${LB}${term}" -- "${PATHSPEC[@]}" >>"$tmp" 2>/dev/null; then
      hits=1
    fi
  done < "$STRICT"
fi

if [[ -f "$CTX" ]]; then
  while IFS= read -r term; do
    [[ -z "$term" || "${term:0:1}" == "#" || ${#term} -lt 3 ]] && continue
    # only flag common-word galaxies inside an explicit galaxy reference
    if git grep -inIIE -e "galaxies/$term" -e "noyau[^\"]*\"$term\"" -e "${LB}$term galaxy" \
         -- "${PATHSPEC[@]}" >>"$tmp" 2>/dev/null; then
      hits=1
    fi
  done < "$CTX"
fi

# (1b) GALAXY-PATH ALLOWLIST — fail-CLOSED on `/srv/cosmon/<name>`.
# The denylist (checks 0/1/contextual) is fail-OPEN: a private galaxy it never
# enumerated ships silently — that is exactly how `/srv/cosmon/playhouse` slipped
# past the gate (pre-mortem CONV-1, 2026-07-14). This check inverts the default:
# a `/srv/cosmon/<name>` path is accepted ONLY if <name> is a known PUBLIC galaxy.
# The accepted set is the built-in public trio plus optional operator-local
# `$ALLOWGAL`; anything else fails closed. Own-goal-free: no real *private* name
# is hardcoded here — only the public project trio and generic doc placeholders.
# The real accepted-galaxy roster lives operator-local beside the denylist, so a
# contributor clone (no $DIR at all) never reaches this code (early-exit above).
# `example-project` / `example-galaxy` are the doc/CLI placeholders shipped by
# this tree itself (`cs examples`, the scheduler help snapshot, the project
# reference chapter). They are manifestly generic — `example*` names cannot
# denote a private galaxy — so they belong in the built-in public set rather
# than in the operator-local `$ALLOWGAL`, which a contributor clone never has.
allow=" cosmon noogram knowledge foo bar baz qux tenant-demo annex example example-project example-galaxy foobar other-noyau "
if [[ -f "$ALLOWGAL" ]]; then
  while IFS= read -r g; do
    g="${g%%#*}"                                   # strip inline comment
    g="$(printf '%s' "$g" | tr -d '[:space:]' | tr '[:upper:]' '[:lower:]')"
    [[ -z "$g" ]] && continue
    allow="${allow}${g} "
  done < "$ALLOWGAL"
fi
while IFS= read -r line; do
  [[ -z "$line" ]] && continue
  file="${line%%:*}"; rest="${line#*:}"; lno="${rest%%:*}"
  # a line may carry several paths (e.g. an allowed one and a leaked one)
  while IFS= read -r tok; do
    name="${tok#\/srv/cosmon/}"
    [[ ${#name} -le 2 ]] && continue             # skip A/B/a/b/G metavariables
    lname="$(printf '%s' "$name" | tr '[:upper:]' '[:lower:]')"
    case "$allow" in *" $lname "*) continue ;; esac
    printf '%s:%s: /srv/cosmon/%s (galaxy not in public allowlist)\n' "$file" "$lno" "$name" >>"$tmp"
    hits=1
    # SC2088: the literal `~` is intentional — we match the string `/srv/cosmon/`
    # as it appears in tracked files, not a path to be tilde-expanded.
    # shellcheck disable=SC2088
  done < <(printf '%s\n' "$rest" | grep -oE '/srv/cosmon/[A-Za-z0-9_-]+')
  # shellcheck disable=SC2088
done < <(git grep -InE '/srv/cosmon/[A-Za-z0-9_-]+' -- "${PATHSPEC[@]}" 2>/dev/null || true)

# (2) VENDORED-ASSET INTEGRITY — a minified vendored blob is a low-cost hiding
# spot (reviewers reflexively exclude it as noise). Pin its hash so a future
# edit that smuggles a real string in trips a diff instead of being skipped.
if [[ -f docs/book/vendored-assets.sha256 ]]; then
  if ! ( cd docs/book && shasum -a 256 -c vendored-assets.sha256 ) >/dev/null 2>&1; then
    echo "❌ confidentiality-lint: vendored asset hash mismatch (docs/book/vendored-assets.sha256)" >&2
    echo "   a pinned vendored blob changed — review the diff, then re-pin if legitimate." >&2
    hits=1
  fi
fi

if [[ "$hits" -ne 0 ]]; then
  echo "❌ confidentiality-lint: confidential identifiers found in the tracked tree:" >&2
  sort -u "$tmp" | sed 's/^/   /' >&2
  echo "→ genericize or relocate these before the tree can go public." >&2
  exit 1
fi
echo "✅ confidentiality-lint: clean (no operator/family/client/private-galaxy identifiers; vendored assets pinned)."
