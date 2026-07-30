#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-only
# sign-and-push.sh — the operator's single command: sign the PINNED
# (tree, parent) and push it, verifying both are unmoved immediately before
# signing.
#
#     scripts/release/sign-and-push.sh --tree <oid> --parent <oid> \
#         --version <X.Y.Z> --dev-sha <oid>
#
# You are not meant to type this. `crossing.sh` prints it, filled in, after it
# has refused everything it can refuse. The four arguments are explicit, and
# that is the point: the tree and the parent were decided by the audited run,
# so this script re-verifies those exact OIDs rather than re-deriving new ones
# from whatever the repository looks like now.
#
# ── It RE-COMPOSES the message, so it owns the trailers ─────────────────────
# The message below is built here, not carried over from the candidate, so the
# commit that actually lands on the public trunk gets its trailers from this
# script. That includes the DCO `Signed-off-by`: `.github/workflows/dco.yml`
# triggers on `pull_request: branches: [main]` only, and step 4 is a direct
# push, so nothing downstream will ever look at this commit.
#
# The identity is resolved here rather than accepted as a fifth argument, and
# that asymmetry with `--tree` / `--parent` is deliberate. Those two were
# DECIDED by the audited run, so re-verifying the pinned OIDs is right. The
# sign-off is not a decision, it is a certification of origin by whoever holds
# the key at step 2 — which is this script's caller. An argument would let that
# caller certify in someone else's name. See `signoff.sh` for the two sources it
# refuses and why. An identity that does not resolve is a refusal at step 1b,
# before anything is signed.
#
# ── Fail-closed comes from the ORDER, not from a flag ───────────────────────
#   1. verify the tree still exists and the parent is still the local and
#      remote public tip;
#   1b. resolve the sign-off identity, or refuse;
#   2. `git commit-tree -S` — the signature happens HERE, before any ref moves.
#      An unavailable key fails at this line and nothing has changed;
#   3. `git update-ref <branch> <new> <old>` — a compare-and-swap. A branch that
#      moved between step 1 and here fails the swap;
#   4. push. A remote that moved is refused by the remote's own non-fast-forward
#      rule; the local branch is rolled back to the parent so the two never
#      disagree silently.
#
# The worst outcome of any failure is therefore a dangling object that `gc`
# collects. There is deliberately NO `--force` and NO `--allow-unclean`: an
# escape hatch adds a decision at exactly the hour decisions are worst, and the
# escape hatch already exists — do not push tonight.
#
# ── What it records, so a tired human never types it again ──────────────────
#   * an ARCHIVE ref `refs/cosmon/crossings/v<version>` pinning the development
#     SHA the release was projected from, so it is reachable dev-side forever;
#   * a ledger line `<version>\t<public sha>\t<dev sha>` in the tracked
#     crossings ledger, committed path-limited so it can never carry anything
#     else along with it.

set -eu

TREE=""
PARENT=""
VERSION=""
DEV_SHA=""
REMOTE="origin"
PUBLIC_BRANCH="main"
LEDGER="docs/release-crossings.tsv"

usage() {
    cat <<'EOF'
usage: scripts/release/sign-and-push.sh --tree <oid> --parent <oid> \
           --version <X.Y.Z> --dev-sha <oid> [--remote NAME] [--public-branch NAME]

Printed for you by scripts/release/crossing.sh. Signs the pinned (tree, parent)
and pushes it. There is no forcing flag and no unclean override: if it refuses,
the way through is to not push tonight.
EOF
}

while [ $# -gt 0 ]; do
    case "$1" in
        --tree) TREE="${2:?--tree needs a value}"; shift 2 ;;
        --parent) PARENT="${2:?--parent needs a value}"; shift 2 ;;
        --version) VERSION="${2:?--version needs a value}"; shift 2 ;;
        --dev-sha) DEV_SHA="${2:?--dev-sha needs a value}"; shift 2 ;;
        --remote) REMOTE="${2:?--remote needs a value}"; shift 2 ;;
        --public-branch) PUBLIC_BRANCH="${2:?--public-branch needs a value}"; shift 2 ;;
        -h|--help) usage; exit 0 ;;
        *) printf 'sign-and-push.sh: unknown argument %s\n' "$1" >&2; usage >&2; exit 2 ;;
    esac
done

die() {
    printf '❌ %s. nothing was pushed.\n' "$1" >&2
    shift
    for _line in "$@"; do
        printf '   %s\n' "$_line" >&2
    done
    exit 1
}

for _required in TREE PARENT VERSION DEV_SHA; do
    eval "_v=\${${_required}}"
    [ -n "$_v" ] || die "missing --$(printf '%s' "$_required" | tr 'A-Z_' 'a-z-')"
done

SCRIPT_DIR="$(CDPATH='' cd -P -- "$(dirname -- "$0")" && pwd -P)"
REPO_ROOT="$(CDPATH='' cd -P -- "${SCRIPT_DIR}/../.." && pwd -P)"
INVOKED_ROOT="$(git rev-parse --show-toplevel 2>/dev/null || echo '')"
if [ -n "$INVOKED_ROOT" ]; then
    INVOKED_ROOT="$(CDPATH='' cd -P -- "$INVOKED_ROOT" && pwd -P)"
fi
if [ "$INVOKED_ROOT" != "$REPO_ROOT" ]; then
    die "run this from the same worktree crossing.sh audited" \
        "script's worktree: ${REPO_ROOT}" \
        "your worktree:     ${INVOKED_ROOT:-<not a git repository>}"
fi
cd "$REPO_ROOT"

# ── 1. verify the pinned pair is unmoved ────────────────────────────────────
if [ "$(git cat-file -t "$TREE" 2>/dev/null || echo none)" != "tree" ]; then
    die "the pinned tree ${TREE} is not a tree object in this repository"
fi
if ! git rev-parse --verify --quiet "${PARENT}^{commit}" >/dev/null; then
    die "the pinned parent ${PARENT} is not a commit in this repository"
fi
LOCAL_TIP="$(git rev-parse "refs/heads/${PUBLIC_BRANCH}" 2>/dev/null || echo '')"
if [ "$LOCAL_TIP" != "$PARENT" ]; then
    die "local ${PUBLIC_BRANCH} moved since the audit" \
        "audited parent: ${PARENT}" \
        "local now:      ${LOCAL_TIP:-<no such branch>}"
fi
if ! git fetch --quiet "$REMOTE" "$PUBLIC_BRANCH" 2>/dev/null; then
    die "could not fetch ${REMOTE}/${PUBLIC_BRANCH}"
fi
REMOTE_TIP="$(git rev-parse FETCH_HEAD)"
if [ "$REMOTE_TIP" != "$PARENT" ]; then
    die "${REMOTE}/${PUBLIC_BRANCH} moved since the audit" \
        "audited parent: ${PARENT}" \
        "remote now:     ${REMOTE_TIP}"
fi
if [ -n "$(git status --porcelain)" ]; then
    # The signature does not read the working tree, but the ledger commit does.
    # Refusing here keeps step 5 from ever needing a judgement call.
    die "the working tree is not clean"
fi

# ── 1b. the sign-off identity must resolve ──────────────────────────────────
if [ ! -f "${SCRIPT_DIR}/signoff.sh" ]; then
    die "scripts/release/signoff.sh is missing beside this script" \
        "it holds the sign-off rule both halves of the crossing share; without it there is no identity to certify with"
fi
. "${SCRIPT_DIR}/signoff.sh"
SIGNOFF="$(dco_signoff_line || true)"
if [ -z "$SIGNOFF" ]; then
    die "the DCO sign-off identity does not resolve in this repository" \
        "every commit on the public trunk carries Signed-off-by, and dco.yml never sees this one — it lands by a direct push" \
        "run this once:" \
        "    $(dco_signoff_remedy)"
fi

# ── 2. sign ─────────────────────────────────────────────────────────────────
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT INT TERM
MSG="${WORK}/msg"
cat > "$MSG" <<EOF
cosmon v${VERSION}

The public trunk advances by one signed commit per release. Its tree is the
development tree verbatim; its single parent is the previous public tip. The
development history that produced it stays in the development repository and
is named by the trailer below.

Projected-From: ${DEV_SHA}
${SIGNOFF}
EOF

NEW="$(git commit-tree -S -p "$PARENT" "$TREE" -F "$MSG")" || die "signing failed"
[ -n "$NEW" ] || die "signing produced no commit"

# ── 3. compare-and-swap the local public branch ─────────────────────────────
git update-ref "refs/heads/${PUBLIC_BRANCH}" "$NEW" "$PARENT" \
    || die "local ${PUBLIC_BRANCH} moved between the check and the swap"

# ── 4. push, and roll the local branch back if the remote refuses ───────────
# The push URL is disarmed on the development repository on purpose, so the
# deliberate push names the fetch URL explicitly.
PUSH_TO="$(git remote get-url "$REMOTE")"
if ! git push "$PUSH_TO" "refs/heads/${PUBLIC_BRANCH}:refs/heads/${PUBLIC_BRANCH}"; then
    git update-ref "refs/heads/${PUBLIC_BRANCH}" "$PARENT" "$NEW"
    die "the remote refused the push; local ${PUBLIC_BRANCH} rolled back to ${PARENT}" \
        "the signed commit ${NEW} is now unreferenced and gc will collect it"
fi

# ── 5. record the pairing, dev-side ─────────────────────────────────────────
git update-ref "refs/cosmon/crossings/v${VERSION}" "$DEV_SHA"
git update-ref -d "refs/cosmon/crossing/v${VERSION}" 2>/dev/null || true

LEDGER_NOTE=""
if [ ! -f "$LEDGER" ]; then
    mkdir -p "$(dirname "$LEDGER")"
    printf '# version\tpublic-sha\tdev-sha\n' > "$LEDGER"
fi
printf '%s\t%s\t%s\n' "$VERSION" "$NEW" "$DEV_SHA" >> "$LEDGER"
if git symbolic-ref -q HEAD >/dev/null; then
    git add -- "$LEDGER"
    # `-s`: this one is a DEVELOPMENT commit, so `dco.yml` would catch it in a
    # pull request rather than never — but a script that composes one signed-off
    # commit and one un-signed-off commit invites the reader to wonder which
    # rule applies, and the answer is the same rule.
    git commit -q -s -m "chore(release): record the v${VERSION} crossing" -- "$LEDGER" \
        || LEDGER_NOTE="the ledger line is written but uncommitted — commit ${LEDGER} yourself"
else
    LEDGER_NOTE="HEAD is detached, so the ledger line is written but uncommitted"
fi

cat <<EOF
✅ pushed. public tip ${NEW} (signed, 1 parent ${PARENT})
   dev tip ${DEV_SHA} untouched, archived at refs/cosmon/crossings/v${VERSION}
   ledger ${LEDGER}: v${VERSION} ${NEW} ${DEV_SHA}
EOF
[ -z "$LEDGER_NOTE" ] || printf '   ⚠ %s\n' "$LEDGER_NOTE"
