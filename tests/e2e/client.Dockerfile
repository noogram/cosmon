# SPDX-License-Identifier: AGPL-3.0-only
#
# The "other machine" of the cross-machine `work` acceptance (issue #147 W4):
# a Linux `cosmon-remote` and nothing else. It is NOT a shipped image — the
# product ships the client as a binary, and this file exists only so the
# second client can run in a container with no bind mount of the host.
#
# Built from the same workspace and lockfile as the adapter image: `BUILDER`
# is that image's `builder` stage (tagged by `harness.work.build_client_image`,
# a layer-cache hit once the adapter has been built), so the client is the
# same source at the same revision, not a binary copied from elsewhere.
ARG BUILDER
FROM ${BUILDER} AS client-builder

# `cosmon-remote` links the Linux keyring backend, whose `libdbus-sys` build
# script panics when pkg-config finds no `dbus-1`. The client is driven with
# the file credential backend; the library is only linked.
RUN apt-get update \
 && apt-get install -y --no-install-recommends libdbus-1-dev \
 && rm -rf /var/lib/apt/lists/*
RUN cargo build --release --locked -p cosmon-remote --bin cosmon-remote

FROM debian:bookworm-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates libdbus-1-3 libssl3 \
 && rm -rf /var/lib/apt/lists/* \
 && useradd --create-home --home-dir /home/tester --uid 10001 tester
COPY --from=client-builder /work/target/release/cosmon-remote /usr/local/bin/cosmon-remote
USER tester
WORKDIR /home/tester
