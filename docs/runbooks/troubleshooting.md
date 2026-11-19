# Troubleshooting plans and applies

## Read the log first

A failed step prints only its exit code in the public log, on purpose. The output is in the plan store:

```
aws s3 cp s3://<plan store>/<run id>/plan/plan.log - | grep -n -i -B2 -A8 error
```

The run id is the number in the run's address. Stages are `plan/` and `apply/`. Each holds one file per command: `init.log`, `plan.log`, `region-guard.log` and so on. If you paste an error to someone, replace account, subscription, project and bucket names first.

A failed plan changes nothing. Do not apply while a plan is failing.

## Errors we have met

| Error | Cause | Fix |
| --- | --- | --- |
| `plan failed with exit code 1. The log is in the plan store` | The expected way a step reports failure. | Read the log, as above. |
| Azure `AuthorizationFailed` on `Microsoft.Storage/storageAccounts/listKeys/action` (resource `azurerm_storage_account.flow`) | The provider reads the storage account's queue settings with an account key by default, and the read-only plan identity may not list keys. | `storage_use_azuread = true` is set on the `network` layer's provider. The plan identity needs `Storage Queue Data Reader` and `Storage Blob Data Reader`, and the apply identity the matching Contributor roles, on the subscription. If the next error is a 403 "not authorized to perform this operation" from the storage account, the storage firewall (default Deny) is blocking the runner. |
| Azure `retrieving Shared Key for Connection ... 500 InternalServerError` on `azurerm_virtual_network_gateway_connection` | The provider reads each VPN connection's shared key when it refreshes. A plain `Reader` is refused and Azure answers 500 instead of 403. The connections were healthy (`Succeeded`, `Connected`). Running the same call as an Owner worked. | Give the plan identity the custom role `VPN shared key reader` on the resource group. See [`credentialed-workflows.md`](../credentialed-workflows.md). Role changes take a few minutes. |
| AWS `Not authorized to perform sts:AssumeRoleWithWebIdentity` | The role's trust names a different subject from the one GitHub sends. | Check `gh api repos/UstDoesTech/OpenLedgerKernel/actions/oidc/customization/sub`. If it uses immutable subjects, use the `sub_claim_prefix` it prints. The subject ends `:environment:infra-plan` or `:environment:infra-apply`. |
| Plan wants to create everything on a layer already applied | The state key does not match. The workflow reads `<env>/<layer>`, with no extension. | List the bucket. If objects end `.tfstate`, copy them to the new key before you plan again. Do not apply. |
| `No value for required variable` | A `TF_VAR_<name>` secret is missing or sits on the wrong environment. | Add it to the environment the job uses. GitHub shows names in capitals. The workflow lower-cases the part after the prefix. |
| `the variable ... must be an s3:// address` or `TOFU_BACKEND_CONFIG is not set` | The secret is missing, or `PLAN_STORE` is not `s3://<bucket>/<prefix>`. | Set it on the environment. No trailing slash. |
| Plan wants to replace a layer's resources and `google_kms_crypto_key ... has lifecycle.prevent_destroy set` | The layer's tenant id (`cell-first`, `tenant` in `main.tf`) differs from the one deployed. The id sets the names of the keys, repositories and cells, so a different id means replacing them all. The `prevent_destroy` on the keys is what stopped it. | Put the deployed id back. Never remove `prevent_destroy` to get past this. Plan again and expect `0 to change`. |
| `The cell-first layer holds a different Postgres image digest from infra/images.json` (`terraform_data.image` precondition in `cells`) | The digest in `infra/images.json` is not the one `cell-first` holds (`TF_VAR_postgres_image_digest`). A placeholder of all zeros in the manifest does this. | Publish the image (`publish-postgres-image`), put its digest in `infra/images.json` through a pull request, set the same digest in the `TF_VAR_postgres_image_digest` secret, plan and apply `cell-first` (output changes only), then plan `cells`. |
| A plan to change an image digest wants to **replace data disks and machines**, with `zone = ... -> (known after apply) # forces replacement` | A `depends_on` on a module call deferred the data sources inside it (here the Google zones lookup) because something it depends on had a change pending. An unknown zone forces replacement. | **Do not apply.** No module call may use `depends_on` (`tests/test_no_module_depends_on.py` enforces it). Fixed in the code. A correct digest-change plan shows only `terraform_data.image` updating. |
| `The plan changed since it was shown` | The apply job's second plan differs from the first. Nothing was applied. | Read why, then run the workflow again. This is the safeguard working. |
| `could not remove the runner's address from the Key Vault's firewall` (a warning) | The last step could not close the firewall rule. | Remove the address by hand: `az keyvault network-rule remove`. |
| A job is skipped, or `Waiting` for ever | The `if:` guard (`main` only, repository owner only) is not met, or the environment's reviewer has not approved. | Dispatch from `main`, as the owner, and approve `infra-apply`. |
| A `<= read` data source "will be read during apply" in a plan with `0 to change` | A `check` block makes OpenTofu reload the configuration. | Nothing. It is a read, not a change. |
| An apply fails with `AttachVolume ... Attachment point /dev/sdf is already in use` on `aws_volume_attachment.data` | The machines were created with the data disk inline, and removing that from the code does not detach it: the provider ignores a removed inline disk. The new volumes were created but cannot take the device the old disk holds. | While a cell holds no data worth keeping: stop each machine, detach and delete the old disk (the one attached at `/dev/sdf`), run the apply again so the attachments are made, then start the machines. See [Moving an AWS data disk out of the machine](#moving-an-aws-data-disk-out-of-the-machine). If it holds data, stop and take a snapshot first. |

## Things that look like problems and are not

- **Many `Refreshing state...` lines.** That is a plan reading what exists.
- **`0 to add, 0 to change, 0 to destroy`.** The layer matches the code. It is the result you want for a layer already applied.

## When a value shows in the public log

A secret shows as `***`. A variable shows in full. If an account, tenant, subscription or project identifier, a bucket or a vault name appears in a public log, it is a variable or a printed value. Make it a secret, or stop the step printing it, and tell whoever maintains the workflows. Deleting the run removes the log. Do not rely on that: logs may already have been read or copied.

## When a key was shown

If a pre-shared key, a token or a `TF_VAR_` value appeared in a log, a chat or a terminal that others can see, treat it as exposed and replace it. For VPN keys, see [Rotating the VPN pre-shared key](rotating-vpn-key.md).

## Moving an AWS data disk out of the machine

Needed once, for a cell whose AWS machines were made before the data disk became its own resource (the Postgres nodes in the `cells` layer, the etcd members in the `control-plane` layer). Only while the cell holds no data worth keeping: this deletes the old disks.

1. Stop one machine, then find the volume attached to it at `/dev/sdf` (the root disk is attached at `/dev/xvda` or `/dev/sda1`, and the new volume is not attached yet).
2. Detach that volume, wait until it is `available`, and delete it. Check the id before deleting. It is the one at `/dev/sdf`, and the machine's other disk is never touched.
3. Do the same for each machine of the layer.
4. Plan the layer again. Expect one attachment to create per machine and nothing else. Apply it.
5. Start the machines. Each finds its new, empty disk by size, formats it and starts the node.
6. Plan once more and expect `0 to change`.
