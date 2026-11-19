# Environments

Root configurations that combine the modules for one environment go here, one directory each,
with one directory for each layer inside it: `infra/envs/<env>/<layer>`. Each layer has its own
state, so the `infra-apply` workflow can plan and apply one layer at a time
(see [`../../docs/infrastructure.md`](../../docs/infrastructure.md)).

| Environment | Layers | Geo |
| --- | --- | --- |
| `europe` | `network` (I-01oa), `private-access` and `interconnect` (I-01ob), `registries` (I-01q) and `control-plane` (I-01ocb) `cell-first` (I-01occ), the first tenant's keys, repositories and administrators, and `cells` (I-01va), the tenant's self-hosted database cells. | `europe` |

Apply the layers in that order, except that `registries` comes before `control-plane` and the etcd image is published between them (see below). Each later layer reads the networks layer's outputs from its state (`europe/network`).

A layer's `backend "s3" {}` block is empty on purpose: the workflow supplies the bucket, region
and a state path of `<env>/<layer>`. Nothing secret is written in a layer. Secrets reach it as
`TF_VAR_<name>` secrets of the GitHub environment.

Secrets and identifiers a layer takes:

| Layer | Variable | Set as | Notes |
| --- | --- | --- | --- |
| `europe/network`, `europe/private-access`, `europe/interconnect` | `azure_subscription_id` | `TF_VAR_azure_subscription_id` secret | The Azure provider needs the subscription named. It is an account identifier, not a credential. |
| `europe/private-access`, `europe/interconnect` | `state_bucket` | `TF_VAR_state_bucket` secret | The bucket that holds the state, to read the networks layer. It names a bucket, it is not a credential. |
| `europe/control-plane` | `azure_subscription_id`, `state_bucket` | as above | |
| `europe/control-plane` | `azure_flatcar_image_id` | `TF_VAR_azure_flatcar_image_id` secret | A resource id of a managed or gallery image in `northeurope`. |
| `europe/control-plane` | `administrator_ssh_public_key` | `TF_VAR_administrator_ssh_public_key` secret | A public key for the Azure members' administrator. |
| `europe/cell-first` | `azure_subscription_id`, `state_bucket` | as above | |
| `europe/cell-first` | `postgres_image_digest` | `TF_VAR_postgres_image_digest` secret | The PostgreSQL node image's digest, `sha256:` and 64 hex digits. No tags. |
| `europe/cell-first` | `azure_administrator_object_id`, `azure_administrator_name` | `TF_VAR_azure_administrator_object_id`, `TF_VAR_azure_administrator_name` secrets | The Entra group that administers the tenant's PostgreSQL servers. A person makes it (`docs/infrastructure.md`). A group's id and name are not credentials. |
| `europe/cells` | `azure_subscription_id`, `state_bucket`, `azure_flatcar_image_id`, `administrator_ssh_public_key` | as for the control plane | The same secrets as the control plane. |
| `europe/interconnect` | `preshared_keys` | `TF_VAR_preshared_keys` secret | The one real secret. A JSON map of tunnel name to key, below. |

`TF_VAR_preshared_keys` holds one key for each of these names. Each key is 32 to 64 letters and
digits, and no two are the same:

```json
{"aws-azure-0": "…", "aws-azure-1": "…", "aws-gcp-0": "…", "aws-gcp-1": "…", "aws-gcp-2": "…",
 "aws-gcp-3": "…", "azure-gcp-0": "…", "azure-gcp-1": "…"}
```

Make them with, for example, `openssl rand -hex 24` (48 characters), once for each name.

The `private-access` layer makes the Azure container registry (Premium) and Key Vault, both with no public access. Machines reach them through private endpoints only. The apply runner cannot reach the Key Vault's data plane, so a later layer that writes secrets into it needs a way in (I-01oc).

The Google Cloud provider takes its project from the federated sign-in. The AWS provider takes
its region from `modules/regions`.

Each environment names its geo in `deployment.json`:

```json
{"geo": "uk"}
```

The geo must be enabled in `../geos.json`. The region guard checks the environment's files
against that geo's regions only. A customer deployed into several geos gets one environment
per geo, each passing its geo to `modules/regions`.

## Register the Azure resource providers once

A run never registers Azure resource providers: the plan identity only reads, and the Azure
provider fails with "does not have the necessary permissions to register Resource Providers"
if it tries. A person registers them once, in the subscription (Subscriptions, Resource
providers in the portal, or `az provider register --namespace <name>`):
`Microsoft.Network`, `Microsoft.Compute`, `Microsoft.Storage`, `Microsoft.KeyVault`,
`Microsoft.ContainerRegistry`, `Microsoft.ManagedIdentity`, `Microsoft.Insights`,
`Microsoft.OperationalInsights`, and `Microsoft.Authorization`. If a plan names another, register that one too.

## The Azure Key Vault's firewall

The Key Vault from `private-access` has its public endpoint switched on, with the firewall on
Deny and no standing address. The `infra-apply` workflow adds each run's own address and takes it
away at the end (see [`../../docs/infrastructure.md`](../../docs/infrastructure.md)). Set the
GitHub variables `AZURE_KEY_VAULT_NAME` and `AZURE_KEY_VAULT_RESOURCE_GROUP` from the layer's
outputs (`azure_key_vault_name`, `azure_key_vault_resource_group_name`) and give both identities
their roles on the vault. Then apply `europe/private-access` again to switch the endpoint on.

## The control plane layer

`europe/registries` makes the private repository for the etcd image in AWS and in Google Cloud
(the Azure registry is made by `private-access`). `europe/control-plane` makes an encryption key
in each cloud, then the five-member etcd cluster over the three clouds. The order is:

1. Apply `europe/registries`.
2. Copy the etcd image into the three repositories (below). The `etcd_repositories` output of
   the registries layer prints the three destinations.
3. Plan, then apply `europe/control-plane`. The members pull the image on first boot.
4. Put the certificates into each member's secret (`certificate_secrets` output) and let the
   members restart. The module makes the secrets and never holds their contents.

**The images are pinned in `infra/images.json`.** It names the etcd image (source, version tag
and digest) and the Flatcar release. The layer reads it, so there are no secrets for them. To
change a version, change the file in a pull request.

- **etcd.** Copy the image named in the manifest to the three repositories (`etcd_repositories`
  output of the registries layer) with a tool that keeps the digest, such as `crane copy <source>@<digest>
  <repository>:<tag>`, and check that `crane digest <repository>:<tag>` gives the manifest's
  digest in each. The `publish-images` workflow does this (see `docs/infrastructure.md`):
  run it from `main` after `europe/registries` is applied.
- **Flatcar on AWS and Google Cloud.** The layer looks up Flatcar's own published images by the
  release in the manifest: in AWS the Flatcar account's AMI named
  `Flatcar-<channel>-<version>-hvm`, and in Google Cloud the image in the `kinvolk-public`
  project named `flatcar-<channel>-<version with dashes>` (for example `flatcar-stable-4757-2-3`),
  fetched by that exact name because listing the project needs a permission most accounts lack. If your organisation
  restricts image projects, allow `kinvolk-public`.
- **Flatcar on Azure.** Not yet automated: make a managed image from Flatcar's Azure VHD (I-01s
  automates it) and give its resource id as `TF_VAR_azure_flatcar_image_id`.

**Azure disk key.** The layer makes a key in the platform Key Vault and a disk encryption set
that uses it. Both the key and its role assignment need the apply identity's roles on the vault
(see the Key Vault section above) and the runner access added by I-01oca.

## The first tenant's cell layer

`europe/cell-first` is for one tenant, `ten_bankofmumanddad` (an example customer), whose id is written in the layer's `main.tf`. Apply it after `control-plane`. It makes what that tenant's database cells need and that nothing else may share:

| Cloud | Made |
| --- | --- |
| AWS | A KMS key and alias of the tenant's own, and an ECR repository `ten_bankofmumanddad/postgres` (tags cannot change, scanned on push, encrypted with that key). |
| Azure | A Key Vault key of the tenant's own in the platform vault, a user-assigned identity and a disk encryption set that may use that key and nothing else. The repository is made by the registry on first push. The administrator is the Entra group you name. |
| Google Cloud | A key ring and key of the tenant's own, an Artifact Registry repository `ten-example-postgres` (tags cannot change), and an administrator service account. |

Its outputs `aws`, `azure` and `gcp` are what the database cells take as inputs, and `postgres_repositories` says where to copy the node image. The cells themselves follow in a later layer. The control plane's keys and this tenant's keys are separate, and a second tenant gets a layer of its own with its own keys.

To publish the Postgres node image: run `publish-postgres-image` (it prints the digest), put the digest in the `postgres` entry of `infra/images.json` in a pull request, set `TF_VAR_postgres_image_digest` to the same digest, apply `europe/cell-first`, then run `publish-images` with `image` set to `postgres`.

## The first tenant's database cells

`europe/cells` makes one self-hosted PostgreSQL cell in each cloud for the tenant that `cell-first` is for: three nodes in three zones, running the node image as a container. Apply it after `cell-first`, once the Postgres image is published to the tenant's repositories (`publish-images` with `image` set to `postgres`). It makes no key, repository or identity: it uses the tenant's own from `cell-first`, and the private endpoints and DNS zones from `private-access` (`reuse_private_access`). It stops with a message if `cell-first` holds a different image digest from `infra/images.json`.

Its sizes are proving sizes (shared-CPU: `t3.medium`, `Standard_B2s`, `e2-medium`) to keep the bill low. Delete the `machine_sizes` lines to use the module's own size table in production. Its outputs give each cell's private host, port and where its administrator's credential is held, never the credential. A Lakebase form follows.

### Managed cells

Set `cell_kind` to `managed` to run each cloud's cell as the cloud's own PostgreSQL service instead (RDS Multi-AZ cluster, Azure zone-redundant flexible server, Cloud SQL regional instance). The default, `self_hosted`, leaves an applied layer unchanged. Changing the kind **replaces** each cell: the old form is destroyed and the new one made, and no data is carried across. Plan first and read it. The machine sizes above apply to self-hosted cells only. The managed services have no burstable form with standby (a Multi-AZ cluster, zone-redundant high availability and a regional instance all need general-purpose sizes), so a managed cell costs noticeably more than the self-hosted one. Pause or destroy it when you are not using it.

Two things come with the managed kind. The layer makes Azure's private DNS zone for flexible servers (`modules/postgres/azure-dns`), and it lets Cloud SQL's service agent use the tenant's own key. The agent has to exist first. Create it once:

```
gcloud beta services identity create --service=sqladmin.googleapis.com --project=<project>
```

`deletion_protection` (true by default) stops a managed database being deleted. To destroy a proving environment, set it to `false`, apply, and then destroy.

