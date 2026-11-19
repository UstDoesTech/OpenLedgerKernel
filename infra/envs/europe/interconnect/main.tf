# The interconnect layer of the Europe environment: IPsec tunnels with BGP between the three
# networks. It reads the networks layer's outputs from that layer's state. The only secret is
# the tunnels' pre-shared keys, which come from a variable with no default.
locals {
  geo  = jsondecode(file("${path.module}/../deployment.json")).geo
  name = "ledger-${local.geo}"
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

locals {
  network = data.terraform_remote_state.network.outputs
}

locals {
  aws_network = {
    vpc_id             = local.network.aws.vpc_id
    route_table_id     = local.network.aws.route_table_id
    security_group_ids = local.network.aws.security_group_ids
  }

  azure_network = {
    location                     = module.regions.azure[0]
    resource_group_name          = local.network.azure.resource_group_name
    network_name                 = local.network.azure.network_name
    network_security_group_names = local.network.azure.network_security_group_names
  }

  gcp_network = {
    region       = module.regions.gcp[0]
    network_name = local.network.gcp.network_name
  }
}

module "interconnect" {
  source = "../../../modules/interconnect"

  name = local.name

  plans = {
    aws   = module.address_plan.aws
    azure = module.address_plan.azure
    gcp   = module.address_plan.gcp
  }

  aws            = local.aws_network
  azure          = local.azure_network
  gcp            = local.gcp_network
  preshared_keys = var.preshared_keys
}
