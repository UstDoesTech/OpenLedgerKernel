# A cell's Postgres, in one of three forms behind one interface. Every form is vanilla
# PostgreSQL 16 with no extensions and no logical replication slots, so the kernel cannot tell
# them apart. Each gives synchronous standbys in other zones of its region, a private address
# only, and storage encrypted with the tenant's own key. Outputs are a host, a port and a
# reference to where the administrator's credential is held, never a credential.
#
#   lakebase     Databricks Lakebase. The default where a databricks input is given.
#   managed      The cloud's own service: RDS Multi-AZ cluster, Azure zone-redundant flexible
#                server, Cloud SQL regional instance.
#   self_hosted  Three virtual machines, one per zone, running the node image as a container.
locals {
  kind = coalesce(var.provider_kind, var.databricks != null ? "lakebase" : "managed")

  # One size name, one machine on each cloud. Lakebase counts compute units.
  sizes = {
    small  = { aws_db = "db.m6gd.large", azure_db = "GP_Standard_D2ds_v5", gcp_db = "db-custom-2-7680", aws_vm = "m6i.large", azure_vm = "Standard_D2ds_v5", gcp_vm = "n2-standard-2", lakebase_cu = 4 }
    medium = { aws_db = "db.m6gd.xlarge", azure_db = "GP_Standard_D4ds_v5", gcp_db = "db-custom-4-15360", aws_vm = "m6i.xlarge", azure_vm = "Standard_D4ds_v5", gcp_vm = "n2-standard-4", lakebase_cu = 8 }
  }
  size = local.sizes[var.size]
  tags = merge(var.tags, { tenant = var.tenant, cell = var.name, managed_by = "opentofu" })
}

resource "terraform_data" "inputs" {
  lifecycle {
    precondition {
      condition     = local.kind != "lakebase" || (var.databricks != null && try(var.databricks.private_link, false) && try(var.databricks.customer_managed_key, false))
      error_message = "Lakebase needs the databricks input, with private_link and customer_managed_key both true: the tenant's workspace must use Private Link and have a customer-managed key configuration bound to it."
    }

    precondition {
      condition     = local.kind == "lakebase" || (var.cloud == "aws" && var.aws != null) || (var.cloud == "azure" && var.azure != null) || (var.cloud == "gcp" && var.gcp != null)
      error_message = "Give the input for the chosen cloud: aws, azure or gcp."
    }

    precondition {
      condition = local.kind != "self_hosted" || (
        (var.cloud == "aws" && try(var.aws.image, null) != null) ||
        (var.cloud == "azure" && try(var.azure.image, null) != null) ||
        (var.cloud == "gcp" && try(var.gcp.image, null) != null)
      )
      error_message = "A self-hosted cell needs the node image, by digest, in the cloud's input."
    }

    precondition {
      condition = local.kind != "self_hosted" || (
        (var.cloud == "aws" && alltrue([for k in ["operating_system_image_id", "registry_repository_arn", "vpc_id", "route_table_id", "region"] : try(var.aws[k], null) != null])) ||
        (var.cloud == "azure" && alltrue([for k in ["operating_system_image_id", "registry_id", "virtual_network_id", "tenant_id"] : try(var.azure[k], null) != null])) ||
        (var.cloud == "gcp" && alltrue([for k in ["operating_system_image", "registry_repository_id"] : try(var.gcp[k], null) != null]))
      )
      error_message = "A self-hosted cell also needs the Flatcar machine image and the private registry details in the cloud's input: AWS operating_system_image_id, registry_repository_arn, vpc_id, route_table_id and region; Azure operating_system_image_id, registry_id, virtual_network_id and tenant_id; Google operating_system_image and registry_repository_id."
    }

    precondition {
      condition     = local.kind != "managed" || var.cloud != "azure" || try(var.azure.private_dns_zone_id, null) != null
      error_message = "Azure managed PostgreSQL needs private_dns_zone_id, from modules/postgres/azure-dns."
    }
  }
}

module "aws_managed" {
  source = "./aws-managed"
  count  = var.cloud == "aws" && local.kind == "managed" ? 1 : 0

  name                  = var.name
  instance_class        = local.size.aws_db
  storage_gb            = var.storage_gb
  subnet_ids            = var.aws.subnet_ids
  security_group_ids    = var.aws.security_group_ids
  kms_key_arn           = var.aws.kms_key_arn
  backup_retention_days = var.backup_retention_days
  deletion_protection   = var.deletion_protection
  tags                  = local.tags
}

module "azure_managed" {
  source = "./azure-managed"
  count  = var.cloud == "azure" && local.kind == "managed" ? 1 : 0

  name                  = var.name
  sku_name              = local.size.azure_db
  storage_gb            = var.storage_gb
  location              = var.azure.location
  resource_group_name   = var.azure.resource_group_name
  subnet_id             = var.azure.subnet_ids[0]
  private_dns_zone_id   = var.azure.private_dns_zone_id
  key_vault_key_id      = var.azure.key_vault_key_id
  identity_id           = var.azure.identity_id
  administrator         = var.azure.administrator
  backup_retention_days = var.backup_retention_days
  tags                  = local.tags
}

module "gcp_managed" {
  source = "./gcp-managed"
  count  = var.cloud == "gcp" && local.kind == "managed" ? 1 : 0

  name                  = var.name
  tier                  = local.size.gcp_db
  storage_gb            = var.storage_gb
  region                = var.gcp.region
  network_id            = var.gcp.network_id
  subnet_id             = var.gcp.subnet_ids[0]
  kms_key_id            = var.gcp.kms_key_id
  administrator_email   = var.gcp.administrator_email
  backup_retention_days = var.backup_retention_days
  deletion_protection   = var.deletion_protection
  labels                = local.tags
}

module "lakebase" {
  source = "./lakebase"
  count  = local.kind == "lakebase" ? 1 : 0

  name                         = var.name
  compute_units                = local.size.lakebase_cu
  administrator_application_id = var.databricks.administrator_application_id
  deletion_protection          = var.deletion_protection
}

module "aws_self_hosted" {
  source = "./aws-self-hosted"
  count  = var.cloud == "aws" && local.kind == "self_hosted" ? 1 : 0

  name                      = var.name
  tenant                    = var.tenant
  instance_type             = coalesce(var.machine_sizes.aws, local.size.aws_vm)
  storage_gb                = var.storage_gb
  image                     = var.aws.image
  operating_system_image_id = var.aws.operating_system_image_id
  registry_repository_arn   = var.aws.registry_repository_arn
  vpc_id                    = var.aws.vpc_id
  region                    = var.aws.region
  route_table_id            = var.aws.route_table_id
  subnet_ids                = var.aws.subnet_ids
  subnet_cidrs              = var.aws.subnet_cidrs
  security_group_ids        = var.aws.security_group_ids
  kms_key_arn               = var.aws.kms_key_arn
  reuse_private_access      = var.reuse_private_access
  tags                      = local.tags
}

module "azure_self_hosted" {
  source = "./azure-self-hosted"
  count  = var.cloud == "azure" && local.kind == "self_hosted" ? 1 : 0

  name                         = var.name
  tenant                       = var.tenant
  vm_size                      = coalesce(var.machine_sizes.azure, local.size.azure_vm)
  storage_gb                   = var.storage_gb
  location                     = var.azure.location
  resource_group_name          = var.azure.resource_group_name
  image                        = var.azure.image
  operating_system_image_id    = var.azure.operating_system_image_id
  registry_id                  = var.azure.registry_id
  virtual_network_id           = var.azure.virtual_network_id
  tenant_id                    = var.azure.tenant_id
  subnet_ids                   = var.azure.subnet_ids
  subnet_cidrs                 = var.azure.subnet_cidrs
  disk_encryption_set_id       = var.azure.disk_encryption_set_id
  key_vault_id                 = var.azure.key_vault_id
  administrator_ssh_public_key = var.azure.administrator_ssh_public_key
  reuse_private_access         = var.reuse_private_access
  tags                         = local.tags
}

module "gcp_self_hosted" {
  source = "./gcp-self-hosted"
  count  = var.cloud == "gcp" && local.kind == "self_hosted" ? 1 : 0

  name                   = var.name
  tenant                 = var.tenant
  machine_type           = coalesce(var.machine_sizes.gcp, local.size.gcp_vm)
  storage_gb             = var.storage_gb
  region                 = var.gcp.region
  image                  = var.gcp.image
  operating_system_image = var.gcp.operating_system_image
  registry_repository_id = var.gcp.registry_repository_id
  network_id             = var.gcp.network_id
  subnet_ids             = var.gcp.subnet_ids
  subnet_cidrs           = var.gcp.subnet_cidrs
  kms_key_id             = var.gcp.kms_key_id
  reuse_private_access   = var.reuse_private_access
  labels                 = local.tags
}
