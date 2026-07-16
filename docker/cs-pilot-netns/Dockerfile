# docker/cs-pilot-netns/Dockerfile — the ONE test that REQUIRES Linux
# (cs-pilot increment 2, TEST B — task-20260601-070b).
#
# WHY this image exists
# ─────────────────────
# On the macOS dev host, cosmon's autonomy egress guard
# (`cosmon-core::egress`, COSMON_EGRESS_POLICY) can only ever run in
# `EnforcementMode::Advisory`: macOS has no unprivileged network
# namespaces, so a `deny-external` policy is *recorded but not enforced* —
# the command runs unchanged. The real "strict-local worker physically
# unable to reach a remote API" behaviour only exists inside a
# netns-capable Linux kernel = colima.
#
# This image is that kernel. It compiles the workspace's
# `exec_command_netns_e2e` integration test (which exercises the REAL
# `ExecCommand` production path → `EgressJail::wrap` → `unshare --net`)
# and runs it with the `COSMON_NETNS_E2E=1` gate set, so the test stops
# being a no-op skip and actually:
#   1. proves the container has egress (baseline, AllowAll → reaches net),
#   2. proves `deny-external` makes the same probe unreachable (netns),
#   3. proves a local command still runs under the strict policy.
#
# It is a SINGLE-STAGE rust image on purpose: the test IS the product, so
# the toolchain + sources are the runtime. `cargo test --no-run` in the
# build pre-compiles the test (cached in an image layer, network at build
# time only); the entrypoint re-runs it offline, fast, with the gate set.
#
# Ephemeral by construction: the host driver runs `docker run --rm` and
# `rmi`s the image at teardown (no persistent test daemon — no-daemon
# invariant). Reviewers: do NOT add a long-lived service here.

# rust:1.88 matches the workspace MSRV (rust-version = "1.88", ADR-065);
# rust-toolchain.toml is intentionally NOT copied so the base image's
# pinned toolchain builds directly.
FROM rust:1.88-bookworm

# openssl-sys (transitive via native-tls/reqwest) needs system OpenSSL +
# pkg-config to link. procps/iproute2 give the netns probe `ip`-adjacent
# tooling; the probe itself uses bash `/dev/tcp`, but `unshare` (from
# util-linux, already in the base image) is the load-bearing binary.
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        libssl-dev \
        pkg-config \
        iproute2 \
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
# Workspace-wide [patch.crates-io] (vendored matrix-sdk) must resolve even
# for a single -p build — the path must exist in the context.
COPY vendor ./vendor

# Pre-compile the netns e2e test (network available at build time). This
# also fails the BUILD fast if the test ever stops compiling.
RUN cargo test -p cosmon-agent-harness --test exec_command_netns_e2e --no-run

LABEL cosmon.test="cs-pilot-netns-egress"
LABEL cosmon.molecule="task-20260601-070b"
LABEL cosmon.invariant="deny-external-is-enforced-on-linux"
LABEL cosmon.requires="netns-capable-linux-kernel"

COPY docker/cs-pilot-netns/in-container-test.sh /usr/local/bin/netns-egress-test
RUN chmod +x /usr/local/bin/netns-egress-test

ENTRYPOINT ["/usr/local/bin/netns-egress-test"]
