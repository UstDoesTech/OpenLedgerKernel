# The API machines of the Europe environment: one small machine next to each of the first
# tenant's database cells, in each cloud, from modules/api. It reads the networks, private
# access, control plane and cell-first layers from state. The machines have private addresses
# only, in the api tier. They reach their own cell's database, etcd and the other cells'
# machines over the flows in infra/flows.json, and nothing else. The image's digest is the
# manifest's (infra/images.json), and the layer stops if the cell-first layer holds a different
# one. The secrets the machines read are made empty here; a person fills them (see
# docs/runbooks/first-entry.md). Nothing in this layer is a secret.
locals {
  geo = jsondecode(file("${path.module}/../deployment.json")).geo
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

data "terraform_remote_state" "control_plane" {
  backend = "s3"
  config = {
    bucket = var.state_bucket
    key    = "${local.geo}/control-plane"
    region = module.regions.aws[0]
  }
}

data "terraform_remote_state" "cell_first" {
  backend = "s3"
  config = {
    bucket = var.state_bucket
    key    = "${local.geo}/cell-first"
    region = module.regions.aws[0]
  }
}

# The cells layer says whether each cell is managed and, if so, names its cluster, so that the
# AWS machine may sign in to that one cluster (K-24ba). Apply cells before this layer.
data "terraform_remote_state" "cells" {
  backend = "s3"
  config = {
    bucket = var.state_bucket
    key    = "${local.geo}/cells"
    region = module.regions.aws[0]
  }
}

data "azurerm_client_config" "current" {}

locals {
  images  = jsondecode(file("${path.module}/../../../images.json"))
  flatcar = local.images.flatcar
  api     = local.images.api

  flatcar_dashed = replace(local.flatcar.version, ".", "-")

  network        = data.terraform_remote_state.network.outputs
  private_access = data.terraform_remote_state.private_access.outputs
  control_plane  = data.terraform_remote_state.control_plane.outputs
  cell_first     = data.terraform_remote_state.cell_first.outputs
  cells          = data.terraform_remote_state.cells.outputs.cells
  plan           = module.address_plan

  # The tenant comes from the cell-first layer, so that these machines and the cells they sit
  # next to are one tenant's. The cells layer names its cells "<slug>-<cloud>-001", and the
  # service must be told the same name.
  tenant = local.cell_first.tenant
  slug   = trimprefix(local.tenant, "ten_")

  etcd_endpoints = tolist(local.control_plane.endpoints)
}

# The cell-first layer builds the image addresses from the manifest's digest. If it was applied
# before the manifest changed, it holds another one, and no machine may take it.
resource "terraform_data" "image" {
  # The machines take their image addresses from here, so none is made until the check passes.
  input = {
    aws   = local.cell_first.api.aws.image
    azure = local.cell_first.api.azure.image
    gcp   = local.cell_first.api.gcp.image
  }

  lifecycle {
    precondition {
      condition = alltrue([
        endswith(local.cell_first.api.aws.image, "@${local.api.digest}"),
        endswith(local.cell_first.api.azure.image, "@${local.api.digest}"),
        endswith(local.cell_first.api.gcp.image, "@${local.api.digest}"),
      ])
      error_message = "The cell-first layer holds a different API image digest from infra/images.json. Apply cell-first again, then publish the image with publish-images."
    }
  }
}

# Flatcar's own images, looked up by the release pinned in the manifest (as the cells do).
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

data "google_compute_image" "flatcar" {
  project = "kinvolk-public"
  name    = "flatcar-${local.flatcar.channel}-${local.flatcar_dashed}"
}

locals {
  # The machines are small. The proving environment holds no real traffic.
  names = {
    aws   = "${local.slug}-aws-001"
    azure = "${local.slug}-azure-001"
    gcp   = "${local.slug}-gcp-001"
  }
}

# The way in from the internet for the load balancer (D-22, K-23a): an internet gateway and a route
# table for the edge subnets only. Off until the load balancer's module (K-23b) turns it on, so
# nothing about the network changes by adding this.
module "edge_aws" {
  source = "../../../modules/edge-aws"
  count  = var.edge_enabled ? 1 : 0

  name       = "${local.names.aws}-api"
  vpc_id     = local.network.aws.vpc_id
  subnet_ids = local.network.aws.subnet_ids["edge"]
  tags       = { managed_by = "opentofu", component = "edge", tenant = local.tenant }
}

# The load balancer in front of the AWS API (D-22, K-23b). Off until the owner gives a name. It is
# made in two steps: first the certificate, whose DNS record is printed, then, with
# certificate_validated true, the load balancer. See docs/runbooks/exposing-the-api-in-aws.md.
module "front_aws" {
  source = "../../../modules/api-front-aws"
  count  = var.front_domain_name != "" ? 1 : 0

  name                   = "${local.names.aws}-api"
  vpc_id                 = local.network.aws.vpc_id
  edge_subnet_ids        = local.network.aws.subnet_ids["edge"]
  edge_security_group_id = local.network.aws.security_group_ids["edge"]
  api_security_group_id  = local.network.aws.security_group_ids["api"]
  api_port               = local.client_port
  target_instance_id     = module.api_aws.instance_id
  domain_name            = var.front_domain_name
  certificate_validated  = var.certificate_validated
  internet_gateway_id    = try(module.edge_aws[0].internet_gateway_id, "")
  edge_client_cidrs      = var.edge_client_cidrs
  open_to_internet       = var.open_to_internet
  tags                   = { managed_by = "opentofu", component = "api-front", tenant = local.tenant }
}

module "api_aws" {
  source = "../../../modules/api/aws"

  name                      = "${local.names.aws}-api"
  cell                      = local.names.aws
  image                     = terraform_data.image.output.aws
  operating_system_image_id = data.aws_ami.flatcar.id
  registry_repository_arn   = local.cell_first.api.aws.registry_repository_arn
  region                    = module.regions.aws[0]
  subnet_id                 = local.network.aws.subnet_ids["api"][0]
  security_group_ids        = [local.network.aws.security_group_ids["api"], local.private_access.aws_client_security_group_id]
  kms_key_arn               = local.cell_first.aws.kms_key_arn
  etcd_endpoints            = local.etcd_endpoints
  trusted_proxy_cidrs       = local.plan.aws.subnets["edge"]
  database_resource_id      = try(local.cells["aws"].kind == "managed" ? local.cells["aws"].database_resource_id : null, null)
  tags                      = { managed_by = "opentofu", component = "api", tenant = local.tenant }
  migrate_database          = var.migrate_database
}

module "api_azure" {
  source = "../../../modules/api/azure"

  name                         = "${local.names.azure}-api"
  cell                         = local.names.azure
  image                        = terraform_data.image.output.azure
  operating_system_image_id    = var.azure_flatcar_image_id
  location                     = module.regions.azure[0]
  resource_group_name          = local.network.azure.resource_group_name
  subnet_id                    = local.network.azure.subnet_ids["api"][0]
  registry_id                  = local.private_access.azure_registry_id
  key_vault_id                 = local.private_access.azure_key_vault_id
  tenant_id                    = data.azurerm_client_config.current.tenant_id
  disk_encryption_set_id       = local.cell_first.azure.disk_encryption_set_id
  administrator_ssh_public_key = var.administrator_ssh_public_key
  etcd_endpoints               = local.etcd_endpoints
  tags                         = { managed_by = "opentofu", component = "api", tenant = local.tenant }
  migrate_database             = var.migrate_database
}

module "api_gcp" {
  source = "../../../modules/api/gcp"

  name                   = "${local.names.gcp}-api"
  cell                   = local.names.gcp
  image                  = terraform_data.image.output.gcp
  operating_system_image = data.google_compute_image.flatcar.self_link
  region                 = module.regions.gcp[0]
  subnet_id              = local.network.gcp.subnet_ids["api"][0]
  kms_key_id             = local.cell_first.gcp.kms_key_id
  registry_repository_id = local.cell_first.api.gcp.registry_repository_id
  etcd_endpoints         = local.etcd_endpoints
  labels                 = { managed_by = "opentofu", component = "api", tenant = replace(local.tenant, "_", "-") }
  migrate_database       = var.migrate_database
}
