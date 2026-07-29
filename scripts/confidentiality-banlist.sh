#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
# confidentiality-banlist.sh — operator-identity tripwire.
#
# Grep the git-tracked byte-set for operator-identity terms that must never
# appear on a surface that ships public. Two scopes, one banlist:
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
#     operator identity and that no book render ever sees. They stay internal until
#     the whole-repo scrub lands (B1′ plan §6-P5, an OPERATOR gesture), so this
#     mode is NOT wired as a blocking CI gate yet — it would false-red the
#     private repo on its own legitimate internal history. Run it by hand before
#     any public flip; promote it to a hard CI gate only AFTER the scrub.
#
# WHAT IT SCANS — `git grep` over tracked files, never the working tree: the
# bytes that ship are exactly `git ls-files`. What the gate sees is what ships.
#
# THE BANLIST is the operator-identity set the B1′ plan enumerates
# ([B1′: R5]). Extend it here (and add a self-test canary) when a new identity
# term must be guarded — this is the single source.
#
# EVERY banned term is ASSEMBLED at runtime from split fragments, without
# exception (ADR-127 §6). The rule is exactly one property, stated no wider
# than it is enforced: THIS COMMITTED FILE CONTAINS NO BANNED TERM AS A
# CONTIGUOUS CLEAR-TEXT BYTE-RUN. Accented and ascii spellings are separate
# byte-runs, so both are assembled.
#
# The reader that property protects against is a SCANNER, not a human. Any
# grep-shaped audit — this gate's own --whole-repo mode, a future scrubbed
# projection, a third party's secret-scanner over the public repo — keys on a
# contiguous run, and a term spelled out here would be a hit that has to be
# special-cased. Keeping the run broken is what lets the keep-list stay short
# enough to read.
#
# It does NOT protect against a human reading this file, and no wording here
# should suggest otherwise. `OP_GIVEN="Emm""anuel"` is perfectly legible to a
# person; the fragments defeat `grep`, and nothing else. That is accepted and
# not a residue to close: this file IS the banlist, and a banlist whose own
# maintainer cannot see which terms it guards is unmaintainable. The header
# used to justify the splitting with "because the file itself is published" —
# which is false: `scripts/` is not in PUBLIC_PATHSPEC below, so nothing here
# is published. An argument that would equally condemn what replaced it is not
# an argument.
#
# The real reason the split still earns its keep is one line lower down: the
# --whole-repo audit scans this script like every other file (it used to be
# excluded and no longer is — see the keep-list at the --whole-repo branch),
# and it comes back clean precisely BECAUSE every term is assembled from
# fragments. The file is inside its own gate rather than exempted from it,
# which is a stronger argument for splitting than "it is published" ever was.
#
# If the human reader ever does become the threat model, the fix is not a
# better split — it is moving the terms out of the committed tree entirely
# (a gitignored or sealed banlist this script sources).
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

# ── the banlist — operator identity ([B1′: R5]) ────────────────────────────
# One extended-regex per term. Case-insensitive matching (`git grep -i`).
# Four terms: the operator real name (accented AND ascii, both surname
# endings), the organization name (accented AND ascii), the operator
# homeserver domain, and the operator username / home-path fragment.
#
# Each is built from fragments so no banned term appears in this file as a
# contiguous byte-run — see the header note. Splitting inside the bracket
# alternations keeps the assembled regex identical to the readable one.
OP_GIVEN="Emm""anuel"
OP_SUR="S[ée]""ri[ée]"
ORG="[ÉE]""pino""ia"
HOMESERVER="ser""ie\\.dev"
OP_USER="es""erie"
BANLIST=(
  "${OP_GIVEN} ${OP_SUR}"
  "${ORG}"
  "${HOMESERVER}"
  "(^|/)${OP_USER}([/[:space:]]|$)"
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
  # Every canary is ASSEMBLED at runtime, including the accented spelling.
  # The earlier rule was "inline the accented form, it does not match the ascii
  # forbid-scan" — assemble only what would trip OUR regex. That leaves the
  # accented spelling as a contiguous run for every OTHER scanner, which is the
  # property the header states. Both spellings are assembled now.
  org_ascii="E""pino""ia Research"       # ascii organization name
  org_acc="É""pino""ia Research"         # accented spelling — pins the [ÉE] branch
  hs_dom="ser""ie"".dev"                 # homeserver domain
  un="e""ser""ie"                        # operator username
  echo "confidentiality-banlist self-test:"
  check 'authored by Noogram'                        clean
  check "authored by ${OP_GIVEN} Serie"              hit
  check "an organization called ${org_ascii}"        hit
  check "an organization called ${org_acc}"          hit
  check "homeserver matrix.${hs_dom}"                hit
  check "/Users/${un}/galaxies/cosmon"               hit
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
  # Exclude the intentional keeps: `.mailmap` (carries the operator
  # commit-emails as the mapping keys that collapse author identity to
  # Noogram — B1′ §6-P5 / [B1′: R5]) and the operator author email (oxymake
  # golden rule keep).
  #
  # This script and its self-test are NO LONGER excluded. They were, back when
  # they carried banned terms as clear-text "patterns as data" and would have
  # self-matched. Now that every term is assembled from fragments they cannot
  # self-match, so the exclusion buys nothing and costs the audit its only
  # blind spot — the two files most likely to reintroduce a literal.
  hits="$(git grep -I -n -iE "$BAN_RE" -- . \
            ':!.mailmap' 2>/dev/null \
          | grep -viE "@ser""ie"'\.dev' || true)"
  if [ -z "$hits" ]; then
    echo "confidentiality-banlist: WHOLE-REPO CLEAN — no operator-identity term."
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
  echo "confidentiality-banlist: PUBLISHABLE SURFACE CLEAN — no operator-identity term."
  exit 0
fi
echo "confidentiality-banlist: RE-LEAK — operator-identity term on the public surface:" >&2
printf '%s\n' "$hits" >&2
echo >&2
echo "This surface ships public (docs.noogram.org + the public repo front matter)." >&2
echo "Remove the term or move the content off the publishable surface." >&2
exit 1
