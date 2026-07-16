#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
# sovereignty-gate.sh — leak scan over the customer-traveling
# dist/avatar-tenant-demo/ bundle. Runs in three places, on the SAME byte-set:
#   • build.sh   — BEFORE staging/baking (the leak never reaches an image);
#   • handoff.sh — BEFORE the git archive leaves the building (last door);
#   • ci.yml     — post-merge backstop.
#
# WHAT IT SCANS — the GIT-TRACKED byte-set, never the working tree.
# The bundle is handed to Tenant-Demo as a `git archive` (see handoff.sh), so the
# bytes that travel are exactly `git ls-files dist/avatar-tenant-demo/`. What the
# gate sees is what ships. Nothing else can.
#
# THE SPEC IS NOT OURS TO WRITE. The class model, the reference motifs, and the
# adversarial test tokens below are TRANSCRIBED from scripts/sovereignty-spec.md,
# which is authored by the delib review panel. This gate is an IMPLEMENTATION
# of that spec, not its author — the failure mode of every prior round was "a
# gate authored by the fix it certifies trails the leak surface by one class"
# (buterin's round-3 prophecy). The panel writes the spec; the gate obeys it.
# Change the spec first, then this file.
#
# THREE MECHANISMS, by the shape of the leak (full rationale: the spec):
#
#   1. DENY-CLASS (hard, exit 1) — SHAPED classes and KNOWN-identity literals
#      (molecule ids, worktree names, operator paths, emails, dates, IPv4,
#      other-avatar ids, known private proper nouns). Each carries a canary
#      the self-test proves it catches.
#
#   2. RESOLVABILITY (hard, exit 1) — the GENERATIVE rule (spec §R). Instead
#      of enumerating what is forbidden, it states what is REQUIRED: every
#      path-like or citation-like reference in the tracked bundle MUST RESOLVE
#      — to a bundle file, a public URL, a public-standard citation, or a path
#      wholly inside a mount the bundle itself declares (§R9). Anything else is
#      a dangling pointer into a corpus the customer cannot reach → DENY. The
#      next lexical shape of the leak fails to resolve by construction.
#      NOTE the honest residual (spec §R8 S2): a BARE identifier with no
#      path/citation shape (`image_init`) is caught by NO mechanical net — the
#      WARN tokenizer splits it into its already-admitted words. The human
#      editorial pass is its only cover.
#
#   3. WARN (soft, never fails the build) — UNKNOWN vocabulary. A proper noun
#      has no fixed shape; `sovereignty-allowlist.txt` freezes the bundle's
#      current vocabulary, and any tracked token (length >= 4) absent from it
#      is surfaced for a human. Advisory by design: the human reading it is
#      the mechanism.
#
# This script deliberately lives in scripts/ (repo-side), NOT in the bundle:
# the class model + the allow-list + the spec are orchestration knowledge that
# must not ship — and the bundle that survives the gate says nothing about it.
#
# Usage:
#   ./scripts/sovereignty-gate.sh                 # scan; exit 0 clean, 1 DENY, 2 broken
#   ./scripts/sovereignty-gate.sh --update-allowlist   # re-freeze the vocabulary
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail
shopt -s extglob   # the resolver's punctuation-trim uses extglob +(...) patterns

# Portability (round-10, karpathy): off a TTY, detach stdin so no descendant
# command can ever block the gate on a stray read. The gate scans FILES; it
# consumes nothing from stdin, so this is a pure no-op for its logic — but it
# turns a non-interactive `cs`/CI invocation with an open-but-idle stdin (the
# shape karpathy saw hang >100 s) into an immediate EOF. Interactive runs keep
# their stdin so `--update-allowlist`-style prompts (if ever added) still work.
[ -t 0 ] || exec </dev/null

REPO_ROOT="$(git -C "$(dirname "${BASH_SOURCE[0]}")" rev-parse --show-toplevel)"
BUNDLE_PREFIX="dist/avatar-tenant-demo"
BUNDLE="${REPO_ROOT}/${BUNDLE_PREFIX}"
ALLOWLIST="${REPO_ROOT}/scripts/sovereignty-allowlist.txt"

[ -d "$BUNDLE" ] || { echo "sovereignty-gate: ${BUNDLE} not found"; exit 2; }

# The tracked byte-set that actually travels (git archive == git ls-files).
mapfile -t TRACKED < <(git -C "$REPO_ROOT" ls-files "$BUNDLE_PREFIX")
if [ "${#TRACKED[@]}" -eq 0 ]; then
  echo "sovereignty-gate: no git-tracked files under ${BUNDLE_PREFIX} (nothing would ship)"; exit 2
fi
# Absolute paths for grep.
TRACKED_ABS=()
for rel in "${TRACKED[@]}"; do TRACKED_ABS+=("${REPO_ROOT}/${rel}"); done

# ── vocabulary token extraction (shared by WARN scan + --update-allowlist) ────
# Lowercase alphabetic runs of length >= 4.
extract_tokens() {
  grep -hoE '[A-Za-z]{4,}' "${TRACKED_ABS[@]}" 2>/dev/null \
    | tr '[:upper:]' '[:lower:]' | sort -u
}

# ── --update-allowlist: re-freeze the vocabulary (deliberate operator gesture) ─
if [ "${1:-}" = "--update-allowlist" ]; then
  {
    sed -n '1,/^# ---$/p' "$ALLOWLIST" 2>/dev/null || {
      echo "# sovereignty-allowlist.txt — frozen vocabulary of what MAY travel."
      echo "# ---"
    }
    extract_tokens
  } > "${ALLOWLIST}.new"
  mv "${ALLOWLIST}.new" "$ALLOWLIST"
  echo "sovereignty-gate: re-froze allow-list ($(grep -cvE '^#|^$' "$ALLOWLIST") tokens) → ${ALLOWLIST#"$REPO_ROOT"/}"
  echo "   review the git diff — it is the record of what vocabulary was admitted to travel."
  exit 0
fi

# ─────────────────────────────────────────────────────────────────────────────
# MECHANISM 2 — RESOLVABILITY  (generative rule, spec §R)
# ─────────────────────────────────────────────────────────────────────────────
# A "reference" is any path-like or citation-like token matching one of the
# motifs the spec enumerates. Each reference RESOLVES iff it points at something
# the CUSTOMER can reach:
#   (u) it lives inside an http(s):// URL, or
#   (b) its basename (allowing a trailing `.example` on the bundle side) is a
#       git-tracked bundle file, or
#   (p) it is a public-standard citation (RFC/CVE/SPDX/license id/…).
# Anything else is a dangling pointer into cosmon's private corpus → DENY.
#
# Bundle basenames (with the `.example`-stripped alias so a reference to the
# concrete file a `*.example` templates resolves too) and bundle top-level names
# (the recipe's own directory roots — a path under one of them is self-reference).
declare -A BUNDLE_BN BUNDLE_TOP
for rel in "${TRACKED[@]}"; do
  __b="$(basename -- "$rel")"
  BUNDLE_BN["$__b"]=1
  [ "${__b%.example}" != "$__b" ] && BUNDLE_BN["${__b%.example}"]=1
  __t="${rel#"$BUNDLE_PREFIX"/}"; __t="${__t%%/*}"
  BUNDLE_TOP["$__t"]=1
done
unset __b __t

# Public-standard citation prefixes. Allow-listing PUBLIC corpora is finite and
# non-leaking — it does NOT reintroduce the internal-name enumeration trap,
# because the trap is enumerating what is PRIVATE, not what is universally
# public. A dashed code whose prefix is one of these resolves by definition.
RESOLV_PUBLIC_PREFIX='RFC|CVE|CWE|ISO|IEC|IEEE|SPDX|ECMA|PEP|NIST|FIPS|AGPL|LGPL|GPL|MIT|BSD|MPL|EPL|Apache|CC|OCI|POSIX|Unicode|UTF|SHA|MD|CRC|OAuth|HTTP|TLS|JWT|OIDC'

# Documented public / reader-reachable HOSTS (round-13 §R10.host, delib-20260707-3b7e).
# The dotted-hostname referent (§R.resolve c) is a small, FINITE, panel-owned
# enumeration — NOT the heuristic "any label with a dot is a URL". A host-shaped
# token resolves iff its host segment (everything before the first `/`, case-folded)
# is a member of this space-padded set. This closes round-13's Door B: a bare dot in
# a segment is never enough, so `vault.tenant-demo-internal/master-key`, `internal.corp/…`,
# `whispers.backup/…`, `noyau-vault.io/…` fall through to DENY, while the two hosts
# the 19-file bundle legitimately cites still resolve. (http(s):// URLs are stripped
# by the scanner, so an explicit scheme never survives to a token — the whitelist is
# the operative test. A NEW host is added to spec §R10.host FIRST, then here.)
RESOLV_PUBLIC_HOST=' codeberg.org registry.vendor.tenant-demo.io '

# Declared mounts — the runtime filesystem the READER can resolve (spec §R9).
# Parsed from the bundle's OWN volumes-*.csv `container_path` column, which
# `validate-local.sh`'s `volume_parity_gate` proves 1:1 with the compose.yml
# mounts (fail-closed) — so a fabricated CSV row cannot silently widen this
# accept-set without failing validation (round-10 B2, buterin); tmpfs
# rows excluded — generic scratch is not a reader-verifiable location. A path
# with a directory component resolves only if the WHOLE path sits under one of
# these. This replaces round-8's head-only runtime-dir list: the lead component
# is never trusted again (round-9 R1 — a one-segment `/tmp/` prefix defeated the
# gate's own S1 canary; the accept-referent is now a customer-facing contract
# artifact, not a gate-internal vocabulary).
declare -A DECLARED_MOUNTS
while IFS=, read -r __mp __mt; do
  case "$__mt" in
    *volume*|*bind*) [ -n "$__mp" ] && DECLARED_MOUNTS["${__mp#/}"]=1 ;;
  esac
done < <(awk -F, 'FNR>1 && NF>1 {print $1","$2}' "${BUNDLE}"/volumes-*.csv 2>/dev/null)
unset __mp __mt

# resolves_mount <normalized-path> → 0 iff the ENTIRE path sits under a
# declared mount (exact or proper subpath). A `..` segment escapes the mount
# lexically, so it never mount-resolves.
resolves_mount() {
  local p="$1" m
  case "/$p/" in */../*) return 1 ;; esac
  for m in "${!DECLARED_MOUNTS[@]}"; do
    case "$p" in "$m"|"$m"/*) return 0 ;; esac
  done
  return 1
}

# Source-code extensions — cosmon's OWN tree. A file with one of these never
# legitimately appears in a container recipe unless it is a bundle file.
RESOLV_SOURCE_EXTS=' rs go py rb java c cc cpp cxx h hh hpp hxx kt swift ex exs erl hs ml mli scala clj cljs jl php pl lua m mm dart '
# Secret-material extensions (round-10 B1, turing Form C). A container RECIPE
# never legitimately carries a private key, cert, keystore, or an on-disk DB as
# a path reference — these are precisely the highest-value leak payloads. Any
# path with one of these (that is not itself a shipped bundle file) is a
# dangling pointer into secret material → DENY, whatever its directory prefix.
# This is a PUBLIC shape enumeration (key/cert/DB file kinds are universal), not
# a private-name enumeration, so it does not reintroduce the enumeration trap.
# round-11 (turing T2b): the round-10 list missed PuTTY keys (.ppk), OpenVPN
# profiles (.ovpn), and bare JWK material (.jwk) — three more UNIVERSAL secret
# file kinds a recipe never legitimately cites. Added here, not as a new
# mechanism: the same closed extension predicate, widened to the shapes turing's
# re-execution surfaced (`/opt/keys/tenant.ppk`, `/opt/creds/vpn.ovpn` were `ok`).
RESOLV_SECRET_EXTS=' pem key p12 pfx crt cer der keystore jks p8 pk8 asc gpg sqlite sqlite3 kdbx ppk ovpn jwk '

# Secret-material BASENAMES (round-11, turing T2b). Some key material carries NO
# extension at all — the SSH default private keys (`id_rsa`, `id_ed25519`, …) are
# the canonical case turing surfaced (`id_ed25519 → ok` with no WARN backstop).
# A recipe never legitimately cites one by name; matched on the LAST path segment
# so both the bare `id_ed25519` and `/opt/x/id_ed25519` are denied. This is the
# extensionless sibling of RESOLV_SECRET_EXTS — a PUBLIC-shape enumeration (these
# filenames are universal), not a private-name blacklist.
RESOLV_SECRET_BASENAMES=' id_rsa id_dsa id_ecdsa id_ed25519 '

# ── DEFENSE-IN-DEPTH (round-12, delib-20260707-8eca): the private-corpus motif,
# the secret-material extensions, and the secret basenames below are NO LONGER the
# primary mechanism. The primary mechanism is the DENY-BY-DEFAULT positive
# predicate (`resolves_path_allow`, spec §R10): a path with a directory component
# resolves ONLY if it points POSITIVELY at a referent the customer can reach
# (bundle file · declared mount · public URL/hostname · a whole-path entry in the
# enumerated positive ALLOW). Anything unreferenced DENYs by construction, so a
# private secret resolves positively nowhere → deny WITHOUT needing to be named in
# any denial list. These lists are kept only as EARLY, cheap, belt-and-suspenders
# denials (they can only ADD a deny, never grant an accept); they are redundant
# with the positive predicate and must never again be treated as the closure.
# ─────────────────────────────────────────────────────────────────────────────
# Cosmon-private-corpus DIRECTORY motifs — early deny for the shaped private names
# (whisper inbox, noyau vault, nucleon bindings, bare secret/private dir). Matched
# at any segment/segment-prefix; a bare single-segment mention (`/nucleons/`) is
# handled by the single-segment rule before this runs, so `/nucleons/|ok` holds.
RESOLV_PRIVATE_MOTIF='whispers|noyau|noyau-vault|nucleon|nucleons|vault|secret|secrets|private'

# POSITIVE whole-path ALLOW (round-12, delib-20260707-8eca §I2). The sense of the
# enumeration is INVERTED: instead of listing the (infinite) forbidden private
# names minus a whitelist of trusted leads, we list the SMALL, FINITE set of
# authorised referents — the exact OS/build/URL/prose paths the 19-file bundle
# legitimately ships. A path-with-directory resolves iff it matches one of these
# entries EXACTLY, or sits under one of the anchored ≥2-segment universal-public
# trees. The lead segment is NEVER trusted alone: `noogram/llama-server` resolves
# but `noogram/client-roster` does not; `linux/amd64` resolves but
# `linux/amd64.noyau-dump` does not; `state/galaxies` resolves but
# `state/tenant-secrets` does not. A NEW legitimate reference is absent from this
# set → DENY, surfaced for human review, exactly as the WARN vocabulary freeze and
# the declared-mount CSV work. The set is externalised to `sovereignty-spec.md`
# §R10 (buterin governance: the accept-referent is a panel-owned contract, not
# "whatever the gate author froze in"); this `case` transcribes it.
#
# resolves_path_allow <normalized-lowercased-path> → 0 iff the WHOLE path is an
# authorised referent, matched EXACTLY. Pure-builtin `case` (no forks). Every entry
# is a complete normalized path the 19-file bundle actually ships — NO open prefix,
# NO anchored `x/*` tree: a prefix would re-admit an arbitrary tail
# (`usr/local/bin/exfil-tenant-db`) and a `..`-traversal (`cosmon/.cosmon/../opt/x`),
# which is the exact hole the lead-whitelists had. Exact match is the closure: a
# path one character different from an authorised referent DENYs, so no invented
# private form can ride an authorised one. The set is FINITE and small by design;
# a NEW legitimate reference is added here (and to spec §R10) with a one-line
# justification, exactly as the WARN vocabulary freeze admits a new word. Bundle
# files resolve earlier by basename (BUNDLE_BN), so binary/config paths whose
# leaf is a shipped file (`.../entrypoint.sh`, `.../provision.sh`) never reach here.
resolves_path_allow() {
  case "$1" in
    # ── prose word-pairs (protocol / platform / stream / status / doc tokens) ──
    401/403|erofs/eacces|http/https|above/below|application/json) return 0 ;;
    docker/dockerfile|fonts/cdn|idp-ext/idp-int|linux/amd64|linux/arm64) return 0 ;;
    models/user|reboot/redeploy|reserved/invalid|sign-up/creation) return 0 ;;
    stdout/stderr|suites/legs|uid/gid|unreachable/healthz-blind) return 0 ;;
    volume/tmpfs|volumes/tmpfs|target/release|cosmon-issuer-handoff/v1) return 0 ;;
    state/galaxies|.example/git) return 0 ;;
    # ── documented filesystem / prose referents (reader-resolvable) ──
    tenant-demo/cosmon-server|.cosmon-provision/admin-pass) return 0 ;;
    internal/underivable|internal/request-derived|credentials/iam) return 0 ;;
    handoff/forgejo-issuer.toml) return 0 ;;
    # ── sanctioned maker image path (maker = Noogram) ──
    noogram/llama-server) return 0 ;;
    # ── URL path segments (post URL-strip; public API / OIDC endpoints) ──
    api/healthz|api/v1/user|api/v1/user/applications/oauth2) return 0 ;;
    v1/auth/me|v1/molecules|login/oauth/keys|.well-known/openid-configuration) return 0 ;;
    # ── OS device nodes (universal, finite) ──
    dev/null|dev/zero|dev/full|dev/random|dev/urandom) return 0 ;;
    dev/stdin|dev/stdout|dev/stderr|dev/tty|dev/console) return 0 ;;
    # ── specific OS / container binary paths the recipe cites (leaf ≠ bundle file) ──
    bin/sh|usr/bin/env|usr/bin/dumb-init|usr/sbin/nologin) return 0 ;;
    usr/local/bin|usr/local/bin/cs|usr/local/bin/cs-oidc-mock) return 0 ;;
    usr/local/bin/cs-rpp-adapter|usr/local/bin/cosmon-rpp-adapter) return 0 ;;
    kernel-bin/cs|kernel-bin/cs-oidc-mock|kernel-bin/cosmon-rpp-adapter) return 0 ;;
    # ── package cache the recipe cites (exact dir, not an open tree) ──
    var/lib/apt/lists) return 0 ;;
    # ── recipe HOME / build / state / URL dirs the recipe cites (exact) ──
    cosmon/.config|cosmon/.config/cosmon|cosmon/.cosmon) return 0 ;;
    build/cosmon|.build/kernel-src|.cosmon/state|tmp/gitea) return 0 ;;
    # ── round-13 §R13: script-syntax fragments the bundle's OWN validation /
    # provision scripts embed. NOT filesystem paths — sed programs (`s/^`, `\1/p`
    # from validate-local.sh) and a curl URL-path with a query string
    # (`api/v1/users/search?limit`, from provision.sh). Round-12's Door A blanket-
    # accepted any token carrying a non-path char as "prose"; that heuristic is
    # DELETED (it also waved `~/tenant-demo-secrets/cap-table` through). These three are
    # instead enumerated EXACTLY here, so class-closure holds: a generative private
    # form dressed with an exotic char matches none of them and DENYs at :457.
    's/^'|'\1/p'|'api/v1/users/search?limit') return 0 ;;
    # ── §R15: two more forms of the ALREADY-sanctioned (line 265) public
    # Forgejo OAuth2-apps endpoint, embedded by provision.sh's idempotent
    # reuse+prune path (list with a query, delete by id). Same class as the
    # bare form and `users/search?limit` above: exact literals, not patterns,
    # so a generative private form still DENYs at :457. The `alt/state` arm is
    # validate-local.sh's OWN throwaway alt-volume mount for the env>rpp.toml
    # precedence proof (P7c) — a test fixture, not a corpus pointer.
    'api/v1/user/applications/oauth2?limit'|'api/v1/user/applications/oauth2/$'|alt/state) return 0 ;;
  esac
  return 1
}

# ── round-14 (delib-20260708-d5a4): close the TWO residual bare-token arms ─────
# Round-13 inverted the HAS-DIRECTORY arm to deny-by-default (§R10). But two
# sibling terminals — the bare single-segment DIR mention (`nucleons/`) and the
# bare FILENAME arm (`app.ini`) — still returned a blanket `RTV=ok`, so every
# unenumerated private form with no directory component passed: `cap-table.xlsx`,
# `credentials.json`, `tenant-secrets.env`, `master.age`, `id_ed25519.bak` (bare
# files) and `client-roster/`, `tenant-secrets/`, `vault/` (bare dirs) all → ok,
# the bare-dir case FULLY SILENT (its word-parts pre-frozen in the WARN list). The
# fix mirrors §R10: a bare token resolves `ok` ONLY IF it is adossé to a POSITIVE
# referent — a numeric-dotted VERSION/IP shape (which can encode no private name),
# a declared mount / §R10 path referent, a shipped bundle file (BUNDLE_BN, resolved
# earlier), or an EXACT §R10.bare enumerated referent. Everything else → deny.
# After this, EVERY `RTV=ok` site of the resolver is referent-backed; the only
# unmatched fall-through in EITHER arm is `deny`. (spec §R14)

# is_version_shape <token> → 0 iff the token is a pure numeric-dotted VERSION,
# build-tag, or IPv4/bind-address literal: optional `v`, ≥2 dot-separated numeric
# groups, optional `-alnum` build suffixes. A private-corpus name never has an
# all-numeric dotted core, so this shape is a POSITIVE, non-leaking referent (it
# subsumes `v3.0`, `v3.0-amd64`, `2.5.0`, `1.88-bookworm`, `0.0.0.0`, `127.0.0.1`
# — none of which is a filesystem reference). NOT an open prose exemption: a token
# with a single letter out of place (`cap-table.xlsx`, `master.age`) fails it.
is_version_shape() {
  # First numeric group capped at 3 digits: real versions/IPs never exceed it
  # (`3.0`, `2.5.0`, `1.88`, `127.0.0.1`), but a 4-digit-year date-dotted token
  # (`2026.07-roster`) is excluded — so a private name shaped like a version DENYs.
  [[ $1 =~ ^v?[0-9]{1,3}(\.[0-9]+)+(-[A-Za-z0-9]+)*$ ]]
}

# resolves_bare_allow <case-folded-bare-token> → 0 iff the WHOLE single-segment
# token is an EXACT authorised BARE referent (spec §R10.bare). Single-segment
# siblings of §R10.allow: the finite legit non-bundle filenames, the OS/build/state
# DIR names the recipe cites bare, the config-key / template / jq accessors the
# scripts embed (dotted but NOT filesystem paths), and the bare script-syntax
# fragments. EXACT match, no open prefix — a token one character different DENYs, so
# no invented private form (`cap-table`, `tenant-secrets`, `master.age`) can ride an
# authorised one. Panel-owned: a NEW bare referent is added to spec §R10.bare FIRST,
# then here (the spec↔gate parity self-test fails closed otherwise).
resolves_bare_allow() {
  case "$1" in
    # ── legit non-bundle FILENAMES (generic, public-shape; bundle files resolve
    #    earlier via BUNDLE_BN and never reach here) ──
    cargo.lock|app.ini|forgejo-issuer.toml|state.json|err.log|nuc.log) return 0 ;;
    docker-entrypoint.sh|docker-setup.sh) return 0 ;;
    # ── single-segment OS / build / state / container DIR names cited bare ──
    tmp|cosmon|handoff|build|forgejo|kernel-bin|git|admin-pass|healthz) return 0 ;;
    nucleons|.build|.cosmon-provision|.git|volumes-) return 0 ;;
    # ── config-key / TOML-section / git-config / template / jq accessors: dotted,
    #    but a KEY, not a filesystem path (their tail is no file extension) ──
    org.opencontainers.image.title|org.opencontainers.image.authors) return 0 ;;
    org.opencontainers.image.url|org.opencontainers.image.licenses) return 0 ;;
    org.opencontainers.image.version|org.opencontainers.image.revision) return 0 ;;
    org.opencontainers.image.description) return 0 ;;
    sibling.llama|sibling.forgejo|logging.driver|user.email|user.name) return 0 ;;
    trust_bootstrap.issuer) return 0 ;;
    .state.health.status|.server.arch|.architecture) return 0 ;;
    .client_id|.id|.name|.iss|.issuer|.is_admin|.data|.d|.so|.csv|.toml) return 0 ;;
    .example|.build-markers|.val-seed-|.gitignore|.env) return 0 ;;
    # ── noreply / test-tenant local email literals the provision scripts embed
    #    (`.local`/`.localhost` are never public TLDs → not caught by the email
    #    DENY-class; enumerated here as the exact non-leaking literals) ──
    @noreply.localhost|cosmon@tenant-alpha.local) return 0 ;;
    # ── bare script-syntax fragments (sed address patterns / shell param-expansion
    #    residue the bundle's own validation scripts embed; NOT references) ──
    '^services'|'^volumes'|'^-'|'ext_root_url%'|'p'|'3'|'s'|'e.g'|'i.e') return 0 ;;
    '.tmp.$$') return 0 ;;
  esac
  return 1
}

# resolves_bundle <token> → 0 if its basename is a tracked bundle file.
resolves_bundle() {
  local base; base="$(basename -- "$1" 2>/dev/null)"
  [ -n "${BUNDLE_BN[$base]:-}" ]
}

# resolv_token_verdict <token> → prints "deny" or "ok" for ONE candidate
# reference. This is the whole generative rule in one place: a reference is a
# leak iff it resolves to NEITHER a bundle file, NOR a public URL (URLs are
# pre-stripped by the scanner), NOR a public-standard citation, NOR a path
# wholly inside a mount the bundle itself declares (§R9). Both the scanner and
# the self-test call it, so the test exercises the exact code the scan runs.
# resolv_token_verdict sets the GLOBAL `RTV` (not stdout) so the scanner can call
# it in the MAIN shell and MEMOIZE — echoing into a `$(…)` command substitution
# would fork a subshell per call and throw the cache away. Over one scan the same
# token recurs heavily (`/dev/null` 73×, `/api/healthz` 21×); the cache collapses
# ~1200 calls to ~460 computations, and `_rtv_impl` uses pure bash builtins (no
# `grep`/`sed`/`basename`/`tr` forks), taking the whole scan from ~30 s to <2 s.
# This is the karpathy portability fix: a fork-light gate cannot appear to "hang"
# under load, and (with the stdin guard at the top) never blocks on a stray read.
declare -A RESOLV_MEMO
RTV=""
resolv_token_verdict() {
  if [ -n "${RESOLV_MEMO[$1]+x}" ]; then RTV="${RESOLV_MEMO[$1]}"; return; fi
  _rtv_impl "$1"
  RESOLV_MEMO[$1]="$RTV"
}

# _rtv_impl <token> → sets RTV to "deny" or "ok". Pure-builtin translation of the
# generative rule; the resolvability self-test is the executable proof that this
# translation is faithful to the sed/grep original it replaced.
_rtv_impl() {
  local tok="$1"
  # ── parenthesized API symbol `(CamelCase)` — check BEFORE trimming parens ────
  [[ $tok =~ ^\([A-Z][a-z]+[A-Z][A-Za-z0-9]*\)$ ]] && { RTV=deny; return; }
  # trim surrounding punctuation / quoting / markdown backticks / shebang / flag
  # (extglob equivalent of the two sed substitutions — the `#!` alternative is
  # subsumed because `#` and `!` are already in the leading class; verified
  # char-for-char against the sed across the delimiter set).
  tok="${tok#"${tok%%[![:space:]]*}"}"
  tok="${tok##+([][(\"\'\`<>,;#!-])}"
  tok="${tok%%+([][)\"\'\`<>,;:.])}"
  [ -z "$tok" ] && { RTV=ok; return; }

  # ── symbol citation: `struct/enum/trait/fn Name` (needs the keyword IN tok) ──
  [[ $tok =~ (struct|enum|trait|impl|mod|fn)[[:space:]]+[A-Z][A-Za-z0-9_]+ ]] && { RTV=deny; return; }
  # ── tracker / design-doc citation: ADR-n, §n, bead codes word-B2/word-M3 ────
  [[ $tok =~ (^|[^A-Za-z])(ADR-[0-9]+|§[0-9]+|[a-z][a-z]+-[MB][0-9]+) ]] && { RTV=deny; return; }
  # ── UPPER-CASE dashed code (FOO-12): public-standard prefix resolves, else DENY
  if [[ $tok =~ ^[A-Z]{2,}-[0-9] ]]; then
    local pfx="${tok%%-*}"
    if [[ $pfx =~ ^(${RESOLV_PUBLIC_PREFIX})$ ]]; then RTV=ok; else RTV=deny; fi
    return
  fi

  # ── secret-material BASENAME (round-11, turing T2b): extensionless key material
  # (`id_ed25519`, `id_rsa`, …). Checked on the LAST path segment BEFORE the
  # path-shape bail below, so a BARE `id_ed25519` (no '/', no '.') — which would
  # otherwise fall through to the allow-by-default identifier rule with no WARN
  # backstop — is denied, as is `/opt/x/id_ed25519`. Trailing slashes are stripped
  # first so a bare dir mention (`/nucleons/`) yields a real segment, not the empty
  # string (which would spuriously match the space-padded set).
  local _seg="${tok%%+(/)}"; _seg="${_seg##*/}"
  [ -n "$_seg" ] && case " $RESOLV_SECRET_BASENAMES " in *" $_seg "*) RTV=deny; return ;; esac

  # ── path / filename references ──────────────────────────────────────────────
  # Only tokens that look like a path or a file (contain '/' or a '.ext'). A
  # BARE identifier (no '/', no '.') is not a shaped reference (spec §R8 S2 —
  # see the header note on the honest residual).
  case "$tok" in
    */*|*.*) : ;;
    *) RTV=ok; return ;;
  esac
  # ── D1: absolute paths are NOT blanket-accepted — the leading `/` is stripped
  # below so absolute and relative flow through the SAME whole-path resolution.
  # (Operator `/Users/…` is also caught by its own DENY-class.)
  # A repo-relative source-tree path is always a leak.
  case "$tok" in crates/*|*/crates/*) RTV=deny; return ;; esac

  local base ext lead
  base="${tok%%+(/)}"; base="${base##*/}"   # basename (builtin): strip trailing
  [ -z "$base" ] && base="/"                # slashes, take last component; all-
                                            # slashes (`/`, `//`) → "/" like basename
  # round-14: a token carrying NO alphanumeric character at all (`/`, `//`, `/^`,
  # `$/`, `/#`) is sed/shell-syntax residue the word-split emits — it names
  # nothing, so it can carry no private-corpus reference. Resolve `ok` (no leak
  # surface). This is NOT the deleted Door A: Door A waved through slashed tokens
  # that DID carry an alphanumeric private tail (`~/tenant-demo-secrets/cap-table`); this
  # fires ONLY when the whole token is punctuation, so no private name can ride it.
  [[ $tok =~ [A-Za-z0-9] ]] || { RTV=ok; return; }

  # A component with no dot has no extension (a MIME type `application/json` or
  # a bare dir `target/release` is NOT a file citation).
  if [[ $base == *.* ]]; then
    ext="${base##*.}"; ext="${ext,,}"
  else
    ext=""
  fi
  # Resolves to a shipped bundle file (by basename, .example-aware).
  [ -n "${BUNDLE_BN[$base]:-}" ] && { RTV=ok; return; }

  # A cosmon source-code file that is NOT in the bundle → dangling source citation.
  if [ -n "$ext" ]; then case "$RESOLV_SOURCE_EXTS" in *" $ext "*) RTV=deny; return ;; esac; fi
  # Secret material (round-10 B1, turing Form C): a key/cert/keystore/DB path that
  # is not a shipped bundle file is a dangling pointer into secret material, no
  # matter its prefix or whether it carries a directory component. Denied here so
  # the check covers both `secret.pem` and `/tmp/…/secret.pem` (the ..-escape that
  # the round-9 mount-resolver let through — Form B collapses into Form C).
  if [ -n "$ext" ]; then case "$RESOLV_SECRET_EXTS" in *" $ext "*) RTV=deny; return ;; esac; fi

  # Normalise: strip a leading `/` (absolute) and `./` / `../` (relative) so an
  # absolute path flows through the SAME dir-resolution as a relative one (D1 —
  # no blanket accept). `norm` still holding a `/` means a real directory
  # component; otherwise it is a single filename (a file at a mount root, e.g.
  # `/forgejo-issuer.toml`) and goes to the bare-filename rule.
  local norm; norm="$tok"
  # Collapse ALL leading slashes (round-11, turing T2c): `//host/whispers-backup`
  # was a degenerate empty-lead path that hit the blanket `[ -z "$lead" ] → ok`.
  # Normalising `//+` → `` folds it into `host/whispers-backup`, so the motif net
  # and the closed lead-terminal see a real lead — the double-slash bypass is gone.
  norm="${norm##+(/)}"; norm="${norm#./}"; norm="${norm#../}"
  case "$norm" in
    */*)
      # A path with a directory component resolves only as a WHOLE (spec §R9):
      #   • its lead is the recipe's own dir (self-reference into the bundle), or
      #   • the ENTIRE path sits under a mount the bundle's own volumes-*.csv
      #     declares (reader-resolvable by construction).
      # The lead component ALONE is never consulted — round-8's head-only rule
      # let a one-segment prefix (`/tmp/…`) smuggle any private tail past the
      # gate, defeating its own S1 canary (round-9 R1, turing).
      lead="${norm%%/*}"
      [ -n "$lead" ] && [ -n "${BUNDLE_TOP[$lead]:-}" ] && { RTV=ok; return; }
      # A doc file is a CITATION, never a mount payload: it resolves in-bundle,
      # by URL, or not at all (source-code exts are already denied above).
      [ "$ext" = md ] && { RTV=deny; return; }
      # Mount-rooted paths resolve FIRST — a legitimate `…/state/nucleons/…` under
      # a declared volume must pass before the private-motif net below sees it.
      resolves_mount "$norm" && { RTV=ok; return; }
      # round-14 (delib-20260708-d5a4): the bare single-segment DIR short-circuit
      # that returned `ok` HERE — BEFORE the motif net — is DELETED. It fired on
      # `nucleons/` (legit) but ALSO on `client-roster/`, `tenant-secrets/`,
      # `vault/`, `secrets/` (private, fully silent — their word-parts pre-frozen in
      # the WARN list). A bare dir mention now flows through the SAME deny-by-default
      # terminal as every other path: it resolves `ok` ONLY via a positive referent
      # (§R10.allow / §R10.bare, checked below), else DENY. `nucleons/` survives via
      # its exact §R10.bare entry; the private forms resolve nowhere → deny.
      # ── round-10 B1 + round-11 (turing T2a / torvalds T3): private-corpus
      # citation. A path whose directory chain passes through a cosmon-private
      # motif, and that did NOT mount-resolve above, points into a corpus the
      # reader cannot reach. Two shapes DENY (case-folded, `-i` semantics):
      #   (a) the motif is the LEAD segment WITH a real subpath (`whispers/inbox`);
      #   (b) the motif is a NON-lead segment or segment-PREFIX anywhere, terminal
      #       included (`/opt/whispers`, `/etc/noyau-vault`, `/tenant-demo/whispers-dump`,
      #       `/opt/x/private-notes`) — round-10's `(motif)/.+` required a subpath
      #       and so missed the bare leaf and the punctuation-fused tail (turing T2a,
      #       torvalds T3). The bare single-segment mention was already returned ok
      #       above, so `/nucleons/` is unaffected.
      if [[ ${norm,,} =~ ^(${RESOLV_PRIVATE_MOTIF})/.+ ]] || \
         [[ ${norm,,} =~ /(${RESOLV_PRIVATE_MOTIF})([-._/]|$) ]]; then
        RTV=deny; return
      fi
      # ── round-12 (delib-20260707-8eca §I1/§I2): DENY-BY-DEFAULT positive
      # terminal. The absolute and relative arms are UNIFIED: after normalisation a
      # path with a directory component flows through ONE predicate, regardless of
      # whether it began with `/`. The two lead-segment whitelists that used to
      # short-circuit to `ok` on a trusted LEAD before ever consulting the tail —
      # `RESOLV_ABS_ROOTS` (the whole OS tree) and `RESOLV_REL_ROOTS` — are DELETED.
      # A path now resolves ONLY if it points POSITIVELY at an authorised referent;
      # anything else DENYs. This closes the CLASS, not one more shape: an invented
      # private name resolves positively NOWHERE (it is not a bundle file, not under
      # a declared mount, not a public URL, not a whole-path entry in the enumerated
      # ALLOW), so `/opt/client-financials`, `state/tenant-secrets`,
      # `noogram/client-roster`, `/opt/keys/master.age` are denied by construction —
      # no denial list has to name them. (The former `was_abs` split is gone: absolute
      # and relative paths take the identical predicate now.)
      # A `..` segment lexically escapes any referent — it never resolves positively
      # (mirrors resolves_mount's own `..` rejection). Deny before the accept surface
      # so no traversal (`cosmon/.cosmon/../../opt/loot`) can dress a private tail.
      case "/$norm/" in */../*) RTV=deny; return ;; esac
      # (i) round-13 §R13 Door A — the non-path-char "prose exemption" is DELETED.
      #     Once a token has a path separator (`/`) it IS a path and MUST resolve to
      #     a positive referent or DENY: the old `*[!A-Za-z0-9._/-]* → ok` blanket
      #     waved `~/tenant-demo-secrets/cap-table` (private tail, only non-path char `~`)
      #     straight through, and any exotic-char dressing (`~ % # &`) escaped the
      #     same way — an open class. The three genuine sed/regex/query fragments the
      #     bundle's own scripts embed (`s/^`, `\1/p`, `api/v1/users/search?limit`)
      #     are enumerated EXACTLY in resolves_path_allow (spec §R10), reached below.
      #     The slash-free prose exemption survives at the `*/*|*.*) : ;; *) ok` rule
      #     far above (a bare word is not a path — §R8 S2).
      # (ii) round-13 §R13 Door B — a dotted-HOSTNAME lead resolves ONLY IF its host
      #      segment is a DOCUMENTED public / reader-reachable domain (RESOLV_PUBLIC_HOST,
      #      spec §R10.host). The old `^[a-z0-9-]+(\.[a-z0-9-]+)+/ → ok` accepted ANY
      #      label-with-a-dot, so `vault.tenant-demo-internal/master-key`, `internal.corp/…`,
      #      `whispers.backup/…`, `noyau-vault.io/…` passed as if public. A bare dot is
      #      never enough; the host must be enumerated (`codeberg.org`,
      #      `registry.vendor.tenant-demo.io`). (http(s):// URLs are already URL-stripped by
      #      the scanner, so a scheme never survives into a token — the whitelist is the
      #      operative test.)
      local _host="${norm%%/*}"
      case " $RESOLV_PUBLIC_HOST " in *" ${_host,,} "*) RTV=ok; return ;; esac
      # (iii) the enumerated POSITIVE whole-path ALLOW (spec §R10) — EXACT referent
      #       only; the lead is never trusted, no open prefix. This single accept
      #       surface replaced the two open lead whitelists (RESOLV_ABS/REL_ROOTS).
      #       Trailing slashes are stripped so a dir mention (`/usr/local/bin/`)
      #       matches its canonical referent (`usr/local/bin`).
      local _np="${norm%%+(/)}"
      resolves_path_allow "${_np,,}" && { RTV=ok; return; }
      # round-14: a bare single-segment DIR mention (`nucleons/` → `_np=nucleons`)
      # resolves ONLY via its exact §R10.bare referent — the deleted :428 blanket is
      # replaced by this positive check. Multi-segment `_np` never matches a §R10.bare
      # entry (all single-segment), so this is safe for the whole-path case too.
      resolves_bare_allow "${_np,,}" && { RTV=ok; return; }
      # Everything else with a directory component is a dangling pointer into a
      # corpus the reader cannot reach → DENY. A NEW legitimate reference surfaces
      # here for human review, exactly as the WARN vocabulary freeze does.
      RTV=deny; return ;;
    *)
      # A BARE token with no directory component: a filename (`app.ini`), a
      # single-segment path normalised from a leading slash (`/tmp`→`tmp`), a
      # version (`v3.0`), or a dotted config/jq accessor (`sibling.llama`).
      # round-14 (delib-20260708-d5a4): DENY-BY-DEFAULT. The former blanket
      # `RTV=ok` here let every unenumerated private bare form pass —
      # `cap-table.xlsx`, `credentials.json`, `tenant-secrets.env`, `payroll.db`,
      # `master.age`, `id_ed25519.bak` all → ok. Now `ok` requires a positive
      # referent, mirroring the has-directory terminal above:
      [ "$ext" = md ] && { RTV=deny; return; }
      # An empty base (`/`, `//` → normalised to nothing) names nothing — not a
      # reference, no leak surface.
      [ -z "$base" ] && { RTV=ok; return; }
      # (a) a shipped bundle file already resolved at the BUNDLE_BN check above.
      # (b) a numeric-dotted version / build-tag / IP literal — no private name.
      is_version_shape "$base" && { RTV=ok; return; }
      # (c) an EXACT §R10.bare referent (legit filenames, OS/build dirs, config
      #     accessors, script fragments). The lead is never trusted; exact only.
      resolves_bare_allow "${base,,}" && { RTV=ok; return; }
      # Everything else — an unenumerated bare filename, dir, or dotted token — is a
      # dangling reference into a corpus the reader cannot reach → DENY. Closes the
      # round-13 residual: no invented private bare form resolves positively.
      RTV=deny; return ;;
  esac
}

# resolvability_self_test — falsifiability. The rule must DENY every historical
# leak (rounds 3/4/5) and ACCEPT every legitimate reference. Transcribed from
# scripts/sovereignty-spec.md §R.tests. A gate that cannot fail is not a gate.
#   token | expect(deny|ok)
RESOLV_SELFTEST=(
  # ── historical leaks — MUST deny ────────────────────────────────────────────
  'crates/cosmon-rpp-adapter/src/jwks_fetch.rs|deny'    # round-5 (buterin)
  'crates/cosmon-rpp-adapter/src/trust_bootstrap.rs|deny'
  'crates/cosmon-rpp-adapter/Cargo.toml|deny'
  'jwks_fetch.rs|deny'
  'image_init.rs|deny'
  'smithy/docs/guides/tenant-demo-operations.md|deny'     # round-3 (smithy/…)
  'LEAN.md|deny'
  'Operations_v1.0.md|deny'
  'struct TrustedIssuer|deny'
  '(TrustedIssuer)|deny'
  'ADR-132|deny'                                         # round-4 (buterin)
  '§6|deny'
  'auth-B2|deny'                                         # round-5 (torvalds)
  'ops-B1|deny'
  'infra-M3|deny'
  'MA-5|deny'
  # ── FORWARD canaries — SYNTHETIC shapes never seen in any prior leak, so the
  # harness can fail on a shape that has not yet appeared in the bundle (the
  # round-3..6 self-test trailed by one class because it was enumerated from
  # KNOWN leaks only — buterin/janis).
  '/noyau-vault/private/handbook.md|deny'   # non-runtime absolute path with a doc file
  'internal-runbooks/deploy.sh|deny'        # dangling repo-internal path
  # ── round-8 S1 canaries: the private-dir whitelist is GONE. A citation rooted
  # at a cosmon-private-tree directory (which round-7's RESOLV_RUNTIME_DIRS
  # false-accepted, torvalds B-infra-1 / turing B-T2) now DENYs by construction.
  'whispers/inbox/secret.json|deny'         # was `ok` in round-7 (whispers was whitelisted)
  'galaxies/speck/private-strategy.md|deny' # private galaxy root, no longer a runtime dir
  # ── round-9 R1 canaries: the lead component is never trusted. Round-8's
  # head-only rule accepted BOTH of these (turing's falsification — the second
  # defeats the S1 canary above with a one-segment /tmp/ prefix).
  '/var/whispers/inbox/secret.json|deny'    # runtime-prefixed private tail
  '/tmp/noyau-vault/private/handbook.md|deny' # prefix-dressed S1 canary
  '/cosmon/.cosmon/state/../../vault/secret.json|deny' # ..-traversal out of a declared mount
  '.claude/settings.json|deny'              # agent-substrate config (M3.1) — no declared mount
  # ── round-10 B1 canaries (turing Forms A/B/C): the allow-by-default terminal
  # is CLOSED. Round-9's `:249 echo ok` fallthrough resolved ALL THREE of these
  # `ok` (turing's falsification); each must now DENY, and the three collapse
  # into two mechanisms — secret-material extension (Form C) and private-corpus
  # motif / closed absolute terminal (Forms A/B). No shipped byte triggers any
  # of them, so this is a gate-CERTIFICATION fix (janis: LATENT), closing the
  # "private paths DENY, no exception" guarantee falsified for two rounds.
  '/var/whispers/inbox/secret|deny'         # Form A: extensionless absolute private path (was ok)
  'noyau-vault/private/handbook|deny'       # Form A: extensionless relative private path (was ok)
  '/cosmon/.cosmon/state/../../noyau-vault/secret.pem|deny' # Form B: ..-escape + unenumerated ext (was ok)
  '/opt/secrets/tenant.key|deny'            # Form C: .key under a universal root (was ok)
  'sealed/store.p12|deny'                   # Form C: .p12 (was ok)
  'data/nucleons.sqlite|deny'               # Form C: .sqlite on-disk DB (was ok)
  # ── round-11 canaries (delib-20260707-f921): the four seats re-executed the
  # round-10 gate and each landed a private `ok` verdict the report had softened.
  # Every reproduced form is wired here as a MUST-DENY so the "no exception"
  # closure is PROVEN each run, not asserted in a report (karpathy #2 verdict-door).
  # T1 — forgemaster/karpathy, the relative deploy-door: motif-free relative paths
  # whose lead is frozen in the WARN allow-list (structurally WARN-blind) — now the
  # relative terminal is a closed predicate, so they DENY.
  'internal/report|deny'                    # was ok + zero WARN (deploy-door)
  'credentials/dump|deny'                   # was ok + zero WARN
  # T2 — turing, the absolute arm.
  '/opt/whispers|deny'                      # (a) motif as a bare terminal leaf (was ok)
  '/etc/noyau-vault|deny'                   # (a) motif leaf under a universal root
  '/opt/x/private-notes|deny'               # (a) motif as a punctuation-fused segment prefix
  '/opt/keys/tenant.ppk|deny'               # (b) unenumerated secret ext .ppk (was ok, no WARN)
  '/opt/creds/vpn.ovpn|deny'                # (b) unenumerated secret ext .ovpn
  'id_ed25519|deny'                         # (b) extensionless SSH key basename (was ok, no WARN)
  '/opt/x/id_ed25519|deny'                  # (b) same, with a path prefix
  '//host/whispers-backup|deny'             # (c) double-slash empty-lead bypass (was ok)
  # T3 — torvalds, punctuation-fused private tails + dropped bundle-specific roots.
  '/tenant-demo/whispers-dump|deny'             # motif prefix fused to a tail under a dropped root
  '/tenant-demo/noyau-notes|deny'               # motif prefix
  '/.cosmon-provision/root-token|deny'      # dropped bare-lead root: a NON-documented tail DENYs
  '/.cosmon-provision/tenant-secrets.key|deny' # dropped root + secret ext
  # ── round-11 anti-over-denial canaries: the closed terminals must still resolve
  # the legitimate references the real bundle ships — the bundle's OWN prose pairs
  # (isomorphic to the T1 leaks but explicitly frozen), the documented provisioner
  # secret (torvalds ruled ADMISSIBLE — the CSV names its exact location), the
  # CloudWatch log group re-admitted as a two-segment path, and public registry
  # refs. A gate that denied these would be unusable (over-denial is symmetric).
  'internal/underivable|ok'                 # bundle prose pair (test-provision-local.sh) — frozen
  'internal/request-derived|ok'             # bundle prose pair (provision.sh) — frozen
  'credentials/IAM|ok'                      # bundle prose pair (compose.local-logs.yml) — frozen
  '/tenant-demo/cosmon-server|ok'               # CloudWatch log-group (two-segment re-admit)
  '/.cosmon-provision/admin-pass|ok'        # documented provisioner secret (torvalds: admissible)
  '.cosmon-provision/admin-pass|ok'         # relative form of the same documented path
  '/var/lib/gitea/.cosmon-provision/admin-pass|ok' # mount-rooted form (under /var/lib/gitea)
  'codeberg.org/forgejo/forgejo|ok'         # public registry ref (dotted-hostname lead)
  'linux/amd64|ok'                          # universal prose/build root
  '/nucleons/|ok'                           # bare single-segment state-dir mention (unchanged)
  # ── legitimate references — MUST resolve (ok) ───────────────────────────────
  './build.sh|ok'
  'validate-local.sh|ok'
  'forgejo/test-provision-local.sh|ok'
  'security/trusted-issuers.toml|ok'                     # resolves via .example
  'trusted-issuers.toml.example|ok'
  'rpp.toml|ok'
  '/var/lib/gitea/custom/conf/app.ini|ok'               # under declared mount /var/lib/gitea
  'target/release|ok'                                   # generic Rust build dir (relative prose)
  'Cargo.lock|ok'                                        # generic, non-private
  'forgejo-issuer.toml|ok'                              # runtime handoff filename
  'RFC-2606|ok'                                          # public standard
  'AGPL-3.0|ok'
  'noogram/llama-server|ok'                             # sanctioned maker (Noogram)
  # ── round-10 B1 anti-over-denial canaries: the CLOSED absolute terminal must
  # still resolve the legitimate absolute references the real bundle ships — the
  # OS/container tree, the recipe HOME + runtime dirs, URL path segments after
  # URL-stripping, and the parc log-group namespace. A gate that denied these
  # would be unusable (over-denial is the failure mode symmetric to the leak).
  '/dev/null|ok'                                        # universal OS device
  '/usr/local/bin/cs|ok'                                # container binary path
  '/api/healthz|ok'                                     # URL path segment (post URL-strip)
  '/.well-known/openid-configuration|ok'               # URL path segment
  '/cosmon/.config/cosmon|ok'                          # recipe HOME (tmpfs, Containerfile-created)
  '/tenant-demo/cosmon-server|ok'                          # CloudWatch log-group namespace
  '/nucleons/|ok'                                       # bare state-subdir mention (1 segment)
  # ── round-12 canaries (delib-20260707-8eca): the resolver is now DENY-BY-DEFAULT.
  # The two lead-segment whitelists (RESOLV_ABS_ROOTS/REL_ROOTS) are DELETED; a path
  # resolves ONLY if it is a POSITIVE referent (bundle file · mount · URL · exact
  # entry in resolves_path_allow / spec §R10). These GENERATIVE forms — invented
  # private names never wired as canaries, carrying NO motif and NO secret ext —
  # prove the CLASS is closed: none resolves positively, so all DENY by construction.
  # A future unknown private name is caught the same way, without being enumerated.
  '/opt/client-financials-2026|deny'        # unshaped private name under a former trusted lead
  '/etc/tenant-roster|deny'                 # was ok (lead `etc` trusted); now no positive referent
  '/root/cap-table|deny'                    # was ok (lead `root`); now denies
  '/home/merger-dossier|deny'               # was ok (lead `home`); now denies
  'application/customer-pii|deny'           # prose-lead reuse: application/json ok, this is not
  'state/tenant-secrets|deny'               # rel-lead reuse: state/galaxies ok, this is not
  'noogram/client-roster|deny'              # maker-lead reuse: noogram/llama-server ok, this is not
  '/opt/keys/master.age|deny'               # unenumerated secret material (.age) — no positive referent
  '/opt/creds/wg0.conf|deny'                # unenumerated secret material (wireguard)
  '/opt/x/shadow|deny'                      # unenumerated secret material (passwd shadow)
  '/opt/x/authorized_keys|deny'             # unenumerated secret material (ssh)
  '/var/lib/tenant-secrets-store|deny'      # the `var/lib` open-tree hole (torvalds Q4) — now closed
  '/tenant-demo/cosmon-server/production-db-dump|deny' # the 2-seg-prefix overrun (torvalds) — exact-match closes it
  'linux/amd64.noyau-dump|deny'             # prose-pair suffix overrun — exact match, no suffix admitted
  'target/backup-noyau|deny'                # target/release ok, this is not
  'usr/local/bin/exfil-tenant-db|deny'      # binary-tree tail (no open prefix — exact enumeration)
  'cosmon/.cosmon/../../opt/loot|deny'      # `..`-traversal dressing a private tail
  # ── round-12 anti-over-denial: the DENY-BY-DEFAULT terminal must still resolve
  # every legitimate reference the 19-file bundle actually ships (trailing-slash dir
  # mentions included). A gate that denied these would be unusable (symmetric risk).
  '/usr/local/bin/|ok'                      # bare binary dir (trailing slash canonicalised)
  '/kernel-bin/cs|ok'                       # build-output binary
  '/var/lib/apt/lists/|ok'                  # apt cache dir (exact, not an open tree)
  '.build/kernel-src/|ok'                   # build source dir
  'application/json|ok'                     # MIME prose pair
  'stdout/stderr|ok'                        # stream prose pair
  '/handoff/forgejo-issuer.toml|ok'         # runtime handoff path
  '/dev/urandom|ok'                         # universal OS device
  # ── round-13 canaries (delib-20260707-3b7e): the TWO heuristic accept-doors are
  # closed. Door A (non-path-char → prose exemption) and Door B (dotted-hostname →
  # public-URL exemption) each short-circuited to `ok` before the :457 deny-by-
  # default terminal; every form the panel re-executed as a leak is wired MUST-DENY
  # so the generational closure is PROVEN each run, not asserted.
  # Door A — the exact turing form + generative exotic-char forms (no motif, no
  # secret ext → they relied SOLELY on the deleted non-path-char exemption).
  '~/tenant-demo-secrets/cap-table|deny'           # turing D443: home path waved through as prose
  '~/mergers-2026/cap-table|deny'           # generative: `~` lead, private tail, no referent
  '%payroll-2026%/roster|deny'              # generative: `%`-dressed private path
  '&board-notes/q3-minutes|deny'            # generative: `&`-dressed private path
  # Door B — the exact turing + karpathy forms + a novel private host (all
  # host-shaped, none in RESOLV_PUBLIC_HOST → deny).
  'internal.corp/tenant-secrets|deny'       # turing D446
  'vault.tenant-demo-internal/master-key|deny'     # turing D446 (literally vault + master-key)
  'whispers.backup/inbox|deny'              # karpathy D446 (motif-net-blind, `whispers.`≠`whispers/`)
  'noyau-vault.io/dump|deny'                # karpathy D446
  'tenant-demo-corp.private/roster|deny'           # generative: novel private host, not enumerated
  # round-13 anti-over-denial — the closed doors must still resolve the genuine
  # script-syntax fragments the bundle's own scripts embed, and the two documented
  # public hosts. A gate that denied these would fail the honest 19-file build.
  's/^/|ok'                                 # sed program fragment (validate-local.sh) — §R10.allow
  '/\1/p|ok'                                # sed backref fragment (validate-local.sh) — §R10.allow
  '/api/v1/users/search?limit|ok'          # curl URL-path + query (provision.sh) — §R10.allow
  'codeberg.org/forgejo/forgejo|ok'         # documented public host (§R10.host)
  'registry.vendor.tenant-demo.io/noogram/llama-server|ok'         # customer vendor registry (§R10.host)
  'registry.vendor.tenant-demo.io/cosmon/cosmon-server-validation|ok' # customer vendor registry (§R10.host)
  # ── round-14 canaries (delib-20260708-d5a4): the TWO residual bare-token arms
  # are inverted to DENY-BY-DEFAULT. Round-13's closure held only for the
  # has-directory arm; the bare-filename arm (`*)`, formerly `RTV=ok`) and the bare
  # single-segment DIR short-circuit (formerly `RTV=ok` before the motif net) let
  # every unenumerated private bare form pass. Each form the panel + this fix
  # re-executed as a leak is wired MUST-DENY so the closure is PROVEN each run, not
  # asserted; the legit bare tokens the 19-file bundle ships are wired MUST-OK so
  # the inversion carries zero over-denial.
  # Bare-FILENAME arm — private data/secret files (every extension escapes the
  # finite deny lists, so ONLY deny-by-default catches them).
  'cap-table.xlsx|deny'                     # business doc (turing/forgemaster D499)
  'credentials.json|deny'                   # .json — same ext as legit state.json (name, not shape)
  'tenant-secrets.env|deny'                 # .env secrets file
  'payroll.db|deny'                         # on-disk DB, unenumerated ext
  'master.age|deny'                         # age-encrypted secret (ext not in SECRET_EXTS)
  'master-key.txt|deny'                     # plaintext key material
  'whispers.db|deny'                        # private corpus DB
  'id_ed25519.bak|deny'                     # ssh key backup (basename escapes .bak)
  'roster.numbers|deny'                     # NEW arm canary: Apple Numbers business doc
  'board-minutes.docx|deny'                 # NEW arm canary: Word business doc
  # Bare-DIR arm — private corpus directory mentions (fully silent before: word
  # parts pre-frozen in the WARN list, no DENY + no WARN).
  'client-roster/|deny'                     # forgemaster D428 (silent: client+roster frozen)
  'tenant-secrets/|deny'                    # buterin D428 (fires before motif net)
  'vault/|deny'                             # bare private dir
  'secrets/|deny'                           # bare private dir
  'cap-table/|deny'                         # bare private dir
  'merger-dossier/|deny'                    # bare private dir
  'payroll/|deny'                           # NEW arm canary: bare private dir
  'noyau/|deny'                             # NEW arm canary: motif word as bare dir
  # No-extension single-segment private names reaching the bare arm via a leading
  # slash (`/tenant-secrets` → `tenant-secrets`) — also closed.
  '/tenant-secrets|deny'
  '/vault|deny'
  '/client-roster|deny'
  # ── round-14 anti-over-denial — the inverted arms must still resolve EVERY legit
  # bare token the 19-file bundle ships. A gate that denied these would be unusable.
  'Cargo.lock|ok'                           # §R10.bare generic filename
  'app.ini|ok'                              # §R10.bare generic filename (also under /var/lib/gitea)
  'forgejo-issuer.toml|ok'                  # §R10.bare runtime-handoff filename
  'state.json|ok'                           # §R10.bare (.json — legit by NAME, not by shape)
  'err.log|ok'                              # §R10.bare log filename
  'nuc.log|ok'                              # §R10.bare log filename
  'docker-entrypoint.sh|ok'                 # §R10.bare cited script filename
  '/tmp|ok'                                 # §R10.bare single-segment OS dir (via leading slash)
  '/cosmon|ok'                              # §R10.bare recipe HOME dir
  '/handoff|ok'                             # declared mount (resolves_mount)
  '/build|ok'                               # §R10.bare build dir
  '/kernel-bin|ok'                          # §R10.bare build-output dir
  '.build/|ok'                              # §R10.bare build dir (bare-dir arm)
  'v3.0|ok'                                 # version shape (is_version_shape)
  'v3.0-amd64|ok'                           # version + arch build-tag
  '1.88-bookworm|ok'                        # base-image version tag
  '0.0.0.0|ok'                              # bind address (version/IP shape)
  '127.0.0.1|ok'                            # loopback (version/IP shape)
  'sibling.llama|ok'                        # §R10.bare TOML section accessor
  'logging.driver|ok'                       # §R10.bare compose key
  'org.opencontainers.image.title|ok'       # §R10.bare OCI label key
  'trust_bootstrap.issuer|ok'               # §R10.bare struct-field accessor
  'user.email|ok'                           # §R10.bare git-config key
)

resolvability_self_test() {
  local row tok expect
  for row in "${RESOLV_SELFTEST[@]}"; do
    tok="${row%|*}"; expect="${row##*|}"
    resolv_token_verdict "$tok"
    if [ "$RTV" != "$expect" ]; then
      echo "sovereignty-gate: RESOLV SELF-TEST FAILED — token '${tok}' classified '${RTV}', spec expects '${expect}'"
      return 1
    fi
  done
  return 0
}

# ── round-14 (delib-20260708-d5a4, janis's structural remedy) — REFERENT-BACKING
# self-test. The operator's decisive rule: EVERY `RTV=ok` must be adossé to a
# POSITIVE referent, never a shape blanket. This asserts it constructively: each
# pair is (a token that resolves `ok` via referent R, a MINIMALLY-mutated sibling
# with R removed) — and the sibling MUST flip to `deny`. A referent-less `ok`
# terminal (the round-13 residual this round closed) is UNWRITEABLE under this test:
# there is no referent to remove, so no sibling denies, and the pairing cannot be
# authored. This is the signal that would have broken the streak BEFORE merge.
# Format: `ok-token|deny-sibling` (both classified; left must be ok, right deny).
REFERENT_BACKING=(
  'rpp.toml|rpp.zzq'                        # bundle file (BUNDLE_BN) → drop the file → deny
  'Cargo.lock|Cargo.zzq'                    # §R10.bare filename → non-referent ext → deny
  'app.ini|app.zzq'                         # §R10.bare filename → mutate ext → deny
  'nucleons/|nucleon5/'                     # §R10.bare bare-dir → one char off → deny
  '.build/|.builz/'                         # §R10.bare bare-dir → one char off → deny
  'v3.0|v3.0zq'                             # version shape → break numeric core → deny
  '127.0.0.1|127.0.0.q'                     # version/IP shape → non-numeric tail → deny
  'sibling.llama|sibling.zzq'               # §R10.bare accessor → mutate tail → deny
  'org.opencontainers.image.title|org.opencontainers.image.zzq' # OCI key → mutate → deny
  '/handoff|/handofz'                       # declared mount → one char off → deny
  '/tmp|/tmz'                               # §R10.bare OS dir → one char off → deny
  'codeberg.org/forgejo/forgejo|codeberg.internal/forgejo/forgejo' # §R10.host → private host → deny
  'dev/null|dev/nulz'                       # §R10.allow whole-path → one char off → deny
)
referent_backing_self_test() {
  local pair ok_tok deny_tok
  for pair in "${REFERENT_BACKING[@]}"; do
    ok_tok="${pair%|*}"; deny_tok="${pair##*|}"
    resolv_token_verdict "$ok_tok"
    if [ "$RTV" != ok ]; then
      echo "sovereignty-gate: REFERENT-BACKING SELF-TEST FAILED — '${ok_tok}' should resolve ok (its backing referent), got '${RTV}'"
      return 1
    fi
    resolv_token_verdict "$deny_tok"
    if [ "$RTV" != deny ]; then
      echo "sovereignty-gate: REFERENT-BACKING SELF-TEST FAILED — '${deny_tok}' (referent removed) must DENY, got '${RTV}' → an ok terminal is a SHAPE BLANKET, not referent-backed"
      return 1
    fi
  done
  return 0
}

# resolvability_scan — extract every candidate reference from the URL-stripped
# tracked bytes and run it through resolv_token_verdict; print + flag the DENYs.
# Paths/filenames come from a whitespace/delimiter word split (robust — the true
# extension is computed in bash, immune to regex-alternation munching); symbol
# and tracker citations come from a targeted grep (they span or embed spaces).
# Sets RESOLV_FAIL.
#
# PERFORMANCE (round-10, karpathy): both extraction passes are hoisted OUT of the
# per-line loop. The citation grep runs ONCE per file (`grep -noE`, one fork of
# ~2200) instead of once per line, and the path split is a fork-free `IFS` read
# (byte-for-byte equivalent to the old `tr`, proven by the split self-test). With
# the memoized fork-free verdict this drops the scan from ~2200 subprocesses to
# ~40, so the gate finishes in <2 s and cannot be mistaken for a hang under load.
RESOLV_FAIL=0
# Delimiter set for the path/filename word split — the exact character set the
# previous `tr '=:,()[]{}"'\''`<>|*'` used, as an IFS string (space + tab close it).
RESOLV_SPLIT_IFS=$'=:,(){}[]"\'`<>|* \t'
resolvability_scan() {
  local rel absf stripped lineno line w cand
  local -a words
  for rel in "${TRACKED[@]}"; do
    absf="${REPO_ROOT}/${rel}"
    stripped="$(sed 's#https\{0,1\}://[^[:space:]"'"'"'`<>]*##g' "$absf")"
    # (a) symbol + tracker citations — ONE grep over the whole file (-n yields
    # the line number per match; matches carry no ':' so the split is safe).
    while IFS=: read -r lineno cand; do
      [ -z "$cand" ] && continue
      resolv_token_verdict "$cand"
      [ "$RTV" = deny ] && { RESOLV_FAIL=1; printf '     %s:%s: %s\n' "$rel" "$lineno" "$cand"; }
    done < <(printf '%s\n' "$stripped" | grep -noE '(struct|enum|trait|impl|mod|fn)[[:space:]]+[A-Z][A-Za-z0-9_]+|\([A-Z][a-z]+[A-Z][A-Za-z0-9]*\)|ADR-[0-9]+|§[0-9]+|[a-z][a-z]+-[MB][0-9]+|[A-Z]{2,}-[0-9]+' 2>/dev/null)
    # (b) path / filename references — fork-free IFS word split, per line.
    lineno=0
    while IFS= read -r line; do
      lineno=$((lineno + 1))
      IFS="$RESOLV_SPLIT_IFS" read -ra words <<< "$line"
      for w in "${words[@]}"; do
        [ -z "$w" ] && continue
        case "$w" in
          */*|*.*) : ;;
          *) continue ;;
        esac
        resolv_token_verdict "$w"
        [ "$RTV" = deny ] && { RESOLV_FAIL=1; printf '     %s:%s: %s\n' "$rel" "$lineno" "$w"; }
      done
    done < <(printf '%s\n' "$stripped")
  done
}

# ─────────────────────────────────────────────────────────────────────────────
# MECHANISM 1 — DENY-CLASS  (shaped classes + known-identity literals)
# ─────────────────────────────────────────────────────────────────────────────
# PATTERNS[i] (ERE) is explained by WHYS[i], proven-falsifiable by CANARIES[i],
# narrowed where a class overlaps sanctioned content by ALLOWS[i] (empty = none),
# and matched case-insensitively unless CASES[i]='s'. The ADR/§ citation class
# that used to live here was REMOVED — the resolvability rule (mechanism 2)
# subsumes it generatively (ADR-132 / §6 fail to resolve → DENY, alongside every
# other dangling private-corpus citation the old class never modelled).
PATTERNS=(
  '(task|delib|idea|issue|signal|spark|decision|mol|temp|verify)-[0-9]{6,8}-[0-9a-f]{4}'
  '(feat|fix|chore|refactor)/(task|idea|delib)-|\.worktrees/'
  'tailscale|tailnet|\.ts\.net|tail[0-9a-f]{5,}'
  '/Users/|~/galaxies'
  '[[:alnum:]._%+-]+@[[:alnum:].-]+\.(com|net|org|dev|io|fr|co|uk|eth)|gmail'
  '20260[1-9][0-9]{2}|2026-[01][0-9]-[0-9]{2}'
  '([0-9]{1,3}\.){3}[0-9]{1,3}'
  'avatar-[a-z]+'
  'you|you|jordan|dave|berenger|s[eé]rie\.dev'
  'democorp'
  'smithy|mailroom|skylight|accord|speck|almanac|showroom|chancery|agora|souffleur|lumen|atlas|cosmon-private'
  'neurion|archive-service|zotero'
)
WHYS=(
  'internal orchestration id (molecule / deliberation state)'
  'internal branch or worktree name'
  'tailnet hostname / network topology'
  'operator absolute filesystem path'
  'operator email address'
  'internal-cadence date (reveals when internal work happened)'
  'IPv4 address (network topology) — loopback/bind-all exempted'
  "other-avatar id (one customer must not leak into another's recipe)"
  'operator personal identity (known literal; the WARN allow-list is the net for UNKNOWN names)'
  'internal tenant fixture (cosmon test placeholder must not travel in a bundle)'
  'private galaxy name (known literal; the WARN allow-list is the net for UNKNOWN names)'
  'internal tooling name (nervous-system layer)'
)
CANARIES=(
  'delib-20260702-93ac'
  'feat/task-20260704-7a26'
  'myhost.tail1a2b3c.ts.net'
  '/Users/you/galaxies'
  'someone@example.com'
  '2026-05-12'
  '10.0.0.5'
  'avatar-jordan'
  'you'
  'democorp'
  'smithy'
  'neurion'
)
ALLOWS=(
  ''
  ''
  ''
  ''
  ''
  ''
  '127\.0\.0\.1|0\.0\.0\.0|255\.255\.255\.255'
  'avatar-tenant-demo|avatar-host'
  ''
  ''
  ''
  ''
)
CASES=(
  'i' 'i' 'i' 's' 'i' 'i' 'i' 'i' 'i' 'i' 'i' 'i'
)

# ── LIB seam (testability): when sourced with SOVEREIGNTY_GATE_LIB=1, stop here.
# Everything above is pure function/array definition with no side effect on the
# filesystem; an external harness sources the gate to drive `resolv_token_verdict`
# against adversarial tokens (the resolver self-test region below runs the same
# functions in-process). The shipped invocation NEVER sets this var, so the gate
# runs exactly as before. This is a no-op guard, not a mechanism.
[ "${SOVEREIGNTY_GATE_LIB:-}" = 1 ] && return 0 2>/dev/null

# ── Self-test (falsifiability) — DENY-class canaries + resolvability spec ─────
for i in "${!PATTERNS[@]}"; do
  ci=""; [ "${CASES[$i]}" = "i" ] && ci="i"
  hit="$(printf '%s\n' "${CANARIES[$i]}" | grep -${ci}E "${PATTERNS[$i]}" || true)"
  if [ -n "${ALLOWS[$i]}" ]; then
    hit="$(printf '%s\n' "$hit" | grep -ivE "${ALLOWS[$i]}" || true)"
  fi
  if [ -z "$hit" ]; then
    echo "sovereignty-gate: SELF-TEST FAILED — pattern '${PATTERNS[$i]}' does not (net) match its canary '${CANARIES[$i]}' (broken regex or over-broad ALLOW → blind gate)"
    exit 2
  fi
done
if ! resolvability_self_test; then
  echo "sovereignty-gate: resolvability self-test failed → refusing to run a possibly-blind gate."
  exit 2
fi
if ! referent_backing_self_test; then
  echo "sovereignty-gate: referent-backing self-test failed → a resolver ok-terminal is not adossé to a positive referent (round-14 closure breached)."
  exit 2
fi

# ── DENY-class scan over the tracked byte-set ────────────────────────────────
FAIL=0
for i in "${!PATTERNS[@]}"; do
  pattern="${PATTERNS[$i]}"; why="${WHYS[$i]}"; allow="${ALLOWS[$i]}"
  ci=""; [ "${CASES[$i]}" = "i" ] && ci="i"
  HITS="$(grep -rn${ci}E "$pattern" "${TRACKED_ABS[@]}" 2>/dev/null || true)"
  if [ -n "$allow" ] && [ -n "$HITS" ]; then
    HITS="$(printf '%s\n' "$HITS" | grep -ivE "$allow" || true)"
  fi
  if [ -n "$HITS" ]; then
    FAIL=1
    echo "❌ DENY [${why}] — class regex '${pattern}':"
    printf '%s\n' "$HITS" | sed "s|^${REPO_ROOT}/||; s/^/     /"
  fi
done

# ── RESOLVABILITY scan (mechanism 2) ─────────────────────────────────────────
RESOLV_OUT="$(resolvability_scan; echo "__RC__${RESOLV_FAIL}")"
RESOLV_FAIL="${RESOLV_OUT##*__RC__}"
RESOLV_HITS="${RESOLV_OUT%__RC__*}"
if [ "$RESOLV_FAIL" = 1 ]; then
  FAIL=1
  echo "❌ DENY [unresolvable reference — dangling pointer into a private corpus]:"
  echo "     every path-like / citation-like reference must resolve to a bundle file,"
  echo "     a public URL, or a public-standard citation. These resolve to none:"
  printf '%s' "$RESOLV_HITS" | grep -v '^[[:space:]]*$' || true
fi

# ── Tracked residue: git metadata / OS junk must not be a TRACKED file. ───────
for rel in "${TRACKED[@]}"; do
  case "$(basename "$rel")" in
    .DS_Store|*.orig|*.swp)
      FAIL=1; echo "❌ DENY [git/OS residue tracked in bundle]:"; echo "     ${rel}" ;;
  esac
done

# ── WARN: unknown vocabulary (allow-list of what may travel) ─────────────────
WARN_TOKENS=""
if [ -f "$ALLOWLIST" ]; then
  ALLOW_SET="$(grep -vE '^#|^$' "$ALLOWLIST" | sort -u)"
  WARN_TOKENS="$(comm -23 <(extract_tokens) <(printf '%s\n' "$ALLOW_SET"))"
else
  echo "⚠️  sovereignty-gate: ${ALLOWLIST#"$REPO_ROOT"/} missing — cannot run the unknown-vocabulary WARN pass."
fi
WARN_COUNT=0
if [ -n "$WARN_TOKENS" ]; then
  WARN_COUNT="$(printf '%s\n' "$WARN_TOKENS" | grep -c . || true)"
  echo "⚠️  WARN [${WARN_COUNT} unknown token(s) not on the travel allow-list — human review]:"
  printf '%s\n' "$WARN_TOKENS" | sed 's/^/     /'
  echo "     If a token is a legitimate new word, re-freeze with --update-allowlist"
  echo "     (its git diff records the admission). If it is a leak, scrub it."
fi

# ── verdict ──────────────────────────────────────────────────────────────────
if [ "$FAIL" = 0 ]; then
  echo "✅ sovereignty-gate: ${BUNDLE_PREFIX}/ tracked byte-set is clean (${#PATTERNS[@]} DENY-classes + resolvability rule, ${#TRACKED[@]} files, ${WARN_COUNT} advisory WARN)."
else
  echo ""
  echo "❌ sovereignty-gate: the bundle carries bytes that must not travel."
  echo "   Scrub the DENY hits above. For an unresolvable reference: state the"
  echo "   internal rule IN CLEAR for the reader and cite provenance by kernel SHA"
  echo "   or a public URL — never by an internal source path, symbol, or tracker code."
  echo "   If a hit is sanctioned content mis-flagged by a class, add it to that"
  echo "   class's ALLOWS[] entry (or the resolver's public list) with a rationale"
  echo "   — never widen a PATTERN to make a real leak invisible."
fi
exit "$FAIL"
