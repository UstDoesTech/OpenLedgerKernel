# The private access layer of the Europe environment: private endpoints, so machines with no
# internet route can reach the container registry and the secret store. It reads the networks
# layer's outputs from that layer's state, and makes the Azure registry and Key Vault that the
# Azure module takes as inputs. Nothing in it is a secret.
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
  plan    = module.address_plan
  # The machines that reach the endpoints sit in the cell and control plane tiers.
  client_cidrs = concat(local.plan.aws.subnets["cell"], local.plan.aws.subnets["control_plane"])
}

module "private_access_aws" {
  source = "../../../modules/private-access-aws"

  name           = local.name
  vpc_id         = local.network.aws.vpc_id
  region         = module.regions.aws[0]
  subnet_ids     = local.network.aws.subnet_ids["control_plane"]
  client_cidrs   = local.client_cidrs
  route_table_id = local.network.aws.route_table_id
}

module "private_access_azure" {
  source = "../../../modules/private-access-azure"

  name                = local.name
  location            = module.regions.azure[0]
  resource_group_name = local.network.azure.resource_group_name
  virtual_network_id  = local.network.azure.network_id
  subnet_id           = local.network.azure.subnet_ids["control_plane"][0]
  registry_id         = azurerm_container_registry.this.id
  key_vault_id        = azurerm_key_vault.this.id

  # The machines that use the endpoints: the cells, the API and the control plane. Each may send
  # HTTPS to the endpoints' subnets, which are in the control plane tier.
  client_groups = {
    for tier in ["cell", "api", "control_plane"] : tier => {
      network_security_group_name = local.network.azure.network_security_group_names[tier]
      cidrs                       = local.plan.azure.subnets[tier]
    }
  }
  endpoint_cidrs                       = local.plan.azure.subnets["control_plane"]
  endpoint_network_security_group_name = local.network.azure.network_security_group_names["control_plane"]
}

module "private_access_gcp" {
  source = "../../../modules/private-access-gcp"

  name       = local.name
  network_id = local.network.gcp.network_id
}
