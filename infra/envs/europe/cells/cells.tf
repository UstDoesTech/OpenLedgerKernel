# One cell in each cloud, self-hosted by default or the cloud's own managed PostgreSQL when
# var.cell_kind says so. The sizes are proving sizes, chosen to keep the bill low
# while the architecture is tested: shared-CPU machines. They run on CPU credits. Do not copy
# them into a production environment, where the module's size table applies (delete the
# machine_sizes lines).
locals {
  aws_input = {
    subnet_ids                = local.network.aws.subnet_ids["cell"]
    subnet_cidrs              = local.plan.aws.subnets["cell"]
    security_group_ids        = [local.network.aws.security_group_ids["cell"], local.private_access.aws_client_security_group_id]
    kms_key_arn               = local.cell_first.aws.kms_key_arn
    image                     = terraform_data.image.output.aws
    operating_system_image_id = data.aws_ami.flatcar.id
    registry_repository_arn   = local.cell_first.aws.registry_repository_arn
    vpc_id                    = local.network.aws.vpc_id
    route_table_id            = local.network.aws.route_table_id
    region                    = module.regions.aws[0]
  }

  azure_input = {
    location                     = module.regions.azure[0]
    resource_group_name          = local.network.azure.resource_group_name
    subnet_ids                   = local.network.azure.subnet_ids["cell"]
    subnet_cidrs                 = local.plan.azure.subnets["cell"]
    private_dns_zone_id          = try(module.azure_dns[0].private_dns_zone_id, null)
    key_vault_key_id             = local.cell_first.azure.key_vault_key_id
    identity_id                  = local.cell_first.azure.identity_id
    disk_encryption_set_id       = local.cell_first.azure.disk_encryption_set_id
    key_vault_id                 = local.private_access.azure_key_vault_id
    administrator_ssh_public_key = var.administrator_ssh_public_key
    image                        = terraform_data.image.output.azure
    operating_system_image_id    = var.azure_flatcar_image_id
    registry_id                  = local.private_access.azure_registry_id
    virtual_network_id           = local.network.azure.network_id
    tenant_id                    = data.azurerm_client_config.current.tenant_id
    administrator                = local.cell_first.azure.administrator
  }

  gcp_input = {
    region                 = module.regions.gcp[0]
    network_id             = local.network.gcp.network_id
    subnet_ids             = local.network.gcp.subnet_ids["cell"]
    subnet_cidrs           = local.plan.gcp.subnets["cell"]
    kms_key_id             = local.cell_first.gcp.kms_key_id
    administrator_email    = local.cell_first.gcp.administrator_email
    image                  = terraform_data.image.output.gcp
    operating_system_image = data.google_compute_image.flatcar.self_link
    registry_repository_id = local.cell_first.gcp.registry_repository_id
  }
}

module "cell_aws" {
  source = "../../../modules/postgres"

  name                 = "${local.slug}-aws-001"
  tenant               = local.tenant
  cloud                = "aws"
  provider_kind        = var.cell_kind
  deletion_protection  = var.deletion_protection
  aws                  = local.aws_input
  reuse_private_access = true
  machine_sizes        = { aws = "t3.medium" }
}

module "cell_azure" {
  source = "../../../modules/postgres"

  name                 = "${local.slug}-azure-001"
  tenant               = local.tenant
  cloud                = "azure"
  provider_kind        = var.cell_kind
  deletion_protection  = var.deletion_protection
  azure                = local.azure_input
  reuse_private_access = true
  machine_sizes        = { azure = "Standard_B2s" }
}

module "cell_gcp" {
  source = "../../../modules/postgres"

  name                 = "${local.slug}-gcp-001"
  tenant               = local.tenant
  cloud                = "gcp"
  provider_kind        = var.cell_kind
  deletion_protection  = var.deletion_protection
  gcp                  = local.gcp_input
  reuse_private_access = true
  machine_sizes        = { gcp = "e2-medium" }
}

# The managed forms need two things the cells do not make for themselves. Azure's private DNS
# zone for flexible servers, made once for the network. And Cloud SQL's service agent must be
# allowed to use the tenant's key, as the Compute Engine and Secret Manager agents are in the
# cell-first layer. Neither is made for self-hosted cells.
module "azure_dns" {
  source = "../../../modules/postgres/azure-dns"
  count  = var.cell_kind == "managed" ? 1 : 0

  resource_group_name = local.network.azure.resource_group_name
  virtual_network_id  = local.network.azure.network_id
  name                = "${local.slug}-postgres"
  tags                = { tenant = local.tenant, managed_by = "opentofu" }
}

data "google_project" "current" {}

resource "google_kms_crypto_key_iam_member" "cloud_sql" {
  count = var.cell_kind == "managed" ? 1 : 0

  crypto_key_id = local.cell_first.gcp.kms_key_id
  role          = "roles/cloudkms.cryptoKeyEncrypterDecrypter"
  member        = "serviceAccount:service-${data.google_project.current.number}@gcp-sa-cloud-sql.iam.gserviceaccount.com"
}
