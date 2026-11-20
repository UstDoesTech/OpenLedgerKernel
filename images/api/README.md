# The API image

The service in `crates/api`, run next to one cell. It holds no secret. Start it read-only, with
every capability dropped, and mount its secrets as files.

```sh
docker build -f images/api/Dockerfile -t ledger-api .
```

## What it needs at start

| Variable | Holds |
| --- | --- |
| `LEDGER_NODE` | The cell it runs next to. |
| `LEDGER_DB_FILE` | A file with the cell's Postgres connection string. |
| `LEDGER_TOKENS_FILE` | The customers' bearer tokens: token, tenant, principal. |
| `LEDGER_LISTEN` | The public address. Not loopback means TLS is required. |
| `LEDGER_INTERNAL_LISTEN` | The address for calls from other cells. Needs mutual TLS. |
| `LEDGER_INTERNAL_AUTH_FILE` | The cells that may call it, with their credentials. |
| `LEDGER_CELLS_FILE` | The other cells' addresses and the credential to call each. |
| `LEDGER_TLS_CERT_FILE`, `LEDGER_TLS_KEY_FILE`, `LEDGER_TLS_CA_FILE` | This cell's certificate and key, and the authority for every cell. |
| `LEDGER_ETCD_ENDPOINTS` | The etcd client addresses. |
| `LEDGER_ETCD_CA_FILE`, `LEDGER_ETCD_CERT_FILE`, `LEDGER_ETCD_KEY_FILE` | The authority that signed etcd's certificates, and the client certificate and key this cell presents. All three or none. An https endpoint needs all three. |

The full list, with defaults, is at the top of `crates/api/src/main.rs`. The service exits with
code 2 for any configuration problem, and says which variable.

## Running it

```sh
docker run --read-only --cap-drop ALL --security-opt no-new-privileges \
  --tmpfs /tmp --volume /etc/ledger-api/secrets:/run/secrets/api:ro \
  --env-file ledger-api.env ledger-api
```

`infra/modules/api` does this on a machine of its own next to each cell, and gets the secret
files from the cloud's secret store with the machine's own identity.
