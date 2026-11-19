# Credentialed workflows

Two workflows hold cloud access: `infra-apply` plans and applies one layer of one environment, and `publish-images` copies a pinned container image into the three clouds' private registries. Applying by hand, as in [`infrastructure.md`](infrastructure.md), still works. Use one or the other for a layer, never both at once.

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
| A named person approves before an apply or a publish | The `infra-apply` environment's required reviewers |
| Only `main` can use the environments | The environments' deployment branches |
| Each cloud trusts one environment of this repository only | The cloud trust policies below |
| Changes to workflows, scripts and infrastructure need the owner's review | `.github/CODEOWNERS` and the ruleset for `main` |
| Third-party actions are pinned to a full commit | `tests/test_infra_apply_workflow.py` |

Agents never dispatch these workflows and never hold these roles.

## Replacing machines

`infra-apply` has an optional `replace` input for rolling a new node image (see [Rolling a new node image](runbooks/rolling-the-node-image.md)). It takes one to three machines of **one cell**, in order, separated by commas. Each is a `node` or `member` of an instance or virtual machine resource, with its index, such as `module.<name>.aws_instance.node[1]`. A pattern, a resource without an index, a repeat, machines of two cells, more than three, or anything else is refused before tofu runs (`scripts/replace_guard.py`).

- Every machine is planned in the plan job, each plan is refused if it deletes a data disk, deletes anything except that machine and its disk attachment, or replaces any other machine, and each plan's fingerprint is in the summary.
- One approval covers the roll. The apply job applies the first saved plan. Before each later machine it plans again with the same address and stops if the fingerprint is not the one shown.
- After each machine, `scripts/roll_gate.py` asks the cloud whether it is running, passes the cloud's checks (AWS) and has its data disk attached, then waits `settle_minutes` (0 to 30, default 10). It stops the roll at the first failure.
- The gate runs from a public runner, so it cannot see inside the private networks. It does not prove a node has caught up. Its output holds no identifiers.

## One-time setup

### 1. GitHub

**Settings, Environments.** Make two environments with exactly these names.

| Environment | Required reviewers | Deployment branches | Holds |
| --- | --- | --- | --- |
| `infra-plan` | Optional | `main` only | Read-only identities |
| `infra-apply` | **You** (and a second person when you have one, with "Prevent self-review") | `main` only | Applying identities |

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

## Running it

**Actions, `infra-apply`, Run workflow**, on `main`. Start with `plan` on the first layer. The summary shows the counts and a fingerprint. **Read the plan** in the plan store:

```
aws s3 cp s3://<plan store>/<run id>/plan/plan.txt -
```

Then run it again with `apply`: it plans a second time, you approve the environment, and it applies only if the new plan has the same fingerprint. Follow the layer order in [`infrastructure.md`](infrastructure.md). Do not cancel a run part-way: VPN gateways take a long time, and Azure's the longest.

If it says "the plan changed since it was shown", nothing was applied. Read why, then run it again. If a step fails, its output is not in the log. Read the step's log file from the plan store (`aws s3 cp s3://<plan store>/<run id>/plan/ - --recursive`, or `.../apply/`).

`publish-images` copies one image (`etcd` or `postgres`) from `infra/images.json` to the three registries by digest, and fails unless each registry gives back exactly that digest. Publish `postgres` after the `cell-first` layer and `etcd` after the `registries` layer.

## Revoking access

- **Stop everything now:** delete the `infra-apply` environment, or remove its reviewers and secrets. Without it the job cannot get a token.
- **Cloud side:** delete the AWS role's trust, the Azure federated credential, or the Google provider's binding. Each is independent of GitHub and works at once.
- **Rotate** the `TF_VAR_` secrets on your own schedule. The other secrets name your accounts and are not credentials, so there is nothing to rotate, but change them if you rebuild an account. The workflows store no cloud credential.
