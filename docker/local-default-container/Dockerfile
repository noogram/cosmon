# docker/local-default-container/Dockerfile — ephemeral functional test
# for cosmon's local-default autonomy loop (task-20260601-989e).
#
# WHY this image exists
# ─────────────────────
# Every live verification of the local-default flip this session was
# confounded by four host-side accidents:
#   1. the resident runtime grabbing the test molecule,
#   2. tmux-socket confusion between fleets,
#   3. a real OPENAI_API_KEY in the environment (billing risk),
#   4. the `~/.config/cosmon` claude override masking the local default.
# A clean container removes all four AND makes the local default activate
# BY CONSTRUCTION: no `~/.config/cosmon/config.toml`, no per-galaxy
# `[adapters.default]`, so a bare `cs tackle` (no --adapter flag) falls to
# the built-in `local` floor → Ollama.
#
# The load-bearing property: there is NO `claude` binary on PATH in this
# image. That makes the exec→claude escape (delib-20260530-0877, turing's
# finding) STRUCTURALLY impossible — "local is true because there is no
# other path", a lightweight slice of the autonomy guard
# (task-20260530-d8bc), cheaper than a netns. Reviewers: do NOT add
# claude / aider / codex to this image. Their absence IS the test.
#
# Day-1 scope = option (a) from the deliberation: the container reaches
# the HOST's Ollama (`host.docker.internal:11434`); the isolation is of
# cosmon, not of the model. A future HERMETIC variant (option b: Ollama
# in a sidecar + egress-deny) is documented as deferred in
# docs/guides/local-default-container-test.md.
#
# Adapted from Dockerfile.strace-test (task-20260421-b6ad), which already
# builds `cs` as a Linux release binary in a runtime image WITHOUT claude
# — the same shape, repurposed from a syscall trace harness to a
# local-default functional gate.

# ── Stage 1 — build the `cs` release binary from the current worktree ──
# rust:1.88 matches the workspace MSRV (rust-version = "1.88", ADR-065),
# so rust-toolchain.toml is intentionally NOT copied: the base image's
# pinned toolchain builds directly, no rustup "stable" re-download. Only
# `cargo build` runs here — clippy/fmt are gates the worker runs on the
# host, not steps of this image.
FROM rust:1.88-bookworm AS builder

# openssl-sys (pulled transitively via native-tls/reqwest) needs the
# system OpenSSL headers + pkg-config to link.
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
# Vendored patch of matrix-sdk 0.16.0 (root Cargo.toml [patch.crates-io],
# adds `#![recursion_limit = "512"]`). cargo resolves the workspace-wide
# patch table even for `-p cosmon-cli`, so this path must exist in context.
COPY vendor ./vendor

RUN cargo build --release -p cosmon-cli --bin cs \
    && strip /build/target/release/cs

# ── Stage 2 — runtime image: cs + git + tmux, but ZERO claude ──────────
# git + tmux are present because real workers need them (`cs tackle`
# creates a worktree and, for the tmux-pane adapters, a tmux session).
# The local InProcess adapter uses neither, but a future test of a
# tmux-pane local adapter must not be blocked by their absence.
# jq + curl are the test harness's own tools (assertions + Ollama probe).
# claude / aider / codex are DELIBERATELY ABSENT — see header.
FROM debian:bookworm-slim

LABEL cosmon.test="local-default-container"
LABEL cosmon.molecule="task-20260601-989e"
LABEL cosmon.invariant="no-claude-on-PATH"
LABEL cosmon.scope="day-1-option-a-host-ollama"

ENV DEBIAN_FRONTEND=noninteractive

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        ca-certificates \
        git \
        tmux \
        jq \
        curl \
        procps \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /build/target/release/cs /usr/local/bin/cs

# Non-root worker user with a fresh, EMPTY HOME. The clean-room invariant
# lives here: this HOME has no `~/.config/cosmon/config.toml`, so adapter
# resolution cannot read a host override.
RUN useradd --create-home --shell /bin/bash cosmon
USER cosmon
WORKDIR /home/cosmon
RUN git config --global user.name  "cosmon local-default test" \
 && git config --global user.email "local-default-test@cosmon.invalid" \
 && git config --global init.defaultBranch main

COPY --chown=cosmon:cosmon docker/local-default-container/in-container-test.sh \
     /usr/local/bin/local-default-test
USER root
RUN chmod +x /usr/local/bin/local-default-test
USER cosmon

ENTRYPOINT ["/usr/local/bin/local-default-test"]
