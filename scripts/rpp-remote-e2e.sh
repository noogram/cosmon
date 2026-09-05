#!/usr/bin/env bash
# scripts/rpp-remote-e2e.sh — container-level smoke of the §8j Remote
# Pilot Port: the real `docker compose` stack from
# `crates/cosmon-rpp-adapter/deploy/`, driven by the real compiled
# `cosmon-remote` binary over the published loopback ports.
#
# What this is for
# ----------------
# Every other test of this surface runs the adapter in-process, against a
# tower of test doubles. That proves the handlers; it cannot prove that
# the *image* boots, that the JWKS hand-off between the two containers
# lands where the adapter looks for it, that the nucleon binding the
# operator provisions is the shape the loader reads, or that a tenant's
# `cosmon-remote login` walks the mock IdP's authorization-code flow all
# the way to a persisted credential. Those are the failures this catches,
# and they are exactly the ones a fresh operator meets first.
#
# The scenario, one NDJSON line per step in `$RUN_DIR/e2e.ndjson`:
#
#   compose-up   two images built, `up --wait` on both healthchecks
#   healthz      GET /healthz through the published port
#   login        the OAuth2-PKCE authorization-code flow, headless
#   auth-me      GET /v1/auth/me — the token the server actually sees
#   nucleate     POST /v1/molecules — library-direct, writes the tenant tree
#   observe      GET /v1/molecules/:id — the molecule reads back
#   land         POST /v1/molecules/:id/land — the NAMED refusal (below)
#
# `tackle` and `done` are deliberately absent. The adapter image is
# library-direct (its Dockerfile ships no `cs`), and `POST …/tackle`
# still shells out; issue #54 owns making that leg library-direct and
# owns adding it here. `land` shells out too, which is why this script
# pins its refusal LABEL rather than asserting a harvest: today the door
# refuses `subprocess_spawn_failed` in-container, and when #54 lands the
# refusal becomes a harvest-door one. Pinning the label is what makes
# that change announce itself here instead of passing silently — see
# RPP_E2E_EXPECT_LAND_LABEL below.
#
# Usage (from anywhere in the repo):
#
#   bash scripts/rpp-remote-e2e.sh
#   bash scripts/rpp-remote-e2e.sh --keep     # leave the stack up
#
# Exit codes:
#   0  every step green
#   1  a step failed (the NDJSON names which, and why)
#   2  a prerequisite is missing (docker, jq, curl, a free port)
#
# There is no SKIP. A harness that prints green when it did not run is
# worse than no harness: it converts an absent prerequisite into a
# passing nightly. Missing docker is exit 2, loudly.
#
# Environment (all optional):
#   RPP_E2E_RUN_DIR          where evidence + the staged deploy tree go.
#                            Default `<repo>/.rpp-remote-e2e/<stamp>`.
#                            MUST be under a path the container engine
#                            can bind-mount (colima/Docker Desktop share
#                            $HOME, not /var/folders) — hence the default.
#   RPP_E2E_RPP_PORT         host port for the adapter   (default 18443)
#   RPP_E2E_OIDC_PORT        host port for the mock IdP  (default 18444)
#   RPP_E2E_AUDIENCE         JWT `aud` == OAuth `client_id`, provisioned
#                            into the IdP, the binding AND the client at
#                            once (so moving it alone stays green)
#   RPP_E2E_IDP_SUB          `sub` the mock IdP signs in as
#   RPP_E2E_EXPECT_SUB       falsifier: what `auth-me` must observe
#   RPP_E2E_CLIENT_AUDIENCE  falsifier: the audience the CLIENT asks for
#   RPP_E2E_EXPECT_STATUS    falsifier: the status `observe` must report
#   RPP_E2E_EXPECT_LAND_LABEL  falsifier: the refusal label `land` returns
#   COSMON_REMOTE_BIN        skip the cargo build, use this binary

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

KEEP=0
for arg in "$@"; do
  case "$arg" in
    --keep) KEEP=1 ;;
    --help|-h) sed -n '2,68p' "$0"; exit 0 ;;
    *) echo "error: unknown flag: $arg" >&2; exit 2 ;;
  esac
done

# ---------------------------------------------------------------------------
# Prerequisites. Each is a hard exit 2 — never a green skip.
# ---------------------------------------------------------------------------
need() {
  command -v "$1" >/dev/null 2>&1 || {
    echo "error: \`$1\` is required by this smoke and is not on PATH." >&2
    echo "       This harness refuses to report success without running." >&2
    exit 2
  }
}
need docker
need jq
need curl

if ! docker compose version >/dev/null 2>&1; then
  echo "error: \`docker compose\` (v2) is required; the legacy docker-compose is not enough." >&2
  exit 2
fi
if ! docker info >/dev/null 2>&1; then
  echo "error: the docker daemon is not reachable (\`docker info\` failed)." >&2
  echo "       Start it (colima start / Docker Desktop) and re-run." >&2
  exit 2
fi

# The OAuth redirect catcher binds this fixed loopback port; a login
# cannot proceed if something else holds it, and finding that out five
# minutes into the login timeout is not a diagnosis.
port_free() {
  command -v lsof >/dev/null 2>&1 || return 0   # cannot check; let compose speak
  ! lsof -nP -iTCP:"$1" -sTCP:LISTEN >/dev/null 2>&1
}

# ---------------------------------------------------------------------------
# Run layout.
# ---------------------------------------------------------------------------
STAMP="$(date -u +%Y%m%dT%H%M%SZ)"
RUN_DIR="${RPP_E2E_RUN_DIR:-$REPO_ROOT/.rpp-remote-e2e/$STAMP}"
mkdir -p "$RUN_DIR"
RUN_DIR="$(cd "$RUN_DIR" && pwd)"

NDJSON="$RUN_DIR/e2e.ndjson"
: >"$NDJSON"
LOGS="$RUN_DIR/logs"
mkdir -p "$LOGS"

PROJECT="cs-rpp-e2e-$$"
RPP_PORT="${RPP_E2E_RPP_PORT:-18443}"
OIDC_PORT="${RPP_E2E_OIDC_PORT:-18444}"
AUDIENCE="${RPP_E2E_AUDIENCE:-cosmon-rpp-tenant-demo}"
# The mock IdP's `--subject` default; `/authorize` signs in as this when
# the request carries no `login_hint`, and cosmon-remote sends none.
IDP_SUB="${RPP_E2E_IDP_SUB:-cs-oidc-mock-user}"

# --- The three falsification seams -----------------------------------------
#
# Each pins what a step must OBSERVE, separately from what this script
# provisions. Overriding one of the three makes exactly one step go red,
# which is what makes the green run evidence rather than a tautology:
# turning a single knob and watching the whole scenario stay green (as it
# does when you move `RPP_E2E_AUDIENCE`, which moves the IdP, the binding
# and the client together) proves only that the world is consistent with
# itself.
#
#   RPP_E2E_EXPECT_SUB         → `auth-me` (what /v1/auth/me must report)
#   RPP_E2E_CLIENT_AUDIENCE    → `login`   (the audience the CLIENT asks for)
#   RPP_E2E_EXPECT_STATUS      → `observe` (the recorded lifecycle status)
#   RPP_E2E_EXPECT_LAND_LABEL  → `land`    (the named refusal)
EXPECT_SUB="${RPP_E2E_EXPECT_SUB:-$IDP_SUB}"
CLIENT_AUDIENCE="${RPP_E2E_CLIENT_AUDIENCE:-$AUDIENCE}"
# A molecule nucleated over the API is assigned to nobody, so it lands
# in `Pending` (cosmon_core::nucleate: Pending if unassigned, Queued if
# assigned) and `observe` renders that as the snake_case label.
EXPECT_OBSERVE_STATUS="${RPP_E2E_EXPECT_STATUS:-pending}"
EXPECT_LAND_LABEL="${RPP_E2E_EXPECT_LAND_LABEL:-subprocess_spawn_failed}"
NOYAU="e2e-noyau"
# The issuer MUST be a URL the *client* can reach: it is where
# cosmon-remote fetches `/.well-known/openid-configuration`. In the
# reference deployment that is the compose service name; here the client
# runs on the host, so it is the published loopback port. The adapter is
# unaffected — it pins the JWKS from disk and never dials the issuer.
ISSUER="http://127.0.0.1:$OIDC_PORT"
HOST_URL="http://127.0.0.1:$RPP_PORT"

COMPOSE_FILE="$RUN_DIR/deploy/docker-compose.yml"

# Three ports must be free before anything is built: the two the stack
# publishes, and the fixed one `cosmon-remote login` binds its OAuth
# redirect catcher on (oidc::loopback::DEFAULT_REDIRECT_PORT). Learning
# this from a compose failure ten minutes into an image build — or from a
# five-minute login timeout — is not a diagnosis. A previous run whose
# teardown did not complete is the usual cause; the message says so.
for port_and_role in "$RPP_PORT:the rpp-adapter" "$OIDC_PORT:the mock IdP" "7777:the OAuth redirect catcher"; do
  port="${port_and_role%%:*}"
  role="${port_and_role#*:}"
  port_free "$port" || {
    echo "error: TCP port $port is already in use, and $role needs it." >&2
    echo "       A leftover stack? \`docker ps\` — then \`docker compose -p <project> down -v\`." >&2
    echo "       Or point this run elsewhere: RPP_E2E_RPP_PORT / RPP_E2E_OIDC_PORT." >&2
    exit 2
  }
done

echo "==> run dir: $RUN_DIR"

# ---------------------------------------------------------------------------
# NDJSON recorder. One line per step: {step, rc, ms, evidence}.
# `evidence` is a short human sentence naming what was actually observed
# — never the token, never a response body verbatim.
# ---------------------------------------------------------------------------
record() {
  local step="$1" rc="$2" ms="$3" evidence="$4"
  jq -cn --arg step "$step" --argjson rc "$rc" --argjson ms "$ms" \
        --arg evidence "$evidence" \
        '{step:$step, rc:$rc, ms:$ms, evidence:$evidence}' >>"$NDJSON"
  if [[ "$rc" -eq 0 ]]; then
    printf '    [ok]   %-12s %sms — %s\n' "$step" "$ms" "$evidence"
  else
    printf '    [FAIL] %-12s %sms — %s\n' "$step" "$ms" "$evidence" >&2
  fi
}

now_ms() { python3 -c 'import time; print(int(time.time()*1000))'; }

# `fail <step> <start_ms> <evidence>` records and aborts. Every step is a
# gate: the first red ends the run, because a scenario whose step N ran
# against the wreckage of step N-1 reports noise, not a second finding.
fail() {
  record "$1" 1 "$(( $(now_ms) - $2 ))" "$3"
  exit 1
}

# ---------------------------------------------------------------------------
# Teardown. Registered before the stack exists so an abort during `up`
# still cleans up.
# ---------------------------------------------------------------------------
CLEANED=0
cleanup() {
  local rc=$?
  # `exit` from inside the INT/TERM handler re-enters here through the
  # EXIT trap; a second `compose down` on a torn-down project prints a
  # confusing error over the real one.
  if [[ $CLEANED -eq 1 ]]; then exit "$rc"; fi
  CLEANED=1
  if [[ $KEEP -eq 1 ]]; then
    echo "==> --keep: leaving project '$PROJECT' up (down with:"
    echo "    docker compose -p $PROJECT -f $COMPOSE_FILE down -v)"
  elif [[ -f "$COMPOSE_FILE" ]]; then
    echo "==> tearing down '$PROJECT'"
    compose logs >"$LOGS/compose.log" 2>&1 || true
    compose down -v --remove-orphans >>"$LOGS/compose-down.log" 2>&1 || true
  fi
  echo "==> NDJSON: $NDJSON"
  exit "$rc"
}
trap cleanup EXIT
# A CI cancel arrives as SIGTERM (SIGINT from a terminal ^C); without
# these the stack survives the run that owns it and the next run meets
# "port already allocated".
trap cleanup INT TERM

compose() {
  COSMON_GALAXIES_HOST="$RUN_DIR/galaxies" \
  COSMON_RPP_AUDIENCE="$AUDIENCE" \
  COSMON_RPP_ISSUER="$ISSUER" \
  COSMON_RPP_HOST_PORT="$RPP_PORT" \
  COSMON_OIDC_HOST_PORT="$OIDC_PORT" \
  COSMON_RPP_NAME_SUFFIX="-e2e-$$" \
  docker compose -p "$PROJECT" -f "$COMPOSE_FILE" "$@"
}

# ---------------------------------------------------------------------------
# Step 0 — the host-side binary. Real compiled `cosmon-remote`, not a
# curl transcript: the point is that the tenant's CLI drives the stack.
# ---------------------------------------------------------------------------
t0=$(now_ms)
if [[ -n "${COSMON_REMOTE_BIN:-}" ]]; then
  REMOTE="$COSMON_REMOTE_BIN"
else
  echo "==> building cosmon-remote (release)"
  if ! cargo build --release --locked -p cosmon-remote --bin cosmon-remote \
       >"$LOGS/cargo-build.log" 2>&1; then
    fail build "$t0" "cargo build -p cosmon-remote failed; see logs/cargo-build.log"
  fi
  REMOTE="$REPO_ROOT/target/release/cosmon-remote"
fi
[[ -x "$REMOTE" ]] || fail build "$t0" "cosmon-remote binary not executable at $REMOTE"
record build 0 "$(( $(now_ms) - t0 ))" "cosmon-remote at $REMOTE ($("$REMOTE" --version 2>/dev/null | head -1))"

# ---------------------------------------------------------------------------
# Step 1 — stage a throwaway copy of `deploy/`.
#
# The tracked deploy tree is never written to. It ships the nucleon
# binding only as `oidc-identity.toml.example` (the real file is
# gitignored, as it must be), so a clean checkout cannot boot the stack
# without materialising one — that materialisation is the operator
# gesture this script performs in a copy.
# ---------------------------------------------------------------------------
t0=$(now_ms)
DEPLOY_SRC="$REPO_ROOT/crates/cosmon-rpp-adapter/deploy"
mkdir -p "$RUN_DIR/deploy"
cp "$DEPLOY_SRC/docker-compose.yml" "$DEPLOY_SRC/rpp.toml" "$RUN_DIR/deploy/"
mkdir -p "$RUN_DIR/deploy/state/nucleons/nuc-$NOYAU"

# The binding is materialised from the tracked `.example` — never from
# an inline heredoc. The `.example` IS the artefact a fresh operator
# provisions from, so the smoke must exercise exactly it: a template
# that has lost a key the loader reads produces a file that resolves no
# noyau, and this step goes red instead of the run passing on a private
# copy the operator will never have.
BINDING_TEMPLATE="$DEPLOY_SRC/state/nucleons/nuc-tenant-demo/oidc-identity.toml.example"
BINDING="$RUN_DIR/deploy/state/nucleons/nuc-$NOYAU/oidc-identity.toml"
[[ -f "$BINDING_TEMPLATE" ]] \
  || fail stage "$t0" "the tracked binding template is missing at $BINDING_TEMPLATE"
sed -e "s|REPLACE_ME_NUCLEON_ID|nuc-$NOYAU|g" \
    -e "s|REPLACE_ME_NOYAU|$NOYAU|g" \
    -e "s|REPLACE_ME_ISSUER|$ISSUER|g" \
    -e "s|REPLACE_ME_SUB|$IDP_SUB|g" \
    -e "s|REPLACE_ME_AUDIENCE|$AUDIENCE|g" \
    -e "s|REPLACE_ME_SEALED_AT|$STAMP|g" \
    "$BINDING_TEMPLATE" >"$BINDING"

# Two assertions on the materialised file, both aimed at the template
# rather than at sed. The first catches a placeholder the template
# renamed or added; the second catches a key the template dropped —
# which is the failure that used to hide behind the heredoc.
# Comment lines are excluded on purpose: the template's own header
# names the placeholder token to tell the operator what to replace, and
# a scan that could not tell that sentence from an unsubstituted value
# would make the header unwritable.
LEFTOVER="$(grep -v '^[[:space:]]*#' "$BINDING" | grep -o 'REPLACE_ME_[A-Z_]*' | sort -u | tr '\n' ' ' || true)"
[[ -z "$LEFTOVER" ]] \
  || fail stage "$t0" "unsubstituted placeholder(s) left by the template: $LEFTOVER"
for required in \
    "^nucleon_id = \"nuc-$NOYAU\"$" \
    "^phase = \"" \
    "^noyau = \"$NOYAU\"$" \
    "^\[oidc\]$" \
    "^issuer = \"$ISSUER\"$" \
    "^sub = \"$IDP_SUB\"$" \
    "^audience = \"$AUDIENCE\"$" \
    "^\[scopes\]$" \
    "^allowed = \[" ; do
  grep -Eq "$required" "$BINDING" \
    || fail stage "$t0" "the binding template does not yield a loadable binding: nothing matches /$required/ in $(basename "$BINDING_TEMPLATE")"
done

# The OAuth client registry the adapter publishes at
# /.well-known/cosmon-oauth-clients — what `login` reads to learn its
# own client_id. Copied into the state volume after `up` (the volume
# does not exist before then).
cat >"$RUN_DIR/oauth-clients.toml" <<TOML
schema_version = 2
issuer = "$ISSUER"

[[clients]]
audience = "$AUDIENCE"
client_id = "$AUDIENCE"
scopes = ["openid", "cosmon:molecule:read", "cosmon:molecule:write"]
TOML

# The throwaway galaxy. `galaxies_root` is bind-mounted; the
# library-direct nucleate route opens <root>/<noyau>/.cosmon/{state,formulas}.
GALAXY="$RUN_DIR/galaxies/$NOYAU"
mkdir -p "$GALAXY/.cosmon/state" "$GALAXY/.cosmon/formulas"
cp "$REPO_ROOT/.cosmon/formulas/task-work.formula.toml" "$GALAXY/.cosmon/formulas/"
# The adapter runs as uid 10000; on a Linux runner the bind-mounted tree
# is owned by the runner's uid and nucleate needs to write into it.
chmod -R 0777 "$RUN_DIR/galaxies"
record stage 0 "$(( $(now_ms) - t0 ))" "staged deploy copy; binding materialised from oidc-identity.toml.example for ($ISSUER, $IDP_SUB, $AUDIENCE) → $NOYAU"

# ---------------------------------------------------------------------------
# Step 2 — build the images and wait on BOTH healthchecks.
#
# `--wait` is the whole point: the compose file already declares the two
# probes and `depends_on: service_healthy`, so a stack that answers here
# has passed its own liveness contract before the scenario starts.
# Both Dockerfiles do a full `cargo build --release --locked`; that is
# minutes, which is why this job is nightly.
# ---------------------------------------------------------------------------
t0=$(now_ms)
echo "==> docker compose up --wait (building images; this takes several minutes)"
# The adapter Dockerfile COPYs the dist-binaries directory, which is
# gitignored and absent on a clean checkout. An empty one is a valid
# (and honest) input: the /dist route 404s with its own hint.
mkdir -p "$REPO_ROOT/crates/cosmon-rpp-adapter/assets/binaries"
if ! compose up -d --build --wait >"$LOGS/compose-up.log" 2>&1; then
  compose ps >>"$LOGS/compose-up.log" 2>&1 || true
  fail compose-up "$t0" "compose up --wait failed; see logs/compose-up.log"
fi
record compose-up 0 "$(( $(now_ms) - t0 ))" "oidc-mock + rpp-adapter both healthy on $HOST_URL / $ISSUER"

# Provision the reverse-discovery registry inside the state volume. The
# operator does this out of band in a real deployment (it is the
# `client_id` publication, not a request-time concern).
t0=$(now_ms)
ADAPTER_CID="$(compose ps -q rpp-adapter)"
[[ -n "$ADAPTER_CID" ]] || fail provision "$t0" "could not resolve the rpp-adapter container id"
if ! docker cp "$RUN_DIR/oauth-clients.toml" \
     "$ADAPTER_CID:/cosmon/.cosmon/state/security/oauth-clients.toml" \
     >"$LOGS/docker-cp.log" 2>&1; then
  fail provision "$t0" "docker cp of oauth-clients.toml into the state volume failed"
fi
DISCOVERY="$(curl -fsS "$HOST_URL/.well-known/cosmon-oauth-clients" 2>>"$LOGS/discovery.log" || true)"
got_issuer="$(printf '%s' "$DISCOVERY" | jq -r '.issuer // empty' 2>/dev/null || true)"
[[ "$got_issuer" == "$ISSUER" ]] || \
  fail provision "$t0" "reverse-discovery issuer is ${got_issuer:-<none>}, expected $ISSUER"
record provision 0 "$(( $(now_ms) - t0 ))" "oauth-clients registry served; issuer=$got_issuer"

# ---------------------------------------------------------------------------
# Step 3 — healthz through the published port.
# ---------------------------------------------------------------------------
t0=$(now_ms)
HEALTH="$(curl -fsS "$HOST_URL/healthz" 2>>"$LOGS/healthz.log" || true)"
printf '%s' "$HEALTH" | jq -e '.ok == true' >/dev/null 2>&1 \
  || fail healthz "$t0" "GET /healthz did not answer {\"ok\":true}"
record healthz 0 "$(( $(now_ms) - t0 ))" "GET /healthz → ok:true"

# ---------------------------------------------------------------------------
# The tenant's environment.
#
# $HOME is redirected so the run reads neither the operator's
# cosmon-remote profiles nor their OS keychain — but ONLY for the
# cosmon-remote process. Exporting it for the whole script also moved
# `docker`'s home, and `docker compose` is a CLI *plugin* resolved out of
# `$HOME/.docker/cli-plugins`: the teardown then failed with a usage dump
# and left the stack up, which the next run met as "port already
# allocated". Scope the redirect to the process that asked for it.
# ---------------------------------------------------------------------------
TENANT_HOME="$RUN_DIR/home"
mkdir -p "$TENANT_HOME"

cs_remote() {
  env HOME="$TENANT_HOME" \
      COSMON_REMOTE_CRED_BACKEND=file \
      COSMON_REMOTE_TOKEN= \
      COSMON_REMOTE_BROWSER='curl -sS -L -o /dev/null' \
      "$REMOTE" --profile e2e "$@"
}

t0=$(now_ms)
cs_remote config init e2e "$HOST_URL"      >"$LOGS/config.log" 2>&1 || fail config "$t0" "config init failed"
cs_remote config set sub      "$IDP_SUB"     >>"$LOGS/config.log" 2>&1 || fail config "$t0" "config set sub failed"
cs_remote config set aud      "$CLIENT_AUDIENCE" >>"$LOGS/config.log" 2>&1 || fail config "$t0" "config set aud failed"
cs_remote config set oidc-url "$ISSUER"     >>"$LOGS/config.log" 2>&1 || fail config "$t0" "config set oidc-url failed"
cs_remote config set noyau    "$NOYAU"      >>"$LOGS/config.log" 2>&1 || fail config "$t0" "config set noyau failed"
record config 0 "$(( $(now_ms) - t0 ))" "profile 'e2e' → host=$HOST_URL aud=$CLIENT_AUDIENCE oidc=$ISSUER"

# ---------------------------------------------------------------------------
# Step 4 — login. The whole authorization-code + PKCE dance against the
# real containerised IdP, ending in a persisted credential.
# ---------------------------------------------------------------------------
t0=$(now_ms)
if ! cs_remote login >"$LOGS/login.log" 2>&1; then
  fail login "$t0" "cosmon-remote login failed: $(tail -3 "$LOGS/login.log" | tr '\n' ' ')"
fi
record login 0 "$(( $(now_ms) - t0 ))" "authorization-code + PKCE login persisted a credential (file backend)"

# ---------------------------------------------------------------------------
# Step 5 — auth me. The token the SERVER sees, not the one we think we
# sent: this is the step that catches a JWKS hand-off or an audience pin
# that only looks right from the client side.
# ---------------------------------------------------------------------------
t0=$(now_ms)
ME="$(cs_remote --json auth me 2>>"$LOGS/auth-me.log")" \
  || fail auth-me "$t0" "GET /v1/auth/me failed: $(tail -2 "$LOGS/auth-me.log" | tr '\n' ' ')"
me_sub="$(printf '%s' "$ME"  | jq -r '.sub // empty')"
me_noyau="$(printf '%s' "$ME" | jq -r '.noyau // empty')"
[[ "$me_sub" == "$EXPECT_SUB" ]] \
  || fail auth-me "$t0" "auth me reported sub=${me_sub:-<absent>}, expected $EXPECT_SUB"
[[ "$me_noyau" == "$NOYAU" ]] \
  || fail auth-me "$t0" "auth me reported noyau=${me_noyau:-<absent>}, expected $NOYAU (nucleon binding not resolved)"
record auth-me 0 "$(( $(now_ms) - t0 ))" "sub=$me_sub noyau=$me_noyau"

# ---------------------------------------------------------------------------
# Step 6 — nucleate. Library-direct: the adapter writes into the
# bind-mounted tenant tree with no subprocess.
# ---------------------------------------------------------------------------
t0=$(now_ms)
NUC="$(cs_remote --json molecule nucleate task-work --topic 'container-level rpp smoke' \
        2>>"$LOGS/nucleate.log")" \
  || fail nucleate "$t0" "POST /v1/molecules failed: $(tail -2 "$LOGS/nucleate.log" | tr '\n' ' ')"
# `.molecule.id`, spelled once and exactly: a `//` fallback chain would
# accept a drifted envelope shape as if nothing had changed, which is the
# drift this smoke exists to notice.
MOL_ID="$(printf '%s' "$NUC" | jq -r '.molecule.id // empty')"
[[ -n "$MOL_ID" ]] || fail nucleate "$t0" "nucleate returned no molecule id: $(printf '%s' "$NUC" | head -c 200)"
[[ -d "$GALAXY/.cosmon/state/fleets/default/molecules/$MOL_ID" ]] \
  || fail nucleate "$t0" "molecule $MOL_ID has no directory in the bind-mounted tenant tree"
record nucleate 0 "$(( $(now_ms) - t0 ))" "molecule $MOL_ID materialised in the host-side galaxy tree"

# ---------------------------------------------------------------------------
# Step 7 — observe. The molecule reads back over the wire.
# ---------------------------------------------------------------------------
t0=$(now_ms)
OBS="$(cs_remote --json molecule get "$MOL_ID" 2>>"$LOGS/observe.log")" \
  || fail observe "$t0" "GET /v1/molecules/$MOL_ID failed: $(tail -2 "$LOGS/observe.log" | tr '\n' ' ')"
obs_id="$(printf '%s' "$OBS" | jq -r '.molecule.id // empty')"
[[ "$obs_id" == "$MOL_ID" ]] || fail observe "$t0" "observe returned id=${obs_id:-<absent>}, expected $MOL_ID"
# The status is asserted, not merely echoed: a molecule that reads back
# by id while reporting a status the nucleate route never produces is
# exactly the envelope drift this smoke exists to notice.
obs_status="$(printf '%s' "$OBS" | jq -r '.molecule.status // empty')"
[[ "$obs_status" == "$EXPECT_OBSERVE_STATUS" ]] \
  || fail observe "$t0" "observe reported status=${obs_status:-<absent>}, expected $EXPECT_OBSERVE_STATUS"
record observe 0 "$(( $(now_ms) - t0 ))" "molecule $MOL_ID reads back, status=$obs_status"

# ---------------------------------------------------------------------------
# Step 8 — land. The harvest door on a molecule nobody armed a grant for.
#
# The assertion is the NAME of the refusal and the CLI's exit code, not
# merely "it failed": a door that refuses for an unnamed reason is the
# defect ADR-176 exists to prevent, and a 500 would satisfy a
# "non-zero exit" assertion just as well as the right refusal does.
# ---------------------------------------------------------------------------
t0=$(now_ms)
set +e
cs_remote --json molecule land "$MOL_ID" >"$LOGS/land.out" 2>"$LOGS/land.err"
land_rc=$?
set -e
[[ $land_rc -ne 0 ]] || fail land "$t0" "land SUCCEEDED on a molecule with no operator grant"
land_label="$(grep -o '"error":"[a-z_]*"' "$LOGS/land.err" | head -1 | cut -d'"' -f4)"
[[ -n "$land_label" ]] || land_label="$(grep -o '"label":"[a-z_]*"' "$LOGS/land.err" | head -1 | cut -d'"' -f4)"
[[ "$land_label" == "$EXPECT_LAND_LABEL" ]] \
  || fail land "$t0" "land refused '${land_label:-<unnamed>}' (exit $land_rc), expected '$EXPECT_LAND_LABEL' — if #54 made the door library-direct, update RPP_E2E_EXPECT_LAND_LABEL"
record land 0 "$(( $(now_ms) - t0 ))" "named refusal '$land_label', cosmon-remote exit $land_rc"

echo
echo "==> all steps green — $(wc -l <"$NDJSON" | tr -d ' ') recorded"
