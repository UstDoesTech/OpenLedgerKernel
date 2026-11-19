# Postgres node image

One image runs one node of a cell's Postgres, as primary or standby. It is vanilla
PostgreSQL 16 from the official image, with no extensions, no logical replication slots and no
replication slots of any kind. A node agent (`crates/node-agent`) prepares the node and then
becomes `postgres`.

## What a node needs when it starts

| Mount | Holds |
| --- | --- |
| `/etc/ledger/node.json` | The node's configuration (below). |
| `/run/secrets/tls` | `server.crt`, `server.key`, `ca.crt`, and on a standby `replication.crt` and `replication.key`. |
| `/var/lib/postgresql/data` | The data volume. Encrypt it with the customer's key. |
| `/secrets/out` | A directory the primary writes the administrator's credential to, once. Mount the customer's secret store here. |
| `/run/ledger` | A small in-memory volume. |

The image can run with a read-only root file system, all capabilities dropped, no privilege
escalation, and no route to the internet.

## Configuration

```json
{
  "tenant": "ten_acme",
  "node": "cell-a-n1",
  "role": "primary",
  "postgres_version": 16,
  "primary_address": "cell-a-n1.db.internal",
  "synchronous_standby_names": "ANY 1 (cell-a-n2, cell-a-n3)",
  "secret_id": "cell-a/administrator"
}
```

`port` (default 5432), `primary_port` and `secret_store` are optional. A standby has an empty
`synchronous_standby_names`. Unknown fields are refused.

## Where the administrator's credential goes

`secret_store` says where the primary keeps the credential it makes on first start. After
writing it the primary reads it back, and only then sets it in the database.

| `secret_store` | `secret_id` is | Signs in with |
| --- | --- | --- |
| `{"kind": "file", "dir": "/secrets/out"}` (the default) | any name; it becomes the file name | nothing: a mounted directory |
| `{"kind": "aws_secrets_manager"}` | the secret's ARN, `arn:aws:secretsmanager:<region>:<account>:secret:<name>` | the machine's role |
| `{"kind": "azure_key_vault"}` | `https://<vault>.vault.azure.net/secrets/<name>` | the machine's managed identity |
| `{"kind": "gcp_secret_manager"}` | `projects/<project>/secrets/<name>` | the machine's service account |

The cloud stores take no key, token or address. The agent gets a short-lived token from the
machine's metadata service each time and reaches the store over HTTPS, so the machine needs a
private route to it (a private endpoint) and the right to write and read that one secret. A
reference in the wrong form stops the node before it makes a database. In the AWS case, run the
container so that it can reach the metadata service (a hop limit of 2, or host networking).

## How the nodes behave

- **Commits.** The primary returns a commit once any one standby has it, so losing one node
  loses nothing. With both standbys down, commits wait.
- **TLS.** TLS 1.3 only. A connection without TLS matches no access rule. Standbys prove who
  they are with a client certificate whose name is `replicator`. There is no replication
  password. The primary's certificate must name `primary_address`.
- **Administrator.** On its first start the primary makes a 256-bit password for the role
  `ledger_admin`, writes it to the secret store, and sets it. It is never logged.
- **Role checks.** A node refuses to start if its configuration disagrees with its data. A
  promoted standby is not quietly turned back into a follower.
- **Failover is manual.** Promote a standby on purpose, then change its configuration.
