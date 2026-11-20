# The control plane layer of the Europe environment: the etcd cluster that decides which cell is
# home for each account and holds the leases. It reads the networks and private access layers
# from state, and makes what the control plane module takes but does not make: an encryption key
# in each cloud. It reads the image repositories from the registries layer. The etcd digest and
# the Flatcar release come from infra/images.json, which is reviewed in a pull request. The Azure
# machine image and the SSH public key come from variables with no default. Nothing in it is a
# secret.
locals {
  geo  = jsondecode(file("${path.module}/../deployment.json")).geo
  name = "ledger-cp"
}

module "regions" {
  source = "../../../modules/regions"
  geo    = local.geo
}

module "address_plan" {
  source = "../../../modules/address-plan"
  geo    = local.geo
}

provider "aws" {
  region = module.regions.aws[0]
}

provider "azurerm" {
  features {}
  subscription_id = var.azure_subscription_id

  # The plan identity only reads, so it cannot register resource providers. A person registers
  # them once, by hand (see infra/envs/README.md).
  resource_provider_registrations = "none"
}

provider "google" {
  region = module.regions.gcp[0]
}

# The state bucket is in the geo's AWS region.
data "terraform_remote_state" "network" {
  backend = "s3"
  config = {
    bucket = var.state_bucket
    key    = "${local.geo}/network"
    region = module.regions.aws[0]
  }
}

data "terraform_remote_state" "private_access" {
  backend = "s3"
  config = {
    bucket = var.state_bucket
    key    = "${local.geo}/private-access"
    region = module.regions.aws[0]
  }
}

data "terraform_remote_state" "registries" {
  backend = "s3"
  config = {
    bucket = var.state_bucket
    key    = "${local.geo}/registries"
    region = module.regions.aws[0]
  }
}

data "azurerm_client_config" "current" {}
data "google_project" "current" {}

locals {
  images         = jsondecode(file("${path.module}/../../../images.json"))
  etcd_digest    = local.images.etcd.digest
  flatcar        = local.images.flatcar
  flatcar_dashed = replace(local.flatcar.version, ".", "-")
}

# Flatcar's own images, looked up by the release pinned in the manifest. AWS: published by the
# Flatcar account. Google Cloud: published in the kinvolk-public project.
data "aws_ami" "flatcar" {
  owners      = ["075585003325"]
  most_recent = true

  filter {
    name   = "name"
    values = ["Flatcar-${local.flatcar.channel}-${local.flatcar.version}-hvm"]
  }

  filter {
    name   = "architecture"
    values = ["x86_64"]
  }
}

# Fetched by its exact name, which a person without permission to list the project can do:
# flatcar-<channel>-<version with dashes>, for example flatcar-stable-4757-2-3.
data "google_compute_image" "flatcar" {
  project = "kinvolk-public"
  name    = "flatcar-${local.flatcar.channel}-${local.flatcar_dashed}"
}

locals {
  network        = data.terraform_remote_state.network.outputs
  private_access = data.terraform_remote_state.private_access.outputs
  registries     = data.terraform_remote_state.registries.outputs
  plan           = module.address_plan

  # One digest, because each image is copied between registries and keeps its digest.
  aws_image   = "${local.registries.aws_repository_url}@${local.etcd_digest}"
  azure_image = "${local.private_access.azure_registry_login_server}/etcd@${local.etcd_digest}"
  gcp_image   = "${module.regions.gcp[0]}-docker.pkg.dev/${data.google_project.current.project_id}/${local.registries.gcp_repository_name}/etcd@${local.etcd_digest}"

  aws_input = {
    subnet_ids                = local.network.aws.subnet_ids["control_plane"]
    subnet_cidrs              = local.plan.aws.subnets["control_plane"]
    security_group_ids        = [local.network.aws.security_group_ids["control_plane"], local.private_access.aws_client_security_group_id]
    kms_key_arn               = aws_kms_key.control_plane.arn
    image                     = local.aws_image
    operating_system_image_id = data.aws_ami.flatcar.id
    registry_repository_arn   = local.registries.aws_repository_arn
    vpc_id                    = local.network.aws.vpc_id
    route_table_id            = local.network.aws.route_table_id
    region                    = module.regions.aws[0]
  }

  azure_input = {
    location                     = module.regions.azure[0]
    resource_group_name          = local.network.azure.resource_group_name
    subnet_ids                   = local.network.azure.subnet_ids["control_plane"]
    subnet_cidrs                 = local.plan.azure.subnets["control_plane"]
    disk_encryption_set_id       = azurerm_disk_encryption_set.control_plane.id
    key_vault_id                 = local.private_access.azure_key_vault_id
    administrator_ssh_public_key = var.administrator_ssh_public_key
    image                        = local.azure_image
    operating_system_image_id    = var.azure_flatcar_image_id
    registry_id                  = local.private_access.azure_registry_id
    virtual_network_id           = local.network.azure.network_id
    tenant_id                    = data.azurerm_client_config.current.tenant_id
  }

  gcp_input = {
    region                 = module.regions.gcp[0]
    network_id             = local.network.gcp.network_id
    subnet_ids             = local.network.gcp.subnet_ids["control_plane"]
    subnet_cidrs           = local.plan.gcp.subnets["control_plane"]
    kms_key_id             = google_kms_crypto_key.control_plane.id
    image                  = local.gcp_image
    operating_system_image = data.google_compute_image.flatcar.self_link
    registry_repository_id = local.registries.gcp_repository_id
  }
}

module "control_plane" {
  source = "../../../modules/control-plane"

  name  = local.name
  aws   = local.aws_input
  azure = local.azure_input
  gcp   = local.gcp_input

  # Proving sizes, chosen to keep the bill low while the architecture is tested: shared-CPU
  # machines with 2 vCPU and 4 GB, except Azure, which is 1 vCPU and 2 GB to fit the subscription's core quota. They run on CPU credits, so a long busy spell can slow etcd and
  # move its leader. Do not copy them into a production environment. The module's own defaults
  # (2 vCPU, 8 GB, steady CPU) are the production sizes, and this block is then deleted.
  machine_sizes = {
    aws   = "t3.medium"
    azure = "Standard_B1ms"
    gcp   = "e2-medium"
  }
}
