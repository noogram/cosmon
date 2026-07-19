#!/bin/sh
# check-install-drift.sh — assert that the PUBLICLY SERVED install.sh is the
# byte-for-byte projection of the canonical infra/install/install.sh.
#
#     scripts/release/check-install-drift.sh --served-url  https://<host>/cosmon/install.sh
#     scripts/release/check-install-drift.sh --served-file ./some-served-copy.sh
#
# ── Why this exists ─────────────────────────────────────────────────────────
# The source of truth is infra/install/install.sh in this repo. The bytes a
# stranger actually pipes into `sh` come from the public endpoint. Whenever
# those two are joined by a HAND-COPY, they drift — and both times they drifted
# it was SILENT:
#
#   1. the gnu→musl target fix was made here, then had to be re-synced to the
#      served copy by hand;
#   2. v0.2.0 shipped the cosmon-remote connector correctly — signed assets on
#      all four triples, the client tarball verifiably carrying BOTH binaries —
#      but the served installer had zero references to cosmon-remote. A fresh
#      one-liner install placed `cs` and silently discarded the connector, so
#      the documented remote-connect workflow was injouable from a fresh public
#      install even though the release was correct.
#
# Nothing reddened either time. This script is the thing that reddens. It is
# deliberately standalone (POSIX sh, no repo deps beyond the two inputs) so it
# runs identically in CI, in the RUNBOOK's pre-publish gesture, and on a laptop.
#
# ── What it checks, in order ────────────────────────────────────────────────
#   1. CONFORMANCE — every marker in infra/install/served-conformance.txt is
#      present in the served bytes. This names the missing capability in human
#      terms ("served copy has no cosmon-remote logic") rather than emitting an
#      anonymous diff. Markers are themselves validated against the canonical
#      source first, so a marker can never rot into a claim the source no
#      longer makes.
#   2. IDENTITY — served == canonical, after a narrow, explicitly-justified
#      normalization (see normalize() below). This is the strict gate: it
#      catches every divergence, including ones no marker anticipated.
#
# Exit 0 = served is the source. Non-zero = drift (with a readable report).
# There is no "warn" mode on purpose: both past incidents were things that
# passed quietly.
#
# ── Normalization (and why each rule is safe) ───────────────────────────────
#   * a leading `COSMON_VERSION='<tag>'` line — the endpoint injects this when
#     the caller passes `?version=`, so it is a legitimate serving-time addition
#     and not drift. Only stripped from the very first line, only when it
#     matches the exact sanitized shape the endpoint emits.
#   * CRLF line endings — some CDNs/object stores rewrite them; the script's
#     semantics are unchanged.
#   * a single trailing newline difference — likewise a transport artifact.
# Nothing else is normalized. Comments, whitespace, and ordering all count:
# a served copy that is "equivalent but edited" is exactly the hand-copy state
# this check exists to abolish.

set -eu

SERVED_URL=""
SERVED_FILE=""
# Resolve the repo root from this script's location so the checker works from
# any cwd (CI, RUNBOOK, laptop).
SCRIPT_DIR="$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)"
REPO_ROOT="$(CDPATH='' cd -- "${SCRIPT_DIR}/../.." && pwd)"
CANONICAL="${REPO_ROOT}/infra/install/install.sh"
MARKERS="${REPO_ROOT}/infra/install/served-conformance.txt"

C_ERR=''; C_OK=''; C_OFF=''
if [ -t 2 ] && [ -z "${NO_COLOR:-}" ]; then
    C_ERR='\033[1;31m'; C_OK='\033[1;32m'; C_OFF='\033[0m'
fi
ok()  { printf "${C_OK}✓${C_OFF} %s\n" "$1" >&2; }
die() { printf "${C_ERR}✗${C_OFF} %s\n" "$1" >&2; exit 1; }

usage() {
    cat >&2 <<'USAGE'
check-install-drift.sh — served install.sh must equal infra/install/install.sh

  --served-url <url>     fetch the served bytes over https
  --served-file <path>   read the served bytes from a file (offline / tests)
  --canonical <path>     override the canonical source (tests)
  --markers <path>       override the conformance marker list (tests)
  -h | --help            this

Exactly one of --served-url / --served-file is required.
Exit 0 = no drift. Non-zero = drift, with a report on stderr.
USAGE
}

while [ $# -gt 0 ]; do
    case "$1" in
        --served-url)   SERVED_URL="${2:?--served-url needs a value}"; shift 2 ;;
        --served-url=*) SERVED_URL="${1#*=}"; shift ;;
        --served-file)  SERVED_FILE="${2:?--served-file needs a value}"; shift 2 ;;
        --served-file=*) SERVED_FILE="${1#*=}"; shift ;;
        --canonical)    CANONICAL="${2:?--canonical needs a value}"; shift 2 ;;
        --canonical=*)  CANONICAL="${1#*=}"; shift ;;
        --markers)      MARKERS="${2:?--markers needs a value}"; shift 2 ;;
        --markers=*)    MARKERS="${1#*=}"; shift ;;
        -h|--help)      usage; exit 0 ;;
        *)              usage; die "unknown argument: $1" ;;
    esac
done

if [ -n "$SERVED_URL" ] && [ -n "$SERVED_FILE" ]; then
    die "pass --served-url OR --served-file, not both"
fi
if [ -z "$SERVED_URL" ] && [ -z "$SERVED_FILE" ]; then
    usage; die "one of --served-url / --served-file is required"
fi
[ -f "$CANONICAL" ] || die "canonical source not found: ${CANONICAL}"
[ -f "$MARKERS" ]   || die "conformance markers not found: ${MARKERS}"

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT INT TERM

# ── obtain the served bytes ──────────────────────────────────────────────────
served_raw="${tmp}/served.raw"
if [ -n "$SERVED_URL" ]; then
    command -v curl >/dev/null 2>&1 || die "curl is required for --served-url"
    # -f so a 4xx/5xx is a hard failure here rather than a body we would go on
    # to diff. A dark endpoint is NOT this script's business: the caller decides
    # whether "not live yet" is acceptable (see the served-drift CI job).
    curl -fsSL --proto '=https' --tlsv1.2 -o "$served_raw" "$SERVED_URL" \
        || die "could not fetch served installer: ${SERVED_URL}"
else
    [ -f "$SERVED_FILE" ] || die "served file not found: ${SERVED_FILE}"
    cat -- "$SERVED_FILE" > "$served_raw"
fi

[ -s "$served_raw" ] || die "served installer is empty: ${SERVED_URL}${SERVED_FILE}"

# ── conformance markers ──────────────────────────────────────────────────────
# Read markers (skip blanks + `#` comments) and check each against BOTH the
# canonical source (invariant: a marker must reflect something the source
# actually says) and the served bytes (the real question).
missing_markers=""
stale_markers=""
while IFS= read -r line || [ -n "$line" ]; do
    case "$line" in ''|'#'*) continue ;; esac
    if ! grep -qF -- "$line" "$CANONICAL"; then
        stale_markers="${stale_markers}    ${line}
"
        continue
    fi
    if ! grep -qF -- "$line" "$served_raw"; then
        missing_markers="${missing_markers}    ${line}
"
    fi
done < "$MARKERS"

if [ -n "$stale_markers" ]; then
    printf '%b\n' "${C_ERR}✗ conformance marker list is stale${C_OFF}" >&2
    printf '\nThese markers are not in the canonical source any more:\n\n%s\n' "$stale_markers" >&2
    printf 'Either the capability was removed on purpose (drop the marker from\n' >&2
    printf '%s), or the source regressed.\n' "$MARKERS" >&2
    exit 1
fi

if [ -n "$missing_markers" ]; then
    printf '%b\n' "${C_ERR}✗ SERVED INSTALLER IS STALE — missing required capability${C_OFF}" >&2
    printf '\nThe canonical source has these, the served bytes do not:\n\n%s\n' "$missing_markers" >&2
    printf 'A stranger running the public one-liner right now does NOT get what\n' >&2
    printf 'this repo ships. Re-publish the served installer from source — see\n' >&2
    printf 'infra/install/RUNBOOK.md ("Re-publish the served installer").\n' >&2
    exit 1
fi
ok "conformance: served installer carries every required capability"

# ── byte identity (after narrow normalization) ───────────────────────────────
# Strip a leading endpoint-injected version pin, normalize CRLF, and let the
# trailing-newline difference wash out. Everything else is drift.
normalize() {
    # sed: on line 1 only, delete an exact `COSMON_VERSION='<sanitized-tag>'`
    # line — the same conservative tag charset the endpoint sanitizes to.
    sed -e "1{/^COSMON_VERSION='[0-9A-Za-z][0-9A-Za-z._-]*'\$/d;}" -- "$1" \
        | tr -d '\r'
}
normalize "$served_raw" > "${tmp}/served.norm"
normalize "$CANONICAL"  > "${tmp}/canonical.norm"

if diff -u "${tmp}/canonical.norm" "${tmp}/served.norm" > "${tmp}/drift.diff" 2>&1; then
    ok "identity: served installer is byte-identical to ${CANONICAL#"${REPO_ROOT}/"}"
    ok "no drift"
    exit 0
fi

printf '%b\n' "${C_ERR}✗ SERVED INSTALLER HAS DRIFTED FROM SOURCE${C_OFF}" >&2
printf '\n  canonical: %s\n' "$CANONICAL" >&2
printf '  served:    %s\n\n' "${SERVED_URL:-$SERVED_FILE}" >&2
printf -- '--- diff (canonical → served) ------------------------------------\n' >&2
# Cap the diff so a wholesale replacement does not bury the headline.
head -n 200 "${tmp}/drift.diff" >&2
lines="$(wc -l < "${tmp}/drift.diff" | tr -d ' ')"
if [ "$lines" -gt 200 ]; then
    printf -- '... (%s more diff lines)\n' "$((lines - 200))" >&2
fi
printf -- '------------------------------------------------------------------\n' >&2
printf '\nThe served bytes are NOT this repo. Re-publish from source — see\n' >&2
printf 'infra/install/RUNBOOK.md ("Re-publish the served installer").\n' >&2
exit 1
