# From an applied environment to a first entry

Use this once the cells are applied and you want to see the first entry go through the API. It is the last proof for gate 1: a person has made a first entry on the proving environment, following this page.

Use the proving environment and a test tenant only. It holds no real money and no customer data, and nothing here changes that. Print counts, never an identifier, a token or a balance, and never paste a log or the script's output into a public place beyond what it prints.

## Before you start

- The cells are applied and healthy (see [First run](first-run.md)).
- You can reach the machines' private addresses: you are on the VPN, or on a machine inside the network. The API has no public address on any cloud.
- You have `curl` and `jq`, and the repository checked out at the release you are proving.

## 1. Publish the API image

1. Run the **publish-api-image** workflow from `main`, and approve its environment. It builds `images/api`, scans every layer, pushes by digest and prints the digest.
2. Put the digest in `infra/images.json` under `api` and open a pull request.
3. Apply `europe/cell-first` again. It makes the tenant's API repositories from the digest in the manifest.
4. Run **publish-images** with `image` set to `api`. It copies the image to each cloud's private registry and checks every copy's digest.

*What it proves:* the image the machines run is the one built from this repository, and nothing else.

## 2. Apply the API machines

1. Apply `europe/interconnect`, so the API machines may call each other on the internal port.
2. Apply `europe/api` through **infra-apply**. Read the plan, and approve it only if it makes machines, secrets, roles and nothing it should not. For each cell it makes one small machine with no public address, one empty secret, and one identity that may read that secret, decrypt with the tenant's key and pull the API image. Set `client_cidrs` to the private range you will test from, or leave it empty and test from a machine in the `api` tier.

*What it proves:* the machines exist, and each can read its own cell's secret and no other.

## 3. Fill each cell's secret

The secrets are made empty, on purpose: their contents never pass through OpenTofu or its state. Two workflows fill them, and you fill two entries.

1. Apply `europe/authority` through **infra-apply**. It makes the authority's signing key in KMS.
2. Fill `db.conn` and `tokens.json` in each cell's secret, with your own cloud sign-in. `db.conn` is the connection string for that cell's own Postgres, and no other cell's. `tokens.json` holds the bearer tokens for the test tenant: token, tenant, principal. Never put either in the repository, an issue, a pull request or a log. The service checks the database's certificate against the system's trusted roots. If the cells' nodes use their own authority, add that authority's public certificate as a third entry, `db-ca.crt`, in the same secret. Without it, a start that cannot trust the database logs "the database's certificate is not from an authority this service trusts (check db-ca.crt)".
3. Run **rotate-credentials** with `kind` set to `all`, and approve its environment. It makes each cell's certificates and cell credentials and writes them into that cell's secret. See [Rotating certificates and credentials](rotating-credentials.md). The line it prints should end `cells still missing a file: 0`.

The `apis` output of the `api` layer says where each secret is. The machine reads its secret again every five minutes, so the files reach it without a restart, and the service starts as soon as every file is there.

*What it proves:* the secret store, the machine's identity and the secret's contents agree.

## 4. Check each cell serves

From inside the network, ask each API for its health. The answer has counts only.

```sh
curl --cacert ca.crt https://<the cell's API address>:8443/healthz
```

`status` should be `ok`. `leases_missing` means the cell holds no lease for some account's home yet. `control_plane_unavailable` means etcd cannot be reached. See the table at the end.

*What it proves:* the service started, reached its database and holds its leases.

## 5. Make the first entry

Put a test tenant's token in a file readable only by you, then run:

```sh
LEDGER_API_URL=https://<first cell's API address>:8443 \
LEDGER_API_TOKEN_FILE=./token \
LEDGER_API_CA_FILE=./ca.crt \
scripts/smoke_write.sh
```

It prints one line for each step and a total. All of these should be `ok`:

| Step | What it proves |
| --- | --- |
| open the payer's account, open the payee's account | The API accepts a token and opens accounts for its tenant at this cell. |
| make the transfer | A journal was posted: both entries, in one transaction. |
| repeat the transfer | The same key and body is answered, not applied twice. |
| the repeat returned the same journal and wrote nothing | Idempotency holds across the cell's database. |
| read both balances | Reads see the entries. |
| the payer was debited exactly once, the payee credited exactly once | The amounts are right, and the repeat did not move value again. |

## 6. Make a transfer across two cells

Run it again with the payee at another cell, on another cloud:

```sh
LEDGER_API_URL_PAYEE=https://<the second cell's API address>:8443 \
LEDGER_API_URL=https://<first cell's API address>:8443 \
LEDGER_API_TOKEN_FILE=./token LEDGER_API_CA_FILE=./ca.crt \
scripts/smoke_write.sh
```

The same steps should be `ok`.

*What it proves:* two cells, on two clouds, finished one transfer by calling each other over mutual TLS, with neither connecting to the other's database.

## 7. Record it

In `docs/evidence/gate-1.md`, write the date, the commit you proved, the clouds, and the last line the script printed (`steps: N, failed: 0`). Do not write anything the script did not print.

## When a step fails

| What you see | What it usually is |
| --- | --- |
| The log says the database's certificate is not from an authority this service trusts | Add the nodes' authority as the `db-ca.crt` entry of the cell's secret. A service reads it when it starts, so restart `ledger-api.service` after. |
| The log says the name in `db.conn` is not one the certificate carries | Use the name the primary's certificate carries (its `primary_address`), not an IP address. |
| The log says the database refused the sign-in, or that nothing answered | Check the user and password in `db.conn`, then the address and the route to the cell's database. |
| The machine never serves | The secret is empty or is missing an entry. The service exits with code 2 and names the file. Run **rotate-credentials**, and fill `db.conn` and `tokens.json` if they are missing. The machine reads the secret again within five minutes. |
| `control_plane_unavailable` | The machine cannot reach etcd: check the etcd addresses in the layer and that the client certificate was issued by etcd's authority. |
| A status class of `4xx` on a step | The token, the tenant or the request is wrong. `401` is the token, `404` is an account that is not this tenant's. |
| `5xx` on the cross-cell step only | The payee's cell cannot be reached from the payer's: check `cells.json`, the interconnect and that the payee's `internal-auth.json` names the payer's cell. |
| Steps pass but a balance check fails | Stop. Do not repeat it. Something wrote twice or not at all, and that is a defect to report, with the counts the script printed. |
