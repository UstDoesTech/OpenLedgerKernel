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

## If something goes wrong

| What you see | What it means |
| --- | --- |
| No `ledger-api: schema` line, and the API says "migrate it first" | The machine was not replaced after the setting was on. Do step 2. |
| `cannot connect to the cell's database` | The database nodes are not up, or the machine cannot reach them. Check the nodes first. |
| `the migration failed` | A migration hit an object that is already there, or a database error. The line after it names the cause. The schema stays at its last good version. |
| `the database is newer than this image` | The image is older than the schema. Roll a newer image. |

Keep logs out of public places, as everywhere else in these runbooks.
