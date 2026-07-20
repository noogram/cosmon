# docker/spore-e2e/Dockerfile — end-to-end validation image for the
# `math-attack` v2 spore (molecule task-20260720-5537, producer-work).
#
# WHY this image exists
# ─────────────────────
# It proves the FIXED cosmon can germinate a whole polymer and drive it to a
# terminal verdict inside a Linux container, entirely on the sovereign LOCAL
# (Ollama) path — no Claude, no cloud, no auth. It is the containerized
# sibling of docker/local-default-container, extended from "drive ONE task-work
# molecule" to "germinate + drive the 14-node math-attack DAG".
#
# The load-bearing property (lifted from local-default-container): there is NO
# `claude` / `aider` / `codex` binary on PATH. The exec→claude escape
# (delib-20260530-0877, turing's finding) is therefore STRUCTURALLY impossible
# — "local is true because there is no other path". Reviewers: do NOT add any
# agent CLI to this image; their absence IS the test.
#
# Extra over the sibling image: a JRE + tla2tools.jar so `cs spore run` can run
# TLC and emit a VERIFIED seal (ADR-140 D4); the driver falls back to an
# explicitly-unchecked seal and records the honest degrade if TLC cannot run.
#
# Build context is the repo ROOT. The spore is staged into
# docker/spore-e2e/_spore_stage/ by build.sh from the canonical sporarium drop
# (kept out of git — an external, unreviewed asset). tla2tools.jar already
# lives at docs/specs/tla2tools.jar.

# ── Stage 1 — build the FIXED `cs` release binary from this worktree ──
# rust:1.88 matches the workspace MSRV (ADR-065); rust-toolchain.toml is
# intentionally NOT copied so the base image's pinned toolchain builds directly.
FROM rust:1.88-bookworm AS builder

# openssl-sys needs libssl-dev + pkg-config; libdbus-sys (pulled transitively,
# e.g. via the keyring/secret-service chain) needs the dbus dev headers.
RUN apt-get update \
    && apt-get install -y --no-install-recommends libssl-dev libdbus-1-dev pkg-config \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
# xtask is a workspace member (root Cargo.toml [workspace].members), so the
# workspace manifest fails to resolve without it — even for `-p cosmon-cli`.
COPY xtask ./xtask
COPY tools ./tools
COPY tests ./tests
COPY examples ./examples
COPY templates ./templates
COPY .cosmon/formulas ./.cosmon/formulas
COPY docs/handbook.md ./docs/handbook.md
# cosmon-cli embeds the gitleaks baseline via include_str! (cmd/init.rs).
COPY assets ./assets

RUN cargo build --release -p cosmon-cli --bin cs \
    && strip /build/target/release/cs

# ── Stage 2 — runtime image: cs + git + tmux + JRE + tla2tools, ZERO claude ──
FROM debian:bookworm-slim

LABEL cosmon.test="spore-e2e"
LABEL cosmon.molecule="task-20260720-5537"
LABEL cosmon.invariant="no-claude-on-PATH"
LABEL cosmon.spore="math-attack-v2"

ENV DEBIAN_FRONTEND=noninteractive

# git + tmux: real workers create a worktree and (for tmux-pane adapters) a
# session. jq + curl: the driver's own tooling. default-jre-headless: the TLC
# seal. claude / aider / codex are DELIBERATELY ABSENT — see header.
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        ca-certificates \
        git \
        tmux \
        jq \
        curl \
        procps \
        libdbus-1-3 \
        default-jre-headless \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /build/target/release/cs /usr/local/bin/cs

# The TLC seal jar, at a path locate_tla2tools() knows (and TLA2TOOLS_JAR).
COPY docs/specs/tla2tools.jar /usr/local/lib/tla2tools.jar
ENV TLA2TOOLS_JAR=/usr/local/lib/tla2tools.jar
# JAVA_HOME so RealTlcRunner::detect() resolves the JRE deterministically.
# NB: even with java + tla2tools + JAVA_HOME all present (TLC verifies spore.tla
# standalone — 452 states, green), `cs spore run` currently reports the seal
# "present, NOT verified" and the harness falls back to --allow-unchecked-seal
# (honestly recorded). That is a cosmon seam in the germinate→TLC wiring, not an
# image gap — surfaced as a finding in the dispatch record, not worked around.
ENV JAVA_HOME=/usr/lib/jvm/default-java

# The shipped spore (staged from the canonical drop by build.sh).
COPY docker/spore-e2e/_spore_stage /opt/spore

# Non-root worker with a fresh, EMPTY HOME — no host cosmon config is reachable.
RUN useradd --create-home --shell /bin/bash cosmon
COPY docker/spore-e2e/germinate-and-drive.sh /usr/local/bin/germinate-and-drive
RUN chmod +x /usr/local/bin/germinate-and-drive \
    && chown -R cosmon:cosmon /opt/spore \
    && mkdir -p /out && chown cosmon:cosmon /out

USER cosmon
WORKDIR /home/cosmon
RUN git config --global user.name  "spore-e2e" \
 && git config --global user.email "spore-e2e@cosmon.invalid" \
 && git config --global init.defaultBranch main

ENTRYPOINT ["/usr/local/bin/germinate-and-drive"]
