#!/usr/bin/env bash
# architecture-audit.sh — verify the seven architectural invariants of ADR-082.
#
# Contract version: 2
# Source: cosmon/scripts/architecture-audit.sh @ commit 50a1015ac0f0bf566a8bbb366bed81c22993aba0
# Vendoring: copy this file verbatim into <galaxy>/scripts/architecture-audit.sh
#            and edit only the "Source" line above to record your local commit.
#            Bumping Contract version is a coordinated migration governed by ADR-082.
#
# Usage:
#   bash scripts/architecture-audit.sh                  # default: --check on non-TTY, --report on TTY
#   bash scripts/architecture-audit.sh --check          # CI gate; exit 1 on any FAIL
#   bash scripts/architecture-audit.sh --report PATH    # write Markdown report to PATH; exit 0
#
# Output: seven rows of {PASS|FAIL|SKIP|WARN}, one line each. On FAIL, the row
# carries file:line and a why-the-rule + how-to-fix message (Karpathy
# audit-as-curriculum discipline). No score. No tier name in output. No badges.
#
# Doctrine: ADR-082 — Architecture baseline. Read before changing this script.

set -uo pipefail

# ---------------------------------------------------------------------------
# Locate the galaxy root (walk up from the script's location until we see CLAUDE.md)
# ---------------------------------------------------------------------------
SCRIPT_DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" && pwd )"
GALAXY_ROOT="$SCRIPT_DIR"
while [[ "$GALAXY_ROOT" != "/" && ! -f "$GALAXY_ROOT/CLAUDE.md" ]]; do
  GALAXY_ROOT="$( dirname "$GALAXY_ROOT" )"
done
if [[ ! -f "$GALAXY_ROOT/CLAUDE.md" ]]; then
  echo "architecture-audit: cannot locate galaxy root (no CLAUDE.md ancestor of $SCRIPT_DIR)" >&2
  exit 2
fi
cd "$GALAXY_ROOT"

# ---------------------------------------------------------------------------
# Read architecture_tier (or governance_tier synonym) from CLAUDE.md
# ---------------------------------------------------------------------------
read_tier() {
  local tier
  tier="$( grep -E '^(architecture_tier|governance_tier)[[:space:]]*[:=]' CLAUDE.md 2>/dev/null \
    | head -n 1 \
    | sed -E "s/.*[:=][[:space:]]*['\"]?([a-z_-]+)['\"]?.*/\1/" )"
  if [[ -z "$tier" ]]; then
    tier="exploration"
  fi
  echo "$tier"
}
TIER="$( read_tier )"

# ---------------------------------------------------------------------------
# Argument parsing
# ---------------------------------------------------------------------------
MODE="auto"
REPORT_PATH=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --check)
      MODE="check"
      shift
      ;;
    --report)
      MODE="report"
      REPORT_PATH="${2:?--report requires a PATH argument}"
      shift 2
      ;;
    --help|-h)
      sed -n '1,/^set -uo/p' "$0" | sed 's/^# \{0,1\}//'
      exit 0
      ;;
    *)
      echo "architecture-audit: unknown argument '$1' (try --help)" >&2
      exit 2
      ;;
  esac
done
if [[ "$MODE" == "auto" ]]; then
  if [[ -t 1 ]]; then MODE="report"; REPORT_PATH="/dev/stdout"; else MODE="check"; fi
fi

# ---------------------------------------------------------------------------
# Result accumulator
# ---------------------------------------------------------------------------
declare -a INV_NAMES=()
declare -a INV_STATUSES=()
declare -a INV_NOTES=()
declare -a INV_DETAILS=()

record() {
  # record <inv-name> <status> <note> [<details>]
  INV_NAMES+=( "$1" )
  INV_STATUSES+=( "$2" )
  INV_NOTES+=( "$3" )
  INV_DETAILS+=( "${4:-}" )
}

# ---------------------------------------------------------------------------
# Tier projection — how strict is each INV at the declared tier?
# Returns: HARD | SOFT | OFF
# ---------------------------------------------------------------------------
strictness() {
  local inv="$1"
  case "$TIER" in
    exploration)
      echo "OFF"
      ;;
    stable)
      case "$inv" in
        INV-NAMED-INVARIANT-HAS-TEST|INV-ADR-OPTIONS-CONSIDERED|INV-PRIVATE-FILE-RM-CACHED-NOT-RM)
          echo "HARD"
          ;;
        *)
          echo "SOFT"
          ;;
      esac
      ;;
    production|substrate)
      echo "HARD"
      ;;
    *)
      echo "SOFT"
      ;;
  esac
}

# ---------------------------------------------------------------------------
# Karpathy failure-message template:
#   FAIL <INV>: <file>:<line>
#     why    : <why the rule exists, one sentence>
#     fix    : <concrete fix, one sentence>
#     adr    : docs/adr/082-architecture-baseline.md#<anchor>
# ---------------------------------------------------------------------------
fail_msg() {
  # fail_msg <why> <fix> <anchor>
  printf 'why    : %s\n  fix    : %s\n  adr    : docs/adr/082-architecture-baseline.md#%s' "$1" "$2" "$3"
}

# ---------------------------------------------------------------------------
# INV-DOMAIN-PURE-NO-IO
# ---------------------------------------------------------------------------
# Pattern of forbidden I/O + ambient-nondeterminism calls in the domain crate.
# Covers entropy (rng), wall-clock (Instant/SystemTime/chrono Utc/Local), and —
# the additions that closed task-20260622-3144's blind spot — filesystem,
# process, and network I/O. Before that fix the gate greped only the rng/now
# half and was blind to fs/process/net, so a domain crate could spawn
# processes and read files while the audit reported PASS.
DOMAIN_IO_PATTERN='Instant::now|SystemTime::now|thread_rng|rand::random|OsRng::default|std::fs|std::process|std::net|TcpStream|reqwest|File::open|Command::new|Utc::now|Local::now'

# awk filter: emit "<file>:<lineno>:<line>" for code lines that are OUTSIDE any
# #[cfg(test)] / #![cfg(test)] region and are not comment-only. This is what
# lets the gate ignore test fixtures and doc-comment references while still
# catching real production I/O. Brace-depth tracking handles nested test
# modules (the Rust convention is tests at the bottom of the file, but the
# tracker is robust to mid-file #[cfg(test)] helpers too — a truncate-at-first
# heuristic would under-measure, which is the exact failure this gate exists
# to prevent).
DOMAIN_STRIP_TEST='
  function braces(s,   i,c,n){n=0;for(i=1;i<=length(s);i++){c=substr(s,i,1);if(c=="{")n++;else if(c=="}")n--}return n}
  FNR==1 {in_test=0;depth=0;pending=0;filetest=0}
  /^[[:space:]]*#!\[cfg\(test\)\]/ {filetest=1; next}
  filetest {next}
  {
    line=$0
    if (in_test) { depth+=braces(line); if(depth<=0)in_test=0; next }
    if (pending) {
      if (index(line,"{")>0) { in_test=1; depth=braces(line); pending=0; next }
      if (line ~ /;[[:space:]]*$/) { pending=0; next }
    }
    if (line ~ /^[[:space:]]*#\[cfg\(test\)\]/) { pending=1; next }
    stripped=line; sub(/^[[:space:]]+/,"",stripped)
    if (stripped ~ /^\/\//) next
    if (stripped ~ /^\/\*/) next
    if (stripped ~ /^\*/) next
    print FILENAME ":" FNR ":" line
  }'

check_domain_pure_no_io() {
  local inv="INV-DOMAIN-PURE-NO-IO"
  local domain_crate="crates/cosmon-core"
  if [[ ! -d "$domain_crate" ]]; then
    record "$inv" "SKIP" "no domain crate at $domain_crate; nothing to check"
    return
  fi
  local violations=""
  local f
  while IFS= read -r f; do
    [[ -z "$f" ]] && continue
    while IFS= read -r hit; do
      [[ -z "$hit" ]] && continue
      grep -qE '(// allow|//! allow)' <<<"$hit" && continue
      violations+="$hit"$'\n'
    done < <( awk "$DOMAIN_STRIP_TEST" "$f" 2>/dev/null | grep -E "$DOMAIN_IO_PATTERN" )
  done < <( find "$domain_crate/src" -name '*.rs' 2>/dev/null | sort )
  if [[ -n "$violations" ]]; then
    local first
    first="$( head -n 1 <<<"$violations" )"
    record "$inv" "FAIL" "$first" \
      "$( fail_msg \
            'domain crates must not read ambient time/entropy or perform filesystem/process/network I/O; the boundary is the only place those enter' \
            'inject the value as an input parameter (a Clock/Rng trait) or move the call to an adapter crate (cosmon-state / cosmon-transport)' \
            'inv-domain-pure-no-io' )"
    return
  fi
  record "$inv" "PASS" "no fs/process/net I/O nor ambient time/entropy (outside #[cfg(test)]) in $domain_crate/src"
}

# ---------------------------------------------------------------------------
# INV-PORT-ADAPTER-NAMING — Cargo dep-graph witness, two-impl test
# ---------------------------------------------------------------------------
check_port_adapter_naming() {
  local inv="INV-PORT-ADAPTER-NAMING"
  if [[ ! -f "Cargo.toml" ]]; then
    record "$inv" "SKIP" "no Cargo.toml at galaxy root; not a Rust workspace"
    return
  fi
  if ! grep -q -E '^\s*members\s*=' Cargo.toml; then
    record "$inv" "SKIP" "single-crate workspace; multi-crate dep-graph witness not applicable (use lib/ports/+lib/adapters/)"
    return
  fi
  local domain_crate="crates/cosmon-core"
  if [[ ! -d "$domain_crate" ]]; then
    record "$inv" "SKIP" "no canonical domain crate found"
    return
  fi
  local depending_count
  depending_count="$( grep -rl 'cosmon-core' --include='Cargo.toml' crates 2>/dev/null \
    | grep -v "^$domain_crate/" \
    | wc -l | tr -d ' ' )"
  if [[ "$depending_count" -lt 2 ]]; then
    record "$inv" "FAIL" "Cargo.toml: only $depending_count crate(s) depend on cosmon-core; port-with-one-impl is an indirection, not a port" \
      "$( fail_msg \
            'a port with one implementation is an indirection, not a port (Knuth two-implementation witness)' \
            'add at least one alternative implementor (mock counts) or remove the trait and inline the impl' \
            'inv-port-adapter-naming' )"
    return
  fi
  record "$inv" "PASS" "$depending_count workspace crates depend on cosmon-core (multi-impl witness via Cargo dep graph)"
}

# ---------------------------------------------------------------------------
# INV-NAMED-INVARIANT-HAS-TEST — closure meta-rule
# ---------------------------------------------------------------------------
check_named_invariant_has_test() {
  local inv="INV-NAMED-INVARIANT-HAS-TEST"
  local citations
  citations="$( grep -rhoE 'INV-[A-Z][A-Z0-9_-]+' docs 2>/dev/null \
    | sort -u )"
  if [[ -z "$citations" ]]; then
    record "$inv" "SKIP" "no INV-* citations found in docs/"
    return
  fi
  local missing=""
  local cited
  while IFS= read -r cited; do
    [[ -z "$cited" ]] && continue
    local lower
    lower="$( echo "$cited" | tr '[:upper:]' '[:lower:]' )"
    local test_file="tests/inv/${lower}.sh"
    if [[ -f "$test_file" ]]; then continue; fi
    if grep -q "^### $cited\b" docs/architectural-invariants.md 2>/dev/null; then continue; fi
    if grep -q "^### $cited\b" docs/adr/082-architecture-baseline.md 2>/dev/null; then continue; fi
    missing+="$cited"$'\n'
  done <<<"$citations"
  if [[ -n "$missing" ]]; then
    local first
    first="$( head -n 1 <<<"$missing" )"
    record "$inv" "FAIL" "INV cited in docs/ but no witness found: $first" \
      "$( fail_msg \
            'every named INV must close to a mechanical witness or to an aggregate-document section, otherwise the invariant is folklore' \
            "create tests/inv/$( echo "$first" | tr '[:upper:]' '[:lower:]' ).sh OR add a section ### $first to docs/architectural-invariants.md" \
            'inv-named-invariant-has-test' )"
    return
  fi
  record "$inv" "PASS" "all INV citations in docs/ resolve to a witness (test file or aggregate-doc section)"
}

# ---------------------------------------------------------------------------
# INV-ADR-OPTIONS-CONSIDERED — cultural load-bearing INV
# Grandfather: ADRs whose Date predates 2026-05-03 are exempt (WARN, not FAIL).
# ---------------------------------------------------------------------------
check_adr_options_considered() {
  local inv="INV-ADR-OPTIONS-CONSIDERED"
  local cutoff="2026-05-03"
  local violations=""
  local warnings=0
  local checked=0
  for adr in docs/adr/*.md; do
    [[ -f "$adr" ]] || continue
    case "$( basename "$adr" )" in
      INDEX.md|README.md) continue ;;
    esac
    checked=$(( checked + 1 ))
    local adr_date
    adr_date="$( grep -m 1 -E '^\*\*Date:\*\*' "$adr" | sed -E 's/.*\*\*Date:\*\*[[:space:]]*([0-9-]+).*/\1/' )"
    [[ -z "$adr_date" ]] && adr_date="1970-01-01"
    if [[ "$adr_date" < "$cutoff" ]]; then
      grep -q -E '^##[[:space:]]+Options Considered' "$adr" || warnings=$(( warnings + 1 ))
      continue
    fi
    if ! grep -q -E '^\*\*Status:\*\*' "$adr"; then
      violations+="$adr:1 missing Status: field"$'\n'; continue
    fi
    if ! grep -q -E '^##[[:space:]]+Options Considered' "$adr"; then
      violations+="$adr:1 missing ## Options Considered section"$'\n'; continue
    fi
    if ! grep -q -E '^##[[:space:]]+(Decision|Decision Outcome)' "$adr"; then
      violations+="$adr:1 missing ## Decision (or Decision Outcome) section"$'\n'; continue
    fi
    if ! grep -q -E '^##[[:space:]]+Consequences' "$adr"; then
      violations+="$adr:1 missing ## Consequences section"$'\n'; continue
    fi
  done
  if [[ -n "$violations" ]]; then
    local first
    first="$( head -n 1 <<<"$violations" )"
    record "$inv" "FAIL" "$first" \
      "$( fail_msg \
            'ADRs that ship without Options Considered + named-rejection + Consequences-with-risk slip from record into decree, and future-pilot loses the audit trail' \
            'add ## Options Considered (>=2 named alternatives, Pros/Cons), reference at least one rejected option in Decision, and at least one risk in Consequences' \
            'inv-adr-options-considered' )"
    return
  fi
  if [[ $warnings -gt 0 ]]; then
    record "$inv" "WARN" "$warnings ADR(s) predate cutoff $cutoff and lack Options Considered (grandfathered; retrofit on next substantive edit)"
    return
  fi
  record "$inv" "PASS" "$checked ADR(s) checked; all post-cutoff ADRs carry Status / Options / Decision / Consequences"
}

# ---------------------------------------------------------------------------
# INV-PUBLIC-SURFACE-SCRUBBED — delegate to publish.sh per ADR-082
# ---------------------------------------------------------------------------
check_public_surface_scrubbed() {
  local inv="INV-PUBLIC-SURFACE-SCRUBBED"
  if [[ ! -x "scripts/publish.sh" ]]; then
    record "$inv" "SKIP" "no scripts/publish.sh; INV not enforceable here (file a temp:warm bead if a publish pipeline is added)"
    return
  fi
  if scripts/publish.sh --check >/dev/null 2>&1; then
    record "$inv" "PASS" "scripts/publish.sh --check exited 0"
  else
    record "$inv" "FAIL" "scripts/publish.sh --check exited non-zero" \
      "$( fail_msg \
            'private paths in a published artefact are irreversible; the publish pipeline is the source of truth and gates here' \
            'run scripts/publish.sh --check locally; address each violation it lists; do not bypass with --no-verify' \
            'inv-public-surface-scrubbed' )"
  fi
}

# ---------------------------------------------------------------------------
# Waiver allowlist for INV-PRIVATE-FILE-RM-CACHED-NOT-RM
#
# Read from .architecture-waivers.toml (canonical structured allowlist) and
# also embedded here as a defensive fallback so the audit remains correct even
# if the toml file is missing. A waivered commit's full SHA matches when its
# 7-char prefix is found in the allowlist (we accept both forms).
#
# Source of truth: .architecture-waivers.toml [[private_file_rm_cached_not_rm]]
# Doctrine: ADR-082 §Named waivers.
# ---------------------------------------------------------------------------
load_private_file_waivers() {
  PRIVATE_FILE_WAIVERS=()
  # Embedded fallback (must stay in sync with .architecture-waivers.toml)
  PRIVATE_FILE_WAIVERS+=( "21cf165da2cd81793b726ee49e2c4321c80dbfc3" )
  PRIVATE_FILE_WAIVERS+=( "a99a3cd52e691eb76c6847650cd46decc5bad3de" )
  # Layered read of the structured file if present (extends the embedded list).
  if [[ -f ".architecture-waivers.toml" ]]; then
    local hash
    while IFS= read -r hash; do
      [[ -z "$hash" ]] && continue
      PRIVATE_FILE_WAIVERS+=( "$hash" )
    done < <( grep -E '^\s*commit\s*=' .architecture-waivers.toml \
              | sed -E 's/^\s*commit\s*=\s*"([0-9a-f]+)".*/\1/' )
  fi
}

is_private_file_waivered() {
  local commit="$1"
  local short="${commit:0:7}"
  local w
  for w in "${PRIVATE_FILE_WAIVERS[@]}"; do
    [[ -z "$w" ]] && continue
    if [[ "$w" == "$commit" || "${w:0:7}" == "$short" ]]; then
      return 0
    fi
  done
  return 1
}

# ---------------------------------------------------------------------------
# INV-PRIVATE-FILE-RM-CACHED-NOT-RM
# Heuristic: walk recent commits that add to .gitignore, check the same commit
# did not run a full `git rm` on those paths. Commits listed in
# .architecture-waivers.toml [[private_file_rm_cached_not_rm]] are muted from
# FAIL to WARN with the tag "historical waiver".
# ---------------------------------------------------------------------------
check_private_file_rm_cached_not_rm() {
  local inv="INV-PRIVATE-FILE-RM-CACHED-NOT-RM"
  if ! git rev-parse --git-dir >/dev/null 2>&1; then
    record "$inv" "SKIP" "not in a git repository"
    return
  fi
  load_private_file_waivers
  local window=200
  local violators=""
  local waivered=""
  local commit
  while IFS= read -r commit; do
    [[ -z "$commit" ]] && continue
    git show --stat --format='' "$commit" 2>/dev/null \
      | grep -qE '\.gitignore' || continue
    local added_paths
    added_paths="$( git show "$commit" -- .gitignore 2>/dev/null \
      | grep -E '^\+[^+]' \
      | sed -E 's/^\+//' \
      | grep -vE '^\s*#' \
      | grep -vE '^\s*$' )"
    [[ -z "$added_paths" ]] && continue
    local path
    while IFS= read -r path; do
      [[ -z "$path" ]] && continue
      path="$( echo "$path" | sed -E 's|^/||; s|/$||' )"
      if git show --name-status "$commit" 2>/dev/null \
        | awk -v p="$path" '$1=="D" && index($2, p)==1 { print; exit }' \
        | grep -q .; then
        if is_private_file_waivered "$commit"; then
          waivered+="${commit:0:7} removes $path while gitignoring it (historical waiver)"$'\n'
        else
          violators+="$commit removes $path while gitignoring it (use git rm --cached)"$'\n'
        fi
      fi
    done <<<"$added_paths"
  done < <( git log -n "$window" --format='%H' -- .gitignore 2>/dev/null )
  if [[ -n "$violators" ]]; then
    local first
    first="$( head -n 1 <<<"$violators" )"
    record "$inv" "FAIL" "$first" \
      "$( fail_msg \
            'privatising a file should preserve it in the worktree; a hard rm in the same commit destroys it for every collaborator that pulls' \
            'use git rm --cached <path> (not git rm) when adding the path to .gitignore' \
            'inv-private-file-rm-cached-not-rm' )"
    return
  fi
  if [[ -n "$waivered" ]]; then
    local first
    first="$( head -n 1 <<<"$waivered" )"
    record "$inv" "WARN" "$first; see .architecture-waivers.toml + ADR-082 §Named waivers"
    return
  fi
  record "$inv" "PASS" "no privatisation events in the last $window .gitignore-touching commits drop the file"
}

# ---------------------------------------------------------------------------
# INV-PUBLISH-DEFAULT-DENY
# On a public repo, a stray `cargo publish` (or `cargo publish --workspace`)
# must not be able to push internal library crates to crates.io. The workspace
# default is `publish = false` ([workspace.package]) and every internal crate
# inherits it (`publish.workspace = true`); the sole reserved name-holder crate
# overrides with `publish = true`. This check enumerates workspace members via
# `cargo metadata` and FAILs if more than one crate is registry-publishable.
# Source: delib-20260622-187a F-ARCH-7 (decorative-port + accidental-publish).
# ---------------------------------------------------------------------------
check_publish_default_deny() {
  local inv="INV-PUBLISH-DEFAULT-DENY"
  if ! command -v cargo >/dev/null 2>&1; then
    record "$inv" "SKIP" "cargo not on PATH; cannot enumerate workspace publish flags"
    return
  fi
  if ! command -v python3 >/dev/null 2>&1; then
    record "$inv" "SKIP" "python3 not on PATH; cannot parse cargo metadata JSON"
    return
  fi
  if [[ ! -f "Cargo.toml" ]] || ! grep -q '^\[workspace\]' Cargo.toml 2>/dev/null; then
    record "$inv" "SKIP" "no cargo workspace at galaxy root"
    return
  fi
  local meta
  meta="$( cargo metadata --no-deps --format-version 1 2>/dev/null )"
  if [[ -z "$meta" ]]; then
    record "$inv" "SKIP" "cargo metadata produced no output (offline/locked?)"
    return
  fi
  # cargo metadata encodes publish=false as [], default/true as null, and an
  # explicit registry allowlist as ["name", ...]. Anything other than []
  # means the crate CAN be pushed to a registry.
  local publishable
  publishable="$( printf '%s' "$meta" | python3 -c '
import json, sys
m = json.load(sys.stdin)
ws = set(m.get("workspace_members", []))
out = [p["name"] for p in m["packages"]
       if p["id"] in ws and p.get("publish") != []]
print(" ".join(sorted(out)))
' 2>/dev/null )"
  if [[ -z "${publishable// /}" ]]; then
    record "$inv" "PASS" "every workspace crate is publish=false (no name-holder declared)"
    return
  fi
  local count
  count="$( wc -w <<<"$publishable" | tr -d ' ' )"
  if [[ "$count" -le 1 ]]; then
    record "$inv" "PASS" "exactly one publishable crate (name-holder: $publishable); all internal libs are publish=false"
  else
    record "$inv" "FAIL" "$count publishable workspace crates: $publishable" \
      "$( fail_msg \
            'on a public repo a stray cargo publish can push internal libraries to crates.io that were never meant to be independent artefacts; only the reserved name-holder crate may publish' \
            'set publish = false on each listed crate (or inherit via [workspace.package] publish = false with publish.workspace = true); keep at most the single name-reservation crate publishable' \
            'inv-publish-default-deny' )"
  fi
}

# ---------------------------------------------------------------------------
# Run all checks
# ---------------------------------------------------------------------------
check_domain_pure_no_io
check_port_adapter_naming
check_named_invariant_has_test
check_adr_options_considered
check_public_surface_scrubbed
check_private_file_rm_cached_not_rm
check_publish_default_deny

# ---------------------------------------------------------------------------
# Apply tier projection: turn FAIL into WARN under SOFT/OFF
# ---------------------------------------------------------------------------
apply_tier() {
  local i
  for i in "${!INV_NAMES[@]}"; do
    local strict
    strict="$( strictness "${INV_NAMES[$i]}" )"
    case "${INV_STATUSES[$i]}" in
      FAIL)
        if [[ "$strict" == "SOFT" || "$strict" == "OFF" ]]; then
          INV_STATUSES[$i]="WARN"
          INV_NOTES[$i]="${INV_NOTES[$i]} (tier=$TIER, soft)"
        fi
        ;;
    esac
  done
}
apply_tier

# ---------------------------------------------------------------------------
# Output
# ---------------------------------------------------------------------------
emit_table() {
  local out="${1:-/dev/stdout}"
  {
    echo "# architecture-audit — Contract version 2"
    echo
    echo "Galaxy: $( basename "$GALAXY_ROOT" )"
    echo "Tier  : $TIER"
    echo "Date  : $( date -u +%Y-%m-%dT%H:%M:%SZ )"
    echo
    echo "| Status | Invariant | Note |"
    echo "|--------|-----------|------|"
    local i
    for i in "${!INV_NAMES[@]}"; do
      printf '| %-4s | %s | %s |\n' \
        "${INV_STATUSES[$i]}" \
        "${INV_NAMES[$i]}" \
        "${INV_NOTES[$i]}"
    done
    echo
    echo "## Details on FAIL"
    echo
    local any=0
    for i in "${!INV_NAMES[@]}"; do
      if [[ "${INV_STATUSES[$i]}" == "FAIL" && -n "${INV_DETAILS[$i]}" ]]; then
        any=1
        echo "### ${INV_NAMES[$i]}"
        echo
        echo '```'
        echo "FAIL ${INV_NAMES[$i]}: ${INV_NOTES[$i]}"
        echo "  ${INV_DETAILS[$i]}"
        echo '```'
        echo
      fi
    done
    if [[ $any -eq 0 ]]; then echo "_(none)_"; fi
  } >"$out"
}

# ---------------------------------------------------------------------------
# Exit policy
# ---------------------------------------------------------------------------
case "$MODE" in
  check)
    emit_table /dev/stdout
    for i in "${!INV_STATUSES[@]}"; do
      [[ "${INV_STATUSES[$i]}" == "FAIL" ]] && exit 1
    done
    exit 0
    ;;
  report)
    emit_table "$REPORT_PATH"
    exit 0
    ;;
esac
