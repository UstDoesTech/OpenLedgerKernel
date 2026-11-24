# The API that runs next to one cell, as a container image.
#
#   docker build -f images/api/Dockerfile -t ledger-api .
#
# What the image holds: the `ledger-api` binary and the libraries it needs. What it never holds:
# a password, a token, a key or a certificate. Those are mounted as files when the container
# starts (see images/api/README.md), and the service reads them from the paths the environment
# names.

FROM rust:1.100-bookworm AS build
WORKDIR /src

# etcd-client compiles its protocol files while it builds, so it needs the protobuf compiler and
# the standard protobuf definitions it imports (libprotobuf-dev; Debian leaves them out of
# --no-install-recommends). The build stage is thrown away, so this adds nothing to the image
# that runs.
RUN apt-get update \
 && apt-get install -y --no-install-recommends protobuf-compiler libprotobuf-dev \
 && rm -rf /var/lib/apt/lists/*
COPY Cargo.toml Cargo.lock ./
COPY crates crates
COPY ui ui
RUN cargo build --release --locked -p api

FROM debian:bookworm-slim

# libssl for the connection to the cell's database; certificates so that nothing has to be mounted
# for the system's own trust store.
RUN apt-get update \
 && apt-get install -y --no-install-recommends libssl3 ca-certificates \
 && rm -rf /var/lib/apt/lists/* \
 && useradd --system --uid 10001 --no-create-home --shell /usr/sbin/nologin ledger

COPY --from=build /src/target/release/ledger-api /usr/local/bin/ledger-api

# Nothing is written to disk, so the root file system can be read-only.
USER 10001
EXPOSE 8443 8444
STOPSIGNAL SIGTERM
ENTRYPOINT ["/usr/local/bin/ledger-api"]
