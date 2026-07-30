#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-only
# signoff.sh — resolve the DCO `Signed-off-by` identity, or refuse.
#
# Sourced, never executed:
#
#     . "${SCRIPT_DIR}/signoff.sh"
#     SIGNOFF="$(dco_signoff_line)" || die "…"
#
# ── Why this exists at all ──────────────────────────────────────────────────
# Every commit on the public trunk must carry a `Signed-off-by` trailer
# (DCO.md, CONTRIBUTING.md). `.github/workflows/dco.yml` triggers on
# `pull_request: branches: [main]` ONLY, and the crossing lands by a direct
# push to that trunk — so no CI check ever looks at a projection commit. The
# trailer therefore has to be right at composition time; there is no second
# chance downstream and no gate that will complain.
#
# ── Why it lives in one file both scripts source ────────────────────────────
# `crossing.sh` composes the candidate's message and `sign-and-push.sh`
# re-composes it for the signature. Two copies of a rule about who certifies
# the origin of the work is two copies that can drift, and a drift here is
# invisible: both would still produce a well-formed commit.
#
# ── Why `user.name` / `user.email` and nothing else ─────────────────────────
# The trailer asserts who certifies the origin of the work, so inventing it is
# worse than omitting it. Two shapes are deliberately refused as sources:
#
#   * `git var GIT_COMMITTER_IDENT` — it looks like the strict answer and is
#     not: with no `user.email` configured it GUESSES, composing a name from
#     the gecos field and an address from the local hostname, and exits 0. That
#     is exactly the commit signed by nobody in particular;
#   * a bespoke environment variable — a caller who can set it can certify in
#     someone else's name, which inverts the point of the trailer.
#
# What is left is the pair `git commit -s` itself reads, resolved by git across
# the local, global and system configuration files. Unset means unset: this
# refuses instead of guessing, and the caller's `die` names the one command
# that fixes it.

# Echo `Signed-off-by: Name <email>` on stdout, or nothing and a non-zero
# status. The caller decides what refusing looks like — the two scripts that
# source this have different voices and different "nothing was built" claims.
dco_signoff_line() {
    _dco_name="$(git config --get user.name 2>/dev/null || true)"
    _dco_email="$(git config --get user.email 2>/dev/null || true)"

    [ -n "$_dco_name" ] || return 1
    [ -n "$_dco_email" ] || return 1

    # A trailer is one line by construction. Anything that would break that
    # invariant is an unresolvable identity, not a trailer to repair: a second
    # line in a commit message is not a sign-off, it is a forgery surface.
    #
    # Counted with `wc -l` rather than matched against a newline glob: a shell
    # pattern has to spell the newline as `"$(printf '\n')"`, and command
    # substitution strips trailing newlines, so that pattern silently collapses
    # to the empty string and then matches EVERY identity.
    if [ "$(printf '%s' "$_dco_name$_dco_email" | wc -l | tr -d ' ')" != "0" ]; then
        return 1
    fi
    # `dco.yml` matches `^Signed-off-by: .+ <.+@.+>`; an address that cannot
    # satisfy it would be a trailer the gate rejects the first time anyone does
    # open a pull request, which is a stale-tomorrow failure, not a green today.
    case "$_dco_email" in
        *@*) : ;;
        *) return 1 ;;
    esac

    printf 'Signed-off-by: %s <%s>\n' "$_dco_name" "$_dco_email"
}

# The one command that restores the precondition, printed as a fact rather than
# as a choice — the same shape both callers use for the disarmed push URL.
dco_signoff_remedy() {
    printf 'git config user.name "Your Name" && git config user.email you@example.org\n'
}
