# syntax=docker/dockerfile:1.7
# ----------------------------------------------------------------------
# cs-oidc-mock — V0 demo IdP for cosmon-rpp-adapter (ADR-080)
#
# DEMO ONLY. Embedded RSA private key is committed in plaintext under
# crates/cosmon-oidc-testkit/assets/test_rsa_private.pem; do NOT deploy
# this image outside an embargoed test loop. Replace with Keycloak
# self-hosted (or equivalent) for V1+.
#
# Build context MUST be the workspace root (compose passes `context:
# ../..`) — the binary depends on path-resolved workspace deps.
# ----------------------------------------------------------------------

# ----- Builder stage --------------------------------------------------
FROM rust:1.88-bookworm AS builder

RUN apt-get update \
 && apt-get install -y --no-install-recommends \
        pkg-config \
        libssl-dev \
        cmake \
 && rm -rf /var/lib/apt/lists/*

WORKDIR /work
COPY . .

RUN cargo build --release --locked -p cosmon-oidc-testkit --bin cs-oidc-mock

# ----- Runtime stage --------------------------------------------------
FROM debian:bookworm-slim AS runtime

# Canonical container uid/gid. Single source of truth:
# `cosmon_oidc_testkit::COSMON_CONTAINER_UID`. This MUST equal the
# `ARG COSMON_UID` in `crates/cosmon-rpp-adapter/Dockerfile` — both
# images mount the shared `rpp-jwks` volume and Docker stamps files by
# numeric id, not by name. The snapshot trace
# `crates/cosmon-oidc-testkit/tests/uid_seed_snapshot.rs` pins this
# default to the const; edit one and the test tells you the other drifted.
ARG COSMON_UID=10000

RUN apt-get update \
 && apt-get install -y --no-install-recommends \
        ca-certificates \
        wget \
 && rm -rf /var/lib/apt/lists/* \
 # uid/gid match `cosmon` in cs-rpp-adapter so the shared `rpp-jwks`
 # named volume is writable from both sides without a chmod-777 hack.
 # (Docker initialises empty named volumes from the first container
 # to mount them; with identical uids we don't care which one wins.)
 && groupadd --system --gid ${COSMON_UID} oidcmock \
 && useradd  --system --uid ${COSMON_UID} --gid oidcmock --shell /usr/sbin/nologin oidcmock

COPY --from=builder /work/target/release/cs-oidc-mock /usr/local/bin/cs-oidc-mock
RUN strip /usr/local/bin/cs-oidc-mock || true

# Output directory the operator typically mounts — `--write-jwks-out`
# lands here so the rpp-adapter (sharing the same volume) can pick the
# JWKS up at boot. We pre-create `/shared/jwks` (not just `/shared`)
# so that when docker-compose mounts the named volume directly at
# `/shared/jwks`, the empty volume inherits the oidcmock ownership
# from this layer rather than starting root-owned and EACCES-ing the
# binary's first write.
RUN install -d -o oidcmock -g oidcmock -m 0755 /shared /shared/jwks

USER oidcmock
ENV RUST_LOG=info
EXPOSE 8444

ENTRYPOINT ["cs-oidc-mock"]
CMD ["--bind", "0.0.0.0:8444"]
