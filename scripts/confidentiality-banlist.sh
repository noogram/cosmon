#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
# confidentiality-banlist.sh — operator/fund identity tripwire.
#
# Grep the git-tracked byte-set for operator- and fund-identity terms that must
# never appear on a surface that ships public. Two scopes, one banlist:
#
#   • DEFAULT (hard, exit 1) — the PUBLISHABLE surface: the rendered mdBook
#     source (`docs/book/src/`), README, LICENSE/NOTICE. This is exactly the
#     byte-set that becomes the public docs.noogram.org site + the public repo's
#     front matter. It is confidentiality-clean today (verified 2026-07-11);
#     wiring this into CI (see .github/workflows/ci.yml, job `confidentiality`)
#     turns a RE-LEAK into the public surface into a red build. This is the
#     tripwire the B1′ plan §6-P0 / [B1′: R5] asks for — it *installs* the guard
#     and flips nothing public.
#
#   • --whole-repo (advisory report; exit 1 on any hit) — the WHOLE tracked
#     tree, minus the intentional keeps (the operator author email,
#     preserved by the oxymake golden rule; this script + its self-test, which
#     carry the patterns as data). This is the pre-public-flip audit surface:
#     the 15+ INTERNAL `docs/` files (ADRs, lore, chronicles) that name the
#     operator/fund and that no book render ever sees. They stay internal until
#     the whole-repo scrub lands (B1′ plan §6-P5, an OPERATOR gesture), so this
#     mode is NOT wired as a blocking CI gate yet — it would false-red the
#     private repo on its own legitimate internal history. Run it by hand before
#     any public flip; promote it to a hard CI gate only AFTER the scrub.
#
# WHAT IT SCANS — `git grep` over tracked files, never the working tree: the
# bytes that ship are exactly `git ls-files`. What the gate sees is what ships.
#
# THE BANLIST is the operator/fund identity set the B1′ plan enumerates
# ([B1′: R5]): the operator's real name, the fund, the operator homeserver, and
# the operator username/path fragment. Extend it here (and add a self-test
# canary) when a new identity term must be guarded — this is the single source.
#
# Usage:
#   ./scripts/confidentiality-banlist.sh              # hard gate, publishable surface
#   ./scripts/confidentiality-banlist.sh --whole-repo # advisory whole-repo audit
#   ./scripts/confidentiality-banlist.sh --self-test  # prove the patterns catch
# Exit: 0 clean · 1 hit(s) found · 2 invocation/environment error.
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail

# Off a TTY, detach stdin so no descendant can block the gate on a stray read
# (the gate scans FILES; it consumes nothing from stdin).
[ -t 0 ] || exec </dev/null

REPO_ROOT="$(git -C "$(dirname "${BASH_SOURCE[0]}")" rev-parse --show-toplevel 2>/dev/null)" || {
  echo "confidentiality-banlist: not inside a git repo" >&2; exit 2; }
cd "$REPO_ROOT" || exit 2

# ── the banlist — operator + fund identity ([B1′: R5]) ───────────────────────
# One extended-regex per term. Case-insensitive matching (`git grep -i`).
#   • operator real name — accented AND ascii spellings, both surname endings.
#   • fund — accented AND ascii.
#   • operator homeserver domain.
#   • operator username / home-path fragment.
BANLIST=(
  'Noogram S[ée]ri[ée]'
  '[ÉE]pinoia'
  'serie\.dev'
  '(^|/)eserie([/[:space:]]|$)'
)
BAN_RE="$(IFS='|'; echo "${BANLIST[*]}")"

# The publishable surface: what actually renders public.
PUBLIC_PATHSPEC=(
  'docs/book/src'
  'README.md'
  'LICENSE' 'LICENSE-APACHE' 'LICENSE-MIT' 'NOTICE'
)

# ── --self-test: falsifiability. Prove each banlist term matches a canary and
# a clean string does not. A gate that cannot fail is not a gate. ─────────────
if [ "${1:-}" = "--self-test" ]; then
  fails=0
  check() { # <string> <expect: hit|clean>
    if echo "$1" | grep -qiE "$BAN_RE"; then got=hit; else got=clean; fi
    if [ "$got" = "$2" ]; then echo "  ok   [$2] $1"
    else echo "  FAIL expected $2, got $got: $1"; fails=$((fails+1)); fi
  }
  # Canaries touching the fund/homeserver terms are ASSEMBLED at runtime, never
  # inlined, so this committed detector does not itself re-leak the literal it
  # guards (ADR-127 §6 — the detector must not carry the confidential string as
  # a clear-text byte). The accented `Épinoia` canary is safe to inline (it does
  # not match the ascii forbid-scan), and pins the `[ÉE]` branch.
  ep_ascii="E""pinoia Research"          # ascii fund name, assembled at runtime
  hs_dom="serie"".dev"                   # homeserver domain, assembled at runtime
  echo "confidentiality-banlist self-test:"
  check 'authored by Noogram'                       clean
  check 'authored by Noogram'                 hit
  check "a fund called ${ep_ascii}"                  hit
  check 'a fund called Épinoia Research'             hit
  check "homeserver matrix.${hs_dom}"                hit
  check '/srv/cosmon/cosmon'            hit
  check 'The Noogram authors, noogram.dev'           clean
  check 'compose, pilot and audit AI missions'       clean
  if [ "$fails" -eq 0 ]; then echo "self-test: PASS"; exit 0
  else echo "self-test: $fails FAILED"; exit 1; fi
fi

whole_repo=0
case "${1:-}" in
  ''|--) : ;;
  --whole-repo) whole_repo=1 ;;
  *) echo "confidentiality-banlist: unknown argument '$1'" >&2; exit 2 ;;
esac

if [ "$whole_repo" -eq 1 ]; then
  echo "confidentiality-banlist: WHOLE-REPO advisory audit (banlist: ${BAN_RE})"
  # Exclude the intentional keeps: this script + its self-test (patterns as
  # data), `.mailmap` (carries the operator commit-emails as the mapping keys
  # that collapse author identity to Noogram — B1′ §6-P5 / [B1′: R5]), and the
  # operator author email (oxymake golden rule keep).
  hits="$(git grep -I -n -iE "$BAN_RE" -- . \
            ':!scripts/confidentiality-banlist.sh' \
            ':!scripts/confidentiality-banlist.test.sh' \
            ':!.mailmap' 2>/dev/null \
          | grep -viE '@serie""\.dev' || true)"
  if [ -z "$hits" ]; then
    echo "confidentiality-banlist: WHOLE-REPO CLEAN — no operator/fund term found."
    exit 0
  fi
  n="$(printf '%s\n' "$hits" | wc -l | tr -d ' ')"
  nf="$(printf '%s\n' "$hits" | cut -d: -f1 | sort -u | wc -l | tr -d ' ')"
  echo "confidentiality-banlist: ${n} hit(s) across ${nf} file(s) (INTERNAL corpus —"
  echo "  scrub is an operator gesture, B1′ plan §6-P5; this mode is advisory):"
  printf '%s\n' "$hits" | cut -d: -f1 | sort | uniq -c | sort -rn
  exit 1
fi

# DEFAULT: hard gate over the publishable surface.
echo "confidentiality-banlist: publishable-surface gate (banlist: ${BAN_RE})"
hits="$(git grep -I -n -iE "$BAN_RE" -- "${PUBLIC_PATHSPEC[@]}" 2>/dev/null || true)"
if [ -z "$hits" ]; then
  echo "confidentiality-banlist: PUBLISHABLE SURFACE CLEAN — no operator/fund term."
  exit 0
fi
echo "confidentiality-banlist: RE-LEAK — operator/fund term on the public surface:" >&2
printf '%s\n' "$hits" >&2
echo >&2
echo "This surface ships public (docs.noogram.org + the public repo front matter)." >&2
echo "Remove the term or move the content off the publishable surface." >&2
exit 1
