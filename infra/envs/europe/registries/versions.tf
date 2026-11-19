terraform {
  required_version = ">= 1.10.0"

  # The infra-apply workflow supplies the bucket, region and state path when it runs
  # `tofu init`, so nothing about the state is written here.
  backend "s3" {}

  required_providers {
    aws = {
      source  = "hashicorp/aws"
      version = "~> 6.0"
    }
    google = {
      source  = "hashicorp/google"
      version = "~> 6.0"
    }
  }
}
