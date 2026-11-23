# Rotating certificates and credentials

Use this to issue the API machines' certificates and cell credentials for the first time, and to renew them. One workflow does it, `rotate-credentials`, and nobody types a certificate, a key or a token. It is the same run for the first issue and for every renewal.

## What it does

One central authority signs every cell's certificates (decision D-17). Its key is a signing key in AWS KMS. KMS signs on request and cannot give the key to anyone. For each cell the run makes:

- a **server certificate and key**, which the cell also shows when it calls another cell;
- a **client certificate and key for etcd**;
- the **cell credentials**: for each pair of cells, one credential that the caller sends and the callee lists.

It writes them, with the authority's public certificate, into that cell's API secret as a new version. The entries are the ones the API module lists as `secret_files`. It keeps `db.conn` and `tokens.json` as they were, because the run does not make those. It also keeps an optional `db-ca.crt`, the authority of the cell's database, which the service trusts as well as the system's roots. The run writes it only when the manifest gives a cell one (`db_ca`), and never removes it.

Certificates last **30 days**. Cell credentials are replaced on every run. Each machine reads its secret again every **5 minutes**, and the service reads its files again every **30 seconds** when they change. So a rotation reaches a running service within about six minutes, with no restart and no dropped request.

## How often

Run it with `kind` set to `all` **every 7 days**. Certificates are then replaced at least every 10 days, they have at least 20 days left at every run, and cell credentials are never older than 7 days. A missed week costs nothing. The signer or the workflow can be down for about three weeks before any certificate runs out.

Nothing schedules it yet: a person dispatches it, as for every credentialed workflow. Setting a reminder is the way to keep to the 7 days.

Run it on demand, with the `kind` shown:

| When | `kind` |
| --- | --- |
| The first time, after the `api` and `authority` layers are applied | `all` |
| Every 7 days | `all` |
| A cell was removed from the `api` layer | `credentials` |
| A cell credential may have been seen by someone it should not | `credentials` |
| A certificate's key may have been seen by someone it should not | `certificates`, then see "A leaked key" |

## Before the first run

1. Apply the `authority` layer through **infra-apply**. It makes the KMS key and an empty secret for the authority's public certificate. It is the only layer that makes an authority.
2. Make the identities with the `identities` layer and set up the `rotate-credentials` environment, as in [Credentialed workflows](../credentialed-workflows.md), section 6. They are separate from the ones that apply infrastructure, and may sign and write these secrets and nothing else.
3. Apply the `api` layer, so each cell has a machine and an empty secret.
4. Fill `db.conn` and `tokens.json` in each cell's secret. For a proving environment, tick `proving` and the run fills a missing one from the cell's administrator credential and makes one fresh token for the tenant. An entry that is there is never replaced, so run it again without worry. For any other environment, fill them yourself: they hold the cell's own database connection and the tenant's tokens.

The run works without step 4, and says so: it prints how many cells still lack a file. A machine does not start until every file is there.

## Run it

**Actions, `rotate-credentials`, Run workflow**, on `main`. Choose `env` and `kind`. Leave `settle_secs` at 420 unless the machines' refresh or the service's check has been changed: it is the wait between steps, and it must be longer than both together. Approve the environment when it asks.

A run takes about 15 minutes with three cells, almost all of it the two waits. The job prints one line:

```
cells: 3, certificates issued: 6, credentials issued: 6, secret versions written: 10, database connections made: 0, login files made: 0, cells still missing a file: 0
```

| Count | What it is |
| --- | --- |
| `cells` | Cells in the `api` layer's outputs. |
| `certificates issued` | Two for each cell, when `kind` is `all` or `certificates`. |
| `credentials issued` | One for each ordered pair of cells, when `kind` is `all` or `credentials`: 6 for three cells. |
| `secret versions written` | Three for each cell when credentials are made, one when only certificates are. The first run adds one for the authority's certificate. |
| `database connections made` | Cells whose `db.conn` was missing and was filled this run. Only a `proving` run makes any. |
| `login files made` | Cells whose `tokens.json` was missing and got one fresh token for the tenant. Only a `proving` run makes any. |
| `cells still missing a file` | Cells whose secret lacks a file the service needs. It should be 0 once `db.conn` and `tokens.json` are filled, by a `proving` run or by a person. |

The log shows nothing else. No certificate, key, token, address, secret name or cell name is printed anywhere. Not by the tool, and not by the cloud tools it runs, whose output stays inside it. If you need to know more, ask the cloud: look at a secret's versions with your own sign-in.

## Why there are three steps

A cell calls another with a credential, and the other must already list it, or the call is refused. The cells' secrets are written one at a time and read a few minutes apart, so a single write would refuse transfers while the cells disagreed. So the run goes in order, and waits between steps:

1. **Every cell lists the new credentials as well as the old ones**, and takes its new certificates.
2. **Every cell starts calling with its new credentials.** Each is already listed.
3. **The old credentials are no longer listed.**

At every moment in between, every credential a cell uses is listed by the cell it calls. A cell that is no longer in the `api` layer is not listed after step 1, so it stops being accepted with the first write. That is why removing a cell is the `credentials` run.

## Removing a cell

1. Remove it from the `api` layer and apply.
2. Run `rotate-credentials` with `kind` set to `credentials`.

The removed cell is no longer accepted within about six minutes of the run starting. Its certificate cannot be recalled, because nothing checks a list of withdrawn certificates, but a certificate alone gets it nowhere: every call also needs a credential, and it has none that anyone lists.

## If a run fails

The run fails loudly: the job is red, and the error names a step. It names no cell and no secret. **Run it again.** It is safe to repeat. Every write makes a new version of one secret, so a failed write changes nothing, and every state in between is one in which the cells can still call each other.

| The error says | What it means | What to do |
| --- | --- | --- |
| `the run stopped at step 1 ...` | A write failed in step 1. Cells already written have new certificates and list old and new credentials. Cells not reached are as they were. | Check the run's identity may write the API secrets, then run again. |
| `the run stopped at step 2 ...` | Some cells call with new credentials and some with old. All are listed, so calls still work. | Run again. |
| `the run stopped at step 3 ...` | Old credentials are still listed on some cells. | Run again. Nothing is broken, only less tidy. |
| `the stored authority certificate is not for the signing key` | The KMS key is not the one that made the stored certificate. Nothing was written. | See "Replacing the authority". |
| `a command failed (exit code N) while reading the authority's public key: aws, <cause>` | The run's AWS identity may not use the key, or the key is gone. | Check the role's permissions in [Credentialed workflows](../credentialed-workflows.md), section 6. |
| `a secret could not be read: a cell's database in <cloud> has no administrator credential yet` | A proving run copies the administrator's credential, and that cell's primary has not written it. The primary writes it on its first start, so the cell may be paused, not started, or failed to start. | Check that cell's machines are running, and that its administrator secret has a version (`<cell>/administrator` in AWS, `<cell>-administrator` in Azure and Google Cloud). See [Self-hosted Postgres](../self-hosted-postgres.md). Then run again. |
| `a command failed ... while reading a secret` or `writing a secret: <tool>, <cause>` | The tool is `aws`, `az` or `gcloud`, which says the cloud. The cause is one of `permission refused`, `blocked by a firewall`, `not signed in`, `not found`, `no network` or `another cause`. It is chosen from a fixed list, and nothing the tool said is printed. `permission refused` on `aws` after a key change usually means the role lacks the key (see `TF_VAR_state_encryption_arn` in section 6). `blocked by a firewall` on `az` means the vault's firewall did not open in time. | Check that cloud's role and, for `az`, the vault's firewall, then run again. |

The job's step log files are in the plan store under the run's id, like every other credentialed run. They hold the cloud tools' own words. Read them with your own sign-in, never in a public place.

## Replacing the authority

Do this only on purpose, for example if the KMS key was scheduled for deletion by mistake, or if you want every certificate to stop being trusted at once. Certificates cannot be recalled one by one, so replacing the authority is how you cut off a certificate that was seen.

1. Apply the `authority` layer with a new key. Give the new key's address to the layer's output.
2. Make the stored certificate empty: write a new version of the authority's certificate secret that holds one space.
3. Run `rotate-credentials` with `kind` set to `all`. It makes a new authority certificate for the new key and issues everything again.

While the cells read the new secret, a cell that has the new certificate and one that has the old do not trust each other. Cross-cell transfers fail for a few minutes, and nothing is written wrongly. Do this when a pause is acceptable.

## A leaked key

A cell's private key, if it was seen, can be used until its certificate runs out, up to 30 days. Run `certificates` at once to replace it. To stop the old one being trusted before that, replace the authority.

## What this does not do yet

- **etcd's own members.** The cells' `etcd-ca.crt` is this authority's certificate, so etcd must trust it and show a certificate it signed. The members' certificates are not issued by this run. Until a task makes them, make them from this authority and keep them in each member's secret.
- **`db.conn` and `tokens.json`.** A person fills these, unless the environment is a proving one and the run was given `proving` (D-19). Then the run copies the cell's administrator credential into `db.conn` and makes a fresh token for the tenant. K-19d replaces the copy with a role the node agent makes. Real sign-in replaces the tokens in A-06.
- **Reading the proving token.** Use your own sign-in and keep it out of the log: for AWS, `aws secretsmanager get-secret-value --secret-id <the cell's API secret> --query SecretString --output text | jq -r '."tokens.json" | fromjson | .[0].token' > ~/smoke-token-aws`. Each cell's token is its own.
- **The 7 day rhythm.** A person dispatches it, because a credentialed workflow runs only when a person starts it.
