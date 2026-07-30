#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-only
# ─────────────────────────────────────────────────────────────────────────────
# publish.test.sh — prove `scripts/publish.sh --check` FAILS on a dirty tree.
#
# WHY A CONSTRUCTED CASE, AND NOT AN ASSERTION
# --------------------------------------------
# The gate this exercises exists because `CLAUDE.md` ordered a command that was
# never written: a check that named a property nobody measured. Replacing it
# with a gate whose only evidence is "it passes on our tree" would reproduce the
# defect one level up — a green run proves the scanner ran, not that it can red.
# So every check below gets a REAL violation in a REAL throwaway git repository,
# and the test fails unless the gate exits non-zero AND names the offending
# path. Deleting a rule from publish.sh must redden this file.
#
# Each case builds a fresh repo under a temp dir, so nothing here can touch the
# development tree — and the gate is read-only by construction anyway.
#
# Usage: ./scripts/publish.test.sh
# Exit:  0 all cases behaved · 1 a case did not · 2 harness error
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
GATE="$ROOT/scripts/publish.sh"
AUDIT="$ROOT/scripts/artifact-map-audit.py"
[ -x "$GATE" ] || { echo "publish.test: $GATE missing or not executable" >&2; exit 2; }
[ -f "$AUDIT" ] || { echo "publish.test: $AUDIT missing" >&2; exit 2; }

WORK="$(mktemp -d)" || exit 2
trap 'rm -rf "$WORK"' EXIT

fails=0
ok()   { printf '  \033[32m✓\033[0m %s\n' "$1"; }
bad()  { printf '  \033[31m✗\033[0m %s\n' "$1"; fails=$((fails + 1)); }

# Build a minimal but REAL repo: the gate under test, the residence referee it
# delegates to, a totality-satisfying artifact map, and an empty pin manifest.
# Everything a case adds on top is the violation being tested.
new_repo() {
  local d="$WORK/$1"
  mkdir -p "$d/scripts" "$d/.cosmon" "$d/assets" || return 1
  cp "$GATE" "$d/scripts/publish.sh"
  cp "$AUDIT" "$d/scripts/artifact-map-audit.py"
  cat >"$d/.cosmon/artifact-map.toml" <<'MAP'
[runtime-state]
location = [".cosmon/state/**/*"]
audience = "solo"

[code]
location = ["**/*"]
audience = "public"
MAP
  printf '# pin manifest (see scripts/publish.sh check D)\n' >"$d/assets/reviewed-binaries.sha256"
  git -C "$d" init -q 2>/dev/null || return 1
  git -C "$d" config user.email t@example.com
  git -C "$d" config user.name test
  printf '%s\n' "$d"
}

# Run the gate inside a fixture and capture output + status to FILES. Reading a
# status through a pipe reports the pipe's last stage, not the gate's — the
# exact mistake that lets a red gate look green.
run_gate() {
  local d="$1"
  git -C "$d" add -A >/dev/null 2>&1
  ( cd "$d" && bash scripts/publish.sh --check >"$WORK/out.txt" 2>&1 )
  printf '%s' "$?" >"$WORK/rc.txt"
}

# Write a sidecar waiver for $2 inside fixture $1, with reason $3 AND the blob
# pin the gate now requires. The pin is not decoration in these fixtures: an
# unpinned sidecar no longer waives anything, so a case that means to exercise
# some OTHER axis has to satisfy this one first or it proves nothing about the
# axis it names.
pin_sidecar() {
  local d="$1" path="$2" reason="$3" sha
  sha="$(git -C "$d" hash-object -- "$d/$path")" || return 1
  { printf '%s\n' "$reason"
    printf 'publish-allow-blob: %s\n' "$sha"; } >"$d/$path.publish-allow"
}

# A case passes when the gate exits 1 and the report names the offending path.
expect_fail() {
  local name="$1" d="$2" needle="$3"
  run_gate "$d"
  local rc; rc="$(cat "$WORK/rc.txt")"
  if [ "$rc" != "1" ]; then
    bad "$name — expected exit 1, got $rc"
    sed 's/^/      /' "$WORK/out.txt" | head -20
    return
  fi
  if ! grep -qF "$needle" "$WORK/out.txt"; then
    bad "$name — exited 1 but never named '$needle'"
    sed 's/^/      /' "$WORK/out.txt" | head -20
    return
  fi
  ok "$name"
}

echo "publish.test: constructing violating repositories"

# ── A. runtime state tracked ────────────────────────────────────────────────
d="$(new_repo case-runtime)" || exit 2
mkdir -p "$d/.cosmon/state/fleets/default"
printf '{"molecules":[]}\n' >"$d/.cosmon/state/fleets/default/index.json"
expect_fail "A. tracked runtime state reddens the gate" "$d" ".cosmon/state/fleets/default/index.json"

# ── B. credential-shaped string ─────────────────────────────────────────────
# Synthetic by construction: a fixed filler run, never a real token.
d="$(new_repo case-cred)" || exit 2
printf 'github_token = "ghp_%s"\n' "$(printf 'B%.0s' $(seq 1 36))" >"$d/conf.toml"
expect_fail "B. credential-shaped string reddens the gate" "$d" "conf.toml"

# …and it must be reported WITHOUT disclosing the value. A scrub gate that
# prints what it found has moved the secret from one file into a build log.
run_gate "$d"
if grep -qF "ghp_BBBB" "$WORK/out.txt"; then
  bad "B. the gate PRINTED the credential value — a scrub gate must never disclose"
else
  ok "B. credential reported without disclosing its value"
fi
if grep -qE 'sha256:[0-9a-f]{8}' "$WORK/out.txt"; then
  ok "B. finding carries a correlation digest instead of the value"
else
  bad "B. no redacted digest in the finding — reviewers cannot correlate reports"
fi

# ── B′. the inline waiver is honoured, and only per line ────────────────────
d="$(new_repo case-cred-waived)" || exit 2
printf 'github_token = "ghp_%s" # publish: allow — synthetic vector\n' "$(printf 'B%.0s' $(seq 1 36))" >"$d/conf.toml"
run_gate "$d"
if [ "$(cat "$WORK/rc.txt")" = "0" ]; then
  ok "B′. an inline 'publish: allow' waiver clears that line"
else
  bad "B′. waived line still red — the escape hatch does not work"
  sed 's/^/      /' "$WORK/out.txt" | head -20
fi
# The waiver must not bleed to a neighbour: a blind spot that spans lines is
# the whole-file exclusion this gate deliberately refuses.
printf 'other = "ghp_%s"\n' "$(printf 'C%.0s' $(seq 1 36))" >>"$d/conf.toml"
expect_fail "B′. the waiver does NOT cover the next line" "$d" "conf.toml"

# ── B′′. a rule whose pattern begins with `-` still runs ─────────────────────
# The regression that motivated `-e`: `git grep -nIE "-----BEGIN …"` parses the
# pattern as an OPTION, errors out, and — with stderr discarded — the loop
# reads nothing and the rule reports a clean tree it never searched. Two of the
# six rules were dead this way. A synthetic PEM header, never a key.
d="$(new_repo case-cred-leading-dash)" || exit 2
printf -- '-----BEGIN RSA PRIVATE KEY-----\n' >"$d/key.txt"
expect_fail "B′′. a rule whose pattern starts with '-' still matches" "$d" "key.txt"

# ── B′′′. the sidecar waiver, for a format with no comment syntax ─────────────
# A PEM cannot carry an inline marker; the tracked `<path>.publish-allow`
# sidecar is its equivalent, and it must behave like the inline one: it clears
# its own file and nothing else.
d="$(new_repo case-cred-sidecar)" || exit 2
printf -- '-----BEGIN RSA PRIVATE KEY-----\n' >"$d/test.pem"
# The pin is REQUIRED, so even the plain "does the hatch work at all" case
# states one. Before it was required, this fixture passed without it.
pin_sidecar "$d" "test.pem" 'publish: allow — synthetic test keypair, authorises nothing.'
run_gate "$d"
if [ "$(cat "$WORK/rc.txt")" = "0" ]; then
  ok "B′′′. a tracked '<path>.publish-allow' sidecar clears that file"
else
  bad "B′′′. sidecar-waived file still red — the escape hatch does not work"
  sed 's/^/      /' "$WORK/out.txt" | head -20
fi
# An EMPTY sidecar is an exclusion in a costume and must NOT waive.
: >"$d/test.pem.publish-allow"
expect_fail "B′′′. an EMPTY sidecar does not waive — a waiver needs a reason" "$d" "test.pem"
# And it must not bleed to a neighbouring file.
pin_sidecar "$d" "test.pem" 'publish: allow — synthetic test keypair, authorises nothing.'
printf -- '-----BEGIN RSA PRIVATE KEY-----\n' >"$d/other.pem"
expect_fail "B′′′. the sidecar does NOT cover a neighbouring file" "$d" "other.pem"

# ── B4. the sidecar waives ONE RULE CLASS, not every rule on the file ────────
# The first version consulted the sidecar before the rule was known and took
# `continue` before the hit was counted, so any single byte in a sidecar waived
# EVERY credential rule on that path forever. That is the whole-file exclusion
# the contributor guide forbids in as many words. A PEM file has no business
# carrying a GitHub token, and the format never forced anyone to waive one.
d="$(new_repo case-cred-sidecar-rule)" || exit 2
printf -- '-----BEGIN RSA PRIVATE KEY-----\n' >"$d/test.pem"
printf 'ghp_%s\n' "$(printf 'C%.0s' $(seq 1 36))" >>"$d/test.pem"
# Pinned, so the refusal below is the RULE-CLASS axis and not the missing pin.
pin_sidecar "$d" "test.pem" 'publish: allow - synthetic test keypair, authorises nothing.'
expect_fail "B4. the sidecar does NOT waive a non-key rule on the same file" "$d" "github-token"

# ── B4b. only a format that genuinely cannot carry a comment ─────────────────
# Anything with a comment syntax has the inline marker, which is strictly
# better because the reason sits on the offending line. The sidecar may not
# become the easy way around it.
d="$(new_repo case-cred-sidecar-ext)" || exit 2
printf -- '-----BEGIN RSA PRIVATE KEY-----\n' >"$d/notes.md"
# Pinned, so the refusal is the EXTENSION axis and not the missing pin.
pin_sidecar "$d" "notes.md" 'publish: allow - I would rather not annotate the line.'
expect_fail "B4b. the sidecar does NOT apply to a commentable format" "$d" "notes.md"

# ── B4c. a byte is not a reason ──────────────────────────────────────────────
# "Non-empty" was the old test. A sidecar containing "x" passed it, which is an
# exclusion wearing a costume with one letter on it.
d="$(new_repo case-cred-sidecar-reason)" || exit 2
printf -- '-----BEGIN RSA PRIVATE KEY-----\n' >"$d/test.pem"
printf 'x\n' >"$d/test.pem.publish-allow"
expect_fail "B4c. a sidecar without the words 'publish: allow' does not waive" "$d" "test.pem"

# ── B4d. the blob pin: a waiver does not survive its file ────────────────────
# A sidecar written for a synthetic test key must not silently inherit whatever
# bytes land at that path next. When it states a hash, the waiver holds only
# while the file still hashes to it.
d="$(new_repo case-cred-sidecar-pin)" || exit 2
printf -- '-----BEGIN RSA PRIVATE KEY-----\n' >"$d/test.pem"
sha="$(git -C "$d" hash-object -- "$d/test.pem")"
{ printf 'publish: allow - synthetic test keypair, authorises nothing.\n'
  printf 'publish-allow-blob: %s\n' "$sha"; } >"$d/test.pem.publish-allow"
run_gate "$d"
if [ "$(cat "$WORK/rc.txt")" = "0" ]; then
  ok "B4d. a pinned sidecar clears the blob it names"
else
  bad "B4d. pinned sidecar still red - the pin rejects its own blob"
  sed 's/^/      /' "$WORK/out.txt" | head -20
fi
# Replace the content the waiver was written for; the pin must stop applying.
printf -- '-----BEGIN OPENSSH PRIVATE KEY-----\n' >"$d/test.pem"
expect_fail "B4d. the pin does NOT survive the file being replaced" "$d" "test.pem"

# ── B4e. the pin is REQUIRED, not opt-in ─────────────────────────────────────
# R3-4. The condition above read "blob pin, WHEN the sidecar states one", which
# handed the strongest of the four sidecar conditions to the party it
# constrains: omit one line and the waiver is accepted unpinned, and then
# survives its file being replaced — the whole property B4d exists to
# establish, opted out of in silence. B4d could never see it, because B4d's own
# fixture states a pin.
#
# No live sidecar in this repository was unpinned when this was found, so this
# closes a latent control weakness rather than an exposure.
d="$(new_repo case-cred-sidecar-unpinned)" || exit 2
printf -- '-----BEGIN RSA PRIVATE KEY-----\n' >"$d/test.pem"
printf 'publish: allow - synthetic test keypair, authorises nothing.\n' >"$d/test.pem.publish-allow"
expect_fail "B4e. an UNPINNED sidecar does not waive - the pin is required" "$d" "test.pem"
# And the counterweight: adding the pin to that same sidecar clears it. Without
# this, B4e is satisfied by a sidecar hatch that never works at all.
pin_sidecar "$d" "test.pem" 'publish: allow - synthetic test keypair, authorises nothing.'
run_gate "$d"
if [ "$(cat "$WORK/rc.txt")" = "0" ]; then
  ok "B4e. stating the pin on that same sidecar clears the file"
else
  bad "B4e. pinned sidecar still red - the hatch cannot be used at all"
  sed 's/^/      /' "$WORK/out.txt" | head -20
fi

# ── B5. the pathspec axis, which the canary cannot reach ─────────────────────
# CONSTRUCTED PROOF that the hole was real: the canary writes its files at the
# root of a throwaway repo, so every real $CRED_EXCLUDE entry is inert there and
# the pathspec machinery is invoked without ever being exercised. Adding
# ':(exclude)crates/*' hid a tracked synthetic hit while the canary stayed
# green. The axis is held by the $CRED_EXCLUDE_REVIEWED literal instead, and
# this case is what makes that pin load-bearing: drift the list, red the build.
d="$(new_repo case-cred-exclude-drift)" || exit 2
mkdir -p "$d/crates"
printf 'ghp_%s\n' "$(printf 'D%.0s' $(seq 1 36))" >"$d/crates/leak.toml"
expect_fail "B5. baseline - the synthetic hit under crates/ is found" "$d" "crates/leak.toml"
# Now smuggle in a blind spot exactly as a regression would.
python3 - "$d/scripts/publish.sh" <<'PYDRIFT'
import sys
p = sys.argv[1]
s = open(p).read()
old = "  ':(exclude)assets/gitleaks/*'\n"
assert old in s, "fixture: CRED_EXCLUDE shape changed"
s = s.replace(old, old + "  ':(exclude)crates/*'\n", 1)
open(p, 'w').write(s)
PYDRIFT
run_gate "$d"
rc="$(cat "$WORK/rc.txt")"
if [ "$rc" = "2" ] && grep -qF 'CRED_EXCLUDE no longer matches the reviewed set' "$WORK/out.txt"; then
  ok "B5. an added pathspec exclusion reddens the build instead of hiding a hit"
else
  bad "B5. a new blind spot was accepted - exit $rc, and the hit is now invisible"
  sed 's/^/      /' "$WORK/out.txt" | head -20
fi

# ── C. operator machine path in content ─────────────────────────────────────
d="$(new_repo case-home)" || exit 2
printf 'OUT="${OUT:-/Users/jrhalpern/galaxies/thing/out}"\n' >"$d/run.sh"
expect_fail "C. operator home path in content reddens the gate" "$d" "run.sh"

# A generic placeholder must NOT red — a gate that flags `/Users/you` in a doc
# is a gate nobody can keep green.
d="$(new_repo case-home-ok)" || exit 2
printf 'example: /Users/you/galaxies/cosmon and /home/cosmon-worker/.cosmon\n' >"$d/README.md"
run_gate "$d"
if [ "$(cat "$WORK/rc.txt")" = "0" ]; then
  ok "C. generic placeholders and service accounts stay green"
else
  bad "C. a placeholder home path false-positived"
  sed 's/^/      /' "$WORK/out.txt" | head -20
fi

# ── C′. absolute symlink target — the surface no text scan sees ─────────────
# This is the case that motivated the check: `git grep` does not read symlink
# blobs, so an absolute target is invisible to every content rule in the tree.
d="$(new_repo case-symlink)" || exit 2
ln -s /opt/somewhere/local/tool "$d/tool"
expect_fail "C′. tracked symlink with an absolute target reddens the gate" "$d" "tool"
# Prove the premise, not just the conclusion: assert a content scan really is
# blind here, so this case cannot silently degrade into a duplicate of C.
git -C "$d" add -A >/dev/null 2>&1
if git -C "$d" grep -qI 'somewhere' -- . 2>/dev/null; then
  bad "C′. premise broken — git grep now DOES see symlink targets; re-derive this rule"
else
  ok "C′. premise holds — a content scan is blind to the symlink target"
fi

# ── D. unpinned binary asset ────────────────────────────────────────────────
d="$(new_repo case-binary)" || exit 2
printf 'PK\003\004\000\001\002' >"$d/blob.bin"
expect_fail "D. unpinned tracked binary reddens the gate" "$d" "blob.bin"

# A pinned binary is fine; a pin that no longer matches is not.
d="$(new_repo case-binary-pinned)" || exit 2
printf 'PK\003\004\000\001\002' >"$d/blob.bin"
printf '%s  blob.bin\n' "$(shasum -a 256 "$d/blob.bin" | cut -d' ' -f1)" >>"$d/assets/reviewed-binaries.sha256"
run_gate "$d"
if [ "$(cat "$WORK/rc.txt")" = "0" ]; then
  ok "D. a correctly pinned binary stays green"
else
  bad "D. a correctly pinned binary false-positived"
  sed 's/^/      /' "$WORK/out.txt" | head -20
fi
printf 'PK\003\004\000\001\002\377' >"$d/blob.bin"
expect_fail "D. a binary edited away from its pin reddens the gate" "$d" "blob.bin"

# ── F. the captured TUI frame the gate used to pass ─────────────────────────
# THE ORIGINAL MISS, REBUILT. Seven `tmux capture-pane` frames of a real Claude
# Code session were committed under `crates/cosmon-transport/tests/fixtures/`
# carrying the operator's account address and the organisation name that TUI
# paints from it. `publish.sh --check` returned CLEAN on them. Check C could not
# have seen it: C reads `/Users/<c>` and `/home/<c>`, so it recognises an
# operator only when they are spelt as a filesystem path, and a captured frame
# is not a path.
#
# The fixture below is that frame's shape — box-drawing furniture, the derived
# organisation line, a synthetic address at a registrable domain that is
# nobody's. If this case ever goes green, the gate has gone back to passing the
# artefact it was extended to catch.
d="$(new_repo case-identity-pane)" || exit 2
mkdir -p "$d/crates/x/tests/fixtures/tui"
cat >"$d/crates/x/tests/fixtures/tui/idle.pane" <<'PANE'
╭─────────────────────────────────────────────────────────────────╮
│ ❯                                                               │
╰─────────────────────────────────────────────────────────────────╯
  Account: a.person@mailbox-provider.net's Organization
PANE
expect_fail "F. a captured TUI frame carrying an operator address reddens the gate" \
  "$d" "crates/x/tests/fixtures/tui/idle.pane"

# …and it is reported WITHOUT the address. Same doctrine as B: a gate that
# prints what it found has moved the leak into every CI log that ran it. The
# domain is what explains the finding; the local part is the part that names a
# human, and it is the part withheld.
run_gate "$d"
if grep -qF "a.person@" "$WORK/out.txt"; then
  bad "F. the gate PRINTED the local part — the finding re-publishes the address"
else
  ok "F. address reported by domain + digest, local part withheld"
fi
if grep -qF "@mailbox-provider.net" "$WORK/out.txt"; then
  ok "F. the finding names the routable domain that explains it"
else
  bad "F. no domain in the finding — a reviewer cannot tell why it fired"
fi

# ── F′. reserved and project domains must NOT red ───────────────────────────
# The counterweight, and the reason the rule is a DOMAIN test. A gate that flags
# `operator@example.invalid` flags the neutralised fixtures it exists to bless,
# and gets switched off in a week. RFC 2606 / 6761 domains provably reach no
# mailbox; `noogram.org` is this project's published maker address.
d="$(new_repo case-identity-ok)" || exit 2
cat >"$d/notes.md" <<'OK'
operator@example.invalid's Org · test@example.com · dev@cosmon.test
u@sub.example.org · hello@noogram.org · noreply@users.noreply.github.com
OK
run_gate "$d"
if [ "$(cat "$WORK/rc.txt")" = "0" ]; then
  ok "F′. reserved, non-routable and maker domains stay green"
else
  bad "F′. a documentation address false-positived — the gate is unkeepable"
  sed 's/^/      /' "$WORK/out.txt" | head -20
fi

# ── F′′. the lockfile sidecar, and its limits ───────────────────────────────
# `supply-chain/imports.lock` carries the PUBLIC addresses of upstream auditors,
# one per cargo-vet `who =` record, and is regenerated wholesale — so an inline
# marker is erased on the next `cargo vet` run and the waiver silently stops
# existing. That is the second reason a format can force the sidecar, alongside
# "has no comment syntax", and the blob pin is what keeps it from outliving the
# file it was written for.
d="$(new_repo case-identity-sidecar)" || exit 2
printf 'who = "A Person <a.person@mailbox-provider.net>"\n' >"$d/imports.lock"
expect_fail "F′′. an unwaived lockfile address reddens the gate" "$d" "imports.lock"
pin_sidecar "$d" "imports.lock" 'publish: allow — upstream auditor identities, published at the source.'
run_gate "$d"
if [ "$(cat "$WORK/rc.txt")" = "0" ]; then
  ok "F′′. a pinned sidecar clears the lockfile"
else
  bad "F′′. sidecar-waived lockfile still red — the escape hatch does not work"
  sed 's/^/      /' "$WORK/out.txt" | head -20
fi
# The format condition holds for THIS rule too: a commentable file has the
# inline marker, which is strictly better, and may not reach for the sidecar.
d="$(new_repo case-identity-sidecar-ext)" || exit 2
printf 'contact: a.person@mailbox-provider.net\n' >"$d/notes.md"
pin_sidecar "$d" "notes.md" 'publish: allow — I would rather not annotate the line.'
expect_fail "F′′. the sidecar does NOT apply to a commentable format" "$d" "notes.md"
# And the rule condition: a lockfile hatch is not a key hatch.
d="$(new_repo case-identity-sidecar-rule)" || exit 2
printf 'token = "ghp_%s"\n' "$(printf 'F%.0s' $(seq 1 36))" >"$d/deps.lock"
pin_sidecar "$d" "deps.lock" 'publish: allow — auditor identities.'
expect_fail "F′′. the lockfile sidecar does NOT waive a credential on the same file" "$d" "deps.lock"

# ── F′′′. the allowlist that swallows everything ────────────────────────────
# The failure mode a must-hit alone cannot see: the regex fires on every address
# and the domain allowlist accepts them all, so the scan runs perfectly and
# reports a clean tree. The gate's own canary checks both directions; this case
# makes that canary load-bearing by breaking the allowlist exactly as a
# regression would.
d="$(new_repo case-identity-allow-drift)" || exit 2
printf 'contact: a.person@mailbox-provider.net\n' >"$d/notes.md"
expect_fail "F′′′. baseline - the routable address is found" "$d" "notes.md"
python3 - "$d/scripts/publish.sh" <<'PYALLOW'
import sys
p = sys.argv[1]
s = open(p).read()
old = 'IDENT_TLD_ALLOW=" example invalid test localhost local "\n'
assert old in s, "fixture: IDENT_TLD_ALLOW shape changed"
s = s.replace(old, 'IDENT_TLD_ALLOW=" example invalid test localhost local net "\n', 1)
open(p, 'w').write(s)
PYALLOW
run_gate "$d"
rc="$(cat "$WORK/rc.txt")"
if [ "$rc" = "2" ] && grep -qF 'operator-identity CANARY FAILED' "$WORK/out.txt"; then
  ok "F′′′. an allowlist that accepts a routable domain reddens the build"
else
  bad "F′′′. a widened allowlist was accepted - exit $rc, and the hit is now invisible"
  sed 's/^/      /' "$WORK/out.txt" | head -20
fi

# ── The refusal to write ────────────────────────────────────────────────────
# The guide says the development repository is never rewritten in place. The
# tool named in that sentence must therefore have no in-place verb at all.
d="$(new_repo case-usage)" || exit 2
for arg in "" "--fix" "--scrub" "--publish" "--check --force"; do
  # shellcheck disable=SC2086
  ( cd "$d" && bash scripts/publish.sh $arg >/dev/null 2>&1 )
  rc=$?
  if [ "$rc" -ne 2 ]; then
    bad "usage: 'publish.sh ${arg:-<no args>}' returned $rc, expected 2 (refusal)"
  fi
done
ok "usage: --check is the only accepted mode; every writing verb is refused"

# ── The path allowlist that must never come back ────────────────────────────
# CARRIED-B3. `docs/architecture-baseline.md` once told adopters to extend a
# path allowlist via `.publish-allowlist.txt` — a file with zero hits in
# `git ls-files` and zero in `scripts/`, describing the very thing this gate
# refuses by name: "a pathspec exclusion is a permanent blind spot no reviewer
# sees again". The prose was corrected in 407fb80, before this test existed;
# what was missing was anything to stop it coming back.
#
# Vendoring is why it matters more than an ordinary doc error. `publish.sh`
# ships verbatim into downstream galaxies and its guidance is read as a
# recommendation, so a sentence naming an allowlist propagates the blind spot
# into repositories nobody here will ever read.
#
# Asserted on THIS repository, not a fixture: the claim is about what cosmon
# ships, and no constructed repo can be wrong about that.
allowlist_hits=0
if git -C "$ROOT" ls-files --error-unmatch -- '.publish-allowlist.txt' >/dev/null 2>&1; then
  bad "allowlist: '.publish-allowlist.txt' is TRACKED — the pathspec blind spot the gate refuses by name"
  allowlist_hits=1
fi
# This file names the string in order to forbid it, exactly as publish.sh
# excludes itself from its own credential scan; every OTHER script is checked.
if grep -rl 'publish-allowlist' "$ROOT/scripts" 2>/dev/null | grep -qv 'publish.test.sh$'; then
  bad "allowlist: scripts/ reads a path allowlist — a waiver no reviewer sees in a diff"
  allowlist_hits=1
fi
# The doc may MENTION the name (it carries the retraction), but must not
# recommend it: the retraction and the prohibition have to travel together.
# Flattened: the prohibition wraps across lines in the source, and a
# line-oriented grep would report it missing while it is sitting right there.
baseline_flat="$(tr '\n' ' ' <"$ROOT/docs/architecture-baseline.md" 2>/dev/null | tr -s ' ')"
if printf '%s' "$baseline_flat" | grep -q 'publish-allowlist' &&
   ! printf '%s' "$baseline_flat" | grep -q 'There is no path allowlist and there must not be one'; then
  bad "allowlist: the baseline names '.publish-allowlist.txt' without the sentence that forbids it"
  allowlist_hits=1
fi
[ "$allowlist_hits" -eq 0 ] &&
  ok "allowlist: no path allowlist is tracked, read, or recommended for vendoring"

echo "----------------------------------------------------------------------------"
if [ "$fails" -eq 0 ]; then
  echo "publish.test: all cases behaved — the gate reds on every violation it claims to catch."
  exit 0
fi
echo "publish.test: $fails case(s) did NOT behave as specified." >&2
exit 1
