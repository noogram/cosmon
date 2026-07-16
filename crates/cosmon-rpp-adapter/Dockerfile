# syntax=docker/dockerfile:1.7
# ----------------------------------------------------------------------
# cosmon-rpp-adapter — §8j HTTPS+OIDC ingress (ADR-080)
#
# Build context MUST be the workspace root (compose passes `context: ../..`)
# so the workspace `Cargo.lock`, the `[workspace]` Cargo.toml, and every
# member crate referenced via `path = ...` are visible.
#
# Multi-stage layout:
#   1. builder — full Rust toolchain, cargo build --release.
#   2. runtime — debian:bookworm-slim + ca-certificates + the binary.
#
# Library-direct since T-RPP-LIB-DIRECT (`task-20260504-6ad4`): the
# container ships ONLY `cs-rpp-adapter`. The legacy `cs` binary that
# served the §3.5 subprocess envelope was removed after a tenant audit
# caught the silent shell-out at
# request time. Routes now call `cosmon_state::ops::*` in-process; no
# `clone()` / `execve()` per request, no shell, no git, no external
# dependency.
# ----------------------------------------------------------------------

# ----- Builder stage --------------------------------------------------
FROM rust:1.88-bookworm AS builder

# System packages required by the workspace (mostly transitive C deps:
# rusqlite-bundled needs gcc; openssl-sys via reqwest needs pkg-config).
RUN apt-get update \
 && apt-get install -y --no-install-recommends \
        pkg-config \
        libssl-dev \
        cmake \
 && rm -rf /var/lib/apt/lists/*

WORKDIR /work

# Copy the entire workspace verbatim. We do NOT do the cargo-chef dance
# here — it speeds up CI rebuilds but the marginal value for a V0 demo
# image is low, and chef adds a base image hop. Buildkit layer caching
# on the COPY step gets us most of the way.
COPY . .

# Build only the rpp-adapter binary. `--locked` requires the on-disk
# Cargo.lock to match; this is the structural defence against a stale
# workspace lockfile silently picking a different transitive version
# inside the container than on the host.
#
# T-RPP-LIB-DIRECT: the `cs` CLI is no longer built here — the adapter
# calls `cosmon_state::ops::*` directly through its `cosmon-state`
# dependency.
RUN cargo build --release --locked \
        -p cosmon-rpp-adapter --bin cosmon-rpp-adapter

# ----- Runtime stage --------------------------------------------------
FROM debian:bookworm-slim AS runtime

# Canonical container uid/gid. Single source of truth:
# `cosmon_oidc_testkit::COSMON_CONTAINER_UID`. This MUST equal the
# `ARG COSMON_UID` in `crates/cosmon-oidc-testkit/Dockerfile` — both
# images mount the shared `rpp-jwks` volume and Docker stamps files by
# numeric id, not by name. The snapshot trace
# `crates/cosmon-oidc-testkit/tests/uid_seed_snapshot.rs` pins this
# default to the const; edit one and the test tells you the other drifted.
ARG COSMON_UID=10000

# ca-certificates for any TLS client work (reserved for V1+ JWKS HTTP
# fetch). No `cs` binary, no `git`, no `bash` shell out — the
# adapter's request path never spawns a subprocess.
#
# `wget` is included strictly for `docker-compose`'s `healthcheck`
# probe (see `deploy/docker-compose.yml`); the adapter binary itself
# never invokes it. This mirrors the cs-oidc-mock image so the two
# services pick up identical compose-side healthchecks. Note: this
# image is `debian:bookworm-slim` — NOT distroless — so adding a
# probe-only utility is cheap and parity with the AWS Phase 1
# healthcheck assumption (`AWS-PORTABILITY-CHECKLIST.md` §C6) is
# preserved.
#
# `--home-dir /cosmon` makes `/etc/passwd` agree with `ENV HOME=/cosmon`
# below. Without it `useradd` defaults the passwd entry to `/home/cosmon`,
# a directory this image never creates: anything that reads the home from
# the passwd database instead of `$HOME` (getpwuid, `su`, tooling that
# shells out) lands on a path that does not exist, and a compose that
# mounts under it makes Docker auto-create the phantom `/home/cosmon`
# tree at runtime — which defeats `read_only: true` on the rootfs. One
# home, spelled the same way in both places.
RUN apt-get update \
 && apt-get install -y --no-install-recommends \
        ca-certificates \
        wget \
 && rm -rf /var/lib/apt/lists/* \
 && groupadd --system --gid ${COSMON_UID} cosmon \
 && useradd  --system --uid ${COSMON_UID} --gid cosmon \
             --home-dir /cosmon --shell /usr/sbin/nologin cosmon

# Single binary. Stripped to keep the image lean (debug info is
# preserved in the build cache for stack-trace introspection on host
# rebuilds).
COPY --from=builder /work/target/release/cosmon-rpp-adapter /usr/local/bin/cs-rpp-adapter
RUN strip /usr/local/bin/cs-rpp-adapter || true

# cosmon-remote Phase 1 dist binaries (`task-20260522-aad5`). Built on
# the host by `just dist-binaries` *before* `docker build`; this layer
# ships them inside the image so `GET /dist/binary/<platform>/cosmon-remote`
# can stream them to tenants. The directory is gitignored — the host
# build is the source of truth; the image just bundles the output.
#
# When `crates/cosmon-rpp-adapter/assets/binaries/` is empty on the
# host, the COPY still succeeds (sparse directory) but the route will
# 404 with a hint pointing at the build recipe. Sized for context: at
# ~6 MB per binary, 4 platforms add ~24 MB to a ~700 MB image.
#
# Destination `/opt/cosmon-remote/dist` is the canonical dist root
# (`task-20260525-a476`, Pierre v1.5 retour): it matches
# `dist::DEFAULT_DIST_ROOT` AND the smithy `Dockerfile.cosmon-server`
# used for the Harbor bakes. One path everywhere → the adapter serves
# the binaries with no `rpp.toml` `dist_root` override required.
COPY --from=builder /work/crates/cosmon-rpp-adapter/assets/binaries/ \
     /opt/cosmon-remote/dist/

# State directories the operator MUST mount in. The container does not
# create them — leaving them missing is a structural signal that the
# operator hasn't pre-provisioned the JWKS / nucleon binding.
#
# Convention (rpp.toml resolves these against $HOME=/cosmon at boot):
#   /cosmon/.cosmon/state         — JWKS + nucleon mappings
#   /cosmon/galaxies              — per-noyau galaxy roots; the
#                                   library-direct path opens
#                                   /cosmon/galaxies/<noyau>/.cosmon/state/
#                                   for both the read (observe) and the
#                                   write (nucleate) routes.
#   /cosmon/.config/cosmon/rpp.toml — adapter config
RUN install -d -o cosmon -g cosmon -m 0755 \
        /cosmon \
        /cosmon/.config \
        /cosmon/.config/cosmon \
        /cosmon/.cosmon \
        /cosmon/.cosmon/state \
        /cosmon/.cosmon/state/security \
        /cosmon/.cosmon/state/security/jwks \
        /cosmon/.cosmon/state/whispers \
        /cosmon/.cosmon/state/whispers/inbox \
        /cosmon/.cosmon/state/whispers/inbox/api \
        /cosmon/galaxies

# Compose mount targets. LEAN rule (tenant-demo infra, 2026-07-16): every
# mount point of a volume or tmpfs MUST already exist in the image's root
# filesystem. When a target is missing, Docker creates it at run time —
# root-owned, and *writing into the rootfs*, which silently defeats
# `read_only: true`. Pre-creating them here (owned by cosmon) means the
# bind lands on a directory that was already there and the rootfs stays
# untouched.
#
# One entry per mount of the reference avatar compose
# (`deployments/test/docker-compose.yml`, tenant-demo; mirrored by
# `docker-compose.stream-b.yml`):
#   /var/lib/cosmon-state    — COSMON_STATE_DIR, the adapter's base state
#   /var/lib/cosmon/secrets  — sealed Claude secret, mounted :ro
#   /var/lib/whispers        — whispers spool
#   /cosmon/handoff          — forgejo's OAuth issuer publication, :ro
#   /cosmon/.claude          — CLAUDE_CONFIG_DIR
#   /cosmon/.claude.json     — single file the claude adapter rewrites
#                              in place; a file mount needs a *file*
#                              target, not a directory.
# `/tmp` and `/dev/shm` are tmpfs targets that debian:bookworm-slim
# already ships — nothing to add.
RUN install -d -o cosmon -g cosmon -m 0755 \
        /var/lib/cosmon-state \
        /var/lib/cosmon \
        /var/lib/cosmon/secrets \
        /var/lib/whispers \
        /cosmon/handoff \
        /cosmon/.claude \
 && install -o cosmon -g cosmon -m 0644 /dev/null /cosmon/.claude.json

USER cosmon
ENV HOME=/cosmon
WORKDIR /cosmon

# RUST_LOG controls the tracing subscriber. The legacy
# `COSMON_RPP_CS` knob is gone — there is no `cs` binary to point at.
ENV RUST_LOG=info

EXPOSE 8443

# ENTRYPOINT lets us pass extra flags via `docker run ... -- --posture
# active` while keeping the bind/posture defaults stable.
ENTRYPOINT ["cs-rpp-adapter"]
CMD ["--bind", "0.0.0.0:8443", "--config", "/cosmon/.config/cosmon/rpp.toml"]
