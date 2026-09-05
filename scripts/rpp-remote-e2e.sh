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
#   tackle       POST /v1/molecules/:id/tackle — a REAL worker, spawned by
#                the shipped image, with no `cs` binary in it
#   worker       the worker finishes: the molecule reaches `completed`
#   land         POST /v1/molecules/:id/land — the NAMED refusal (below)
#
# The `tackle` leg is what issue #54 U7 adds, and it is the reason the
# rest of the scenario exists. Until U6 the adapter reached `tackle`,
# `run` and `land` by shelling out to `cs` — a binary its own Dockerfile
# has never shipped — so all three failed against the image an operator
# actually deploys, while every in-process suite stayed green. U6 cut
# dispatch over to `cosmon_runtime::LibraryExecutor` over the tmux
# transport port. Whether that is *true of the image* is not a claim any
# in-process test can make, and it is the only claim this step makes:
# the adapter, in the container, spawns a worker pane, hands it its
# briefing, and the worker drives `cs` against the tenant store until
# the molecule is `completed`.
#
# The worker is a dummy — `tests/fakes/fake-claude` in its
# `complete-molecule` mode — and it, together with the worker-side `cs`
# it runs, is provisioned in a Dockerfile stage the deployment never
# builds (`target: e2e`, selected by `deploy/docker-compose.e2e.yml`).
# The SHIPPED stage still contains no `cs` and no agent CLI. That
# separation is load-bearing: a smoke that provisioned the tenant-facing
# image would be proving the claim against an image nobody runs.
#
# `land` is still asserted as a NAMED refusal, but the name has changed
# and the reason is different. It is no longer "the binary is missing":
# the door's decision half runs in-process and this script now ARMS it
# (`[harvest_authority] required` in the throwaway galaxy), so the
# decision admits the harvest and the refusal comes from the effect
# half, which has no library implementation yet — `501
# land_effect_unavailable`, ADR-176 §12. Pinning the label is what makes
# the day it becomes a real harvest announce itself here instead of
# passing silently — see RPP_E2E_EXPECT_LAND_LABEL below.
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
#   RPP_E2E_EXPECT_LAND_LABEL  falsifier: the refusal label `land` returns
#   RPP_E2E_EXPECT_TACKLE_LABEL
#                            falsifier: run the scenario against an image
#                            whose `tackle` is expected to REFUSE, and
#                            name the refusal. Empty (the default) means
#                            tackle must succeed. This is how the pre-U6
#                            claim is falsified without a second script:
#                            build from a checkout that predates the
#                            library cut-over (RPP_E2E_BUILD_ROOT) and
#                            pin `tackle_unavailable` here — the run ends
#                            after `tackle`, because a refused dispatch
#                            has no worker to wait for.
#   RPP_E2E_BUILD_ROOT       workspace the two images are BUILT from.
#                            Default `<repo>` — this checkout. The staged
#                            compose file's build context is rewritten to
#                            this absolute path, so the compose file can
#                            come from here while the source tree comes
#                            from somewhere else (the falsifier above).
#   RPP_E2E_E2E_STAGE        `1` (default) layers
#                            `deploy/docker-compose.e2e.yml` on top of
#                            the deployment compose file: the adapter
#                            image is built from the Dockerfile's `e2e`
#                            target, which adds the dummy agent and the
#                            worker-side `cs`. Set `0` for a build root
#                            that has no such stage.
#   RPP_E2E_WORKER_TIMEOUT   seconds to wait for the spawned worker to
#                            drive the molecule to `completed`
#                            (default 120).
#   COSMON_REMOTE_BIN        skip the cargo build, use this binary

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

KEEP=0
for arg in "$@"; do
  case "$arg" in
    --keep) KEEP=1 ;;
    --help|-h) sed -n '2,117p' "$0"; exit 0 ;;
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
#   RPP_E2E_EXPECT_LAND_LABEL  → `land`    (the named refusal)
EXPECT_SUB="${RPP_E2E_EXPECT_SUB:-$IDP_SUB}"
CLIENT_AUDIENCE="${RPP_E2E_CLIENT_AUDIENCE:-$AUDIENCE}"
#   RPP_E2E_EXPECT_TACKLE_LABEL → `tackle`  (empty = must succeed)
#
# `land` no longer refuses for want of a binary. The door's decision
# half runs in-process (issue #54 U3) and this script arms it, so the
# decision ADMITS and the refusal is the effect half's: `501
# land_effect_unavailable` (ADR-176 §12). The label is deliberately
# outside the closed seven-refusal set — it names a missing
# implementation, not a verdict about this molecule.
EXPECT_LAND_LABEL="${RPP_E2E_EXPECT_LAND_LABEL:-land_effect_unavailable}"
EXPECT_TACKLE_LABEL="${RPP_E2E_EXPECT_TACKLE_LABEL:-}"
BUILD_ROOT="${RPP_E2E_BUILD_ROOT:-$REPO_ROOT}"
BUILD_ROOT="$(cd "$BUILD_ROOT" && pwd)"
E2E_STAGE="${RPP_E2E_E2E_STAGE:-1}"
WORKER_TIMEOUT="${RPP_E2E_WORKER_TIMEOUT:-120}"
NOYAU="e2e-noyau"
# The issuer MUST be a URL the *client* can reach: it is where
# cosmon-remote fetches `/.well-known/openid-configuration`. In the
# reference deployment that is the compose service name; here the client
# runs on the host, so it is the published loopback port. The adapter is
# unaffected — it pins the JWKS from disk and never dials the issuer.
ISSUER="http://127.0.0.1:$OIDC_PORT"
HOST_URL="http://127.0.0.1:$RPP_PORT"

COMPOSE_FILE="$RUN_DIR/deploy/docker-compose.yml"
# Layered on top of COMPOSE_FILE when RPP_E2E_E2E_STAGE=1: the test-only
# override that builds the adapter from the Dockerfile's `e2e` target.
# Kept a SEPARATE file rather than a flag on the deployment one, so the
# compose configuration an operator reads renders identically whether or
# not a smoke ever ran.
COMPOSE_E2E_FILE="$RUN_DIR/deploy/docker-compose.e2e.yml"

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
FAILED_STEP=""
record() {
  local step="$1" rc="$2" ms="$3" evidence="$4"
  jq -cn --arg step "$step" --argjson rc "$rc" --argjson ms "$ms" \
        --arg evidence "$evidence" \
        '{step:$step, rc:$rc, ms:$ms, evidence:$evidence}' >>"$NDJSON"
  if [[ "$rc" -eq 0 ]]; then
    printf '    [ok]   %-12s %sms — %s\n' "$step" "$ms" "$evidence"
  else
    printf '    [FAIL] %-12s %sms — %s\n' "$step" "$ms" "$evidence" >&2
    [[ -z "$FAILED_STEP" ]] && FAILED_STEP="$step"
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
cleanup() {
  local rc=$?
  if [[ $KEEP -eq 1 ]]; then
    echo "==> --keep: leaving project '$PROJECT' up (down with:"
    echo "    docker compose -p $PROJECT ${COMPOSE_ARGS[*]} down -v)"
  elif [[ -f "$COMPOSE_FILE" ]]; then
    echo "==> tearing down '$PROJECT'"
    compose logs >"$LOGS/compose.log" 2>&1 || true
    compose down -v --remove-orphans >>"$LOGS/compose-down.log" 2>&1 || true
  fi
  echo "==> NDJSON: $NDJSON"
  exit "$rc"
}
trap cleanup EXIT

# The `-f` list, built once. `up` and `down` MUST see the same set: a
# teardown that forgot the override would leave the e2e-tagged image's
# containers behind, and the next run would meet them as "port already
# allocated".
COMPOSE_ARGS=(-f "$COMPOSE_FILE")
if [[ "$E2E_STAGE" == "1" ]]; then
  COMPOSE_ARGS+=(-f "$COMPOSE_E2E_FILE")
fi

compose() {
  COSMON_GALAXIES_HOST="$RUN_DIR/galaxies" \
  COSMON_RPP_AUDIENCE="$AUDIENCE" \
  COSMON_RPP_ISSUER="$ISSUER" \
  COSMON_RPP_HOST_PORT="$RPP_PORT" \
  COSMON_OIDC_HOST_PORT="$OIDC_PORT" \
  COSMON_RPP_NAME_SUFFIX="-e2e-$$" \
  docker compose -p "$PROJECT" "${COMPOSE_ARGS[@]}" "$@"
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
if [[ "$E2E_STAGE" == "1" ]]; then
  cp "$DEPLOY_SRC/docker-compose.e2e.yml" "$RUN_DIR/deploy/"
fi
mkdir -p "$RUN_DIR/deploy/state/nucleons/nuc-$NOYAU"

# Pin the build context to an absolute path.
#
# The tracked files say `context: ../../..`, which is right where they
# live and wrong everywhere else — it resolved correctly here only
# because RUN_DIR defaulted to exactly two levels under the repo, a
# coincidence one `RPP_E2E_RUN_DIR` away from silently building the
# wrong tree. Rewriting it in the COPY makes the source tree an explicit
# input, which is also what lets the falsifier build the images from a
# pre-U6 checkout while using this checkout's compose files.
for f in "$RUN_DIR/deploy/docker-compose.yml" "$RUN_DIR/deploy/docker-compose.e2e.yml"; do
  [[ -f "$f" ]] || continue
  # `|` as the sed delimiter: BUILD_ROOT is a path and contains `/`.
  sed -i.bak "s|context: \.\./\.\./\.\.|context: $BUILD_ROOT|" "$f"
  rm -f "$f.bak"
done
grep -q "context: $BUILD_ROOT" "$RUN_DIR/deploy/docker-compose.yml" \
  || fail stage "$t0" "could not pin the build context to $BUILD_ROOT in the staged compose file"

cat >"$RUN_DIR/deploy/state/nucleons/nuc-$NOYAU/oidc-identity.toml" <<TOML
# Materialised from oidc-identity.toml.example for one e2e run.
# Binds the (iss, sub, aud) triple the mock IdP mints to a throwaway noyau.
nucleon_id = "nuc-$NOYAU"
phase = "Biological"
noyau = "$NOYAU"

[oidc]
issuer = "$ISSUER"
sub = "$IDP_SUB"
audience = "$AUDIENCE"
sealed_at = "$STAMP"

# The mock IdP mints whatever scopes /authorize was asked for; the
# binding grants the same set explicitly so admission does not depend on
# the IdP being generous (T23).
[scopes]
allowed = ["cosmon:molecule:read", "cosmon:molecule:write"]
TOML

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

# Arm the harvest door.
#
# `harvest_door::decide` fails closed on a galaxy that has not armed
# `[harvest_authority] required` — it refuses `not_authorized` before it
# has even loaded the molecule. Leaving it unarmed would make the `land`
# step green for the wrong reason: the label under test
# (`land_effect_unavailable`) belongs to the EFFECT half, and it is only
# reached by a decision that admitted the harvest. Arming is the
# operator gesture the tenant cannot make, which is the point of the
# second key — so it is done here, on the host, in a galaxy that lives
# for one run. No seal is minted and none is ever committed.
cat >"$GALAXY/.cosmon/config.toml" <<'TOML'
# Throwaway e2e galaxy. Arms the ADR-176 harvest door so its decision
# half admits and the refusal under test comes from the effect half.
[harvest_authority]
required = true
TOML

# The tenant root must be a git repository: the library tackle executor
# resolves the repo root from it and cuts the worker's worktree with
# `git worktree add`. `ensure_base_commit` covers a commit-less repo, so
# `git init` alone is enough — but a plain directory is not, and the
# failure without this is a `tackle_unavailable` whose cause is three
# layers down in the adapter log.
git init -q "$GALAXY" 2>>"$LOGS/stage.log" \
  || fail stage "$t0" "git init of the throwaway galaxy failed; see logs/stage.log"

# The adapter runs as uid 10000; on a Linux runner the bind-mounted tree
# is owned by the runner's uid and nucleate needs to write into it. The
# worker's worktree and its branch are cut inside this tree too, so the
# permission has to survive the `.git` directory `git init` just made.
chmod -R 0777 "$RUN_DIR/galaxies"
record stage 0 "$(( $(now_ms) - t0 ))" "staged deploy copy (build context $BUILD_ROOT, e2e stage=$E2E_STAGE), nucleon binding for ($ISSUER, $IDP_SUB, $AUDIENCE) → $NOYAU, harvest door armed, galaxy git-initialised"

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
obs_status="$(printf '%s' "$OBS" | jq -r '.molecule.status // "unknown"')"
record observe 0 "$(( $(now_ms) - t0 ))" "molecule $MOL_ID reads back, status=$obs_status"

# ---------------------------------------------------------------------------
# Step 8 — tackle. The claim issue #54 exists to make, asserted against
# the image rather than against a tower of in-process doubles.
#
# What has to be true for this to pass, and was not true before U6:
# the adapter resolves the molecule and its formula in-process, cuts a
# git worktree, writes the dispatch ledger entry BEFORE the spawn, opens
# a tmux session running the resolved adapter command under the worker
# envelope's `env -i`, and pastes the briefing into it — all with no
# `cs` binary anywhere in its own image.
#
# The assertion is the worker SESSION NAME, not the HTTP status. A 200
# carrying no session would satisfy "it did not fail" while describing a
# dispatch that spawned nothing, and that is precisely the shape of the
# lie this whole script exists to catch.
# ---------------------------------------------------------------------------
t0=$(now_ms)
set +e
TACKLE="$(cs_remote --json molecule tackle "$MOL_ID" 2>"$LOGS/tackle.err")"
tackle_rc=$?
set -e
printf '%s' "$TACKLE" >"$LOGS/tackle.out"

if [[ -n "$EXPECT_TACKLE_LABEL" ]]; then
  # Falsifier mode. The scenario is pointed at an image whose `tackle`
  # must REFUSE, and the refusal is named. Everything downstream (a
  # worker, a completion, the door's effect half) is unreachable by
  # construction, so the run ends here rather than reporting a cascade
  # of failures that all say the same thing once.
  [[ $tackle_rc -ne 0 ]] \
    || fail tackle "$t0" "tackle SUCCEEDED, but RPP_E2E_EXPECT_TACKLE_LABEL pinned the refusal '$EXPECT_TACKLE_LABEL'"
  tackle_label="$(grep -o '"error":"[a-z_]*"' "$LOGS/tackle.err" | head -1 | cut -d'"' -f4)"
  [[ -n "$tackle_label" ]] || tackle_label="$(grep -o '"label":"[a-z_]*"' "$LOGS/tackle.err" | head -1 | cut -d'"' -f4)"
  [[ "$tackle_label" == "$EXPECT_TACKLE_LABEL" ]] \
    || fail tackle "$t0" "tackle refused '${tackle_label:-<unnamed>}' (exit $tackle_rc), expected '$EXPECT_TACKLE_LABEL'"
  record tackle 0 "$(( $(now_ms) - t0 ))" "named refusal '$tackle_label', cosmon-remote exit $tackle_rc (falsifier mode: this image cannot dispatch)"
  echo
  echo "==> falsifier run complete — $(wc -l <"$NDJSON" | tr -d ' ') steps recorded"
  exit 0
fi

[[ $tackle_rc -eq 0 ]] \
  || fail tackle "$t0" "POST /v1/molecules/$MOL_ID/tackle failed (exit $tackle_rc): $(tail -2 "$LOGS/tackle.err" | tr '\n' ' ')"
# `.tackle.worker_session`, spelled once. See the nucleate step for why
# there is no `//` fallback chain here.
WORKER_SESSION="$(printf '%s' "$TACKLE" | jq -r '.tackle.worker_session // empty')"
[[ -n "$WORKER_SESSION" ]] \
  || fail tackle "$t0" "tackle answered 200 with no worker_session: $(printf '%s' "$TACKLE" | head -c 200)"
# The worktree is the other half of the dispatch, and it lands on the
# bind mount — so the host can see it. A session name with no worktree
# beside it would mean the ledger and the pane disagree about what was
# dispatched.
[[ -d "$GALAXY/.worktrees/$MOL_ID" ]] \
  || fail tackle "$t0" "no worktree at galaxies/$NOYAU/.worktrees/$MOL_ID — the dispatch spawned a session without cutting one"
record tackle 0 "$(( $(now_ms) - t0 ))" "worker session '$WORKER_SESSION' spawned by the image, worktree cut at .worktrees/$MOL_ID"

# ---------------------------------------------------------------------------
# Step 9 — the worker does its job.
#
# The dummy agent (`fake-claude` in `complete-molecule` mode, staged into
# the e2e image only) reads the briefing the adapter pasted into its
# pane, takes the molecule id out of it, and runs `cs complete`. So this
# step proves three things the tackle step alone cannot: the BRIEFING
# reached the pane, the worker's own environment is usable (`PATH`,
# `COSMON_STATE_DIR` pinned by the envelope), and the tenant store the
# worker writes is the same one the API reads.
#
# Polled through the API, not off the disk: the question is what a
# tenant can observe.
# ---------------------------------------------------------------------------
t0=$(now_ms)
worker_status=""
deadline=$(( $(now_ms) + WORKER_TIMEOUT * 1000 ))
while [[ $(now_ms) -lt $deadline ]]; do
  OBS="$(cs_remote --json molecule get "$MOL_ID" 2>>"$LOGS/worker.log" || true)"
  worker_status="$(printf '%s' "$OBS" | jq -r '.molecule.status // empty')"
  [[ "$worker_status" == "completed" ]] && break
  sleep 2
done
if [[ "$worker_status" != "completed" ]]; then
  # The pane is the diagnosis when this fails, and it dies with the
  # stack — capture it while it still exists.
  compose exec -T rpp-adapter tmux capture-pane -p -t "$WORKER_SESSION" \
    >"$LOGS/worker-pane.log" 2>&1 || true
  fail worker "$t0" "molecule $MOL_ID is '${worker_status:-<unreadable>}' after ${WORKER_TIMEOUT}s, not 'completed' — see logs/worker-pane.log"
fi
record worker 0 "$(( $(now_ms) - t0 ))" "the spawned worker drove $MOL_ID to 'completed' through the tenant store"

# ---------------------------------------------------------------------------
# Step 10 — land. The harvest door on a molecule that is now genuinely
# harvestable: completed by a real worker, in a galaxy whose operator
# armed `[harvest_authority] required` at stage time.
#
# That is what makes this step reach the EFFECT half. Every pre-effect
# refusal — `not_authorized`, `not_completed`, `reservation_requires_seal`,
# `backlog_full` — has been made inapplicable on purpose, so the only
# thing left to answer is the transaction itself, and it answers
# `501 land_effect_unavailable`: the sealed `cs done` path has exactly
# one implementation and it is not callable as a library yet (ADR-176
# §12, the enumerated U6 follow-up). The refusal is the CONTRACT here,
# not a defect to route around — a `202` that integrated nothing is the
# defect issue #51 reported, and a `cs` fallback is what U6 retired.
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
[[ $land_rc -ne 0 ]] || fail land "$t0" "land SUCCEEDED — the sealed effect half has no library implementation, so a success here means the door integrated nothing and said otherwise"
land_label="$(grep -o '"error":"[a-z_]*"' "$LOGS/land.err" | head -1 | cut -d'"' -f4)"
[[ -n "$land_label" ]] || land_label="$(grep -o '"label":"[a-z_]*"' "$LOGS/land.err" | head -1 | cut -d'"' -f4)"
[[ "$land_label" == "$EXPECT_LAND_LABEL" ]] \
  || fail land "$t0" "land refused '${land_label:-<unnamed>}' (exit $land_rc), expected '$EXPECT_LAND_LABEL' — if SealedHarvestEffect grew a library implementation, this is where it announces itself (ADR-176 §12)"
record land 0 "$(( $(now_ms) - t0 ))" "decision half admitted, effect half refused '$land_label' (ADR-176 §12), cosmon-remote exit $land_rc"

echo
echo "==> all steps green — $(wc -l <"$NDJSON" | tr -d ' ') recorded"
