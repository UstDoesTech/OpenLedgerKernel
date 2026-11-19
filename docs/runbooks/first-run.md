# First run

The order to follow the first time, and again for any new environment. The reasons and the full settings are in [`credentialed-workflows.md`](../credentialed-workflows.md). This page is the checklist.

## 1. Lock down the repository (once)

Settings you make as the repository's owner:

1. **Actions, General:** require approval for all outside collaborators. Read-only workflow permissions. Do not let Actions approve pull requests.
2. **Code security:** secret scanning and push protection, Dependabot alerts and updates, private vulnerability reporting.
3. **A ruleset for `main`:** pull request required, code-owner review required, status checks required, no force pushes, no deletion. A sole owner cannot approve their own pull request, so add yourself to the bypass list until there is a second reviewer.
4. **Interaction limits:** collaborators only while the repository is quiet.

## 2. Environments (once)

Create `infra-plan` and `infra-apply`. Limit both to the `main` branch. Make yourself a required reviewer on `infra-apply`.

## 3. The plan store (once)

Plan text lists resource names and identifiers, so it goes to a private bucket under the run's id, never to the public log. Use a separate bucket in the same region as `AWS_REGION` (the examples use `eu-west-2`). A prefix in the state bucket also works, but anyone who can read that bucket can then read both.

```
BUCKET=<globally unique name>
REGION=eu-west-2

aws s3api create-bucket --bucket "$BUCKET" --region "$REGION" \
  --create-bucket-configuration LocationConstraint="$REGION"
aws s3api put-public-access-block --bucket "$BUCKET" --public-access-block-configuration \
  BlockPublicAcls=true,IgnorePublicAcls=true,BlockPublicPolicy=true,RestrictPublicBuckets=true
aws s3api put-bucket-encryption --bucket "$BUCKET" --server-side-encryption-configuration \
  '{"Rules":[{"ApplyServerSideEncryptionByDefault":{"SSEAlgorithm":"AES256"}}]}'
aws s3api put-bucket-ownership-controls --bucket "$BUCKET" \
  --ownership-controls 'Rules=[{ObjectOwnership=BucketOwnerEnforced}]'
aws s3api put-bucket-policy --bucket "$BUCKET" --policy '{"Version":"2012-10-17","Statement":[{
  "Sid":"TlsOnly","Effect":"Deny","Principal":"*","Action":"s3:*",
  "Resource":["arn:aws:s3:::'"$BUCKET"'","arn:aws:s3:::'"$BUCKET"'/*"],
  "Condition":{"Bool":{"aws:SecureTransport":"false"}}}]}'
aws s3api put-bucket-lifecycle-configuration --bucket "$BUCKET" --lifecycle-configuration \
  '{"Rules":[{"ID":"expire-plans","Status":"Enabled","Filter":{"Prefix":"plans/"},"Expiration":{"Days":90}}]}'
```

Use the default `AES256` encryption, not a KMS key, unless you also give both roles key permissions. Leave versioning off.

Give both roles write access to the prefix and nothing more. The log step only uploads, so a compromised run cannot read earlier plans:

```
aws iam put-role-policy --role-name infra-plan --policy-name plan-store-write --policy-document '{
  "Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":"s3:PutObject",
  "Resource":"arn:aws:s3:::'"$BUCKET"'/plans/*"}]}'
# the same for infra-apply
```

Give your own sign-in `s3:GetObject` on `plans/*` and `s3:ListBucket`. Then set the `PLAN_STORE` secret to `s3://<bucket>/plans`, with no trailing slash.

## 4. Cloud trust and roles (once per cloud)

Each cloud trusts one environment of this repository, with no wildcard in the subject.

1. Check which subject GitHub sends: `gh api repos/UstDoesTech/OpenLedgerKernel/actions/oidc/customization/sub`. If it uses immutable subjects, build the trust from the prefix it prints, not from the repository name.
2. Create the `infra-plan` and `infra-apply` identities in each cloud and bind each to its environment.
3. Roles the plan identity needs beyond `Reader` (each was found by a failed plan):
   - Azure: `Storage Queue Data Reader` and `Storage Blob Data Reader` on the subscription, because the `network` layer reads its storage account through Microsoft Entra ID.
   - Azure: the custom role `VPN shared key reader` on the resource group that holds the VPN connections.
   - AWS: `s3:PutObject` on the plan store prefix.
4. The apply identity needs the matching write roles, including `Storage Queue Data Contributor` and `Storage Blob Data Contributor`.

## 5. Secrets and variables (once per environment)

Put these on each environment.

- **Secrets, because they identify your accounts and GitHub prints a variable in full in a public log:** `AWS_ROLE_ARN`, `AZURE_CLIENT_ID`, `AZURE_TENANT_ID`, `AZURE_SUBSCRIPTION_ID`, `GCP_WORKLOAD_IDENTITY_PROVIDER`, `GCP_SERVICE_ACCOUNT`, `TOFU_BACKEND_CONFIG`, `PLAN_STORE`, `AZURE_KEY_VAULT_NAME`, `AZURE_KEY_VAULT_RESOURCE_GROUP`, and every `TF_VAR_*` input.
- **Variables:** `AWS_REGION`. `TOFU_STATE_KEY_NAME` is optional and defaults to `key`. Leave it unset unless state moves to a Google Cloud Storage bucket, where it is `prefix`.
- `TOFU_BACKEND_CONFIG` holds `bucket`, `region`, `use_lockfile` and `encrypt`, one per line. It does not hold `key`: the workflow adds `key = "<env>/<layer>"` itself.

## 6. Check the state before the first plan

List the state bucket: `aws s3 ls s3://<state bucket>/ --recursive`. The workflow looks for `<env>/<layer>`, with no extension. If your state sits under `<env>/<layer>.tfstate`, the plan will see empty state and plan to create everything. Do not apply. Copy or rename the state objects first, and plan again.

## 7. Plan every layer, in order

Dispatch `infra-apply` with `plan`, one layer at a time: `network`, `private-access`, `interconnect`, `registries`, `control-plane`, `cell-first`, `cells`.

- Expect `0 to change` for layers you have already applied.
- The public log shows only counts and a fingerprint. Read the plan from the plan store: `aws s3 cp s3://<plan store>/<run id>/plan/plan.txt -`.
- **Read the first public log, step by step, for anything that is not masked.** The sign-in actions write their own lines, and this is the one check the tests cannot make.
- A `<= read` data source marked "will be read during apply" is a read, not a change.

## 8. Apply

Only when a plan is clean and you have read it: dispatch with `apply`, approve the `infra-apply` environment, and do not cancel part-way. The apply plans again and stops if the fingerprint changed.

## 9. Publish the images

1. Run `publish-postgres-image` on `main`. It prints the image digest.
2. Put the digest in `infra/images.json`, through a pull request.
3. Run `publish-images` for `etcd` after the `registries` layer, and for `postgres` after the `cell-first` layer.
