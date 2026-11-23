# The identities the rotate-credentials workflow signs in as, one in each cloud, made here so no
# person makes them by hand (decision D-18). Each trusts only the repository's rotate-credentials
# environment, and each may do only what infra/rotation-identity.json lists, on the resources of this
# environment's API secrets and authority. Nothing here is a secret: the layer's outputs are
# identifiers that a person enters, as the environment's secrets, in GitHub, which OpenTofu cannot
# do (docs/credentialed-workflows.md).
locals {
  geo      = jsondecode(file("${path.module}/../deployment.json")).geo
  identity = jsondecode(file("${path.module}/../../../rotation-identity.json"))

  subject_prefix = var.oidc_subject_prefix == "" ? "repo:${local.identity.repository}" : var.oidc_subject_prefix

  plan_store        = regex("^s3://(?P<bucket>[a-z0-9.-]+)/?(?P<prefix>[A-Za-z0-9._/-]*)$", var.plan_store)
  plan_store_prefix = trim(local.plan_store.prefix, "/")
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

  # A run does not register resource providers. A person registers them once, by hand (see
  # infra/envs/README.md).
  resource_provider_registrations = "none"
}

provider "google" {
  region = module.regions.gcp[0]
}

data "terraform_remote_state" "private_access" {
  backend = "s3"
  config = {
    bucket = var.state_bucket
    key    = "${local.geo}/private-access"
    region = module.regions.aws[0]
  }
}

data "terraform_remote_state" "cells" {
  backend = "s3"
  config = {
    bucket = var.state_bucket
    key    = "${local.geo}/cells"
    region = module.regions.aws[0]
  }
}

data "terraform_remote_state" "api" {
  backend = "s3"
  config = {
    bucket = var.state_bucket
    key    = "${local.geo}/api"
    region = module.regions.aws[0]
  }
}

data "terraform_remote_state" "authority" {
  backend = "s3"
  config = {
    bucket = var.state_bucket
    key    = "${local.geo}/authority"
    region = module.regions.aws[0]
  }
}

locals {
  apis  = data.terraform_remote_state.api.outputs.apis
  cells = data.terraform_remote_state.cells.outputs.cells
}

module "aws" {
  source = "../../../modules/rotation-identity/aws"

  name                      = local.identity.name
  environment               = local.identity.environment
  subject_prefix            = local.subject_prefix
  authority_key_arn         = data.terraform_remote_state.authority.outputs.key_id
  authority_certificate_arn = data.terraform_remote_state.authority.outputs.certificate_secret
  api_secret_arn            = local.apis["aws"].secret_ref
  administrator_secret_arn  = var.proving ? local.cells["aws"].secret_ref : ""
  encryption_arn            = var.encryption_arn
  state_encryption_arn      = var.state_encryption_arn
  plan_store_encryption_arn = var.plan_store_encryption_arn
  state_bucket              = var.state_bucket
  state_keys                = ["${local.geo}/api", "${local.geo}/authority", "${local.geo}/cells"]
  node_secret_arns          = [for n in try(local.cells["aws"].nodes, []) : n.secret_ref]
  plan_store_bucket         = local.plan_store.bucket
  plan_store_prefix         = local.plan_store_prefix
}

module "azure" {
  source = "../../../modules/rotation-identity/azure"

  name           = local.identity.name
  location       = module.regions.azure[0]
  environment    = local.identity.environment
  subject_prefix = local.subject_prefix
  vault_id       = data.terraform_remote_state.private_access.outputs.azure_key_vault_id
}

module "gcp" {
  source = "../../../modules/rotation-identity/gcp"

  name                    = local.identity.name
  environment             = local.identity.environment
  workload_identity_pool  = var.workload_identity_pool
  api_secret_id           = local.apis["gcp"].secret_ref
  administrator_secret_id = var.proving ? local.cells["gcp"].secret_ref : ""
  node_secret_ids         = [for n in try(local.cells["gcp"].nodes, []) : n.secret_ref]
}
