# The registries layer of the Europe environment: a private repository for the etcd image in
# AWS and in Google Cloud. They are made before the control plane layer so the image can be
# published first (see infra/envs/README.md). The Azure registry is made by the private access
# layer, and its repository comes into being when the image is pushed. Nothing in it is a secret.
locals {
  geo = jsondecode(file("${path.module}/../deployment.json")).geo
}

module "regions" {
  source = "../../../modules/regions"
  geo    = local.geo
}

provider "aws" {
  region = module.regions.aws[0]
}

provider "google" {
  region = module.regions.gcp[0]
}

# The state bucket is in the geo's AWS region.
data "terraform_remote_state" "private_access" {
  backend = "s3"
  config = {
    bucket = var.state_bucket
    key    = "${local.geo}/private-access"
    region = module.regions.aws[0]
  }
}

data "google_project" "current" {}
