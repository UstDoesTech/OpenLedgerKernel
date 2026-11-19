# The networks layer of the Europe environment: the regions, the address plan and the three
# private networks. Nothing in it is a secret. Each cloud signs in through the federated
# identity the infra-apply workflow sets up, and the Google Cloud project comes from that
# identity (GOOGLE_CLOUD_PROJECT).
module "regions" {
  source = "../../../modules/regions"
  geo    = local.geo
}

module "address_plan" {
  source = "../../../modules/address-plan"
  geo    = local.geo
}

locals {
  geo  = jsondecode(file("${path.module}/../deployment.json")).geo
  name = "ledger-${local.geo}"
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

module "network_aws" {
  source = "../../../modules/network-aws"

  name   = local.name
  region = module.regions.aws[0]
  plan   = module.address_plan.aws
}

module "network_azure" {
  source = "../../../modules/network-azure"

  name     = local.name
  location = module.regions.azure[0]
  plan     = module.address_plan.azure
}

module "network_gcp" {
  source = "../../../modules/network-gcp"

  name   = local.name
  region = module.regions.gcp[0]
  plan   = module.address_plan.gcp
}
