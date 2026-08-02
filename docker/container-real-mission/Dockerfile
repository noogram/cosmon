# docker/container-real-mission/Dockerfile — the missing arm of the
# container bench: a REAL mission driven through the production dispatch
# path, inside the external tester's published environment, up to the
# credential gate (issue #20, task-20260726-1f8e).
#
# The two build stages below are byte-identical to
# docker/container-worker-doors/Dockerfile on purpose. That is not
# copy-paste laziness: the environment fidelity argument of issue #20 is
# only transferable if this image's userland is the SAME userland, and
# identical instructions also mean the (expensive) builder layers are
# shared with the doors bench instead of rebuilt.
#
# THE ONE DIFFERENCE from the doors bench, and the whole point: this
# image mints NO placeholder credential. The doors bench stops in front
# of a file named PLACEHOLDER-NOT-A-CREDENTIAL; this one arrives at the
# gate empty-handed and measures the refusal itself.
#
# (original header of the shared stages follows)
#
# docker/container-worker-doors/Dockerfile — replay bench for the three
# container startup doors of issue #20 (task-20260725-ec02).
#
# WHY this image exists
# ─────────────────────
# An external tester (@jdthaler, issue #20 on the public noogram/cosmon
# repo) reported two container failure modes against the signed v0.3.0 and
# then published a complete, SECRET-FREE reproduction recipe. Three fixes
# landed on `feat/container-worker-doors` in answer:
#
#   9123ec9  chown the worktree to the demote target before the preflight
#   bb4554d  pre-grant Claude Code startup consent (folder trust + bypass)
#   c454422  refuse a TUI claude worker that has no credential
#
# Until this bench existed, every public message we sent carried the same
# honest reservation: "fixed, but not proven on the reporter's bench". This
# image IS that bench. It transcribes the tester's recipe and changes
# exactly one thing: the `cs` under test is built from the LOCAL worktree
# instead of installed from the v0.3.0 tag, because the point is to
# exercise our corrected code.
#
# WHAT IS DELIBERATELY NOT HERE — read before extending
# ────────────────────────────────────────────────────
# NO CREDENTIAL OF ANY KIND. The tester's recipe carries an optional step
# that seeds a credentials file from an environment variable; it is NOT
# transcribed here and must never be. Two of the three doors are provable
# with zero secrets (see docs/guides/container-worker-doors-bench.md), and
# the third is exercised with an obviously-invalid PLACEHOLDER credential
# minted inside the container by the bench script — never a real token,
# never read from the host, never logged.
#
# Reviewers: do not add ANTHROPIC_API_KEY, CLAUDE_CODE_OAUTH_TOKEN, or a
# host credential mount to this image. Their absence is part of the test.

# ── Stage 1 — build `cs` from the current worktree ─────────────────────
# rust:1.88 matches the workspace MSRV (rust-version = "1.88", ADR-065),
# so rust-toolchain.toml is intentionally NOT copied: the base image's
# pinned toolchain builds directly, with no rustup "stable" re-download.
FROM rust:1.88-bookworm AS builder

# openssl-sys (transitively via native-tls/reqwest) needs system OpenSSL
# headers; libdbus-1-dev is in the tester's own package list and is a real
# build dependency of the keychain/secret-service probe path.
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        libssl-dev \
        libdbus-1-dev \
        pkg-config \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
COPY xtask ./xtask
COPY tools ./tools
COPY tests ./tests
COPY examples ./examples
COPY templates ./templates
COPY .cosmon/formulas ./.cosmon/formulas
# cosmon-cli also include_str!s the gitleaks baseline from the repo root.
COPY assets ./assets
# cosmon-cli embeds the operator handbook via include_str!.
COPY docs/handbook.md ./docs/handbook.md
# NB: no `COPY vendor` — the vendored matrix-sdk patch and its only
# consumer were removed in the pre-publication scope trim
# (delib-20260622-187a B1), so the workspace has no [patch.crates-io] left.

RUN cargo build --release -p cosmon-cli --bin cs \
    && strip /build/target/release/cs

# ── Stage 2 — the tester's runtime image, transcribed ──────────────────
# Base image and package list are HIS, verbatim from the recipe, so that a
# divergence in our verdict cannot be blamed on a different userland.
FROM rust:1-bookworm

LABEL cosmon.test="container-worker-doors"
LABEL cosmon.molecule="task-20260725-ec02"
LABEL cosmon.issue="20"
LABEL cosmon.invariant="no-real-credential-in-image"

ENV DEBIAN_FRONTEND=noninteractive

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        tmux \
        jq \
        curl \
        ca-certificates \
        procps \
        pkg-config \
        libdbus-1-dev \
        util-linux \
    && rm -rf /var/lib/apt/lists/*

# `cs` under test — OUR branch, not the v0.3.0 tag. This is the one
# deliberate deviation from the tester's recipe.
COPY --from=builder /build/target/release/cs /usr/local/bin/cs

# The demotion target, exactly as the recipe creates it.
RUN useradd -u 10001 -m -s /bin/bash cosmon-worker

# `claude` itself. The recipe omits the install step, but the tester's
# scenario 2 observes a live TUI panel, so the binary is necessarily
# present on his bench. Installed to a world-readable prefix so both root
# and uid 10001 can exec it. This downloads a binary, not a credential.
# The installer ignores a destination override and always lands in
# `$HOME/.local/share/claude/versions/<v>` with a symlink from
# `$HOME/.local/bin/claude` — under /root here, i.e. unreachable by uid
# 10001, which would make every arm refuse on "claude not found" instead
# of on the door under test. So the tree is relocated to /opt and linked
# onto PATH. `chmod -R a+rX` is what makes the demoted arms possible at
# all; without it the demote arms measure a missing binary.
RUN curl -fsSL https://claude.ai/install.sh -o /tmp/claude-install.sh \
    && bash /tmp/claude-install.sh \
    && rm -f /tmp/claude-install.sh \
    && mv /root/.local/share/claude /opt/claude \
    && chmod -R a+rX /opt/claude \
    && ln -sf "$(readlink /root/.local/bin/claude | sed 's#/root/.local/share/claude#/opt/claude#')" \
              /usr/local/bin/claude \
    && rm -rf /root/.local/bin/claude \
    && chmod -R a+rX /usr/local/bin \
    && claude --version

# The mission directory the walkthrough works in, created here — at build
# time, by root, once — rather than by a root `docker exec` the reader has to
# remember at run time. Issue #32 measured what its absence costs: the guide
# told the reader to open the shell with `-w /srv/mission` and then, once
# inside, to `mkdir -p /srv/mission`. Neither can run first, and uid 10001
# cannot write into a root-owned `/srv` at all, so a reader following the page
# in order was stopped at both ends. Shipping the directory already owned by
# the uid that will fill it is the same rule as ADR-165: the resource is
# created under the identity that consumes it, and nothing is handed over.
RUN mkdir -p /srv/mission && chown 10001:10001 /srv/mission

WORKDIR /work

COPY docker/container-real-mission/in-container-mission.sh \
     /usr/local/bin/container-real-mission
COPY docker/container-real-mission/login-in-container.sh \
     /usr/local/bin/container-real-mission-login
COPY docker/container-real-mission/in-container-nonroot-pilot.sh \
     /usr/local/bin/container-nonroot-pilot
RUN chmod +x /usr/local/bin/container-real-mission \
             /usr/local/bin/container-real-mission-login \
             /usr/local/bin/container-nonroot-pilot \
 # A harness whose verdict block does not parse runs every measurement,
 # prints them all, and then dies before writing the record — which reads
 # as INCONCLUSIVE over a run that was actually green. Measured once, on
 # an apostrophe inside a single-quoted jq program. Parsing is checked at
 # BUILD time so the failure cannot be discovered at the end of a
 # twenty-minute run.
 && for s in /usr/local/bin/container-real-mission \
             /usr/local/bin/container-real-mission-login \
             /usr/local/bin/container-nonroot-pilot; do bash -n "$s"; done

# Provenance of the bytes, baked in at build time (ADR-165 capture).
#
# A capture that names a source commit but was produced by older bytes is
# worse than no capture — `cs` printed "your binary was NOT refreshed" on
# every harvest for a full day and it was read as noise. So the commit AND
# the working-tree state at build time travel INSIDE the image, and the
# harness reads them from there rather than being told them by whoever
# writes the report. `COSMON_SOURCE_CLEAN` must be the literal `clean` for
# the commit to describe the bytes; anything else is a finding the harness
# raises on its own.
#
# These ARGs sit at the very tail on purpose: a build arg invalidates its
# layer and everything after it, and everything expensive is above.
ARG COSMON_SOURCE_SHA=unknown
ARG COSMON_SOURCE_CLEAN=unknown
RUN printf '%s\n' "$COSMON_SOURCE_SHA"   >/etc/cosmon-source-sha \
 && printf '%s\n' "$COSMON_SOURCE_CLEAN" >/etc/cosmon-source-clean

# Labels sit at the TAIL, not beside the tester's `FROM`, so that every
# instruction this image shares with the doors bench stays cache-identical.
# A LABEL near the top would invalidate the apt layer and the (networked)
# Claude Code install below it for nothing.
LABEL cosmon.test="container-real-mission"
LABEL cosmon.molecule="task-20260726-1f8e"
LABEL cosmon.issue="20"
LABEL cosmon.invariant="no-credential-of-any-kind-in-image"

ENTRYPOINT ["/usr/local/bin/container-real-mission"]
