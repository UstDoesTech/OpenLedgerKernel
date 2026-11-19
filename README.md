# Open Ledger Kernel

A thin double-entry ledger designed to run active-active across regions and clouds on plain PostgreSQL 16. One Postgres database in each region or cloud is a *node*. The rules live in the API layer and in constraints inside each database, so any Postgres (Neon, Lakebase, RDS, Cloud SQL, Azure Database for PostgreSQL or self-hosted) can be a node. There are no extensions, no logical-replication slots and no dependence on platform replication.

Each account has one home cell at a time, fenced by an epoch. Entries are append-only, so replicating them cannot conflict. Cells ship entries to each other as a pull-based outbox over ordinary SQL.

## What is here

| Path | What it is |
|---|---|
| `crates/` | The Rust kernel: `ledger-core`, `ledger-store`, `ledger-repl`, `control-plane`, `ledger-check`, `ledgerctl`, `node-agent` and the `conformance` tests |
| `ledger/`, `tests/` | The Python reference implementation and its tests. The Rust kernel matches every scenario in `tests/test_active_active.py` (see [`docs/conformance.md`](docs/conformance.md)) |
| `schema.sql`, `api/openapi.yaml` | The schema and the command API |
| `images/postgres/`, `infra/` | A Postgres node image, and OpenTofu modules and layers that build the three-cloud architecture: networks, encrypted links, a five-member control plane and a database cell on each cloud |
| `deploy/`, `compose.yaml`, `dev/` | Local three-node environment with fault injection (`make up`) and Kubernetes manifests |

Start with [`docs/design.md`](docs/design.md), then [`docs/api-design.md`](docs/api-design.md). To run it across three clouds, see [`docs/infrastructure.md`](docs/infrastructure.md).

## Try it

```
make up      # three Postgres nodes, etcd and Toxiproxy
make check   # formatting, lints and tests
```

## Licence

Apache License, Version 2.0. See [`LICENSE`](LICENSE) and [`NOTICE`](NOTICE). Copyright 2026 Ust Oldfield. Contributions are accepted under the same licence (section 5 of the licence).

The licence does not cover the name "Open Ledger Kernel", which is not licensed for use as a trademark. The node image contains PostgreSQL and other third-party software, each under its own licence.
