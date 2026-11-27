# Applying the ledger schema to a deployed cell

Use this when an API machine stops with *"the cell's database has no ledger schema at this version: migrate it first"*. A cell's database is built empty. The API checks the schema at start and never changes it. `ledger-api migrate` puts the schema there, and a one-off unit on the API machine runs it from inside the network, where the database can be reached.

The step lets a service change a database schema, so keep it on only for the time it takes.

Every step below changes a real cloud account, so a person runs it, through its workflow. An agent never does.

## What it does

- Runs the same image as the API, with the one word `migrate`. It reads the connection in `db.conn` and nothing else: no listener, no tokens, no certificates.
- Applies each missing migration in order, each in its own transaction. A migration that fails leaves the schema at the last version that worked, and the unit stops.
- Is safe to run again. With nothing to apply it says so and changes nothing.
- Prints one line to the machine's console: `ledger-api: schema <name> is at version <n> (<a> applied, <b> already there)`. It never prints a host, a role or a password.
- Makes the API wait for it. If it fails, the API unit does not start.

With the setting off there is no such unit, and the machine is exactly as it was.

## Before you start

- The cell's database nodes are healthy: one steady container start each on the console.
- The API machine runs an image that has `ledger-api migrate`. If it does not, roll the image first: [Rolling a new API image](rolling-the-api-image.md), steps 1 to 5.
- A machine reads its boot configuration once, when it is made. The setting reaches a machine only when that machine is **replaced**.

## 1. Switch the setting on

Add the repository secret `TF_VAR_migrate_database` = `true` (see [How the inputs are given](exposing-the-api-in-aws.md#how-the-inputs-are-given)). Dispatch **`infra-apply`** with `env` = `europe`, `layer` = `api` and `action` = `plan`. Read the plan. It shows nothing replaced, because a machine ignores boot changes. That is expected.

## 2. Replace the machine

Replace one API machine as in [Rolling a new API image](rolling-the-api-image.md), step 7: plan with `replace` set to its address, read it, then apply and approve. That cell's API is down for a few minutes.

## 3. Check the result

Open "Get system log" for the machine in the cloud's console. Look for the line `ledger-api: schema ... is at version ...`. Then check the cell: `/healthz` says `status` `ok`.

If the unit failed, the log says `the migration failed, and the schema stays at its last good version`, and the API did not start. Nothing is half applied. Fix the cause. The unit tries again every 30 seconds by itself.

## 4. Switch the setting off

Delete `TF_VAR_migrate_database`. Plan the `api` layer, and read it. It shows no change to the machine, which keeps the unit until it is replaced. The unit does no more harm: it finds nothing to apply and ends. Remove it at the next replacement.

Do not leave the setting on. A machine that has it holds a role that can change the schema.

## Making a managed cell's application role

A managed database (RDS) has a private address only, so a hosted runner cannot reach it. A one-off unit on the API machine makes the role the API signs in as, from inside the network, before the schema step (K-24cc). AWS only so far.

What it does: it reads the database's administrator secret with the machine's own role, into a file in memory that only the unit's container sees. It runs `ledger-api managed-role` (ten statements: a role with no password and no powers, tied to `rds_iam`, owning only the `ledger` schema). It removes the file whether it worked or not. It is safe to run again. It prints one line of counts, never a host, a role or a password.

What it costs: while the setting is on, the machine's role can read the administrator's secret. That is the only time it can. Turn it on for one apply and off again.

1. Before: `rotate-credentials` has run for this cell, so its `db.conn` has no password and its `db-sign-in` entry says `aws_iam` (K-24d, K-24e, K-24f). Switch the schema step on too ([above](#1-switch-the-setting-on)), so the order is role, then schema, then the API.
2. Add the repository secret `TF_VAR_create_database_role` = `true`. Plan the `api` layer with `replace` set to the AWS API machine, and read it. It shows the new policy `api-administrator-once` and the machine replaced, and nothing else.
3. Apply and approve. Open "Get system log" and look for `ledger-api: managed role: statements run: 10, failed: 0`.
4. Delete `TF_VAR_create_database_role`. Plan and apply the `api` layer again. The policy is removed at once, and the machine can no longer read the administrator's secret. The unit stays on the machine but is skipped on every later start, because it leaves a marker. It does no harm, and a replacement drops it.

If it fails, the log names the step (for example `the database refused a statement while limiting the role`), and the API does not start. Nothing is half made: each statement can be run again.

## If something goes wrong

| What you see | What it means |
| --- | --- |
| No `ledger-api: schema` line, and the API says "migrate it first" | The machine was not replaced after the setting was on. Do step 2. |
| `cannot connect to the cell's database` | The database nodes are not up, or the machine cannot reach them. Check the nodes first. |
| `the migration failed` | A migration hit an object that is already there, or a database error. The line after it names the cause. The schema stays at its last good version. |
| `the database is newer than this image` | The image is older than the schema. Roll a newer image. |

Keep logs out of public places, as everywhere else in these runbooks.
