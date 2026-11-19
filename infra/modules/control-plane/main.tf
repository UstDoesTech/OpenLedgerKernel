# The control plane: an etcd v3 cluster that decides which cell is home for each account and
# holds the leases (docs/design.md, "Cells, homes and epochs"). Decision D-03: five members over
# the three clouds by default, placed two, two and one, so that losing any one cloud leaves a
# majority; never fewer than three. Where a geography has one region, one cloud and its three
# zones stand in for the three clouds.
#
# Each member is a machine on Flatcar Container Linux running etcd as a container from an image
# given by digest, with no public address, reaching its registry and its certificates over
# private endpoints. Clients and peers use mutual TLS from one authority, with TLS 1.3 at least.
# The certificates are made by whoever runs the authority and put into each member's secret in
# the cloud's secret store: this module makes the secrets and never holds their contents.
#
# Members use fixed addresses, so the initial cluster is known before any machine exists.
# The control plane tier's flows between clouds (ports 2379 and 2380) are already allowed by
# modules/interconnect, and no other flow is.
module "placement" {
  source = "./placement"

  name    = var.name
  members = var.members
  clouds  = var.clouds
  cidrs = { for c, v in {
    aws   = try(var.aws.subnet_cidrs, null)
    azure = try(var.azure.subnet_cidrs, null)
    gcp   = try(var.gcp.subnet_cidrs, null)
  } : c => v if v != null }
}

resource "terraform_data" "inputs" {
  lifecycle {
    precondition {
      condition     = !contains(var.clouds, "aws") || var.aws != null
      error_message = "AWS is one of the clouds, so give the aws input."
    }

    precondition {
      condition     = !contains(var.clouds, "azure") || var.azure != null
      error_message = "Azure is one of the clouds, so give the azure input."
    }

    precondition {
      condition     = !contains(var.clouds, "gcp") || var.gcp != null
      error_message = "Google Cloud is one of the clouds, so give the gcp input."
    }
  }
}

locals {
  members         = module.placement.placed
  initial_cluster = module.placement.initial_cluster
  cluster_id      = "${var.name}-cluster"

  in_cloud = { for c in ["aws", "azure", "gcp"] : c => [for m in local.members : { name = m.full_name, zone = m.zone, address = m.address } if m.cloud == c] }

  tags = merge(var.tags, { component = "control-plane" })
}

module "aws_members" {
  source = "./aws-members"
  count  = length(local.in_cloud.aws) > 0 ? 1 : 0

  name                      = var.name
  members                   = local.in_cloud.aws
  initial_cluster           = local.initial_cluster
  cluster_id                = local.cluster_id
  instance_type             = var.machine_sizes.aws
  storage_gb                = var.storage_gb
  image                     = var.aws.image
  operating_system_image_id = var.aws.operating_system_image_id
  registry_repository_arn   = var.aws.registry_repository_arn
  region                    = var.aws.region
  subnet_ids                = var.aws.subnet_ids
  security_group_ids        = var.aws.security_group_ids
  kms_key_arn               = var.aws.kms_key_arn
  tags                      = local.tags

  depends_on = [terraform_data.inputs]
}

module "azure_members" {
  source = "./azure-members"
  count  = length(local.in_cloud.azure) > 0 ? 1 : 0

  name                         = var.name
  members                      = local.in_cloud.azure
  initial_cluster              = local.initial_cluster
  cluster_id                   = local.cluster_id
  vm_size                      = var.machine_sizes.azure
  storage_gb                   = var.storage_gb
  location                     = var.azure.location
  resource_group_name          = var.azure.resource_group_name
  image                        = var.azure.image
  operating_system_image_id    = var.azure.operating_system_image_id
  registry_id                  = var.azure.registry_id
  tenant_id                    = var.azure.tenant_id
  subnet_ids                   = var.azure.subnet_ids
  disk_encryption_set_id       = var.azure.disk_encryption_set_id
  key_vault_id                 = var.azure.key_vault_id
  administrator_ssh_public_key = var.azure.administrator_ssh_public_key
  tags                         = local.tags

  depends_on = [terraform_data.inputs]
}

module "gcp_members" {
  source = "./gcp-members"
  count  = length(local.in_cloud.gcp) > 0 ? 1 : 0

  members                = local.in_cloud.gcp
  initial_cluster        = local.initial_cluster
  cluster_id             = local.cluster_id
  machine_type           = var.machine_sizes.gcp
  storage_gb             = var.storage_gb
  region                 = var.gcp.region
  image                  = var.gcp.image
  operating_system_image = var.gcp.operating_system_image
  registry_repository_id = var.gcp.registry_repository_id
  subnet_ids             = var.gcp.subnet_ids
  kms_key_id             = var.gcp.kms_key_id
  labels                 = local.tags

  depends_on = [terraform_data.inputs]
}
