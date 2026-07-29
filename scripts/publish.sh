#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-only
# ─────────────────────────────────────────────────────────────────────────────
# publish.sh --check — the release membrane's STRUCTURAL referee.
#
# WHY THIS EXISTS
# ---------------
# `CLAUDE.md` has ordered "Run `scripts/publish.sh --check` for release-bound
# changes" for as long as the contributor guide has existed. The file never did.
# `git log --all -- scripts/publish.sh` is EMPTY: it was not deleted, it was
# never written. So the sentence guarding the public projection named a property
# nobody measured — the exact defect class this repository exists to refuse.
#
# The instruction was not retired in favour of `release-checklist.sh`, because
# that referee does not cover the clauses the guide states loudest. Measured on
# a green tree (2026-07-28, every gate PASS, `release-checklist.sh` exit 0):
#
#   bench/smoke-dispatch.sh:22   MOLECULE_DIR default → an operator home path
#   docker/spore-e2e/build.sh:12 SPORE_ZIP default → an operator home path
#   smoke-dispatch.sh:38         MOLECULE_DIR default → an operator home path
#   docs/specs/openjdk.jdk       a tracked SYMLINK whose target is an absolute
#                                machine path — invisible to every text scan in
#                                the tree, because `git grep` does not read
#                                symlink blobs.
#
# Four violations of "machine paths ... must never be tracked", tracked on main,
# with the membrane reporting READY. That is what this script is for.
#
# WHAT IT CHECKS — derived from the invariant, not from the filename
# ------------------------------------------------------------------
# The sentences around the instruction state the actual rule:
#
#   "Runtime state, credentials, machine paths, internal identifiers, and
#    unreviewed binary assets must never be tracked. A public release is
#    produced from an isolated scrubbed projection; never rewrite the
#    development repository in place."
#
#   A. RUNTIME STATE   — no tracked path under a runtime-state root.
#   B. CREDENTIALS     — no credential-shaped string in a tracked file.
#   C. MACHINE PATHS   — no per-operator home path in tracked content, and no
#                        absolute target on a tracked symlink.
#   D. BINARY ASSETS   — every tracked binary is pinned in a reviewed manifest.
#   E. RESIDENCE       — delegated to `scripts/artifact-map-audit.py`, which
#                        owns "who is this artifact for" (ADR-133).
#
# "Internal identifiers" is deliberately NOT a check here: the confidential
# denylist is operator-private by construction (ADR-127 §6 — a detector that
# names what it forbids re-leaks it), and it already has two referees,
# `release-checklist.sh` GATE 4 and `confidentiality-lint.sh`. This script owns
# only the clauses that are STRUCTURAL — decidable from a fresh clone with no
# operator-local file, no network, and no installed scanner. That is why it can
# be a hard CI gate and they cannot.
#
# CREDENTIALS ARE DETECTED, NEVER PRINTED
# ---------------------------------------
# A scrub gate is the code most likely to be handed a real secret. Check B never
# writes a matched value to stdout, stderr, or a file: a finding is reported as
# `path:line: <rule> (value withheld, sha256:xxxxxxxx)`. The digest is stable
# enough to correlate two reports and useless for recovering the secret.
#
# WHY THERE IS NO MODE THAT WRITES
# --------------------------------
# `--check` is the only accepted argument. The guide says the public release is
# produced from an isolated scrubbed projection and that the development
# repository is never rewritten in place — so the tool named in that sentence
# must have no in-place verb to reach for. The refusal is the surface, not a
# policy comment. Anything other than `--check` exits 2.
#
# USAGE
#   scripts/publish.sh --check
#
# EXIT
#   0 — the tracked tree would produce a clean public projection.
#   1 — at least one violation; every offending path is named above the summary.
#   2 — usage error, or a referee that could not run (never a silent pass).
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail

if [ "${1:-}" != "--check" ] || [ "$#" -ne 1 ]; then
  cat >&2 <<'USAGE'
usage: scripts/publish.sh --check

--check is the only mode. This tool never rewrites the development repository:
the public release is produced from an isolated scrubbed projection, so the
publish gate deliberately has no in-place verb.
USAGE
  exit 2
fi

ROOT="$(git rev-parse --show-toplevel 2>/dev/null)" || {
  echo "publish: not a git repository — the gate audits the TRACKED tree." >&2
  exit 2
}
cd "$ROOT" || exit 2

FAILS=0
c_pass=$'\033[32m'; c_fail=$'\033[31m'; c_off=$'\033[0m'
pass() { printf '  %s[PASS]%s %s\n' "$c_pass" "$c_off" "$1"; }
fail() { printf '  %s[FAIL]%s %s\n' "$c_fail" "$c_off" "$1"; FAILS=$((FAILS + 1)); }

# Findings accumulate here so the offending paths print together, above the
# summary, instead of interleaving with the check headings.
findings="$(mktemp)" || exit 2
trap 'rm -f "$findings"' EXIT

echo "============================================================================"
echo " publish --check — structural release membrane (runtime state · credentials"
echo "                   · machine paths · binary assets · residence)"
echo "============================================================================"

# ── Shared: the tracked text surface, and the tracked symlink surface ────────
# Two disjoint sets, because they need different scans. `git grep` reads blobs
# of regular files only, so a symlink's target is invisible to every content
# rule in this repository — that is how an absolute machine path survived on
# main. Enumerate symlinks explicitly and read their blobs by hand.
list_symlinks() { git ls-files -s | awk '$1 == "120000" { $1=$2=$3=""; sub(/^\t/, ""); sub(/^ +/, ""); print }'; }

# ── A. RUNTIME STATE ─────────────────────────────────────────────────────────
# Regenerable, machine-local, and frequently carrying molecule payloads. It must
# not be in the index at all; whether it is gitignored is a separate question
# (`release-checklist.sh` GATE 6 covers `.cosmon/state/` specifically — this is
# the broader family, and it is cheap to keep both honest).
#
# NOT `*.log`. The first draft of this rule flagged every tracked `.log` and
# reported 30 violations — all of them wrong. `docs/benches/**/bench-*.log` and
# `docs/specs/tlc-out-*.log` are REVIEWED EVIDENCE: a benchmark transcript and a
# TLC model-checker output, committed on purpose so a claim in a doc has a
# receipt a reader can open. The artifact map classifies them public-audience.
# "Runtime state" in the guide means state a MACHINE regenerates and a clone
# must not inherit — a fleet's live molecules, a build directory, a worker
# roster — not a frozen artifact someone chose to publish. Matching on the
# extension would have made this gate a 30-line wall of noise on its first run,
# which is how a gate teaches its operator to ignore it.
RUNTIME_STATE_RE='^(\.cosmon/state/|target/|.*/target/|\.cosmon/worker-roster|\.cosmon/autopilot\.(on|off)$)'
if git ls-files | grep -qE "$RUNTIME_STATE_RE"; then
  git ls-files | grep -E "$RUNTIME_STATE_RE" | sed 's/^/  runtime-state: /' >>"$findings"
  fail "A. runtime state is TRACKED (see paths below) — git rm --cached and gitignore"
else
  pass "A. no runtime state in the tracked tree"
fi

# ── B. CREDENTIALS ───────────────────────────────────────────────────────────
# High-precision, vendor-anchored patterns only. The broad entropy net is
# gitleaks' job (`release-checklist.sh` GATE 1), and it is not always installed;
# these rules are the subset that can be a HARD gate from a bare clone because
# they essentially cannot false-positive: each one matches a token shape that no
# prose produces by accident.
#
# The detector must not be its own leak (ADR-127 §6), so the rules below are
# written as shapes, never as sample values — and this file excludes itself and
# the gitleaks baseline (which legitimately carries a pattern table and the
# vendor's own published documentation key) from the scan.
#
# OPT-OUT — a synthetic test vector (cosmon's own leak detector must contain the
# shapes it detects, or its tests assert nothing) declares itself inline:
#   let line = b"token: ghp_AAAA...\n"; // publish: allow — synthetic test vector
# Per LINE wherever a line can carry a comment — which is everywhere except a
# handful of formats (PEM) whose first byte is already the shape. Those get the
# tracked sidecar below, which keeps the same property by other means. What is
# never allowed is a pathspec exclusion: that is a permanent blind spot no
# reviewer sees again, whereas a marker or a sidecar is a waiver someone had to
# type and a reviewer reads in the diff. Same escape-hatch doctrine as
# `check-fixture-independence.sh`. It is deliberately the only way through:
# a real secret can be waived, but only by writing the word "allow" next to it.
OPTOUT_RE='publish: allow'

# THE SIDECAR WAIVER — for a format that cannot carry a comment.
#
# The inline marker above is the only escape hatch for anything with a comment
# syntax, and that is the whole point: someone had to type a sentence on that
# line and a reviewer reads it in the diff. A PEM file has no such line. Its
# first byte is `-----BEGIN`, and any preamble risks the parsers that read it,
# so the marker is unwritable exactly where a private-key SHAPE is guaranteed.
#
# The answer is NOT a pathspec exclusion in this script: that is the permanent
# blind spot the doctrine refuses, invisible in every future review. It is a
# tracked sidecar `<path>.publish-allow` whose CONTENT is the reason. That
# keeps every property the inline marker has — one waiver per artefact, written
# by a human, visible in the diff, greppable from the tree — and adds one the
# inline marker cannot have here: it names a whole file, because a whole file
# is what the format forces.
#
# An EMPTY sidecar does not waive. A waiver with no reason is an exclusion
# wearing a costume.
#
# FOUR CONDITIONS, and each closes a way the first version was a whole-file
# exclusion in disguise. It required only that the sidecar be non-empty and
# tracked, and it was consulted BEFORE the rule was known — so a single byte in
# `x.pem.publish-allow` waived every credential rule on `x.pem` forever, not
# merely the PEM shape the format forced. The contributor guide's words are
# "per line, never per file: a whole-file exclusion is a blind spot nobody sees
# again". That was one.
#
#   1. FORMAT. Only extensions that genuinely cannot carry a comment. Anything
#      else has the inline marker, which is strictly better, and this hatch may
#      not become the easy way around it.
#   2. REASON. The body must contain "$OPTOUT_RE". Non-empty is not a reason;
#      a byte is not a sentence someone had to write.
#   3. RULE. Only the two key-shaped rules. A PEM file has no business
#      containing a GitHub token, and if it does, that is a finding the format
#      never forced anyone to waive.
#   4. BLOB. The waived content is PINNED by hash in the sidecar
#      (`publish-allow-blob: <sha>`). Replace the file and the waiver stops
#      applying — otherwise a waiver written for a synthetic test key silently
#      inherits whatever bytes land at that path next.
#
# All four are decidable from the tree alone and all four are REQUIRED. Nothing
# here is opt-in: condition 4 used to apply "when the sidecar states a hash",
# which handed the strongest of the four conditions to the party it constrains.
# A sidecar omitting one line was accepted unpinned and its waiver outlived its
# file.
SIDECAR_EXT_RE='\.(pem|der|key|crt|cer|p12|pfx|jks|keystore)$'
SIDECAR_RULES='pem-private-key private-key-openssh'

credential_waived_by_sidecar() {
  local path="$1" rule="$2" sidecar="$1.publish-allow" body pinned actual
  # (3) rule-specific.
  case " $SIDECAR_RULES " in *" $rule "*) ;; *) return 1 ;; esac
  # (1) comment-less formats only.
  printf '%s' "$path" | grep -qE "$SIDECAR_EXT_RE" || return 1
  [ -s "$sidecar" ] || return 1
  git ls-files --error-unmatch -- "$sidecar" >/dev/null 2>&1 || return 1
  body="$(cat "$sidecar" 2>/dev/null)" || return 1
  # (2) a real reason, not any byte.
  case "$body" in *"$OPTOUT_RE"*) ;; *) return 1 ;; esac
  # (4) blob pin. REQUIRED, not opt-in.
  #
  # This read `if [ -n "$pinned" ]` — the pin applied only when the sidecar
  # chose to state one, which makes the control opt-in by the party it
  # constrains. A sidecar that simply omits its `publish-allow-blob:` line was
  # accepted unpinned, so a waiver written for a synthetic test key survived
  # the file being replaced by a real one. That is the same shape as a pathspec
  # exclusion, arriving through the escape hatch: a waiver nobody has to renew.
  #
  # An author who cannot state the hash of the file they are waiving has not
  # read it, and the hash is one command away (`git hash-object -- <path>`).
  pinned="$(printf '%s\n' "$body" | sed -n 's/^publish-allow-blob:[[:space:]]*\([0-9a-f]\{7,\}\).*$/\1/p' | head -1)"
  [ -n "$pinned" ] || return 1
  actual="$(git hash-object -- "$path" 2>/dev/null)" || return 1
  case "$actual" in "$pinned"*) ;; *) return 1 ;; esac
  return 0
}
CRED_EXCLUDE=(
  ':(exclude)scripts/publish.sh'
  ':(exclude)scripts/publish.test.sh'
  ':(exclude)assets/gitleaks/*'
)
# rule-name<TAB>ERE
CRED_RULES=$'pem-private-key\t-----BEGIN [A-Z ]*PRIVATE KEY-----\naws-access-key-id\t(A3T[A-Z0-9]|AKIA|ASIA|ABIA|ACCA)[A-Z0-9]{16}\ngithub-token\tgh[pousr]_[A-Za-z0-9]{36,}\nslack-token\txox[abposr]-[0-9A-Za-z-]{12,}\njwt\teyJ[A-Za-z0-9_-]{10,}\\.eyJ[A-Za-z0-9_-]{10,}\\.[A-Za-z0-9_-]+\nprivate-key-openssh\t-----BEGIN OPENSSH PRIVATE KEY-----'

# Digest a matched value without ever emitting it. Reads on stdin so the value
# never becomes an argv entry (argv is world-readable in `ps`).
digest() {
  if command -v shasum >/dev/null 2>&1; then
    shasum -a 256 | cut -c1-8
  elif command -v sha256sum >/dev/null 2>&1; then
    sha256sum | cut -c1-8
  else
    # No digest available: still report the finding, just without correlation.
    cat >/dev/null
    printf 'nodigest'
  fi
}

cred_hits=0
while IFS=$'\t' read -r rule re; do
  [ -z "$rule" ] && continue
  # Whole lines, so the inline waiver is visible; the line itself is never
  # emitted — only its path, its number, and the digest of the matched token.
  while IFS= read -r line; do
    [ -z "$line" ] && continue
    loc="${line%%:*}"; rest="${line#*:}"
    lno="${rest%%:*}"; body="${rest#*:}"
    case "$body" in *"$OPTOUT_RE"*) continue ;; esac
    credential_waived_by_sidecar "$loc" "$rule" && continue
    # `-e` for the same reason as the `git grep` below: a pattern starting
    # with `-` is otherwise parsed as an option, and the digest silently
    # becomes the digest of the empty string.
    val="$(printf '%s' "$body" | grep -oE -e "$re" | head -1)"
    d="$(printf '%s' "$val" | digest)"
    unset val body
    printf '  credential: %s:%s: %s (value withheld, sha256:%s)\n' \
      "$loc" "$lno" "$rule" "$d" >>"$findings"
    cred_hits=$((cred_hits + 1))
    # `-e` is load-bearing, not style. Two of these rules begin with `-`
    # (`-----BEGIN … PRIVATE KEY-----`), and without `-e` git parses the
    # pattern as an option, errors out, and — with stderr sent to /dev/null —
    # the loop simply reads nothing and the rule reports a clean tree it never
    # searched. Found by the per-rule canary below on the day it was written:
    # both PEM rules had never once run.
  done < <(git grep -nIE -e "$re" -- . "${CRED_EXCLUDE[@]}" 2>/dev/null)
done <<<"$CRED_RULES"

# CANARY — a detector that cannot fire is not a detector. Prove on every run
# that EVERY rule in $CRED_RULES still fires, through the SAME engine the scan
# above uses, so a REGEX regression reds the build instead of reporting a clean
# tree it never scanned.
#
# IT DOES NOT COVER THE PATHSPEC AXIS, and this sentence used to claim it did.
# The canary's files live at the root of a throwaway repo where every real
# exclusion is inert, so the pathspec machinery is invoked and never exercised.
# That axis is held by the $CRED_EXCLUDE_REVIEWED pin above instead, by a
# different mechanism, for the reason stated there. Saying so here is the point:
# a control that names a property it does not measure is the defect this whole
# gate exists to refuse.
#
# THREE PROPERTIES, EACH ONE LEARNED FROM A WAY THE OLD CANARY LIED:
#
#   1. The pattern is READ OUT OF $CRED_RULES, never re-typed here. The old
#      canary carried its own hardcoded copy of the github-token regex, so
#      breaking or deleting the real rule left it firing happily on its
#      private copy. A canary that tests a copy tests nothing.
#   2. The engine is `git grep -nIE … -- <path> "${CRED_EXCLUDE[@]}"`, in a
#      throwaway git repository — the same binary, the same flags, the same
#      pathspec machinery as the scan. The old canary used plain `grep` on a
#      loose file, so the git-grep regression it named as its whole purpose
#      was the one thing it could not see.
#   3. EVERY rule is exercised, not one. Under the old canary every rule but
#      `github-token` could be dead and the PASS line still printed.
#
# The must-hits below are SHAPE-ONLY synthetic samples — filler runs and the
# literal word EXAMPLE. None is, or has ever been, a credential. They are the
# same doctrine as the module header: cosmon's own leak detector must contain
# the shapes it detects. They are keyed BY RULE NAME, and the coverage is
# checked in both directions: a rule with no must-hit is a rule nobody proved
# alive, and a must-hit with no rule is the fossil of a rule that was deleted
# while the canary went on reporting green.
#
# rule-name<TAB>synthetic shape-only must-hit
CRED_MUST_HITS=$'pem-private-key\t-----BEGIN RSA PRIVATE KEY-----\naws-access-key-id\tAKIAEXAMPLEEXAMPLE00\ngithub-token\tghp_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\nslack-token\txoxb-EXAMPLE00000000\njwt\teyJEXAMPLE0000.eyJEXAMPLE0000.EXAMPLE\nprivate-key-openssh\t-----BEGIN OPENSSH PRIVATE KEY-----'

# Echo the synthetic must-hit registered for rule $1; non-zero when none is.
# A linear scan rather than an associative array: bash 3.2 ships on macOS and
# has none, and the table is six rows.
canary_must_hit() {
  local want="$1" r v
  while IFS=$'\t' read -r r v; do
    if [ "$r" = "$want" ]; then
      printf '%s' "$v"
      return 0
    fi
  done <<<"$CRED_MUST_HITS"
  return 1
}

# Echo the ERE registered for rule $1; non-zero when the rule is gone.
canary_rule_exists() {
  local want="$1" r v
  while IFS=$'\t' read -r r v; do
    [ "$r" = "$want" ] && return 0
  done <<<"$CRED_RULES"
  return 1
}

canary_dir="$(mktemp -d)" || exit 2
canary_fail() {
  rm -rf "$canary_dir"
  echo "publish: credential-matcher CANARY FAILED — $1" >&2
  echo "  Refusing to report a clean tree we did not actually scan." >&2
  exit 2
}
git -C "$canary_dir" init -q >/dev/null 2>&1 ||
  canary_fail "could not create the throwaway git repository the canary scans"

# THE PATHSPEC AXIS, which this canary cannot reach and must therefore pin.
#
# The canary below writes `canary-<rule>.txt` at the ROOT of a throwaway repo,
# while every $CRED_EXCLUDE entry is a real-repo path. So all three exclusions
# are inert in there BY CONSTRUCTION: the pathspec machinery is invoked and can
# never be exercised. Constructed proof: adding ':(exclude)crates/*' to
# $CRED_EXCLUDE hid a tracked synthetic hit while the canary stayed green — the
# regex axis is closed (corrupt a rule's ERE and the loop below reds), the
# pathspec axis was wide open, and the canary's own header claimed both.
#
# Making the throwaway repo mirror the real exclusions would be a second copy of
# the list, drifting on its own. The property that actually matters is simpler
# and is checkable directly: the exclusion set is REVIEWED, and any change to it
# is a change someone had to make here AND here. A new blind spot cannot be
# added without editing this literal, and editing this literal is what a
# reviewer sees in the diff — the same doctrine as the inline waiver.
#
# Each entry is justified below. `publish.sh` and `publish.test.sh` are the
# detector and its suite: they must contain the shapes they detect (ADR-127 §6).
# `assets/gitleaks/*` is the vendor's own pattern table and published
# documentation key. Nothing else is excluded, and nothing else may be without
# this line changing.
CRED_EXCLUDE_REVIEWED=$'\n:(exclude)scripts/publish.sh\n:(exclude)scripts/publish.test.sh\n:(exclude)assets/gitleaks/*\n'
cred_exclude_actual=$'\n'
for e in "${CRED_EXCLUDE[@]}"; do cred_exclude_actual="$cred_exclude_actual$e"$'\n'; done
if [ "$cred_exclude_actual" != "$CRED_EXCLUDE_REVIEWED" ]; then
  canary_fail "\$CRED_EXCLUDE no longer matches the reviewed set. A pathspec
  exclusion is a permanent blind spot no reviewer sees again, and the canary
  below cannot detect one (its throwaway repo has no paths any exclusion
  covers). Every entry must be justified in the comment above this check, and
  \$CRED_EXCLUDE_REVIEWED updated in the same diff, so adding a blind spot is
  something a human writes down twice and a reviewer reads once.
    reviewed: $(printf '%s' "$CRED_EXCLUDE_REVIEWED" | tr '\n' ' ')
    actual:   $(printf '%s' "$cred_exclude_actual" | tr '\n' ' ')"
fi

while IFS=$'\t' read -r rule re; do
  [ -z "$rule" ] && continue
  hit="$(canary_must_hit "$rule")" ||
    canary_fail "rule '$rule' has no synthetic must-hit in \$CRED_MUST_HITS, so nothing proves it can still fire"
  f="canary-$rule.txt"
  printf 'shape-only synthetic sample, never a credential: %s\n' "$hit" >"$canary_dir/$f"
  git -C "$canary_dir" add -- "$f" >/dev/null 2>&1 ||
    canary_fail "could not track the canary file for rule '$rule'"
  # Same engine, same flags, same excludes as the scan — and scoped to THIS
  # rule's own file, so one rule's must-hit can never stand in for another's.
  if ! git -C "$canary_dir" grep -nIE -e "$re" -- "$f" "${CRED_EXCLUDE[@]}" >/dev/null 2>&1; then
    canary_fail "rule '$rule' did not match its own synthetic must-hit through git grep"
  fi
done <<<"$CRED_RULES"

# The other direction: a must-hit whose rule is gone means the scan quietly
# stopped covering a shape while this canary went on reporting green.
while IFS=$'\t' read -r rule hit; do
  [ -z "$rule" ] && continue
  canary_rule_exists "$rule" ||
    canary_fail "\$CRED_MUST_HITS still carries '$rule' but \$CRED_RULES no longer does — the shape is unscanned"
done <<<"$CRED_MUST_HITS"

rm -rf "$canary_dir"

if [ "$cred_hits" -gt 0 ]; then
  fail "B. ${cred_hits} credential-shaped string(s) in the tracked tree (values withheld)"
else
  # The parenthetical is precise on purpose: what the canary establishes is
  # that EVERY rule still fires through git grep, not that some rule does.
  pass "B. no credential-shaped strings in the tracked tree (every rule canaried)"
fi

# ── C. MACHINE PATHS ─────────────────────────────────────────────────────────
# A per-OPERATOR home path is a leak: it names a human and cannot resolve on any
# other machine, so a clone silently inherits a broken default. A per-PLATFORM
# path (`/opt/homebrew/...`, `/usr/local/...`) is NOT a leak — it names a
# package layout, not a person, and this tree probes several of them with
# fallbacks on purpose. The rule therefore targets `/Users/<c>` and `/home/<c>`
# and accepts them only when `<c>` is a manifestly generic placeholder or a
# service account. Fail-closed: an unknown component is a leak until an
# attributable commit adds it below.
#
# No real login is ever listed here — every entry is generic by inspection, so
# the allowlist cannot itself become the leak it guards against.
#
# The membership test is `case " $ALLOW " in *" $c "*`, which compares against
# SPACE-delimited entries — so the list is normalised to a single line before
# use. Written across several source lines it silently failed for every entry
# that happened to sit at a line boundary: `cosmon-worker`, the container
# service account, was in the list and still produced 34 findings, because its
# left neighbour was a newline and not a space. An allowlist that quietly
# ignores a third of itself is a gate that reds on its own configuration.
HOME_ALLOW_RAW="you me my user users someone somebody op ops x y z e u it its
 test tester alice bob carol foo foobar bar baz qux example demo dev ci runner
 root ubuntu tenant tenants tenant_auditor custom worker workers cosmon
 cosmon-worker researcher researchers tmp home name login"
HOME_ALLOW=" $(printf '%s' "$HOME_ALLOW_RAW" | tr -s '[:space:]' ' ' | sed 's/^ *//; s/ *$//') "

home_hits=0
while IFS= read -r line; do
  [ -z "$line" ] && continue
  loc="${line%%:*}"; rest="${line#*:}"; lno="${rest%%:*}"; rest="${rest#*:}"
  case "$rest" in *"$OPTOUT_RE"*) continue ;; esac
  while IFS= read -r tok; do
    comp="${tok#/Users/}"; comp="${comp#/home/}"
    [ -z "$comp" ] && continue
    lcomp="$(printf '%s' "$comp" | tr '[:upper:]' '[:lower:]')"
    case "$HOME_ALLOW" in *" $lcomp "*) continue ;; esac
    printf '  machine-path: %s:%s: %s (home component "%s" is not a generic placeholder)\n' \
      "$loc" "$lno" "$tok" "$comp" >>"$findings"
    home_hits=$((home_hits + 1))
  done < <(printf '%s\n' "$rest" | grep -oE '/(Users|home)/[A-Za-z0-9_][A-Za-z0-9_.+-]*')
done < <(git grep -nIE '/(Users|home)/[A-Za-z0-9_]' -- . ':(exclude)scripts/publish.sh' ':(exclude)scripts/publish.test.sh' 2>/dev/null)

# Symlink targets — the surface no content scan in this tree can see. A tracked
# symlink must be repo-relative; an absolute target points outside the clone and
# is by construction a machine path.
sym_hits=0
while IFS= read -r ln; do
  [ -z "$ln" ] && continue
  blob="$(git rev-parse ":$ln" 2>/dev/null)" || continue
  tgt="$(git cat-file -p "$blob" 2>/dev/null)" || continue
  case "$tgt" in
    /*)
      printf '  machine-path: %s -> %s (tracked symlink with an ABSOLUTE target)\n' "$ln" "$tgt" >>"$findings"
      sym_hits=$((sym_hits + 1))
      ;;
  esac
done < <(list_symlinks)

if [ $((home_hits + sym_hits)) -gt 0 ]; then
  fail "C. ${home_hits} operator home path(s) + ${sym_hits} absolute symlink target(s) tracked"
else
  pass "C. no operator machine paths in tracked content or symlink targets"
fi

# ── D. BINARY ASSETS ─────────────────────────────────────────────────────────
# "Unreviewed binary assets must never be tracked." A binary cannot be reviewed
# in a diff, so the reviewable substitute is a PIN: every tracked binary must
# appear in `assets/reviewed-binaries.sha256` with the digest that was reviewed.
# Adding a binary, or changing one, then requires editing that manifest in the
# same commit — which is the visible, attributable act a blob diff is not.
MANIFEST="assets/reviewed-binaries.sha256"
bin_hits=0
if [ ! -f "$MANIFEST" ]; then
  printf '  binary-asset: %s is missing — every tracked binary must be pinned there\n' "$MANIFEST" >>"$findings"
  bin_hits=1
else
  # Tracked, regular (not symlink), and not detected as text by grep -I.
  while IFS= read -r f; do
    [ -z "$f" ] && continue
    want="$(awk -v p="$f" '$2 == p { print $1 }' "$MANIFEST" | head -1)"
    if [ -z "$want" ]; then
      printf '  binary-asset: %s is TRACKED but not pinned in %s\n' "$f" "$MANIFEST" >>"$findings"
      bin_hits=$((bin_hits + 1))
      continue
    fi
    got="$(git cat-file -p ":$f" 2>/dev/null | digest)"
    wantshort="$(printf '%s' "$want" | cut -c1-8)"
    if [ "$got" != "$wantshort" ]; then
      printf '  binary-asset: %s content differs from its reviewed pin in %s\n' "$f" "$MANIFEST" >>"$findings"
      bin_hits=$((bin_hits + 1))
    fi
  done < <(
    git ls-files -s | awk '$1 != "120000" { $1=$2=$3=""; sub(/^[ \t]+/, ""); print }' |
      while IFS= read -r f; do
        [ -f "$f" ] || continue
        grep -qI . "$f" 2>/dev/null || printf '%s\n' "$f"
      done
  )
  # A pin for a path that is no longer tracked is stale bookkeeping, not a leak,
  # but it rots the manifest into noise — so it fails too.
  while IFS= read -r p; do
    [ -z "$p" ] && continue
    if ! git ls-files --error-unmatch "$p" >/dev/null 2>&1; then
      printf '  binary-asset: %s is pinned in %s but no longer tracked (stale pin)\n' "$p" "$MANIFEST" >>"$findings"
      bin_hits=$((bin_hits + 1))
    fi
  done < <(grep -vE '^\s*(#|$)' "$MANIFEST" | awk '{ print $2 }')
fi

if [ "$bin_hits" -gt 0 ]; then
  fail "D. ${bin_hits} unreviewed / unpinned / stale binary asset(s)"
else
  pass "D. every tracked binary is pinned in ${MANIFEST}"
fi

# ── E. RESIDENCE (delegated) ─────────────────────────────────────────────────
# "Who is this artifact for" is owned by the artifact map (ADR-133). Delegate
# rather than re-implement; a second opinion on audience would be a second
# source of truth. A referee that cannot run is exit 2, never a silent pass.
PY="python3"
if ! "$PY" -c 'import tomllib' >/dev/null 2>&1; then
  for cand in python3.13 python3.12 python3.11; do
    if command -v "$cand" >/dev/null 2>&1 && "$cand" -c 'import tomllib' >/dev/null 2>&1; then PY="$cand"; break; fi
  done
fi
if [ ! -f scripts/artifact-map-audit.py ]; then
  fail "E. scripts/artifact-map-audit.py is missing — residence has no referee"
elif ! "$PY" -c 'import tomllib' >/dev/null 2>&1; then
  echo "publish: no python3.11+ (stdlib tomllib) — residence referee cannot run." >&2
  exit 2
elif "$PY" scripts/artifact-map-audit.py >/dev/null 2>&1; then
  pass "E. artifact-map residence audit → every tracked path is public-audience"
else
  fail "E. artifact-map residence audit failed — run scripts/artifact-map-audit.py for detail"
fi

echo "----------------------------------------------------------------------------"
if [ -s "$findings" ]; then
  echo "offending paths:"
  sort -u "$findings"
  echo "----------------------------------------------------------------------------"
fi
if [ "$FAILS" -eq 0 ]; then
  printf '%s CLEAN: the tracked tree would produce a clean public projection.%s\n' "$c_pass" "$c_off"
  exit 0
fi
printf '%s NOT PUBLISHABLE: %d check(s) failed. Scrub or relocate before release.%s\n' "$c_fail" "$FAILS" "$c_off"
exit 1
