# The cell's Postgres node as a container image: vanilla PostgreSQL 16 plus the node agent.
# It runs on any OCI host (Docker, Podman, Kubernetes) inside a customer's own network.
#
#   docker build -f images/postgres/Dockerfile -t ledger-postgres .
#
# What the image holds: PostgreSQL 16 from the official image, with no added extensions, and
# the agent at /usr/local/bin/ledger-node. What it never holds: a password, a key or a
# certificate. Those are mounted when the container starts (see images/postgres/README.md).

FROM rust:1.100-bookworm AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY crates crates
RUN cargo build --release --locked -p node-agent

FROM postgres:16-bookworm

COPY --from=build /src/target/release/ledger-node /usr/local/bin/ledger-node
# The agent reaches a cloud's secret store over HTTPS and must verify its certificate. The
# official Postgres image has the OpenSSL library but no bundle of trusted certificates, so take
# the builder's: the same Debian release, and certificates only.
COPY --from=build /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/ca-certificates.crt

# The writable places, owned by the postgres user. Mount the data volume at
# /var/lib/postgresql/data, and a small in-memory volume at /run/ledger, so that the root file
# system can be read-only.
RUN install -d -o postgres -g postgres -m 0700 /var/lib/postgresql/data /run/ledger /secrets/out \
 # The base image carries Debian's shared "snakeoil" test key. Nothing here uses it.
 && rm -f /etc/ssl/private/ssl-cert-snakeoil.key

USER postgres

# A fast shutdown on stop. PostgreSQL treats SIGTERM as a slow one.
STOPSIGNAL SIGINT
EXPOSE 5432
HEALTHCHECK --interval=10s --timeout=5s --start-period=60s --retries=6 \
  CMD pg_isready -h /run/ledger -q

# The agent prepares the node and then becomes postgres, so postgres is the main process.
ENTRYPOINT ["/usr/local/bin/ledger-node"]
CMD []
