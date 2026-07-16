# docker/cs-pilot-install-clean/Dockerfile — install-clean smoke for the
# cs-pilot cognitive pilot (cs-pilot increment 2, TEST A —
# task-20260601-070b).
#
# WHY this image exists
# ─────────────────────
# The v0 cs-pilot host smoke (crates/cosmon-pilot/SMOKE.md, GREEN
# 2026-06-01) proved `cs pilot --experimental` works ON THE OPERATOR'S
# MACHINE — a machine thick with `~/.cosmon` sediment: a live fleet, a
# `~/.config/cosmon/config.toml`, transcripts, tmux sockets, pulled state.
# That smoke cannot tell a genuine "cs boots from nothing" from "cs boots
# because some host file it silently depends on already exists".
#
# This image is a VIRGIN BOX: a fresh container HOME with NO `~/.cosmon`,
# NO `~/.config/cosmon`, NO transcript, NO state — built fresh from source
# (`cargo build`). It runs `cs pilot --experimental` against the HOST's
# Ollama over a TINY `.cosmon` fixture and asserts the pilot BOOTS, runs a
# REPL turn, and exits 0. Any hidden host-state dependency surfaces here as
# a crash that the host smoke could never catch.
#
# The host driver (scripts/cs-pilot-install-clean-test.sh) runs this image
# on a THROWAWAY colima VM (profile cs-pilot-test, ephemeral) so the
# virginity extends below the container HOME to the docker daemon, image
# cache, and kernel — the fullest "freshly-built cs on a virgin box" the
# muscle allows. Ephemeral by construction: docker run --rm + colima delete.

# ── Stage 1 — build `cs` fresh from the current worktree ───────────────
# rust:1.88 matches the workspace MSRV (rust-version = "1.88", ADR-065),
# so rust-toolchain.toml is intentionally NOT copied.
FROM rust:1.88-bookworm AS builder

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        libssl-dev \
        pkg-config \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
COPY tools ./tools
COPY tests ./tests
COPY examples ./examples
COPY templates ./templates
COPY .cosmon/formulas ./.cosmon/formulas
COPY docs/handbook.md ./docs/handbook.md
# Workspace-wide [patch.crates-io] (vendored matrix-sdk) must resolve.
COPY vendor ./vendor

RUN cargo build --release -p cosmon-cli --bin cs \
    && strip /build/target/release/cs

# ── Stage 2 — runtime: cs + git, ZERO host sediment ────────────────────
FROM debian:bookworm-slim

LABEL cosmon.test="cs-pilot-install-clean"
LABEL cosmon.molecule="task-20260601-070b"
LABEL cosmon.invariant="cs-pilot-boots-on-a-virgin-box"
LABEL cosmon.scope="host-ollama"

ENV DEBIAN_FRONTEND=noninteractive

# git: `cs init`/`cs nucleate` expect a git repo. jq + curl: the harness's
# own assertion + Ollama probe tools. coreutils `timeout`: bound the pilot
# turn so a stuck model can't hang the test. NO claude / aider / codex.
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        ca-certificates \
        git \
        jq \
        curl \
        coreutils \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /build/target/release/cs /usr/local/bin/cs

# Non-root worker with a fresh, EMPTY HOME — the virgin-box invariant:
# this HOME has no ~/.config/cosmon, no ~/.cosmon, no transcript.
RUN useradd --create-home --shell /bin/bash cosmon
USER cosmon
WORKDIR /home/cosmon
RUN git config --global user.name  "cs-pilot install-clean test" \
 && git config --global user.email "install-clean-test@cosmon.invalid" \
 && git config --global init.defaultBranch main

COPY --chown=cosmon:cosmon docker/cs-pilot-install-clean/in-container-test.sh \
     /usr/local/bin/cs-pilot-install-clean-test
USER root
RUN chmod +x /usr/local/bin/cs-pilot-install-clean-test
USER cosmon

ENTRYPOINT ["/usr/local/bin/cs-pilot-install-clean-test"]
