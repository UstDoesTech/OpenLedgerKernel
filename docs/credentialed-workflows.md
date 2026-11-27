# Credentialed workflows

Three workflows hold cloud access: `infra-apply` plans and applies one layer of one environment, `publish-images` copies a pinned container image into the three clouds' private registries, and `rotate-credentials` issues and rotates each API machine's certificates and cell credentials. Applying by hand, as in [`infrastructure.md`](infrastructure.md), still works. Use one or the other for a layer, never both at once.

## Why this repository needs more care

The repository is public, so **anyone can read a workflow's log and its job summary**. A plan or an apply prints account numbers, subscription and project ids, key addresses and resource names. None of those is a secret, but together they identify your accounts. So these workflows are built to keep them out of the log:

- **Every tofu command's output goes to a log file**, not the job's log (`scripts/quiet.sh`). The files and the readable plan are copied to a **private plan store**, a private S3 bucket, under the run's id (`scripts/keep_logs.sh`).
- **The job summary holds only counts and a fingerprint**: how many resources are added, changed, destroyed and replaced, and a hash of the plan text (`scripts/plan_counts.py`). You read the plan itself from the plan store before you approve an apply.
- **The Azure commands that print a vault's or a registry's settings have their output dropped**, and `publish-images` masks every registry address, account number and project id before anything can print one.
- **The plan's JSON form is never uploaded.** It holds raw sensitive values, so it is read for its counts and left on the runner.

What the workflows cannot promise: the sign-in actions (`azure/login`, `google-github-actions/auth`) write their own log lines. Read the public log of your first run and check it holds nothing you would not publish.

## The guards

| Guard | Where it is enforced |
| --- | --- |
| Started only by dispatch, never by a push, a pull request or `pull_request_target` | The workflows' `on:` |
| Only from `main`, and only by the repository's owner | Each job's `if:` (`github.ref`, `github.actor == github.repository_owner`) |
| A named person approves before an apply, a publish or a rotation | The `infra-apply` and `rotate-credentials` environments' required reviewers |
| Only `main` can use the environments | The environments' deployment branches |
| Each cloud trusts one environment of this repository only | The cloud trust policies below |
| Changes to workflows, scripts and infrastructure need the owner's review | `.github/CODEOWNERS` and the ruleset for `main` |
| Third-party actions are pinned to a full commit | `tests/test_infra_apply_workflow.py` |

Agents never dispatch these workflows and never hold these roles.

## Replacing one machine

`infra-apply` has an optional `replace` input for rolling a new node image (see [Rolling a new node image](runbooks/rolling-the-node-image.md)). It takes the address of **one** machine. That is a `node` or `member` of an instance or virtual machine resource, with its index, such as `module.<name>.aws_instance.node[1]`, or an API machine, which has no index because each cell has one, such as `module.api_aws.aws_instance.api` (use the `api` layer; the Azure and Google Cloud machines are `module.api_azure.azurerm_linux_virtual_machine.api` and `module.api_gcp.google_compute_instance.api`). A pattern, a list, any other resource without an index or anything else is refused before tofu runs (`scripts/replace_guard.py`).

The plan job then refuses a plan that deletes a data disk, deletes anything except the machine and its disk attachment, or replaces any other machine. The one addition is for an API machine on Azure: its role assignments take their principal from the machine's system-assigned identity, so they are replaced with it, and the check accepts exactly that (a role assignment that is deleted and made again, in the machine's own module) and nothing else. The other addition is for the AWS API machine when a load balancer is in front of it (K-23e): the load balancer's attachment of the machine to its target group is built from the machine's id, so it is replaced with it, and the check accepts exactly that one attachment, deleted and made again, and nothing else of the load balancer. The approved fingerprint covers the replacement, and the apply job plans again with the same address and compares. A replacement of that kind is one apply: the disk is detached (on AWS the machine is stopped first), the machine is replaced, and the same disk is attached to the new machine. Replace one machine per run, and check the cell is healthy between runs.

## One-time setup

### 1. GitHub

**Settings, Environments.** Make these environments with exactly these names. `rotate-credentials` has its own section below, because its identities are different.

| Environment | Required reviewers | Deployment branches | Holds |
| --- | --- | --- | --- |
| `infra-plan` | Optional | `main` only | Read-only identities |
| `infra-apply` | **You** (and a second person when you have one, with "Prevent self-review") | `main` only | Applying identities |
| `rotate-credentials` | **You** | `main` only | Identities that sign certificates and write the API secrets, and nothing else |

**Settings, Rules, Rulesets.** A ruleset for `main`: require a pull request, require review from the code owners, block force pushes and deletion.

**Settings, Actions, General.** Require approval for all outside contributors. Set the workflow permissions to read-only. **Settings, Moderation, Interaction limits:** collaborators only.

Each environment gets **secrets** and **variables** (Settings, Environments, the environment), with its own values.

**Anything that identifies your accounts is a secret, not a variable.** GitHub prints a step's environment and an action's inputs in the log header, and anyone can read that log. A secret is masked there as `***`. A variable is printed in full. Masking only hides values in logs. It does not control access: the cloud trust below does that.

| Secret | Value |
| --- | --- |
| `AWS_ROLE_ARN` | The role for this environment. Required, because the plan store is in AWS. |
| `PLAN_STORE` | The private bucket and prefix for logs, such as `s3://<state bucket>/plans`. Required. |
| `TOFU_BACKEND_CONFIG` | The partial backend settings, one `name = "value"` per line, such as `bucket = "..."`, `region = "..."`, `use_lockfile = true`, `encrypt = true` |
| `AZURE_CLIENT_ID`, `AZURE_TENANT_ID`, `AZURE_SUBSCRIPTION_ID` | The app registration for this environment, the tenant and the subscription. Azure is skipped when `AZURE_CLIENT_ID` is empty or unset. |
| `GCP_WORKLOAD_IDENTITY_PROVIDER`, `GCP_SERVICE_ACCOUNT` | The provider and the service account for this environment. Google Cloud is skipped when the provider is unset. |
| `AZURE_KEY_VAULT_NAME`, `AZURE_KEY_VAULT_RESOURCE_GROUP` | Optional. From the `private-access` layer's outputs. See "The Azure Key Vault and the runs" below. |
| `TF_VAR_<variable name>` | One per sensitive input of the layers. Only secrets with this prefix reach OpenTofu. GitHub shows names in capitals and the workflow lowers the part after the prefix. |

| Variable | Value |
| --- | --- |
| `AWS_REGION` | The region the role is assumed in (the state bucket's region). Already public in `infra/geos.json`. |
| `TOFU_STATE_KEY_NAME` | Optional. `key` (the default) or `prefix` |

A secret set on the repository works too, and an environment's secret of the same name wins. Put them on the environments, so only jobs that run in `infra-plan` or `infra-apply` can read them.

The state path for a layer is added for you: `<env>/<layer>`.

### 2. The plan store

A private S3 bucket (the state bucket will do, under its own prefix). Both AWS roles may `s3:PutObject` on `<bucket>/plans/*` and nothing more there. **You** read it, with your own cloud sign-in, not through GitHub.

### 3. AWS

1. IAM, Identity providers: an OpenID Connect provider with URL `https://token.actions.githubusercontent.com` and audience `sts.amazonaws.com`.
2. Two roles, `infra-plan` and `infra-apply`. Each has this trust policy, with its own environment name in the last line. **Never use a wildcard in the subject.**

```json
{
  "Version": "2012-10-17",
  "Statement": [{
    "Effect": "Allow",
    "Principal": {"Federated": "arn:aws:iam::<account id>:oidc-provider/token.actions.githubusercontent.com"},
    "Action": "sts:AssumeRoleWithWebIdentity",
    "Condition": {"StringEquals": {
      "token.actions.githubusercontent.com:aud": "sts.amazonaws.com",
      "token.actions.githubusercontent.com:sub": "repo:UstDoesTech/OpenLedgerKernel:environment:infra-apply"
    }}
  }]
}
```

The `infra-plan` role's last line ends `:environment:infra-plan`. **Check which subject your repository sends:** `gh api repos/UstDoesTech/OpenLedgerKernel/actions/oidc/customization/sub`. If it says `"use_immutable_subject": true`, use the `sub_claim_prefix` it prints (it carries the owner and repository ids) in place of `repo:UstDoesTech/OpenLedgerKernel`. Otherwise AWS refuses the plain form with "Not authorized to perform sts:AssumeRoleWithWebIdentity".

3. `infra-plan`: `ReadOnlyAccess`, read access to the state bucket and its key, and `PutObject` on the plan store's prefix. It never writes state. `infra-apply`: what the modules need to create networks, VPN, instances, keys and roles, plus read and write on the state bucket. This is a wide role, so what limits it is the approval, the trust condition above and your account's region controls.
4. Maximum session: 4 hours for `infra-apply` (VPN gateways are slow), 1 hour for `infra-plan`.

### 4. Azure

1. Entra ID, App registrations: two, `infra-plan` and `infra-apply`. Do not make a client secret.
2. For each, Federated credentials, "GitHub Actions deploying Azure resources": organisation `UstDoesTech`, repository `OpenLedgerKernel`, entity type **Environment**, name `infra-plan` or `infra-apply`. The subject is `repo:UstDoesTech/OpenLedgerKernel:environment:infra-apply`. If the repository sends the immutable subject, choose "Other issuer" and type the subject by hand, as under AWS. The issuer is `https://token.actions.githubusercontent.com` and the audience `api://AzureADTokenExchange`.
3. Role assignments on the subscription. `infra-plan`: `Reader` and `Storage Blob Data Reader` on the state container, plus `Storage Queue Data Reader` and `Storage Blob Data Reader` on the subscription (the `network` layer reads the flow-log storage account through Microsoft Entra ID, never with an account key, so the plan identity needs no `listKeys`). `infra-apply`: `Contributor`, `User Access Administrator` and `Storage Blob Data Contributor` on the state container, plus `Storage Queue Data Contributor` and `Storage Blob Data Contributor` on the subscription. For `publish-images`, the applying identity also needs `AcrPush` on the registry.

#### The VPN shared key (the `interconnect` layer)

Refreshing the VPN connections makes the provider read each connection's pre-shared key. A plain `Reader` is refused, and Azure answers with a 500 (`retrieving Shared Key for Connection ... InternalServerError`) rather than a 403. Give the `infra-plan` identity this narrow custom role on the resource group that holds the connections (the `infra-apply` identity's `Contributor` already covers it):

```
az role definition create --role-definition '{
  "Name": "VPN shared key reader",
  "Description": "Read VPN connection shared keys so tofu plan can refresh connections",
  "Actions": ["Microsoft.Network/connections/sharedKey/action", "Microsoft.Network/connections/sharedKey/read"],
  "AssignableScopes": ["/subscriptions/<subscription id>/resourceGroups/<resource group>"]}'
az role assignment create --assignee <infra-plan client id> --role "VPN shared key reader" \
  --scope /subscriptions/<subscription id>/resourceGroups/<resource group>
```

This lets the plan identity read the VPN keys, which the apply identity and the `TF_VAR_` secret already hold. The plan job runs only on `main`, only for the owner, in an environment limited to `main`. Role changes take a few minutes. Never paste `az network vpn-connection shared-key show` output anywhere public.

#### The Azure Key Vault and the runs

The Key Vault from `infra/envs/europe/private-access` keeps its firewall on **Deny**. A run adds its own address (one host, `/32`) to the vault's firewall, plans or applies, and a last step that always runs removes it, even after a failure. If the removal fails, the run warns, and you remove the address by hand (`az keyvault network-rule remove`). Give each identity these roles **on the vault only**:

| Identity | Roles on the vault |
| --- | --- |
| `infra-plan` | `Key Vault Contributor`, `Key Vault Reader`, `Key Vault Secrets User` |
| `infra-apply` | `Key Vault Contributor`, `Key Vault Secrets Officer`, `Key Vault Crypto Officer` |

### 5. Google Cloud

1. A workload identity pool and an OpenID Connect provider with the issuer `https://token.actions.githubusercontent.com`, the attribute mapping `google.subject=assertion.sub,attribute.repository=assertion.repository,attribute.environment=assertion.environment` and the attribute condition `assertion.repository == 'UstDoesTech/OpenLedgerKernel' && assertion.environment in ['infra-plan','infra-apply']`.
2. Two service accounts, `infra-plan` (`roles/viewer` and read on the state bucket) and `infra-apply` (what the modules need, and read and write on the state bucket).
3. Let each environment act as its service account only:

```
gcloud iam service-accounts add-iam-policy-binding infra-apply@<project>.iam.gserviceaccount.com \
  --role roles/iam.workloadIdentityUser \
  --member "principalSet://iam.googleapis.com/projects/<project number>/locations/global/workloadIdentityPools/<pool>/attribute.environment/infra-apply"
```

and the same for `infra-plan`.

### 6. Rotating certificates and credentials (`rotate-credentials`)

One central certificate authority signs every cell's certificates (decision D-17). Its key is an AWS KMS signing key made by the `authority` layer, which can sign and cannot be exported. The `rotate-credentials` workflow asks KMS to sign, and writes each cell's certificates and cell credentials into that cell's API secret as a new version. Its identities are separate from `infra-apply`, and each may do only this:

| Cloud | What the `rotate-credentials` identity may do |
| --- | --- |
| AWS | `kms:Sign` and `kms:GetPublicKey` on the authority key. `secretsmanager:GetSecretValue`, `PutSecretValue` and `ListSecretVersionIds` on the authority's certificate secret and on the AWS API secret. Read on the state bucket for the `api` and `authority` layers, and `PutObject` on the plan store's prefix. Nothing else. |
| Azure | On the vault only: `Key Vault Secrets Officer`, so it can read and write the Azure API secret, and `Key Vault Contributor`, so the run can open the vault's firewall to its own address and close it again. Nothing else. |
| Google Cloud | `roles/secretmanager.secretVersionAdder`, `roles/secretmanager.secretAccessor` and `roles/secretmanager.viewer` on the Google Cloud API secret only. Nothing else. |

A run with `proving` on (decision D-19) may also read each cell's administrator credential, only to fill a database connection that is missing. It needs, besides the table:

| Cloud | The extra rights for a proving run |
| --- | --- |
| AWS | `secretsmanager:GetSecretValue` and `secretsmanager:ListSecretVersionIds` on the AWS cell's administrator secret only. |
| Azure | `Key Vault Secrets Officer` already reads a secret. If the Azure cell's administrator secret is in another vault, `Key Vault Secrets User` and `Key Vault Contributor` on that vault (reading, and opening its firewall the same way). |
| Google Cloud | `roles/secretmanager.secretAccessor` and `roles/secretmanager.viewer` on the Google Cloud cell's administrator secret only. |

Leave these off an environment that holds real money or customer data. K-19d replaces the copy: the node agent writes a dedicated role's connection string itself, so no run reads the administrator credential, and these rights go.

#### How the identities are made (K-19c, decision D-18)

You do not make these identities by hand. The `identities` layer (`infra/envs/europe/identities`) makes them, and you apply it with `infra-apply` like any other layer. The one list of what they may do is [`infra/rotation-identity.json`](../infra/rotation-identity.json). The modules read it, and a test holds this page to it. The identities are:

- AWS: the role `rotate-credentials`, trusted only for the web identity subject `repo:UstDoesTech/OpenLedgerKernel:environment:rotate-credentials` and the audience `sts.amazonaws.com`. A second policy, `ledger-rotate-credentials-boundary`, holds the same document and is the role's permissions boundary.
- Azure: a user-assigned managed identity `rotate-credentials` with a federated credential for the same subject. It is a managed identity and not an app registration, so making it needs no right in Microsoft Entra ID. The workflow signs in with its client id in the same way.
- Google Cloud: the service account `rotate-credentials`, which only the `rotate-credentials` environment can act as, and no role on the project.

The identities may do these things, and nothing else. For AWS the actions are, in full:

| Group | Actions |
| --- | --- |
| Signing | `kms:GetPublicKey`, `kms:Sign` |
| Secrets | `secretsmanager:GetSecretValue`, `secretsmanager:ListSecretVersionIds`, `secretsmanager:PutSecretValue` |
| The AWS cell's secrets' key, if it is not the account's default (`TF_VAR_encryption_arn`). It must be the tenant's key when the API secret, the administrator credential or the database machines' certificates use it | `kms:Decrypt`, `kms:GenerateDataKey` |
| The state bucket's key, if it is not the bucket's default (`TF_VAR_state_encryption_arn`) | `kms:Decrypt` on that key only |
| The plan store's key, if it is not the bucket's default (`TF_VAR_plan_store_encryption_arn`) | `kms:GenerateDataKey` on that key only |
| The etcd members' key, which the control-plane layer makes and outputs as `aws_kms_key_arn` (never set by hand) | `kms:Decrypt`, `kms:GenerateDataKey` on that key only |
| State | `s3:GetObject` on the `api`, `authority`, `cells` and (when the environment has a control plane) `control-plane` state objects only, and `s3:ListBucket` on the state bucket |
| Database machines' certificates | `secretsmanager:PutSecretValue` on each of the AWS cell's database machines' secrets only |
| The etcd members' certificates | `secretsmanager:PutSecretValue` on each of the control plane's AWS etcd members' secrets only |
| Logs | `s3:PutObject` on the plan store's prefix only |

For Azure the roles are `Key Vault Contributor` and `Key Vault Secrets Officer`, on the API vault only (Secrets Officer also writes the database machines' and the etcd members' certificates, which are in the same vault), and, with `proving` on, `Key Vault Secrets User` on the vault of the administrator credential. For Google Cloud they are `roles/secretmanager.secretAccessor`, `roles/secretmanager.secretVersionAdder` and `roles/secretmanager.viewer` on the API secret only, `roles/secretmanager.secretVersionAdder` on each of the Google Cloud cell's database machines' secrets only and on each of the control plane's Google Cloud etcd members' secrets only, and, with `proving` on, `roles/secretmanager.secretAccessor` and `roles/secretmanager.viewer` on the administrator secret only. Tests over the OpenTofu fail if a policy grants an action that is not in the list, grants an action on every resource, gives a role on a subscription or project, or lets an identity change any identity or permission.

#### What the infra-apply identities need to make them

Making an identity is itself a right, so it is kept as small as it can be and bounded. It is in [`infra/applier-rights.json`](../infra/applier-rights.json), and one script for each cloud gives it, once, in your own session (each prints what it would do until you add `--apply`):

| Cloud | The extra rights of the `infra-apply` identity | The bound |
| --- | --- | --- |
| AWS | `iam:CreateRole`, `iam:UpdateRole`, `iam:UpdateAssumeRolePolicy`, `iam:PutRolePolicy`, `iam:PutRolePermissionsBoundary` and the matching read, delete and tag actions on the one role `rotate-credentials`. `iam:CreatePolicy`, `iam:CreatePolicyVersion` and the matching read, delete and tag actions on the one policy `ledger-rotate-credentials-boundary`. `iam:GetOpenIDConnectProvider` on the GitHub provider. Run `scripts/identity_rights_aws.sh`. | Named resources only. An explicit deny of `iam:DeleteRolePermissionsBoundary`, `iam:AttachRolePolicy` and `iam:DetachRolePolicy`. It can change this role and its boundary, so read the plan of the `identities` layer. It cannot touch any other role. |
| Azure | The custom role `Ledger rotation identity manager`: managed identities, their federated credentials, resource groups and role assignments. Run `scripts/identity_rights_azure.sh`, then remove `User Access Administrator` from the identity. | The custom role replaces `User Access Administrator`, which was wider. Narrowing which roles it may hand out, with a condition, is a follow-up. |
| Google Cloud | The custom role `ledgerRotationIdentityManager`: create, update, delete and set the policy on service accounts, and set the policy on secrets. Run `scripts/identity_rights_gcp.sh`. | Bound with a condition to service accounts whose name starts with `rotate-credentials@` and to secrets, with a deny of project, folder and organisation policy changes. |

After `infra-apply` makes the layer, its output `environment_secrets` gives four identifiers. A person enters them in GitHub, because OpenTofu has no GitHub token and should not. They are not credentials. The values are `AWS_ROLE_ARN`, `AZURE_CLIENT_ID`, `AZURE_TENANT_ID` and `GCP_SERVICE_ACCOUNT`.

Two steps stay by hand, because nothing in OpenTofu can do them safely:

1. GitHub: the `rotate-credentials` environment, limited to `main`, with you as the required reviewer. Enter the four identifiers above, and `PLAN_STORE`, `TOFU_BACKEND_CONFIG`, `AZURE_SUBSCRIPTION_ID` and `GCP_WORKLOAD_IDENTITY_PROVIDER`, exactly as in step 1. Also set `AZURE_KEY_VAULT_NAME` and `AZURE_KEY_VAULT_RESOURCE_GROUP` (the `private-access` layer's outputs). Without them the run never opens the vault's firewall, and reading the Azure secret fails with `az, blocked by a firewall`. The run warns when `AZURE_CLIENT_ID` is set and the vault name is not.
2. Google Cloud: widen the provider's attribute condition to `assertion.repository == 'UstDoesTech/OpenLedgerKernel' && assertion.environment in ['infra-plan','infra-apply','rotate-credentials']`. The provider is made by hand, as in step 5.

Then apply the `authority` layer, the `api` layer and the `identities` layer with `infra-apply`, in that order. The layer's inputs are `TF_VAR_` secrets like the other layers': `TF_VAR_azure_subscription_id`, `TF_VAR_state_bucket`, `TF_VAR_plan_store`, `TF_VAR_workload_identity_pool` and, only for a proving environment, `TF_VAR_proving` set to `true`. The first rotation makes the authority's certificate and keeps it in the layer's secret. The runbook is [Rotating certificates and credentials](runbooks/rotating-credentials.md).

## Running it

**Actions, `infra-apply`, Run workflow**, on `main`. Start with `plan` on the first layer. The summary shows the counts and a fingerprint. **Read the plan** in the plan store:

```
aws s3 cp s3://<plan store>/<run id>/plan/plan.txt -
```

Then run it again with `apply`: it plans a second time, you approve the environment, and it applies only if the new plan has the same fingerprint. Follow the layer order in [`infrastructure.md`](infrastructure.md). Do not cancel a run part-way: VPN gateways take a long time, and Azure's the longest.

If it says "the plan changed since it was shown", nothing was applied. Read why, then run it again. If a step fails, its output is not in the log. Read the step's log file from the plan store (`aws s3 cp s3://<plan store>/<run id>/plan/ - --recursive`, or `.../apply/`).

`publish-images` copies one image (`etcd` or `postgres`) from `infra/images.json` to the three registries by digest, and fails unless each registry gives back exactly that digest. Publish `postgres` after the `cell-first` layer and `etcd` after the `registries` layer.

`rotate-credentials` is run by a person, on `main`, and waits for the approval of its environment. It prints one line of counts and nothing else, so you read nothing from its log except whether it worked. See [Rotating certificates and credentials](runbooks/rotating-credentials.md).

## Revoking access

- **Stop everything now:** delete the `infra-apply` environment, or remove its reviewers and secrets. Without it the job cannot get a token.
- **Cloud side:** delete the AWS role's trust, the Azure federated credential, or the Google provider's binding. Each is independent of GitHub and works at once.
- **Rotate** the `TF_VAR_` secrets on your own schedule. The other secrets name your accounts and are not credentials, so there is nothing to rotate, but change them if you rebuild an account. The workflows store no cloud credential.
