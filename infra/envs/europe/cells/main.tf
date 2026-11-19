# The database cells of the Europe environment for the first tenant: one self-hosted PostgreSQL
# cell in each cloud (three nodes in three zones, running the node image as a container), made
# from modules/postgres. It reads the networks, private access, registries and cell-first
# layers from state. It makes no key, repository or identity: those are the tenant's own, made
# by the cell-first layer, and the cells use them and no other tenant's. It uses the private
# endpoints and DNS zones that the private-access layer made. The node image's digest comes from
# the cell-first layer's outputs and is checked against infra/images.json. The Azure machine
# image and the SSH public key come from variables with no default. Nothing in it is a secret.
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

data "terraform_remote_state" "cell_first" {
  backend = "s3"
  config = {
    bucket = var.state_bucket
    key    = "${local.geo}/cell-first"
    region = module.regions.aws[0]
  }
}

data "azurerm_client_config" "current" {}

locals {
  images         = jsondecode(file("${path.module}/../../../images.json"))
  flatcar        = local.images.flatcar
  flatcar_dashed = replace(local.flatcar.version, ".", "-")
  postgres       = local.images.postgres

  network        = data.terraform_remote_state.network.outputs
  private_access = data.terraform_remote_state.private_access.outputs
  cell_first     = data.terraform_remote_state.cell_first.outputs
  plan           = module.address_plan

  # The tenant comes from the cell-first layer, so that the cells and the keys they use are one
  # tenant's.
  tenant = local.cell_first.tenant
  slug   = trimprefix(local.tenant, "ten_")
}

# The cell-first layer was given an image digest as a variable. It must be the manifest's.
resource "terraform_data" "image" {
  # The cells take their image addresses from here, so none is made until the check has passed.
  input = {
    aws   = local.cell_first.aws.image
    azure = local.cell_first.azure.image
    gcp   = local.cell_first.gcp.image
  }

  lifecycle {
    precondition {
      condition = alltrue([
        endswith(local.cell_first.aws.image, "@${local.postgres.digest}"),
        endswith(local.cell_first.azure.image, "@${local.postgres.digest}"),
        endswith(local.cell_first.gcp.image, "@${local.postgres.digest}"),
      ])
      error_message = "The cell-first layer holds a different Postgres image digest from infra/images.json. Apply it again with TF_VAR_postgres_image_digest set to the manifest's digest."
    }
  }
}

# Flatcar's own images, looked up by the release pinned in the manifest (as the control plane does).
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
