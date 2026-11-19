# The first tenant's cell layer of the Europe environment: what that tenant's database cells
# need and that nothing else may share. In each cloud: an encryption key of the tenant's own, a
# repository for the PostgreSQL node image, and the cell administrator identity. It reads the
# networks, private access and registries layers from state. The database cells themselves
# follow in a later layer, which takes these as inputs. The tenant's id is written here because a
# person gave it (an example customer). The image digest and the administrator group come from
# variables with no default. Nothing in it is a secret.
locals {
  geo    = jsondecode(file("${path.module}/../deployment.json")).geo
  tenant = "ten_example"

  # The tenant id without its prefix, and the forms each cloud's naming rules allow.
  slug   = trimprefix(local.tenant, "ten_")
  name   = "cell-${local.slug}"
  hyphen = replace(local.tenant, "_", "-")

  tags = { managed_by = "opentofu", component = "cell", tenant = local.tenant }
}

resource "terraform_data" "tenant" {
  lifecycle {
    precondition {
      condition     = can(regex("^ten_[0-9a-z]{1,24}$", local.tenant))
      error_message = "A tenant id is \"ten_\" followed by 1 to 24 lower-case letters or digits."
    }
  }
}

module "regions" {
  source = "../../../modules/regions"
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

data "google_project" "current" {}
