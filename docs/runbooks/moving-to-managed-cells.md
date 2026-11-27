# Moving the proving environment to managed cells, and back

Use this when you want each cell's Postgres run by its cloud (an RDS Multi-AZ cluster, an Azure zone-redundant flexible server, a Cloud SQL regional instance) instead of by three machines running the node image. Use it the other way round to go back.

The steps are the same for every cloud. What differs is how far each cloud has got, and that is the first thing to read.

Every step below changes a real cloud account, so a person runs it, through its workflow. An agent never does. Use the proving environment only. It holds no real money and no customer data, and nothing here changes that.

## What is ready, and what is not

Two settings choose the kind. `cell_kind` sets it for all three cells. `cell_kinds` sets it for one cloud on its own, and wins over `cell_kind` for the cloud it names (K-24i). Today a managed cell can be made in every cloud, but only AWS can sign its API in and be given its role:

| Piece | AWS | Azure | Google Cloud |
| --- | --- | --- | --- |
| The managed database | Yes | Yes | Yes |
| The API machine's right to sign in (K-24ba, K-24bb, K-24bc) | Yes | Not yet | Not yet |
| `rotate-credentials` writes a passwordless `db.conn` and the sign-in word (K-24d) | Yes | Yes | Yes |
| `rotate-credentials` fetches the database's public CA bundle (K-24f, K-24g) | Yes | Not yet | Not yet |
| The one-off unit that makes the application role (K-24cc) | Yes | Not yet | Not yet |

Because of the CA row, `rotate-credentials` stops, before it writes anything, with `a managed database in azure has no public CA bundle this run can fetch yet` (or `gcp`) while any Azure or Google Cloud cell is managed. So **move AWS alone**: set `cell_kinds` to `{"aws":"managed"}` and leave `cell_kind` at its default. **Do not set `cell_kind` to `managed` yet.** It makes three managed databases that cost money, two of which cannot be used, and it stops every credentials run. K-24g, K-24bb and K-24bc bring in the other two clouds, and I-01ve then brings this page up to date for them.

## What you give up

- **No data is carried across.** Changing the kind destroys the old cells and makes new, empty ones. There is no copy. Everything in the old databases is gone: accounts, journals, entries, idempotency keys and the schema.
- **The control plane is not touched.** The `cells` layer does not write to etcd. Etcd keeps each account's home cell and epoch, its lease and every fence. After the move it still names a cell as the home of accounts that cell no longer holds. A new cell gets the fences back from etcd when it rejoins, but never the entries. In a proving environment that is acceptable: make the first entry again afterwards ([First entry](first-entry.md)). If you made accounts you want to keep, do not move.
- **The API machines are not replaced by the switch.** A machine reads its boot configuration once, when it is made. A machine that has to read a new setting is replaced afterwards, one at a time (step 6).

## What it costs

A managed database costs for as long as it exists, running or not.

- **AWS.** A Multi-AZ cluster is a writer and two standbys in three zones. `scripts/proving_power.sh pause --yes` stops the cluster, and AWS starts it again by itself after seven days.
- **Azure.** The flexible server has zone-redundant high availability, and **Azure cannot stop a server that has it**. Pausing for cost leaves it running. Only destroying it stops the cost.
- **Google Cloud.** `scripts/proving_power.sh pause --yes` stops the Cloud SQL instance. Its storage still costs.
- **All three.** Storage costs while paused. See [Pausing for cost](pausing-for-cost.md) for what keeps costing, and for the order to resume in.

A self-hosted cell is three machines, so a paused one costs its disks only. Moving to managed raises the bill for the time it exists. Move back when you have finished.

## Before you start

- You can approve the `infra-apply` and `rotate-credentials` environments, and you are working on `main`.
- Every cell is healthy now, and you have decided you can lose what is in it.
- The API machines run an image that has `ledger-api managed-role` and reads `LEDGER_DB_SIGN_IN_FILE`. If you are not sure, roll the image first: [Rolling a new API image](rolling-the-api-image.md).
- Nothing is paused. [Pausing for cost](pausing-for-cost.md) says how to resume.

## 1. Set the kind

Add the repository secret `TF_VAR_cell_kinds` = `{"aws":"managed"}` (see [How the inputs are given](exposing-the-api-in-aws.md#how-the-inputs-are-given)). It is not a secret, but the secrets are how `infra-apply` is given a setting. Leave `TF_VAR_cell_kind` out. A cloud that `cell_kinds` does not name stays as it is. Leave `TF_VAR_deletion_protection` out. It defaults to `true`, which is right while a managed cell holds data.

## 2. Plan the `cells` layer, and read it

Dispatch **`infra-apply`** with `env` = `europe`, `layer` = `cells` and `action` = `plan`. Read it. You should see:

- The AWS cell's three database machines and their data disks **destroyed**, and its managed database made. The Azure and Google Cloud cells do not appear.
- Nothing else. The `network`, `interconnect`, `private-access` and `api` layers are not in this plan, and an API machine and the load balancer are in the `api` layer. If the plan names anything outside the cells, stop.

The counts change as the modules do, so do not hold to a number. Hold to the shape: machines and disks destroyed, databases made, nothing outside the `cells` layer.

## 3. Apply the `cells` layer

Dispatch **`infra-apply`** again with `action` = `apply`, and approve. The cloud makes each database, which takes a while. Nothing else moves.

## 4. Plan and apply the `api` layer

The `api` layer reads the new cells' outputs. Dispatch **`infra-apply`** with `layer` = `api` and `action` = `plan`. Read it. On AWS you should see the policy that lets the API machine sign in to its database, and no machine replaced. A machine ignores a changed boot configuration, so a plan that replaces one is wrong. Do not apply it; see [Troubleshooting](troubleshooting.md). Then apply it.

## 5. Write the connection and the sign-in word

Dispatch **`rotate-credentials`** with `env` = `europe` and `kind` = `credentials`. Leave `proving` off: a managed cell's `db.conn` has no password, and the run never reads the managed database's administrator secret. Approve it. The run:

- fetches each managed cell's public CA bundle and puts it in the cell's secret as `db-ca.crt`;
- writes `db.conn` (a host, a port, the role `ledger_app` and no password) and `db-sign-in` (the word the service signs in by) into the secret of each managed cell, whatever was there before.

Read its counts line. `database node certificates issued` is `0` for a managed cell, because no machine is its own. `database connections made` is the number of managed cells. `cells still missing a file` should be `0`.

## 6. Make the role and the schema, and replace the API machines

A new database has no application role and no schema. On AWS, follow [Making a managed cell's application role](applying-the-ledger-schema.md#making-a-managed-cells-application-role) and the schema steps above it, in that order: role, then schema, then the API. Both run as one-off units on the API machine, from inside the network, because a managed database has a private address. They need `TF_VAR_create_database_role` and `TF_VAR_migrate_database`, each switched on for one apply.

A machine reads its boot configuration once, so the new setting and the sign-in file reach it only when it is **replaced**. Replace each API machine once, one at a time, as in [Rolling a new API image](rolling-the-api-image.md), step 7, and check each before the next.

## 7. Check

- `/healthz` says `status` `ok` on every cell.
- The machine's console shows `ledger-api: managed role: statements run: 10, failed: 0` and `ledger-api: schema ... is at version ...`. It shows no host, no role and no password.
- Run `scripts/first_entry_check.sh` ([First entry](first-entry.md)). It ends `steps: N, failed: 0`.
- Plan the `api` layer again and expect `0 to change`.
- Delete `TF_VAR_create_database_role` and `TF_VAR_migrate_database`, then plan and apply the `api` layer. The machine can no longer read the administrator's secret, and the service can no longer change the schema.

## Going back to self-hosted

This also destroys the managed database, with no data carried across.

1. Add `TF_VAR_deletion_protection` = `false`. Plan the `cells` layer and apply it. It changes the managed databases in place and destroys nothing. Without this the apply is refused, on purpose: an AWS cluster and a Cloud SQL instance refuse to be deleted while protection is on. Azure's server has no such setting, so it is destroyed without being asked.
2. Delete `TF_VAR_cell_kinds` (it goes back to `self_hosted`), and delete `TF_VAR_deletion_protection` once the cells have changed. Plan the `cells` layer and read it: the managed database **destroyed**, and the cell's three machines and data disks made. Apply it.
3. Plan and apply the `api` layer. Expect no machine replaced and the policy for signing in removed.
4. **Delete the old connection by hand.** A credentials run writes the managed cell's `db-sign-in` and a passwordless `db.conn` and never removes them (K-24h will). Left there, the service tries to sign in by token to a database that wants a password, and cannot. Remove the entries `db.conn` and `db-sign-in` from each cell's API secret, with your own cloud sign-in. Do not print them. Leave `db-ca.crt`: the next step replaces it with the cell's own authority.
5. Run **`rotate-credentials`** with `kind` = `all` and `proving` off. Wait about five minutes for each database machine to start and write its administrator credential. Run it again with `proving` on, which fills the `db.conn` you removed. This is the order in [Rotating certificates and credentials](rotating-credentials.md), "The first run, in order".
6. Replace the AWS API machine as in step 6, so that it stops reading the sign-in setting. Check as in step 7, and make the first entry again.

## If something goes wrong

Stop. A failed plan changes nothing, and so does a failed credentials run. Do not apply while a plan is failing.

| What you see | What it means |
| --- | --- |
| `a managed database in azure has no public CA bundle this run can fetch yet` (or `gcp`) | A managed Azure or Google Cloud cell is in the environment (`cell_kind` set to `managed` does that). See [What is ready](#what-is-ready-and-what-is-not). Nothing was written. |
| The `cells` plan wants to replace or destroy something in the `network` or `api` layers | It is not this layer's plan. Stop and read [Troubleshooting](troubleshooting.md). |
| The apply says the database cannot be deleted | Deletion protection is on, for AWS or Google Cloud. Do step 1 of "Going back". |
| The API does not start and the console says the schema is missing | Step 6 did not finish. The API checks the schema and never changes it. |
| The API does not start and the console names a sign-in step | `db-sign-in` is stale or missing, or the machine was not replaced after step 5. |
| Sign-in is refused after going back | `db-sign-in` is still in the secret. Do step 4 of "Going back". |

Keep logs out of public places, as everywhere else in these runbooks.
