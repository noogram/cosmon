# cosmon v0.2.1 regression bench — WITH build deps (expected to SUCCEED).
#
# Linux/glibc target that compiles cosmon v0.2.1 FROM SOURCE. This image is the
# positive half of probe #2: it installs pkg-config + libdbus-1-dev (the native
# requirement pulled in by keyring v3 -> secret-service -> libdbus) and so the
# from-source build succeeds. It also serves as the runtime host for the other
# probes' decisive halves (a real built `cs` binary + ollama).
#
# Context is staged by bench/lib/common.sh:stage_docker_context — ./uut is the
# pristine v0.2.1 tree. The bench NEVER mutates cosmon source; it compiles the
# released tree as-is.
FROM rust:1.89-bookworm

# The deps that issue #2 is about. Present here -> build succeeds.
RUN apt-get update && apt-get install -y --no-install-recommends \
        pkg-config \
        libdbus-1-dev \
        libssl-dev \
        git \
        ca-certificates \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /src
COPY uut/ /src/

# Build the cs CLI from the released tree. Failure here is a probe finding.
RUN cargo build --release -p cosmon-cli 2>&1 | tee /tmp/build.log \
    && (test -x target/release/cs || ls -la target/release)

# Marker so probes can assert the image built with deps.
RUN echo "cosmon-bench: built WITH pkg-config + libdbus-1-dev" > /BENCH_BUILD_OK
