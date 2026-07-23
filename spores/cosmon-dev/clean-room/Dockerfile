# ==========================================================================
# cosmon-dev clean-room — ONE image, pinned by digest, three runtime postures.
# blueprint §4. Register: plain, outgoing-surface.
#
# PIN EVERYTHING (determinism is the point):
#   * base image by OCI DIGEST, not a moving tag;
#   * claude-code EXACT version + autoupdater OFF;
#   * Rust toolchain EXACT release, cargo --locked;
#   * apt via a DATED snapshot;
#   * cosmon source injected by `git archive <affected_ref>` at RUN time (a build
#     ARG placeholder here), NEVER the live checkout.
#
# BUILD:
#   docker build \
#     --build-arg RUST_VERSION=1.83.0 \
#     -t cosmon-dev-cleanroom:pinned \
#     -f Dockerfile .
# Then always run by DIGEST (docker images --digests) so a re-tag cannot swap the
# image under you.
# ==========================================================================

# Pin the base by digest. Replace the digest below with the current
# debian:bookworm-slim digest you validated (docker buildx imagetools inspect
# debian:bookworm-slim), then treat THIS line as frozen for the mission.
# Example shape (DO NOT trust this literal digest; re-pin before use):
#   FROM debian@sha256:<validated-bookworm-slim-digest>
FROM debian:bookworm-slim

# apt via a dated snapshot for reproducibility. Re-pin SNAPSHOT_DATE per mission.
ARG SNAPSHOT_DATE=20260701T000000Z
ARG RUST_VERSION=1.83.0
ARG CLAUDE_CODE_VERSION=2.1.215

ENV DEBIAN_FRONTEND=noninteractive \
    DISABLE_AUTOUPDATER=1 \
    ANTHROPIC_API_KEY="" \
    CARGO_HOME=/opt/cargo \
    RUSTUP_HOME=/opt/rustup \
    PATH=/opt/cargo/bin:/usr/local/bin:/usr/bin:/bin

# Deterministic apt from the dated snapshot mirror. On a machine without snapshot
# access, drop the snapshot line and pin exact package versions instead.
RUN set -eux; \
    printf 'deb http://snapshot.debian.org/archive/debian/%s bookworm main\n' "$SNAPSHOT_DATE" > /etc/apt/sources.list; \
    printf 'Acquire::Check-Valid-Until "false";\n' > /etc/apt/apt.conf.d/10no-check-valid; \
    apt-get -o Acquire::Retries=3 update; \
    apt-get -y --no-install-recommends install \
        ca-certificates curl git jq nodejs npm build-essential pkg-config; \
    rm -rf /var/lib/apt/lists/*

# Rust toolchain pinned to an EXACT release (not `stable`, which moves).
RUN set -eux; \
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | \
        sh -s -- -y --default-toolchain "$RUST_VERSION" --profile minimal; \
    rustc --version

# claude-code EXACT, autoupdater OFF (baked into ENV above).
RUN set -eux; \
    npm install -g "@anthropic-ai/claude-code@${CLAUDE_CODE_VERSION}"; \
    claude --version || true

# The cognitive worker runs demoted to a non-root UID even under repro-root.
RUN useradd -u 10001 -m -s /bin/bash worker

# The three postures are selected at RUN time by run-clean-room.sh (UID + mounts),
# NOT by three images. /output is the only writable path the profiles mount; the
# cosmon source is injected read-only via `git archive <affected_ref>` at run time.
WORKDIR /work
RUN mkdir -p /output && chown worker /output

# Default: do nothing. The entrypoint is the run script, which enforces the
# posture, the no-host-mount assertion, and the network mode.
CMD ["/bin/bash", "-lc", "echo 'cosmon-dev clean-room: invoke via scripts/run-clean-room.sh'"]
