#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-only
# crossing.test.sh — prove the crossing primitive REFUSES, one case per
# refusal, and that the candidate it builds has the exact shape claimed.
#
#     scripts/release/crossing.test.sh
#
# A gate nobody has seen fail is indistinguishable from a gate that cannot
# fail, and every refusal here exists because its absence would have been
# silent: a dirty tree ships the previous commit's content and exits green; two
# disagreeing trunks reconcile behind the operator's back; a waiver written and
# reviewed by the same tired person at 2am waives itself.
#
# Every case is a hermetic fixture repository in a temp dir with a LOCAL bare
# remote. Nothing here touches the development repository, no network, no
# signing key: the signature is `sign-and-push.sh`'s half, and it is the half a
# machine may not rehearse.

set -eu

SCRIPT_DIR="$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)"
CROSSING="${SCRIPT_DIR}/crossing.sh"
SIGN="${SCRIPT_DIR}/sign-and-push.sh"

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT INT TERM

pass=0
fail=0
case_n=0

# Assembled, never written literally: this file is tracked, and a literal
# marker in it would read as a waiver to the very scan under test. Same reason
# crossing.sh assembles it — a path exclusion would be the blind spot the
# doctrine refuses.
MARKER="publish:"' allow'

# Build a fixture: a bare "public" remote, a work repo whose `main` matches it,
# and one extra commit on top so the development tree genuinely differs.
# Echoes the work repo's path.
#
# `fixture` runs in a command substitution, i.e. a SUBSHELL, so it cannot bump
# the case counter itself — `new_fixture` does that in the caller's shell and
# leaves the path in `$w`.
new_fixture() {
    case_n=$((case_n + 1))
    w="$(fixture "$case_n")"
}
fixture() {
    _root="${tmp}/case-${1}"
    _bare="${_root}/public.git"
    _work="${_root}/dev"
    mkdir -p "$_root"
    git init --quiet --bare "$_bare"
    git init --quiet -b main "$_work"
    git -C "$_work" config user.email dev@example.invalid
    git -C "$_work" config user.name 'Fixture Dev'
    git -C "$_work" config commit.gpgsign false

    mkdir -p "${_work}/scripts/release"
    cp "$CROSSING" "${_work}/scripts/release/crossing.sh"
    cp "$SIGN" "${_work}/scripts/release/sign-and-push.sh"
    chmod +x "${_work}/scripts/release/crossing.sh" "${_work}/scripts/release/sign-and-push.sh"

    # A stub structural gate. The real publish.sh is tested by publish.test.sh;
    # what is under test here is that the crossing calls a BLOCKING gate before
    # it captures a tree, and stops when that gate reddens.
    cat > "${_work}/scripts/publish.sh" <<'STUB'
#!/bin/sh
if [ -f .publish-must-fail ]; then
    echo "publish gate: FAILED (fixture)" >&2
    exit 1
fi
echo "publish gate: PASS (fixture)"
STUB
    chmod +x "${_work}/scripts/publish.sh"

    printf '[workspace.package]\nversion = "9.9.9"\n' > "${_work}/Cargo.toml"
    printf 'first\n' > "${_work}/README.md"
    git -C "$_work" add -A
    git -C "$_work" commit --quiet -m 'public base'
    git -C "$_work" remote add origin "$_bare"
    git -C "$_work" push --quiet origin main
    git -C "$_work" remote set-url --push origin 'DISABLED://accidental-push-from-dev'

    # The development trunk is its OWN branch: `main` mirrors the public tip
    # and never moves dev-side. That separation is the whole premise — the two
    # are two histories, not one.
    git -C "$_work" checkout --quiet -b dev
    printf 'second\n' >> "${_work}/README.md"
    git -C "$_work" add -A
    git -C "$_work" commit --quiet -m 'development work'

    printf '%s' "$_work"
}

# Run crossing.sh inside a fixture, assert exit status, assert a phrase.
expect() {
    _name="$1"; _work="$2"; _want_status="$3"; _want_phrase="${4:-}"
    _out="${tmp}/out.${case_n}"
    set +e
    ( cd "$_work" && ./scripts/release/crossing.sh --skip-checklist ) > "$_out" 2>&1
    _got=$?
    set -e
    if [ "$_got" -ne "$_want_status" ]; then
        printf '✗ %s\n' "$_name"
        printf '  expected exit %s, got %s. Output:\n' "$_want_status" "$_got"
        sed 's/^/    /' "$_out"
        fail=$((fail + 1))
        return 1
    fi
    if [ -n "$_want_phrase" ] && ! grep -qF -- "$_want_phrase" "$_out"; then
        printf '✗ %s\n' "$_name"
        printf '  exit status %s was right, but output never said "%s". Output:\n' \
            "$_got" "$_want_phrase"
        sed 's/^/    /' "$_out"
        fail=$((fail + 1))
        return 1
    fi
    printf '✓ %s\n' "$_name"
    pass=$((pass + 1))
    return 0
}

ok() { printf '✓ %s\n' "$1"; pass=$((pass + 1)); }
ko() { printf '✗ %s\n    %s\n' "$1" "$2"; fail=$((fail + 1)); }

echo "── crossing.sh ────────────────────────────────────────────────────────"

# ── 1. THE ONE THAT MATTERS MOST ────────────────────────────────────────────
# `commit-tree` reads a COMMITTED tree. With uncommitted edits it succeeds,
# signs, and ships the PREVIOUS commit's content while the operator watches a
# green exit. A failure that succeeds is worse than a crash.
new_fixture
printf 'uncommitted\n' >> "${w}/README.md"
expect "refuses a dirty tree (would have shipped the previous commit, green)" \
    "$w" 1 "the working tree is not clean" || true
if [ -n "$(git -C "$w" for-each-ref refs/cosmon/ 2>/dev/null)" ]; then
    ko "dirty tree builds nothing" "a candidate ref exists after a refusal"
else
    ok "dirty tree builds nothing (no candidate ref)"
fi

# ── 2. two trunks that disagree are never reconciled ────────────────────────
new_fixture
# Move the remote ahead behind the operator's back.
other="${tmp}/other-${case_n}"
git clone --quiet "$(git -C "$w" remote get-url origin)" "$other"
git -C "$other" config user.email o@example.invalid
git -C "$other" config user.name 'Other'
git -C "$other" config commit.gpgsign false
printf 'elsewhere\n' > "${other}/NEW.md"
git -C "$other" add -A
git -C "$other" commit --quiet -m 'someone else pushed'
git -C "$other" push --quiet origin main
expect "refuses when local main and origin/main differ" \
    "$w" 1 "differ" || true
grep -q "local  main:" "${tmp}/out.${case_n}" \
    && grep -q "origin/main:" "${tmp}/out.${case_n}" \
    && ok "prints both tips and stops (no fetch-and-merge, no rebase)" \
    || ko "prints both tips" "one of the two tips is missing from the refusal"

# ── 3. the push URL must already be disarmed ────────────────────────────────
new_fixture
git -C "$w" remote set-url --push origin "$(git -C "$w" remote get-url origin)"
expect "refuses while the dev repo can still push to origin" \
    "$w" 1 "can still push" || true
grep -q "git remote set-url --push origin" "${tmp}/out.${case_n}" \
    && ok "names the one daylight command that disarms it" \
    || ko "names the disarming command" "the refusal did not print it"

# ── 4. a waiver INTRODUCED by this candidate is a refusal ───────────────────
# The marker is safe because a reviewer reads it in the diff. Tonight the
# author and the reviewer are the same person, so a marker not already
# reachable from the public tip does not count.
new_fixture
printf 'let t = b"token";  // %s — synthetic test vector\n' "$MARKER" > "${w}/leaky.rs"
git -C "$w" add -A
git -C "$w" commit --quiet -m 'introduce a waiver'
expect "refuses a waiver introduced by the candidate itself" \
    "$w" 1 "introduced by this very candidate" || true

# ── 5. a waiver already reachable from the public tip does NOT refuse ───────
# The mirror of case 4. Without this the gate would be indistinguishable from
# "never allow a waiver", which is a different rule than the one written down.
new_fixture
# Put the waiver on the PUBLIC side first, in daylight, then carry it into
# development.
git -C "$w" checkout --quiet main
printf 'let t = b"token";  // %s — synthetic test vector\n' "$MARKER" > "${w}/leaky.rs"
git -C "$w" add -A
git -C "$w" commit --quiet -m 'waiver, reviewed in daylight'
git -C "$w" push --quiet "$(git -C "$w" remote get-url origin)" main:main
git -C "$w" checkout --quiet dev
git -C "$w" merge --quiet --no-edit main
expect "accepts a waiver already reachable from the public tip" "$w" 0 "ready. one command left" || true

# ── 6. the structural gate is BLOCKING and runs before anything is built ────
new_fixture
touch "${w}/.publish-must-fail"
git -C "$w" add -A -f
git -C "$w" commit --quiet -m 'make the gate red'
expect "refuses when publish.sh --check fails" \
    "$w" 1 "publish.sh --check failed" || true
if [ -n "$(git -C "$w" for-each-ref refs/cosmon/ 2>/dev/null)" ]; then
    ko "red gate builds nothing" "a candidate ref exists after the gate reddened"
else
    ok "red gate builds nothing (the finding costs one reset of nothing)"
fi

# ── 7. an empty crossing is a refusal, not a no-op commit ───────────────────
new_fixture
git -C "$w" branch -f main HEAD
git -C "$w" push --quiet "$(git -C "$w" remote get-url origin)" main:main
expect "refuses when the two trees are already identical" \
    "$w" 1 "already identical" || true

# ── 8. Q8 — auditing one worktree and crossing from another is refused ──────
new_fixture
elsewhere="${tmp}/elsewhere-${case_n}"
git init --quiet -b main "$elsewhere"
set +e
out8="$( cd "$elsewhere" && "${w}/scripts/release/crossing.sh" --skip-checklist 2>&1 )"
rc8=$?
set -e
if [ "$rc8" -ne 0 ] && printf '%s' "$out8" | grep -qF "different worktree"; then
    ok "refuses to cross from a worktree other than the one it will audit"
else
    ko "refuses a cross-worktree run" "exit ${rc8}: ${out8}"
fi

# ── 9. HAPPY PATH — the candidate's shape is the claim ──────────────────────
new_fixture
dev_sha="$(git -C "$w" rev-parse HEAD)"
public_sha="$(git -C "$w" rev-parse main)"
expect "builds the candidate on a clean, agreeing, gate-green tree" "$w" 0 "ready. one command left" || true
happy_out="${tmp}/out.${case_n}"

cand="$(git -C "$w" rev-parse --verify --quiet refs/cosmon/crossing/v9.9.9 || echo '')"
if [ -n "$cand" ]; then ok "the candidate lives at refs/cosmon/crossing/v9.9.9"
else ko "candidate ref" "refs/cosmon/crossing/v9.9.9 does not exist"; fi

if [ -n "$cand" ]; then
    parents="$(git -C "$w" rev-list --parents -n1 "$cand" | cut -d' ' -f2-)"
    [ "$parents" = "$public_sha" ] \
        && ok "exactly one parent, and it is the public tip" \
        || ko "one parent = public tip" "got parents: ${parents}"

    [ "$(git -C "$w" rev-parse "${cand}^{tree}")" = "$(git -C "$w" rev-parse "${dev_sha}^{tree}")" ] \
        && ok "its tree is the development tree verbatim" \
        || ko "tree identity" "candidate tree differs from the development tree"

    trailers="$(git -C "$w" log -1 --format=%B "$cand" | grep -c '^Projected-From: ')"
    [ "$trailers" = "1" ] \
        && ok "exactly one Projected-From trailer (not 120 stacked blocks)" \
        || ko "one trailer" "found ${trailers} Projected-From lines"

    git -C "$w" log -1 --format=%B "$cand" | grep -qF "Projected-From: ${dev_sha}" \
        && ok "the trailer names the development SHA" \
        || ko "trailer content" "the trailer does not name ${dev_sha}"
fi

# The development branch is untouched: the script builds a candidate and stops.
[ "$(git -C "$w" rev-parse HEAD)" = "$dev_sha" ] \
    && ok "the development tip is untouched" \
    || ko "dev untouched" "HEAD moved"
[ "$(git -C "$w" rev-parse main)" = "$public_sha" ] \
    && ok "the public branch is untouched — nothing is signed or pushed here" \
    || ko "main untouched" "main moved"

# The printed line must be pasteable and carry the pinned pair explicitly.
grep -qF "sign-and-push.sh --tree $(git -C "$w" rev-parse "${dev_sha}^{tree}") --parent ${public_sha}" "$happy_out" \
    && ok "prints one pasteable command pinning (tree, parent) explicitly" \
    || ko "pasteable line" "the printed command does not carry the pinned pair"

echo "── sign-and-push.sh ───────────────────────────────────────────────────"

# ── 10. the pinned pair is re-verified immediately before signing ───────────
# Nothing here needs a key: each case must refuse BEFORE reaching commit-tree.
new_fixture
dev_sha="$(git -C "$w" rev-parse HEAD)"
public_sha="$(git -C "$w" rev-parse main)"
tree="$(git -C "$w" rev-parse "${dev_sha}^{tree}")"

run_sign() {
    set +e
    ( cd "$w" && ./scripts/release/sign-and-push.sh "$@" ) 2>&1
    _rc=$?
    set -e
    return $_rc
}

# Move local main after the audit — the audited parent is stale.
git -C "$w" branch -f main "$dev_sha"
set +e
out="$(run_sign --tree "$tree" --parent "$public_sha" --version 9.9.9 --dev-sha "$dev_sha")"
rc=$?
set -e
if [ "$rc" -ne 0 ] && printf '%s' "$out" | grep -qF "local main moved since the audit"; then
    ok "refuses when local main moved between the audit and the signature"
else
    ko "stale parent refusal" "exit ${rc}: ${out}"
fi

# Move the REMOTE after the audit — local still matches, the remote does not.
git -C "$w" branch -f main "$public_sha"
other="${tmp}/other-sign"
git clone --quiet "$(git -C "$w" remote get-url origin)" "$other"
git -C "$other" config user.email o@example.invalid
git -C "$other" config user.name 'Other'
git -C "$other" config commit.gpgsign false
printf 'elsewhere\n' > "${other}/NEW.md"
git -C "$other" add -A
git -C "$other" commit --quiet -m 'someone else pushed'
git -C "$other" push --quiet origin main
set +e
out="$(run_sign --tree "$tree" --parent "$public_sha" --version 9.9.9 --dev-sha "$dev_sha")"
rc=$?
set -e
if [ "$rc" -ne 0 ] && printf '%s' "$out" | grep -qF "origin/main moved since the audit"; then
    ok "refuses when the remote moved between the audit and the signature"
else
    ko "moved-remote refusal" "exit ${rc}: ${out}"
fi
[ "$(git -C "$w" rev-parse main)" = "$public_sha" ] \
    && ok "a refused signature leaves main exactly where it was" \
    || ko "main after refusal" "main moved during a refused run"

# There is no escape hatch, and its absence is the design. Comments are
# stripped first: the header EXPLAINS why the flags are absent, and that
# sentence must not read as the flags being present.
if grep -v '^[[:space:]]*#' "$SIGN" | grep -qE -e '--force' -e '--allow-unclean'; then
    ko "no escape hatch" "sign-and-push.sh grew a --force or --allow-unclean flag"
else
    ok "no --force and no --allow-unclean (the escape hatch is: do not push tonight)"
fi

# Ordering is load-bearing: the signature must precede the ref move, so a
# missing key leaves at worst a dangling object.
sign_line="$(grep -n 'commit-tree -S' "$SIGN" | head -n1 | cut -d: -f1)"
swap_line="$(grep -n 'update-ref "refs/heads/\${PUBLIC_BRANCH}" "\$NEW"' "$SIGN" | head -n1 | cut -d: -f1)"
if [ -n "$sign_line" ] && [ -n "$swap_line" ] && [ "$sign_line" -lt "$swap_line" ]; then
    ok "commit-tree -S runs before update-ref (fail-closed comes from the order)"
else
    ko "sign-before-swap" "commit-tree -S at line ${sign_line:-none}, update-ref at ${swap_line:-none}"
fi

echo "── the whole crossing, end to end (real signature) ────────────────────"

# A real signature, hermetically: an ephemeral SSH signing key in the temp
# dir. Nothing else in this suite exercises the half that actually moves the
# public trunk, and a mechanism whose happy path has never run is a claim.
sign_key() {
    ssh-keygen -q -t ed25519 -N '' -f "${tmp}/key-${case_n}" -C crossing-test
    git -C "$w" config gpg.format ssh
    git -C "$w" config user.signingkey "${tmp}/key-${case_n}.pub"
}

new_fixture
sign_key
dev_sha="$(git -C "$w" rev-parse HEAD)"
public_sha="$(git -C "$w" rev-parse main)"
dev_tree="$(git -C "$w" rev-parse "${dev_sha}^{tree}")"
bare="$(git -C "$w" remote get-url origin)"

expect "e2e: the candidate is built" "$w" 0 "ready. one command left" || true
cmd="$(grep -o 'scripts/release/sign-and-push.sh .*' "${tmp}/out.${case_n}" | head -n1)"
set +e
e2e="$( cd "$w" && sh -c "./${cmd}" 2>&1 )"
rc=$?
set -e
if [ "$rc" -eq 0 ]; then ok "e2e: the printed command runs and exits 0"
else ko "e2e: printed command" "exit ${rc}: ${e2e}"; fi

new_public="$(git -C "$w" rev-parse main)"
[ "$new_public" != "$public_sha" ] \
    && ok "e2e: the public branch advanced" \
    || ko "public advanced" "main is still ${public_sha}"

git -C "$w" cat-file commit "$new_public" | grep -q '^gpgsig' \
    && ok "e2e: the new public tip carries a signature" \
    || ko "signature" "the new public commit has no gpgsig header"

parents="$(git -C "$w" rev-list --parents -n1 "$new_public" | cut -d' ' -f2-)"
[ "$parents" = "$public_sha" ] \
    && ok "e2e: exactly one parent, the previous public tip" \
    || ko "one parent" "got: ${parents}"
[ "$(git -C "$w" rev-parse "${new_public}^{tree}")" = "$dev_tree" ] \
    && ok "e2e: the public tree is the development tree verbatim" \
    || ko "tree identity" "public tree differs from the development tree"

[ "$(git --git-dir="$bare" rev-parse main)" = "$new_public" ] \
    && ok "e2e: the remote received exactly that commit" \
    || ko "push" "the bare remote's main is not ${new_public}"

[ "$(git -C "$w" rev-parse refs/cosmon/crossings/v9.9.9)" = "$dev_sha" ] \
    && ok "e2e: the archive ref pins the development SHA" \
    || ko "archive ref" "refs/cosmon/crossings/v9.9.9 does not pin ${dev_sha}"
git -C "$w" rev-parse --verify --quiet refs/cosmon/crossing/v9.9.9 >/dev/null \
    && ko "candidate ref cleanup" "the unsigned candidate ref survived the push" \
    || ok "e2e: the unsigned candidate ref is gone once the signed one exists"

if grep -qF "9.9.9	${new_public}	${dev_sha}" "${w}/docs/release-crossings.tsv" 2>/dev/null; then
    ok "e2e: the ledger records (version, public-sha, dev-sha)"
else
    ko "ledger" "the pairing line is missing from docs/release-crossings.tsv"
fi
if [ -z "$(git -C "$w" status --porcelain)" ] \
   && [ "$(git -C "$w" log -1 --name-only --format= HEAD)" = "docs/release-crossings.tsv" ]; then
    ok "e2e: the ledger line is committed, path-limited, and nothing else rode along"
else
    ko "ledger commit" "the ledger commit is missing or carried other paths"
fi

# ── the key is unavailable: nothing moves, nothing is pushed ────────────────
# This is the whole fail-closed claim. `commit-tree -S` is the first thing that
# can fail and the first thing that runs, so a missing key costs nothing.
new_fixture
git -C "$w" config gpg.format ssh
git -C "$w" config user.signingkey "${tmp}/there-is-no-key-here.pub"
public_sha="$(git -C "$w" rev-parse main)"
bare="$(git -C "$w" remote get-url origin)"
expect "no-key: the candidate is still built (nothing is signed yet)" "$w" 0 "ready. one command left" || true
cmd="$(grep -o 'scripts/release/sign-and-push.sh .*' "${tmp}/out.${case_n}" | head -n1)"
set +e
out="$( cd "$w" && sh -c "./${cmd}" 2>&1 )"
rc=$?
set -e
if [ "$rc" -ne 0 ] && printf '%s' "$out" | grep -qF "signing failed"; then
    ok "no-key: signing fails, and it fails FIRST"
else
    ko "no-key refusal" "exit ${rc}: ${out}"
fi
[ "$(git -C "$w" rev-parse main)" = "$public_sha" ] \
    && ok "no-key: local main never moved" \
    || ko "local main" "main moved despite a failed signature"
[ "$(git --git-dir="$bare" rev-parse main)" = "$public_sha" ] \
    && ok "no-key: the remote never moved" \
    || ko "remote main" "the remote moved despite a failed signature"

echo "──────────────────────────────────────────────────────────────────────"
printf '%s passed, %s failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
