# Rolling a new API image onto running cells

Use this when you have built a new API image and the running API machines still use the old one. It is also how a change to a machine's boot configuration reaches a running cell, such as a new environment setting (K-22 added `LEDGER_USERS_FILE`).

## Why a restart is not enough

Each API machine's boot configuration names the image by digest and is read once, when the machine is made. After that the machine ignores changes to it (`ignore_changes` on `user_data`, `custom_data` and `metadata["user-data"]`). Restarting the service pulls the same digest again, and a plan that changes the digest changes only `terraform_data.image`, never a machine. So a new image or a new setting reaches a cell only when its API machine is **replaced**, one at a time, on purpose.

Every step below changes a real cloud account, so a person runs it, through its workflow. An agent never does.

## Before you start

- The environment is `europe`, or the one you are rolling. Its `cell-first`, `registries` and `api` layers have been applied.
- You are on `main`, as the repository's owner, and can approve the `infra-apply` and `rotate-credentials` environments.
- Every cell is healthy. From inside the network, run `scripts/first_entry_check.sh` (see [First entry](first-entry.md)) or ask each cell for `/healthz`: `status` is `ok`. Stop if one is not.

## 1. Publish the image

Dispatch **`publish-api-image`** on `main`. It has no inputs. When it finishes, read the **Digest** in the job summary: `sha256:` and 64 hexadecimal characters. The workflow has already checked that the registry gives back the digest it pushed.

## 2. Record the digest

Put the digest in the `api` entry of `infra/images.json`, in a pull request, and merge it. `infra/images.json` is the only place a person says which API image the cells should run.

## 3. Apply the `cell-first` layer

Dispatch **`infra-apply`** with `env` = `europe`, `layer` = `cell-first` and `action` = `plan`. Read the plan: it should change the API image address to the new digest and nothing else. Then dispatch it again with `action` = `apply` and approve the `infra-apply` environment.

If you skip this, step 5 stops with *"The cell-first layer holds a different API image digest from infra/images.json"*. Nothing is changed when it does.

## 4. Copy the image into the three registries

Dispatch **`publish-images`** with `env` = `europe` and `image` = `api`, and approve the `infra-apply` environment. It copies the image into each cloud's private registry by digest, and fails unless each registry gives back exactly that digest.

## 5. Apply the `api` layer's image address

Dispatch **`infra-apply`** with `layer` = `api` and `action` = `plan`. Expect `terraform_data.image` to update and nothing else: `0 to add, 1 to change, 0 to destroy`. A plan that shows an API machine being **replaced** here is wrong. Do not apply it; see [Troubleshooting](troubleshooting.md). Apply it with `action` = `apply`.

## 6. Make the test user, if you want one

A cell that has no user list cannot be signed in to. Dispatch **`rotate-credentials`** with `env` = `europe`, `kind` = `all` and `proving` ticked, only for a proving environment, and read [Rotating certificates and credentials](rotating-credentials.md) first: the run also issues new certificates and credentials and takes about 15 minutes. Its counts line says `test users made`. A cell that already has a user list keeps it.

The service reads `users.json` again when it appears or changes, so this works before or after the machines are replaced. A machine reads its secret every 5 minutes, and the service looks for a change every 30 seconds.

## 7. Replace the API machines, one at a time

One machine, then check its cell, then the next. Never replace two at once, and know that **that cell's API is down for a few minutes** while its machine is replaced, so transfers that need it are declined meanwhile.

For each cell:

1. Dispatch **`infra-apply`** with `env` = `europe`, `layer` = `api`, `action` = `plan` and `replace` set to the machine's address:

   | Cloud | `replace` |
   | --- | --- |
   | AWS | `module.api_aws.aws_instance.api` |
   | Azure | `module.api_azure.azurerm_linux_virtual_machine.api` |
   | Google Cloud | `module.api_gcp.google_compute_instance.api` |

2. Read the plan: one machine replaced and nothing else deleted. The workflow refuses a plan that deletes a disk, deletes anything but that machine and its disk attachment, or replaces another machine, and says so without printing a name. If it refuses, nothing was applied. Stop and read the plan before you try anything else.
3. Dispatch it again with `action` = `apply`, the same `replace`, and approve the `infra-apply` environment. The approved fingerprint covers the replacement, and the apply plans again and compares.
4. Wait for the machine to start, then check the cell. `/healthz` says `status` `ok`. For the first image that has the basic page, `/ui` answers `200` where the old image answers `404`, which shows the new image is the one running. If you made a test user, signing in at `/ui` works.
5. Only then go to the next cell.

After the last machine, dispatch **`infra-apply`** for the `api` layer with `action` = `plan` again and expect `0 to change`.

## If something goes wrong

Stop. Do not replace the next machine. A cell with a broken API machine still holds its data: the database and etcd are separate machines, and the API machine holds nothing. The other cells keep serving, and accounts homed at the broken cell are unavailable until it is back.

| What you see | What it means |
| --- | --- |
| The machine starts and `/healthz` does not answer | Its secret may lack a file the service needs. Run `rotate-credentials` and read its counts: `cells still missing a file` should be 0. See [First entry](first-entry.md#when-a-step-fails). |
| `/healthz` answers and `/ui` is `404` | The machine is running the old image. Check that step 5 was applied before the replacement, and that the digest in `infra/images.json` is the new one. |
| `/ui` answers and sign-in says it is not set up on this cell | There is no `users.json` in the cell's secret yet. Do step 6. It reaches the machine within about 5 minutes. |
| The machine cannot pull the image | `publish-images` did not finish for that cloud (step 4), or the cell's registry was changed. Run step 4 again. |

Keep logs out of public places, as everywhere else in these runbooks.
