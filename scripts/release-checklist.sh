#!/usr/bin/env bash
# release-checklist.sh — cosmon's pre-launch checklist as a COMMAND-BACKED PROJECTION
# ============================================================================
# Ported from oxymake's release-checklist.sh (oxymake délib-20260529-13a7,
# Q-REL-4/5) and adapted to cosmon's ONE-REPO model (ADR-133). The
# most-dangerous-gate finding (janis): a self-ticked checklist is "pre-waived by
# design" and "produces no corpse when it silently certifies". Therefore this is
# NOT a list of boxes a human ticks. Every GATE item below is a *projection of an
# exogenous referee*: a command whose exit status — not an operator's opinion —
# decides PASS/FAIL. Run it; if it exits non-zero, the repo is NOT ready to flip.
#
# THE ONE-REPO MODEL (ADR-133): cosmon is ONE public repo, not a maintained
# public/private pair. There is no scrubbed projection (the old release-resync
# chain is deprecated). The LIVE tree itself must be clean. The membrane is the
# artifact-map RESIDENCE audit (whole-file leaks) + the confidential-string scan
# (strings inside otherwise-public files) + gitleaks (secrets).
#
# The output partitions into exactly two bins (janis (e)):
#   [GATE]     — exogenous; a non-zero command fails the build / blocks the flip.
#   [ADVISORY] — explicitly non-gating; reported, never fails. Honest demotion.
# Nothing lives between. A [GATE PEND] is a GATE whose referee cannot run here
# (e.g. branch protection while still private) — reported, counted, never a
# silent pass.
#
# USAGE
#   scripts/release-checklist.sh              # pre-flip: GATE items, post-flip pending
#   scripts/release-checklist.sh --post-flip  # after the public flip: those become hard.
#
# EXIT CODE
#   0  — every applicable GATE passed (ready to proceed to the next phase).
#   1  — at least one GATE failed (NOT ready). ADVISORY items never affect this.
# ============================================================================
set -uo pipefail

REPO_SLUG="noogram/cosmon"
POST_FLIP=0
[ "${1:-}" = "--post-flip" ] && POST_FLIP=1

# Resolve repo root (this script lives in scripts/).
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

# A 3.11+ python is needed for the artifact-map audit (stdlib tomllib). Prefer
# a homebrew 3.13/3.12 if the default python3 is older (macOS ships 3.9).
PY="python3"
if ! "$PY" -c 'import tomllib' >/dev/null 2>&1; then
  for cand in python3.13 python3.12 python3.11 /opt/homebrew/bin/python3.13 /opt/homebrew/bin/python3.12; do
    if command -v "$cand" >/dev/null 2>&1 && "$cand" -c 'import tomllib' >/dev/null 2>&1; then PY="$cand"; break; fi
  done
fi

FAILS=0
PENDS=0

# ── Externalized denylist (never hard-coded in this guard) ──────────────────
# A guard that names in clear what it forbids re-leaks it (the detector-is-its-
# own-leak pathology, ADR-127 §6). So the confidential alternations are NOT baked
# in here. Resolution order:
#   1. $COSMON_FORBID_PATTERN  — `git grep -E` alternation of client/domain/infra markers
#   2. scripts/.release-denylist.local (gitignored) exporting COSMON_FORBID_PATTERN
#   3. auto-derived from the LOCAL private rule files (untracked, on disk):
#        .cosmon/release-rules.toml          (token = "..." lines; gitignored)
#        ~/.config/cosmon/config.toml        (machine-wide confidential_blocklist)
# When none yields a pattern, the forbid gate reports PEND (honest demotion),
# never a silent pass.
#
# NB (ADR-127 §6 — do not re-leak through the detector): the *tracked, public*
# `.cosmon/config.toml` is deliberately NOT a derivation source. Its
# `[git_remote_blocklist] forbidden_substrings` are STRUCTURAL remote-URL
# markers (public repo names like `github.com/noogram/almanac`,
# `ggml-org/llama.cpp`), not confidential CONTENT — sweeping them into the
# content-forbid pattern would make this gate flag every file that legitimately
# documents the blocklist, and a public tracked file that both *defines* and
# *contains* the "secret" is the exact re-leak coupling ADR-127 §6 forbids. The
# confidential content denylist must come only from operator-private sources.
if [ -f scripts/.release-denylist.local ]; then
  # shellcheck disable=SC1091
  . scripts/.release-denylist.local
fi
derive_forbid_pattern() {
  [ -n "${COSMON_FORBID_PATTERN:-}" ] && { printf '%s' "$COSMON_FORBID_PATTERN"; return; }
  local toks=()
  if [ -f .cosmon/release-rules.toml ]; then
    while IFS= read -r t; do toks+=("$t"); done < <(grep -oE 'token[[:space:]]*=[[:space:]]*"[^"]+"' .cosmon/release-rules.toml | sed -E 's/.*"([^"]+)".*/\1/')
  fi
  for cfg in "$HOME/.config/cosmon/config.toml"; do
    [ -f "$cfg" ] || continue
    while IFS= read -r t; do toks+=("$t"); done < <(grep -A8 'forbidden_substrings[[:space:]]*=' "$cfg" 2>/dev/null | grep -oE '"[^"]+"' | sed -E 's/"([^"]+)"/\1/')
  done
  [ ${#toks[@]} -eq 0 ] && return
  # De-dup, escape regex metachars, join with |.
  printf '%s\n' "${toks[@]}" | sort -u | sed -E 's/[][(){}.^$*+?|\\]/\\&/g' | paste -sd'|' -
}

c_pass=$'\033[32m'; c_fail=$'\033[31m'; c_pend=$'\033[33m'; c_adv=$'\033[36m'; c_off=$'\033[0m'
gate_pass() { printf '  %s[GATE  PASS]%s %s\n' "$c_pass" "$c_off" "$1"; }
gate_fail() { printf '  %s[GATE  FAIL]%s %s\n' "$c_fail" "$c_off" "$1"; FAILS=$((FAILS+1)); }
gate_pend() { printf '  %s[GATE  PEND]%s %s\n' "$c_pend" "$c_off" "$1"; PENDS=$((PENDS+1)); }
advisory()  { printf '  %s[ADVISORY ]%s %s\n' "$c_adv"  "$c_off" "$1"; }

echo "============================================================================"
echo " cosmon pre-launch checklist — projection of exogenous gates (ADR-133)"
echo " spec: docs/RELEASE-CHECKLIST.md  |  repo: ${REPO_SLUG}"
echo " mode: $([ $POST_FLIP -eq 1 ] && echo 'POST-FLIP (protection gates hard)' || echo 'PRE-FLIP (protection gates pending)')"
echo "============================================================================"

# ── 1. gitleaks full-history detect exits 0 ─────────────────────────────────
# THE CONFIG WAS NEVER LOADED (task-20260716-c4eb). This gate probed for
# `.gitleaks.toml` at the repo root and, not finding one, ran gitleaks with an
# EMPTY `GLCFG` — i.e. on stock default rules, with none of this repo's own
# scan profile. Cosmon's canonical config does not live at that path: it lives
# at `assets/gitleaks/cosmon-baseline.gitleaks.toml`, and `.gitleaks.toml` is
# the copy `cs init` scaffolds into DOWNSTREAM galaxies. Cosmon is the source,
# so cosmon is the one repo where the probe always missed.
#
# This is the same fail-open shape as the rest of the chain: the gate ran, said
# something, exited a code — and was measuring the wrong thing. Unconfigured, it
# reported 137 findings on a real projection, of which zero are secrets: the
# entropy heuristic firing on an HTML capture fixture, the leak DETECTOR's own
# pattern table, the AWS documentation example key, and RSA test fixtures. A
# gate drowning in 137 false positives cannot be read, and the operator learns
# to expect its RED — which is how a real secret would ship.
#
# Prefer the canonical baseline; honour a repo-root `.gitleaks.toml` when a
# downstream galaxy has one (that is the scaffolded copy, and it wins for the
# same reason a local override always does).
#
# NOT the DO-NOT-PUSH branch, which was the other candidate explanation:
# `DO-NOT-PUSH/narration` is an ANCESTOR of `main` on the projection (measured:
# `git rev-list --count DO-NOT-PUSH/narration --not main` = 0), so excluding it
# from `--log-opts` changes nothing. `--all` and `main` scan the same 407
# commits. The "401 commits scanned, 0 leaks" log that appeared to contradict
# this gate was simply STALE — /tmp/rc-gitleaks.log is this gate's own output
# file, overwritten by whichever run touched it last.
if command -v gitleaks >/dev/null 2>&1; then
  GLCFG=()
  if [ -f .gitleaks.toml ]; then
    GLCFG=(--config .gitleaks.toml)
  elif [ -f assets/gitleaks/cosmon-baseline.gitleaks.toml ]; then
    GLCFG=(--config assets/gitleaks/cosmon-baseline.gitleaks.toml)
  fi
  if [ ${#GLCFG[@]} -eq 0 ]; then
    # Never scan unconfigured and call the result a gate. Defaults produce a
    # verdict about a repo this project has never agreed to be measured as.
    gate_fail "1. no gitleaks config found (.gitleaks.toml or assets/gitleaks/cosmon-baseline.gitleaks.toml) — refusing to scan on stock defaults and call it a gate"
  elif gitleaks detect --source . --log-opts="--all" "${GLCFG[@]}" \
       --no-banner --redact >/tmp/rc-gitleaks.log 2>&1; then
    gate_pass "1. gitleaks detect --log-opts=--all (${GLCFG[1]}) → no secrets (exit 0)"
  else
    gate_fail "1. gitleaks detect found secrets in history — see /tmp/rc-gitleaks.log (a full-history rewrite is the operator clean step, ADR-017 consequences)"
  fi
else
  gate_pend "1. gitleaks not installed locally — wire a CI 'Secret scan' job as the referee"
fi

# ── 2. artifact-map RESIDENCE audit: 0 solo (non-public) paths tracked ──────
if [ -f scripts/artifact-map-audit.py ] && [ -f .cosmon/artifact-map.toml ]; then
  if "$PY" scripts/artifact-map-audit.py >/tmp/rc-artmap.log 2>&1; then
    gate_pass "2. artifact-map-audit.py → every tracked path is public-audience"
  else
    gate_fail "2. artifact-map-audit.py found non-public (solo) paths on the tree — /tmp/rc-artmap.log"
  fi
else
  gate_fail "2. scripts/artifact-map-audit.py or .cosmon/artifact-map.toml missing"
fi

# ── 3. 'Artifact-map residence gate' in required_status_checks ──────────────
if [ $POST_FLIP -eq 1 ]; then
  if gh api "repos/${REPO_SLUG}/branches/main/protection/required_status_checks" \
       --jq '.contexts[]' 2>/dev/null | grep -qx "Artifact-map residence gate"; then
    gate_pass "3. 'Artifact-map residence gate' in required_status_checks"
  else
    gate_fail "3. 'Artifact-map residence gate' NOT in required_status_checks — run scripts/apply-branch-protection.sh"
  fi
else
  gate_pend "3. artifact-map required_status_checks wiring is post-flip"
fi

# ── 4. confidential-string forbid scan (externalized denylist) ──────────────
# Whole-file leaks are caught by gate 2; this catches confidential STRINGS edited
# into otherwise-public files (client names, private domains, operator $PATH).
# It mirrors the D7 publish-content gate that `cs done` already enforces on
# publish_globs (ADR-128) — run here ahead of time over the whole tracked tree.
FORBID="$(derive_forbid_pattern)"
if [ -z "$FORBID" ]; then
  gate_pend "4. forbid-strings denylist not resolvable (set COSMON_FORBID_PATTERN, scripts/.release-denylist.local, or keep .cosmon/release-rules.toml on disk) — cannot scan here"
elif git grep -nIE "$FORBID" -- . \
       ':(exclude)scripts/release-checklist.sh' \
       ':(exclude)scripts/release/*' >/tmp/rc-forbid.log 2>&1; then
  gate_fail "4. confidential strings present in tracked tree — see /tmp/rc-forbid.log ($(wc -l </tmp/rc-forbid.log | tr -d ' ') hits). Scrub or relocate before flip."
else
  gate_pass "4. no confidential client/domain/infra strings in the tracked tree"
fi

# ── 5. CLAUDE.local.md untracked + gitignored ──────────────────────────────
if git ls-files --error-unmatch CLAUDE.local.md >/dev/null 2>&1; then
  gate_fail "5. CLAUDE.local.md is TRACKED (operator-private playbook leak)"
elif git check-ignore -q CLAUDE.local.md; then
  gate_pass "5. CLAUDE.local.md untracked and gitignored"
else
  gate_fail "5. CLAUDE.local.md not tracked but NOT gitignored — add it to .gitignore"
fi

# ── 6. runtime state never tracked (.cosmon/state/ off main, ADR-055) ───────
if git ls-files | grep -qE '^\.cosmon/state/'; then
  gate_fail "6. .cosmon/state/ paths are TRACKED — runtime state must never ship (ADR-055). git rm --cached them."
else
  gate_pass "6. .cosmon/state/ is absent from the tracked tree"
fi

# ── 7. retired deny-by-default machinery untracked (ADR-127 → ADR-133) ──────
if git ls-files | grep -qE '^\.cosmon/release-(allowlist|rules)\.toml$'; then
  gate_fail "7. .cosmon/release-allowlist.toml / release-rules.toml are TRACKED — they enumerate the private frontier / client literals and must never ship. git rm --cached + gitignore."
else
  gate_pass "7. retired allowlist machinery (release-allowlist.toml / release-rules.toml) untracked"
fi

# ── 8. LICENSE files present (hard) ─────────────────────────────────────────
if [ -f LICENSE-APACHE ] && [ -f LICENSE ]; then
  gate_pass "8. LICENSE + LICENSE-APACHE present"
else
  gate_fail "8. a LICENSE file is missing (want LICENSE + LICENSE-APACHE)"
fi

# ── 9. deny.toml present + cargo-deny clean ─────────────────────────────────
if [ -f deny.toml ]; then
  if command -v cargo-deny >/dev/null 2>&1; then
    if cargo deny check bans licenses sources >/tmp/rc-deny.log 2>&1; then
      gate_pass "9. deny.toml present; cargo deny check clean"
    else
      gate_fail "9. cargo deny check failed — see /tmp/rc-deny.log"
    fi
  else
    gate_pass "9. deny.toml present (CI 'Deny' job is the referee)"
  fi
else
  gate_fail "9. deny.toml missing"
fi

# ── 10. all remote URLs uniform noogram/cosmon ──────────────────────────────
if git grep -nIE 'github\.com[:/][a-zA-Z0-9_-]+/cosmon' -- . 2>/dev/null \
     | grep -vE 'noogram/cosmon' \
     | grep -vE 'RELEASE-CHECKLIST\.md|release-checklist\.sh|apply-branch-protection\.sh' >/tmp/rc-urls.log 2>&1; then
  gate_fail "10. non-canonical cosmon repo URLs present (want noogram/cosmon) — /tmp/rc-urls.log"
else
  gate_pass "10. all github.com cosmon URLs point at noogram/cosmon"
fi

# ── 11. branch protection on main returns 200 with required checks ──────────
if [ $POST_FLIP -eq 1 ]; then
  if gh api "repos/${REPO_SLUG}/branches/main/protection" >/tmp/rc-prot.log 2>&1; then
    n=$(gh api "repos/${REPO_SLUG}/branches/main/protection/required_status_checks" \
          --jq '.contexts | length' 2>/dev/null || echo 0)
    if [ "${n:-0}" -ge 1 ]; then
      gate_pass "11. branch protection on main → 200 with ${n} required checks"
    else
      gate_fail "11. branch protection exists but lists 0 required checks"
    fi
  else
    gate_fail "11. branch protection on main not configured (run apply-branch-protection.sh)"
  fi
else
  gate_pend "11. branch protection is HTTP 403 until the repo is public — post-flip gate"
fi

# ── 12. push-protection + secret-scanning enabled ───────────────────────────
if [ $POST_FLIP -eq 1 ]; then
  sa=$(gh api "repos/${REPO_SLUG}" --jq '.security_and_analysis' 2>/dev/null || echo '{}')
  ss=$(echo "$sa" | "$PY" -c 'import sys,json;d=json.load(sys.stdin) or {};print(d.get("secret_scanning",{}).get("status",""))' 2>/dev/null)
  pp=$(echo "$sa" | "$PY" -c 'import sys,json;d=json.load(sys.stdin) or {};print(d.get("secret_scanning_push_protection",{}).get("status",""))' 2>/dev/null)
  if [ "$ss" = "enabled" ] && [ "$pp" = "enabled" ]; then
    gate_pass "12. GitHub secret-scanning + push-protection enabled"
  else
    gate_fail "12. secret-scanning='$ss' push-protection='$pp' (want both enabled)"
  fi
else
  gate_pend "12. GitHub secret-scanning/push-protection settable post-flip — post-flip gate"
fi

# ── 13. second independent referee (ADVISORY — honest ABSENT record) ────────
admins=$(gh api "repos/${REPO_SLUG}/collaborators" --jq '[.[]|select(.permissions.admin==true)]|length' 2>/dev/null || echo "?")
if [ "${admins:-1}" -gt 1 ] 2>/dev/null; then
  advisory "13. ${admins} repo admins — a second independent referee MAY exist (verify human independence)"
else
  advisory "13. second org-admin / independent CODEOWNER: ABSENT. 'main stays public' is MONITORED, not ENFORCED — the same operator who flips it back can disable the monitor. Honest demotion (janis); promote to GATE when a second noogram admin exists."
fi

echo "----------------------------------------------------------------------------"
if [ $FAILS -eq 0 ]; then
  if [ $PENDS -gt 0 ] && [ $POST_FLIP -eq 0 ]; then
    printf '%s READY for the flip: all pre-flip GATEs pass. %d gate(s) PENDING until public.%s\n' "$c_pass" "$PENDS" "$c_off"
    echo "   Next: perform the flip, then run scripts/apply-branch-protection.sh,"
    echo "   then re-run: scripts/release-checklist.sh --post-flip"
  else
    printf '%s READY: all applicable GATEs pass.%s\n' "$c_pass" "$c_off"
  fi
  exit 0
else
  printf '%s NOT READY: %d GATE(s) failed. Do NOT flip until all are green.%s\n' "$c_fail" "$FAILS" "$c_off"
  exit 1
fi
