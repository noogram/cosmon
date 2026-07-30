#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-only
# crossing.sh — build the UNSIGNED public projection candidate, and refuse
# before building anything if the tree is not in a state that may be crossed.
#
#     scripts/release/crossing.sh
#
# ── What "the crossing" is ──────────────────────────────────────────────────
# The development trunk and the public trunk are two histories, not one. The
# public trunk advances by ONE signed commit per release whose TREE is the
# development tree verbatim and whose single PARENT is the previous public tip.
# `git commit-tree` is the whole mechanism: it takes a tree object that already
# exists and gives it a new parent, so it needs no checkout, no working tree,
# and it cannot conflict.
#
# The two rejected alternatives, recorded so they are not re-proposed:
#
#   * squash-per-release — same resulting shape, but it needs a checkout and a
#     working tree and can conflict. Strictly worse for an identical result.
#   * rebase-and-fast-forward — rewrites every development SHA, dangling every
#     merge SHA recorded in `events.jsonl`. The ledger's own witnesses would
#     then point at objects that no longer exist. Both alternatives also force
#     deleting `ensure_attribution_carrier` (crates/cosmon-cli/src/cmd/done.rs),
#     which the deliberation refused unanimously.
#
# ── Why a script and not a `cs` verb ────────────────────────────────────────
# The §8p command surface is frozen; and a subcommand that needs a human at the
# keyboard for two of its three steps is a verb pretending to be a primitive.
# This script does everything a machine may do alone and then stops, printing
# ONE command for the operator. Signing and pushing live in the companion
# `sign-and-push.sh`, which the printed line invokes with the tree and the
# expected parent pinned as explicit arguments.
#
# ── The order below is the design, not a style ──────────────────────────────
#   0. the remote's push URL must already be disarmed;
#   0b. refuse if the DCO sign-off identity does not resolve — see below;
#   1. refuse on a dirty tree — `commit-tree` reads a COMMITTED tree, so with
#      uncommitted edits it would succeed, sign, and ship the PREVIOUS commit's
#      content while the operator watched a green exit. A failure that succeeds
#      is worse than a crash, and this is the one that would have been silent;
#   2. refuse if the local public branch and its remote disagree — print both
#      and stop. Never fetch-and-merge, never rebase: a tired operator told
#      "these differ, run git fetch" recovers; a tired operator whose tool
#      silently reconciled two trunks does not;
#   3. refuse on a waiver introduced by this very candidate (see below);
#   4. run the structural publish gate, BLOCKING, in this same worktree;
#   5. only then capture the tree and build the candidate.
#
# Gate placement is before the crossing on purpose: a finding at that point
# costs one reset of nothing at all, because the commit does not exist yet.
# After, un-making it means either rewriting the public trunk — forbidden — or
# shipping a follow-up that leaves the leak permanently reachable.
#
# ── Q8: audit, tree capture and crossing are ONE breath, ONE worktree ───────
# `scripts/publish.sh` audits `git ls-files` and the working tree of the
# CURRENT checkout. It has no `--tree <sha>` argument and cannot be pointed at
# an arbitrary tree object. So auditing in one worktree and crossing from
# another audits a tree nobody ships. With ~30 live worktrees on this machine
# that is not hypothetical, which is why this script refuses to run against any
# repository other than the one it is itself checked out in.
#
# The gate is `publish.sh --check` called DIRECTLY, never through
# `release-checklist.sh`: that checklist's gates 1 and 4 honestly PEND without
# `gitleaks` and the operator's private denylist, and a sequence whose blocking
# gate can PEND is not a sequence that fails closed. The full checklist is run
# too, immediately after, and is advisory.
#
# ── Why the sign-off is composed here and checked first ─────────────────────
# Every commit on the public trunk must carry a `Signed-off-by` trailer, and
# `.github/workflows/dco.yml` triggers on `pull_request: branches: [main]`
# ONLY. The crossing lands by a direct push to that trunk, so no CI check ever
# looks at a projection commit: the trailer has to be right at composition time
# because there is no second chance downstream and no gate that will complain.
#
# The identity is resolved the way `git commit -s` resolves it — `user.name`
# and `user.email` as git reads them — and an unresolvable identity is a
# refusal, placed with the other configuration preconditions so it costs
# nothing. The rule lives in `signoff.sh` because `sign-and-push.sh`
# re-composes this same message for the signature; see that file's header for
# why `git var GIT_COMMITTER_IDENT` and an environment variable are both
# refused as sources.
#
# ── Waiver policy ───────────────────────────────────────────────────────────
# The inline publish-gate waiver earns its safety entirely from being a
# sentence someone had to write and a reviewer reads in the diff. At 2am the
# author and the reviewer are the same tired person. So the gate stays
# blocking, and a waiver marker that this candidate INTRODUCES is itself a
# refusal here: only waivers already reachable from the public tip count. One
# comparison against the public tree decides it.
#
# The marker string is ASSEMBLED below rather than written literally, so this
# file and its test do not read as waivers to their own scan. A path exclusion
# would be the permanent blind spot the doctrine refuses; assembling the needle
# keeps the scan exclusion-free.
#
# Exit 0 = a candidate exists and one command is left. Non-zero = one named
# reason, and nothing was built.

set -eu

REMOTE="origin"
PUBLIC_BRANCH="main"
VERSION=""
SKIP_CHECKLIST="no"

usage() {
    cat <<'EOF'
usage: scripts/release/crossing.sh [options]

  --remote NAME         remote holding the public trunk (default: origin)
  --public-branch NAME  local branch mirroring the public trunk (default: main)
  --version X.Y.Z       release version (default: [workspace.package] version)
  --skip-checklist      skip the ADVISORY release-checklist.sh run
  -h, --help            this text

Builds the unsigned candidate and prints the one command left. It never signs,
never pushes, and never writes to the development branch.
EOF
}

while [ $# -gt 0 ]; do
    case "$1" in
        --remote) REMOTE="${2:?--remote needs a value}"; shift 2 ;;
        --public-branch) PUBLIC_BRANCH="${2:?--public-branch needs a value}"; shift 2 ;;
        --version) VERSION="${2:?--version needs a value}"; shift 2 ;;
        --skip-checklist) SKIP_CHECKLIST="yes"; shift ;;
        -h|--help) usage; exit 0 ;;
        *) printf 'crossing.sh: unknown argument %s\n' "$1" >&2; usage >&2; exit 2 ;;
    esac
done

# ── the two output shapes, and nothing else ─────────────────────────────────
# The failure shape names ONE reason and stops. It never offers an alternative,
# because offering an alternative at 2am is handing the operator a decision.
# Where a single command restores the precondition, that command is printed as
# a fact, not as a choice.
die() {
    printf '❌ %s. nothing was built. nothing to undo.\n' "$1" >&2
    shift
    for _line in "$@"; do
        printf '   %s\n' "$_line" >&2
    done
    exit 1
}

SCRIPT_DIR="$(CDPATH='' cd -P -- "$(dirname -- "$0")" && pwd -P)"
REPO_ROOT="$(CDPATH='' cd -P -- "${SCRIPT_DIR}/../.." && pwd -P)"

# Q8, enforced rather than documented: the audit, the tree capture and the
# crossing must all name the same worktree. The script's own location is the
# authority, because that is the tree `publish.sh` will audit.
#
# Both sides are resolved PHYSICALLY before they are compared: on macOS the
# same directory is reachable as /var/… and /private/var/…, and a symlink is
# not a second worktree.
INVOKED_ROOT="$(git rev-parse --show-toplevel 2>/dev/null || echo '')"
if [ -n "$INVOKED_ROOT" ]; then
    INVOKED_ROOT="$(CDPATH='' cd -P -- "$INVOKED_ROOT" && pwd -P)"
fi
if [ "$INVOKED_ROOT" != "$REPO_ROOT" ]; then
    die "this script is checked out in a different worktree than the one you ran it from" \
        "script's worktree: ${REPO_ROOT}" \
        "your worktree:     ${INVOKED_ROOT:-<not a git repository>}" \
        "the publish gate audits the checkout it runs in; crossing from a second one ships a tree nobody audited"
fi
cd "$REPO_ROOT"

# ── 0. the push URL must already be disarmed ────────────────────────────────
# An accidental `git push` from the development repository must be
# UNREPRESENTABLE, not merely caught by a later review. This is a one-line
# daylight gesture the operator makes once; `sign-and-push.sh` pushes through
# the fetch URL explicitly, so the deliberate path is unaffected.
PUSH_URL="$(git remote get-url --push "$REMOTE" 2>/dev/null || echo '')"
if [ -z "$PUSH_URL" ]; then
    die "remote '${REMOTE}' does not exist in this repository"
fi
case "$PUSH_URL" in
    DISABLED://*) : ;;
    *)
        die "the development repository can still push to '${REMOTE}'" \
            "run this once, in daylight:" \
            "    git remote set-url --push ${REMOTE} 'DISABLED://accidental-push-from-dev'"
        ;;
esac

# ── 0b. the sign-off identity must resolve, or nothing is built ─────────────
# Composed now rather than at step 5 so the refusal joins the other
# configuration preconditions: a candidate that exists and cannot be signed off
# is a candidate whose only honest fate is deletion.
. "${SCRIPT_DIR}/signoff.sh"
SIGNOFF="$(dco_signoff_line || true)"
if [ -z "$SIGNOFF" ]; then
    die "the DCO sign-off identity does not resolve in this repository" \
        "every commit on the public trunk carries Signed-off-by, and dco.yml never sees this one — it lands by a direct push" \
        "run this once:" \
        "    $(dco_signoff_remedy)"
fi

# ── 1. a dirty tree is the refusal that matters most ────────────────────────
DIRTY="$(git status --porcelain)"
if [ -n "$DIRTY" ]; then
    die "the working tree is not clean" \
        "commit-tree reads the COMMITTED tree, so this would have shipped the previous commit's content and exited green" \
        "$(printf '%s' "$DIRTY" | head -n 10)"
fi

DEV_SHA="$(git rev-parse HEAD)"

# ── 2. the local public branch and the remote must already agree ────────────
if ! git rev-parse --verify --quiet "refs/heads/${PUBLIC_BRANCH}" >/dev/null; then
    die "there is no local '${PUBLIC_BRANCH}' branch mirroring the public trunk"
fi
if ! git fetch --quiet "$REMOTE" "$PUBLIC_BRANCH" 2>/dev/null; then
    die "could not fetch ${REMOTE}/${PUBLIC_BRANCH}"
fi
PUBLIC_SHA="$(git rev-parse "refs/heads/${PUBLIC_BRANCH}")"
REMOTE_SHA="$(git rev-parse FETCH_HEAD)"
if [ "$PUBLIC_SHA" != "$REMOTE_SHA" ]; then
    die "local ${PUBLIC_BRANCH} and ${REMOTE}/${PUBLIC_BRANCH} differ" \
        "local  ${PUBLIC_BRANCH}: ${PUBLIC_SHA}" \
        "${REMOTE}/${PUBLIC_BRANCH}: ${REMOTE_SHA}"
fi

TREE="$(git rev-parse "HEAD^{tree}")"
PUBLIC_TREE="$(git rev-parse "refs/heads/${PUBLIC_BRANCH}^{tree}")"
if [ "$TREE" = "$PUBLIC_TREE" ]; then
    die "the development tree and the public tree are already identical" \
        "there is nothing to project: a crossing here would add a commit carrying no change"
fi

# ── 3. a waiver this candidate INTRODUCES is a refusal ──────────────────────
# Assembled, never written literally — see the header.
MARKER="publish:"' allow'

marker_keys() {
    # `<path>\t<whitespace-squeezed line>` for every marker occurrence in $1.
    # The line number is dropped on purpose: a waiver that merely moved within
    # its file is the same waiver, and re-refusing it would train the operator
    # to expect noise from this gate.
    git grep -n -F -e "$MARKER" "$1" -- . 2>/dev/null \
        | sed -e "s|^${1}:||" -e 's|^\(.*\):[0-9][0-9]*:|\1\t|' \
              -e 's|[[:space:]][[:space:]]*| |g' -e 's| *$||' \
        | sort -u
}

sidecar_keys() {
    # The sidecar waiver (`<path>.publish-allow`) exists for formats that
    # cannot carry a comment. It is a waiver too, so a NEW one is refused on
    # exactly the same footing: identity is path + content hash.
    git ls-tree -r --name-only "$1" \
        | grep -e '\.publish-allow$' 2>/dev/null \
        | while IFS= read -r _p; do
            printf '%s\t%s\n' "$_p" "$(git rev-parse "${1}:${_p}")"
          done \
        | sort -u
}

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT INT TERM

{ marker_keys HEAD; sidecar_keys HEAD; } | sort -u > "${WORK}/head.keys"
{ marker_keys "refs/heads/${PUBLIC_BRANCH}"; sidecar_keys "refs/heads/${PUBLIC_BRANCH}"; } \
    | sort -u > "${WORK}/public.keys"
comm -23 "${WORK}/head.keys" "${WORK}/public.keys" > "${WORK}/new.keys"

if [ -s "${WORK}/new.keys" ]; then
    NEW_COUNT="$(wc -l < "${WORK}/new.keys" | tr -d ' ')"
    die "${NEW_COUNT} publish-gate waiver(s) are introduced by this very candidate" \
        "a waiver is safe because a reviewer reads it in the diff; tonight the author and the reviewer are the same person" \
        "$(sed 's|\t|  |' "${WORK}/new.keys" | head -n 10)"
fi

# ── 4. the structural gate, blocking, in this worktree ──────────────────────
printf '   running scripts/publish.sh --check …\n' >&2
if ! "${REPO_ROOT}/scripts/publish.sh" --check >"${WORK}/publish.out" 2>&1; then
    die "scripts/publish.sh --check failed" \
        "$(tail -n 20 "${WORK}/publish.out")"
fi

# ── 4b. the full checklist, advisory ────────────────────────────────────────
# Broader than the structural subset, but it cannot be a hard gate here: its
# secret scan needs `gitleaks` installed and its content denylist is
# operator-private, so both honestly report PEND without them.
if [ "$SKIP_CHECKLIST" = "no" ] && [ -x "${REPO_ROOT}/scripts/release-checklist.sh" ]; then
    printf '   running scripts/release-checklist.sh (advisory) …\n' >&2
    if ! "${REPO_ROOT}/scripts/release-checklist.sh" >"${WORK}/checklist.out" 2>&1; then
        printf '   ⚠ release-checklist.sh (advisory) exited non-zero — read it before you paste:\n' >&2
        sed 's/^/     /' "${WORK}/checklist.out" | tail -n 20 >&2
    fi
fi

# ── 5. build the unsigned candidate ─────────────────────────────────────────
if [ -z "$VERSION" ]; then
    # The release version is `[workspace.package] version` in the root
    # Cargo.toml — the same field `release-version-conformance.sh` seals
    # against every shipped binary's `--version`.
    VERSION="$(awk '
        /^\[workspace\.package\]/ { in_ws = 1; next }
        /^\[/ { in_ws = 0 }
        in_ws && /^version[[:space:]]*=/ {
            gsub(/^version[[:space:]]*=[[:space:]]*"/, ""); gsub(/".*$/, "")
            print; exit
        }' Cargo.toml)"
fi
if [ -z "$VERSION" ]; then
    die "could not read the release version from [workspace.package] in Cargo.toml"
fi

CANDIDATE_REF="refs/cosmon/crossing/v${VERSION}"

# Exactly ONE provenance trailer, parseable by `lineage::parse_trailers`, beside
# the DCO sign-off resolved at step 0b. Not 120 stacked
# provenance blocks: the public commit has one parent and one tree, and
# enumerating 120 development commits under it would describe a history the
# public trunk does not have — actively false rather than merely lossy. The
# development SHA named here is reachable dev-side through the archive ref
# `sign-and-push.sh` writes.
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

CANDIDATE="$(git commit-tree -p "$PUBLIC_SHA" "$TREE" -F "$MSG")"
git update-ref "$CANDIDATE_REF" "$CANDIDATE"

SIGN_CMD="scripts/release/sign-and-push.sh --tree ${TREE} --parent ${PUBLIC_SHA} --version ${VERSION} --dev-sha ${DEV_SHA}"

cat <<EOF
✅ ready. one command left. paste it:
     ${SIGN_CMD}
   dev tip ${DEV_SHA} (untouched, still yours) / public tip ${PUBLIC_SHA} gains
   one signed child carrying tree ${TREE}
   read it first if you like: git show ${CANDIDATE_REF} (${CANDIDATE}, unsigned)
EOF
