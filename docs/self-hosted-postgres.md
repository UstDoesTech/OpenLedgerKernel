# Running a cell's Postgres in the customer's own network

A cell's Postgres can run as three containers inside a customer's network, with no cloud
account of ours involved. The image is built from [`images/postgres`](../images/postgres/README.md).
It is vanilla PostgreSQL 16 with no extensions and no replication slots. This guide says what
the customer provides, what the platform needs from their network, and how to run it on
Kubernetes or on plain hosts.

## What you get

- Three nodes: one primary and two standbys, one to each zone or host.
- A commit returns once the primary and at least one standby have it. Losing any one node
  loses nothing.
- TLS 1.3 on every connection. Standbys prove who they are with a certificate, so there is no
  replication password.
- An administrator password made on first start. By default it is written once to a volume for
  the customer to keep in their own secret store. Inside AWS, Azure or Google Cloud the node can
  write it straight to that cloud's secret store instead, using the machine's own identity (see
  [the image's README](../images/postgres/README.md#where-the-administrators-credential-goes)).

**Failover is manual.** If the primary is lost, a person promotes a standby. See
[Failover](#failover-is-manual).

## What the customer provides

| Needs | Why |
| --- | --- |
| A private registry that holds the image, pulled by digest | The nodes have no route to the internet. |
| Three zones, or three separate hosts | One node in each, so a zone or a host can fail. |
| Volumes encrypted with the customer's own key | The data and the credential live on them. |
| Certificates from the customer's own authority | They name the nodes and the standbys. See below. |
| A secret store | To keep the administrator's credential after first start. |
| Network rules for the platform | See the next section. |

We build the image and a person publishes it. Copy it into your registry and note its digest.

## What the platform needs from your network

One port: TCP 5432, the `postgres` port.

- **Between the three nodes.** Each node must reach the others.
- **From the platform.** The api and cell tiers must reach every node (the flows
  `api-to-cell-postgres` and `cell-to-cell-postgres`). Give the ranges they connect from.
  The Kubernetes network policy holds the document range `192.0.2.0/24` until you replace it,
  so until you do, no platform traffic is let in.
- **Nothing else.** The nodes need no route to the internet. The policy lets out only the
  peers and name lookups. It does not open the relay port or any etcd port.

## Certificates

Five files, from one authority the nodes and the platform trust.

| File | What it must be |
| --- | --- |
| `ca.crt` | The authority's certificate. |
| `server.crt`, `server.key` | A server certificate. It must name every address the nodes use to reach the primary. On Kubernetes that is `ledger-pg-0.ledger-pg.<namespace>.svc`, `ledger-pg-1...` and `ledger-pg-2...`. On plain hosts it is each host's name. |
| `replication.crt`, `replication.key` | A client certificate whose common name is `replicator`. |

A standby checks the primary's name before it trusts it, so a certificate that does not name
the primary is refused. Keep the keys readable by the container user (uid 999) only.

### On machines in a cloud (the OpenTofu modules)

The modules in `infra/modules/postgres` make one secret for each node, in the cloud's own secret
store: Secrets Manager on AWS, Key Vault on Azure, Secret Manager on Google Cloud. The secret
holds the node's files as one JSON object, with an entry for each file name above:
`{"server.crt": "...", "server.key": "...", "ca.crt": "..."}`. A standby's secret also has
`replication.crt` and `replication.key`.

- **Who puts the values in.** Whoever issues the certificates: the customer's own authority, or
  a certificate manager they run. In this platform's own environments that is the
  `rotate-credentials` workflow (K-20b), which issues them from the platform's authority.
  OpenTofu makes the secret and never its contents, so no
  certificate or key is in the repository, the boot file or the state. On AWS and Google Cloud
  the secret starts empty. On Azure it starts as `{}`, and OpenTofu ignores any later value.
- **Where to find them.** The `cells` layer's `cells` output lists each self-hosted cell's three `nodes`: name, role (`primary` or `standby`), private address, and `secret_ref`, the identifier of the node's secret. A certificate must name the primary's address and the node's own. The output holds no certificate or key.
- **Who may read it.** Each node, with its own cloud identity, and no other node. Each node has
  an identity of its own for this. A node cannot read another node's certificates.
- **When it is read.** At every start, before the container runs, by a script on the machine.
  It tries for about two minutes, because a new machine's access takes a short while to apply.
  A missing file stops the start. The node never runs on half its certificates.
- **Renewal.** Add a new version of the secret, then restart the node (`systemctl restart
  ledger-postgres`), one node at a time, standbys first. A node reads the certificates when it
  starts and not before. Renew well before they expire.
- **What nothing here does.** The platform does not issue, rotate or watch the expiry of
  certificates. That stays with the customer's authority.

## On Kubernetes

The manifests are in [`deploy/kubernetes`](../deploy/kubernetes/kustomization.yaml): a
StatefulSet of three pods spread across zones, a headless service, a pod disruption budget and
a network policy. Pods run as non-root with a read-only root file system and all capabilities
dropped.

1. **Make the namespace and the certificate secret.**
   `kubectl create secret generic ledger-pg-tls -n ledger --from-file=...` with the five files
   above. Or have your certificate manager make it.
2. **Make an overlay.** Copy `deploy/kubernetes` and change, in your copy, the image name and
   digest, the storage class (`ledger-encrypted` is a placeholder for one that encrypts with
   your key), `TENANT`, `SECRET_ID` and the network policy's platform ranges.
3. **Apply it.** `kubectl apply -k <your overlay>`. The three pods start together. The
   standbys wait for the primary.
4. **Collect the credential.** On first start the primary writes it to its `credential` volume.
   The file name is the `SECRET_ID` with anything but letters, digits, dots, hyphens and
   underscores turned into underscores, so `cell-a/administrator` is `cell-a_administrator`.
   Read it with `kubectl exec -n ledger ledger-pg-0 -- cat /secrets/out/cell-a_administrator`,
   keep it in your secret store, and tell us the reference. It stays on the encrypted volume.
5. **Check.** Run the query in [Checking](#checking).

Pods use an `OnDelete` update strategy. A change to the manifests does not restart a database.
A person deletes one pod at a time, standbys first.

## On plain hosts

[`deploy/compose`](../deploy/compose/compose.yaml) runs the same nodes with Docker Compose.

- **To try it on one machine.** Run `deploy/compose/make-test-certs.sh` for throwaway
  certificates, set `LEDGER_IMAGE`, run `docker compose up -d`, then `deploy/compose/verify.sh`.
  Never use those certificates for real data.
- **On three hosts.** Run one service on each host. In `config/pg1.json`, `config/pg2.json` and
  `config/pg3.json`, put the real host names in `primary_address`, and make the server
  certificate name them. Bind the published ports to the private address, never a public one.
  Collect the credential from the primary's `credential` volume as above.

## Checking

On the primary, with the administrator's password in `PGPASSWORD`:

```sql
select count(*) from pg_stat_replication where sync_state = 'quorum';  -- 2
select count(*) from pg_replication_slots;                              -- 0
```

`deploy/compose/verify.sh` does this and also commits a row and checks both standbys have it.

## Failover is manual

If a standby is lost, nothing stops: the primary commits with the other one. If the primary is
lost, writes stop until a person promotes a standby. No automatic failover runs in this form.

What the nodes do to keep you safe:

- Every committed change is on at least one standby, so losing one node loses nothing. Losing
  the primary and a standby together can lose changes that only they held.
- Which node is the primary is a setting (`PRIMARY_ORDINAL` on Kubernetes, the config file on
  plain hosts). A node refuses to start if its setting disagrees with its data. A promoted
  standby is not turned back into a follower, and an old primary is not started as a second
  primary. The refusal is the guard against two primaries.

The steps in outline: stop the old primary and keep it stopped, promote the standby that has
received the most of the log, change the setting so that standby is the primary, restart the
other standby so it follows the new primary, and rebuild the old primary from the new one by
emptying its data volume. **These steps follow from how the agent behaves. They have not been
rehearsed against real failures.** The failover tasks in the plan do that.

## What this does not cover

- Backups, upgrades and monitoring. Plan these for the customer's side.
- Automatic failover.
- Running the Kubernetes manifests in a live cluster. CI checks them against the Kubernetes
  schema and the rules above, and runs the same nodes through Compose against the real image.
